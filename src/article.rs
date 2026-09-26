use crate::model::{Article, Document, Sort};
use anyhow::{Result, bail};
use encoding_rs::{Encoding, ISO_2022_JP, UTF_8};
use mailparse::MailHeaderMap;
use std::collections::{HashMap, HashSet};

pub fn decode_bytes(raw: &[u8], label: Option<&str>) -> (String, bool) {
    let encoding = label
        .and_then(|s| Encoding::for_label(s.as_bytes()))
        .unwrap_or_else(|| {
            if raw.windows(2).any(|x| x == b"\x1b$" || x == b"\x1b(") {
                ISO_2022_JP
            } else {
                UTF_8
            }
        });
    let (text, _, errors) = encoding.decode(raw);
    (text.into_owned(), errors)
}

pub fn header_text(raw: &[u8]) -> String {
    // MIME encoded words are decoded by mailparse; bare historical JIS headers need a fallback.
    let raw = decode_bytes(raw, None).0;
    let line = format!("X: {raw}");
    mailparse::parse_header(line.as_bytes())
        .map(|(h, _)| h.get_value())
        .unwrap_or(raw)
}

pub fn parse_overview(line: &[u8]) -> Result<Article> {
    let fields: Vec<_> = line.split(|x| *x == b'\t').collect();
    if fields.len() < 8 {
        bail!("記事概要の形式が不正です");
    }
    let number: u64 = std::str::from_utf8(fields[0])?.parse()?;
    if number == 0 || number > i64::MAX as u64 {
        bail!("記事番号が範囲外です");
    }
    let date = header_text(fields[3]);
    let id = String::from_utf8_lossy(fields[4]).trim().to_owned();
    if !valid_message_id(&id) {
        bail!("記事のMessage-IDが不正です");
    }
    Ok(Article {
        number,
        subject: header_text(fields[1]),
        author: header_text(fields[2]),
        timestamp: mailparse::dateparse(&date).unwrap_or_default(),
        date,
        message_id: id,
        references: message_ids(&String::from_utf8_lossy(fields[5])),
        ..Default::default()
    })
}

pub fn valid_message_id(id: &str) -> bool {
    id.len() >= 3
        && id.len() <= 998
        && id.starts_with('<')
        && id.ends_with('>')
        && id[1..id.len() - 1]
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'<' && b != b'>')
}
pub fn message_ids(s: &str) -> Vec<String> {
    s.split_whitespace()
        .filter(|s| valid_message_id(s))
        .take(256)
        .map(str::to_owned)
        .collect()
}

pub fn decode_article(raw: &[u8], charset: Option<&str>) -> Result<Document> {
    // Bound nesting before calling the recursive MIME parser.
    if raw
        .windows(8)
        .filter(|w| w.eq_ignore_ascii_case(b"boundary"))
        .count()
        > 64
    {
        bail!("MIMEの入れ子が上限を超えています（元データは保存済み）");
    }
    let mail = mailparse::parse_mail(raw)?;
    let h = &mail.headers;
    let header_end = raw
        .windows(4)
        .position(|x| x == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|x| x == b"\n\n"))
        .unwrap_or(raw.len());
    let mut doc = Document {
        subject: h
            .get_first_header("Subject")
            .map(|h| header_text(h.get_value_raw()))
            .unwrap_or_default(),
        author: h
            .get_first_header("From")
            .map(|h| header_text(h.get_value_raw()))
            .unwrap_or_default(),
        date: h.get_first_value("Date").unwrap_or_default(),
        groups: h.get_first_value("Newsgroups").unwrap_or_default(),
        headers: decode_bytes(&raw[..header_end], charset).0,
        ..Default::default()
    };
    let mut parts = vec![&mail];
    let mut texts = Vec::new();
    let mut errors = false;
    while let Some(part) = parts.pop() {
        if part.get_content_disposition().disposition == mailparse::DispositionType::Attachment {
            continue;
        }
        if part.subparts.is_empty() && part.ctype.mimetype.eq_ignore_ascii_case("text/plain") {
            let data = part.get_body_raw()?;
            let label = charset.or_else(|| part.ctype.params.get("charset").map(String::as_str));
            let (text, bad) = decode_bytes(&data, label);
            errors |= bad;
            texts.push(text);
        } else {
            parts.extend(part.subparts.iter().rev());
        }
    }
    if texts.is_empty() {
        doc.warning = Some("対応するテキスト本文がありません。HTML・添付ファイルは表示しません。「元データ」で確認できます。".into());
    } else if errors {
        doc.warning = Some("復号できない文字があります。文字コードを選び直してください。".into());
    }
    doc.body = texts.join("\n\n").replace("\r\n", "\n");
    Ok(doc)
}

/// Iterative forest traversal: no recursion, each article appears exactly once even with cycles.
pub fn article_order(
    articles: &[Article],
    threaded: bool,
    sort: Sort,
    collapsed: &HashSet<String>,
) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..articles.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&articles[a], &articles[b]);
        let cmp = match sort {
            Sort::Date => b.timestamp.cmp(&a.timestamp),
            Sort::Subject => a.subject.cmp(&b.subject),
            Sort::Author => a.author.cmp(&b.author),
        };
        cmp.then_with(|| b.number.cmp(&a.number))
    });
    if !threaded {
        return order.into_iter().map(|i| (i, 0)).collect();
    }
    let ids: HashMap<&str, usize> = articles
        .iter()
        .enumerate()
        .map(|(i, a)| (a.message_id.as_str(), i))
        .collect();
    let mut children = vec![Vec::new(); articles.len()];
    let mut roots = Vec::new();
    for &i in &order {
        let parent = articles[i]
            .references
            .iter()
            .rev()
            .find_map(|id| ids.get(id.as_str()).copied().filter(|p| *p != i));
        if let Some(p) = parent {
            children[p].push(i);
        } else {
            roots.push(i);
        }
    }
    let mut seen = vec![false; articles.len()];
    let mut result = Vec::new();
    for root in roots.into_iter().chain(order) {
        let mut stack = vec![(root, 0, false)];
        while let Some((i, depth, hidden)) = stack.pop() {
            if seen[i] {
                continue;
            }
            seen[i] = true;
            if !hidden {
                result.push((i, depth.min(16)));
            }
            let hide = hidden || collapsed.contains(&articles[i].message_id);
            for &child in children[i].iter().rev() {
                stack.push((child, depth + 1, hide));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn japanese_and_mime() {
        for encoding in [
            UTF_8,
            ISO_2022_JP,
            encoding_rs::SHIFT_JIS,
            encoding_rs::EUC_JP,
        ] {
            let (bytes, _, _) = encoding.encode("日本語の記事です。");
            let mut raw = format!(
                "Subject: =?UTF-8?B?5pel5pys6Kqe?=\r\nContent-Type: text/plain; charset={}\r\n\r\n",
                encoding.name()
            )
            .into_bytes();
            raw.extend_from_slice(&bytes);
            let doc = decode_article(&raw, None).unwrap();
            assert_eq!(doc.subject, "日本語");
            assert_eq!(doc.body, "日本語の記事です。");
        }
    }
    #[test]
    fn multipart_does_not_render_html_or_attachments() {
        let raw = b"Content-Type: multipart/mixed; boundary=abc\r\n\r\n--abc\r\nContent-Type: text/html\r\n\r\n<script>bad</script>\r\n--abc\r\nContent-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--abc\r\nContent-Type: text/plain\r\nContent-Disposition: attachment\r\n\r\nsecret\r\n--abc--\r\n";
        assert_eq!(decode_article(raw, None).unwrap().body, "hello");
    }
    #[test]
    fn cycles_missing_parents_and_collapsing() {
        let a = vec![
            Article {
                message_id: "<a>".into(),
                references: vec!["<b>".into()],
                ..Default::default()
            },
            Article {
                message_id: "<b>".into(),
                references: vec!["<a>".into()],
                ..Default::default()
            },
            Article {
                message_id: "<c>".into(),
                references: vec!["<missing>".into()],
                ..Default::default()
            },
        ];
        let order = article_order(&a, true, Sort::Date, &HashSet::new());
        assert_eq!(order.len(), 3);
        assert_eq!(order.iter().map(|x| x.0).collect::<HashSet<_>>().len(), 3);
        let order = article_order(&a, true, Sort::Date, &HashSet::from(["<a>".to_owned()]));
        assert_eq!(order.len(), 2);
    }
}
