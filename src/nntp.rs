use crate::{
    article::parse_overview,
    model::{Article, Group, Security, Server},
};
use anyhow::{Context, Result, bail, ensure};
use native_tls::{TlsConnector, TlsStream};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_LINE: usize = 1024 * 1024;
const MAX_BLOCK: usize = 32 * 1024 * 1024;
const MAX_ARTICLE: usize = 8 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct Cancel(Arc<CancelState>);
#[derive(Default)]
struct CancelState {
    cancelled: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}
impl Cancel {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
        if let Ok(s) = self.0.socket.lock()
            && let Some(s) = s.as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.0.cancelled.load(Ordering::Relaxed),
            "キャンセルしました"
        );
        Ok(())
    }
    fn track(&self, stream: &TcpStream) -> Result<()> {
        *self
            .0
            .socket
            .lock()
            .map_err(|_| anyhow::anyhow!("通信状態の取得に失敗しました"))? =
            Some(stream.try_clone()?);
        self.check()
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(TlsStream<TcpStream>),
}
impl Read for Stream {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(b),
            Self::Tls(s) => s.read(b),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(b),
            Self::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

pub struct Client {
    reader: BufReader<Stream>,
    cancel: Cancel,
    deadline: Instant,
}
impl Client {
    pub fn connect(server: &Server, password: &str, cancel: Cancel) -> Result<Self> {
        let connector = TlsConnector::builder()
            .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
            .build()?;
        Self::connect_with_connector(server, password, cancel, connector)
    }
    fn connect_with_connector(
        server: &Server,
        password: &str,
        cancel: Cancel,
        connector: TlsConnector,
    ) -> Result<Self> {
        validate_server(server)?;
        cancel.check()?;
        let addresses: Vec<_> = (server.host.as_str(), server.port)
            .to_socket_addrs()
            .context("サーバー名を解決できません")?
            .collect();
        ensure!(!addresses.is_empty(), "接続先アドレスがありません");
        let mut stream = None;
        for addr in addresses.iter().take(4) {
            cancel.check()?;
            if server.security == Security::LocalPlain {
                ensure!(
                    addr.ip().is_loopback(),
                    "平文接続はループバックアドレス限定です"
                );
            }
            if let Ok(s) = TcpStream::connect_timeout(addr, Duration::from_secs(5)) {
                stream = Some(s);
                break;
            }
        }
        let tcp = stream
            .context("接続に失敗しました。サーバー名・ポート・ネットワークを確認してください")?;
        tcp.set_read_timeout(Some(Duration::from_secs(15)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(15)))?;
        cancel.track(&tcp)?;
        let wire =
            if server.security == Security::Tls {
                Stream::Tls(connector.connect(&server.host, tcp).context(
                    "TLS接続に失敗しました。証明書・ホスト名・接続方式を確認してください",
                )?)
            } else {
                Stream::Plain(tcp)
            };
        let mut c = Self {
            reader: BufReader::new(wire),
            cancel,
            deadline: Instant::now() + Duration::from_secs(90),
        };
        let code = c.response()?.0;
        ensure!(
            matches!(code, 200 | 201),
            "サーバーが接続を拒否しました (NNTP {code})"
        );
        if server.security == Security::StartTls {
            c.expect("STARTTLS", 382)?;
            ensure!(
                c.reader.buffer().is_empty(),
                "STARTTLS応答に余分なデータがあります"
            );
            let Stream::Plain(tcp) = c.reader.into_inner() else {
                unreachable!()
            };
            c.reader = BufReader::new(Stream::Tls(
                connector
                    .connect(&server.host, tcp)
                    .context("STARTTLSの証明書検証または接続に失敗しました")?,
            ));
        }
        c.capabilities()?;
        let mode = c.command("MODE READER")?.0;
        ensure!(
            matches!(mode, 200 | 201 | 480 | 500 | 501 | 502),
            "閲覧モードへ移れません (NNTP {mode})"
        );
        if !server.username.is_empty() {
            ensure!(
                server.security != Security::LocalPlain,
                "平文接続では認証できません"
            );
            token(&server.username)?;
            token(password)?;
            let code = c.command(&format!("AUTHINFO USER {}", server.username))?.0;
            match code {
                281 => (),
                381 => {
                    let code = c.command(&format!("AUTHINFO PASS {password}"))?.0;
                    ensure!(code == 281, "認証に失敗しました (NNTP {code})");
                }
                _ => bail!("認証方式が未対応、または認証に失敗しました (NNTP {code})"),
            }
            c.capabilities()?;
            let code = c.command("MODE READER")?.0;
            ensure!(
                matches!(code, 200 | 201 | 500 | 501 | 502),
                "認証後の閲覧モードに移れません (NNTP {code})"
            );
        } else {
            ensure!(
                mode != 480,
                "認証が必要です。設定でユーザー名とパスワードを入力してください"
            );
        }
        Ok(c)
    }
    fn capabilities(&mut self) -> Result<()> {
        let code = self.command("CAPABILITIES")?.0;
        match code {
            101 => {
                self.block(MAX_BLOCK)?;
            }
            500 | 501 | 480 | 502 => (),
            _ => bail!("サーバー機能を取得できません (NNTP {code})"),
        };
        Ok(())
    }
    fn line(&mut self) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            self.cancel.check()?;
            ensure!(
                Instant::now() < self.deadline,
                "通信の制限時間を超えました。再取得してください"
            );
            let buf = self
                .reader
                .fill_buf()
                .context("通信が中断したか、タイムアウトしました")?;
            ensure!(!buf.is_empty(), "応答の途中で接続が切断されました");
            let count = buf
                .iter()
                .position(|b| *b == b'\n')
                .map_or(buf.len(), |i| i + 1);
            ensure!(
                line.len() + count <= MAX_LINE,
                "応答の1行が上限を超えています"
            );
            line.extend_from_slice(&buf[..count]);
            self.reader.consume(count);
            if line.last() == Some(&b'\n') {
                ensure!(line.ends_with(b"\r\n"), "NNTPの行末が不正です");
                line.truncate(line.len() - 2);
                return Ok(line);
            }
        }
    }
    fn response(&mut self) -> Result<(u16, String)> {
        let line = self.line()?;
        ensure!(
            line.len() >= 3
                && line[..3].iter().all(u8::is_ascii_digit)
                && (line.len() == 3 || line[3] == b' '),
            "NNTP応答が不正です"
        );
        let code = std::str::from_utf8(&line[..3])?.parse()?;
        Ok((
            code,
            String::from_utf8_lossy(line.get(4..).unwrap_or_default()).into_owned(),
        ))
    }
    fn command(&mut self, command: &str) -> Result<(u16, String)> {
        self.cancel.check()?;
        ensure!(
            !command.contains(['\r', '\n', '\0']),
            "コマンドに不正な文字があります"
        );
        self.reader.get_mut().write_all(command.as_bytes())?;
        self.reader.get_mut().write_all(b"\r\n")?;
        self.reader.get_mut().flush()?;
        self.response()
    }
    fn expect(&mut self, command: &str, expected: u16) -> Result<String> {
        let (code, text) = self.command(command)?;
        if code == 480 {
            bail!("認証が必要です。接続設定を確認してください (NNTP 480)");
        }
        if matches!(code, 423 | 430) {
            bail!("記事が失効したか、サーバーに存在しません (NNTP {code})");
        }
        ensure!(
            code == expected,
            "サーバーが要求を拒否しました (NNTP {code})"
        );
        Ok(text)
    }
    fn block(&mut self, limit: usize) -> Result<Vec<Vec<u8>>> {
        let mut lines = Vec::new();
        let mut bytes = 0;
        loop {
            let mut line = self.line()?;
            if line == b"." {
                break;
            }
            if line.starts_with(b"..") {
                line.remove(0);
            }
            bytes += line.len() + 2;
            ensure!(
                bytes <= limit,
                "受信サイズが上限を超えました。取得件数を減らしてください"
            );
            lines.push(line);
        }
        Ok(lines)
    }
    pub fn groups(&mut self) -> Result<Vec<Group>> {
        let (code, _) = self.command("LIST ACTIVE")?;
        match code {
            215 => (),
            500 | 501 => {
                self.expect("LIST", 215)?;
            }
            480 => bail!("グループ一覧には認証が必要です"),
            _ => bail!("グループ一覧を取得できません (NNTP {code})"),
        };
        let mut result = Vec::new();
        for line in self.block(MAX_BLOCK)? {
            let s = std::str::from_utf8(&line)?;
            let f: Vec<_> = s.split_whitespace().collect();
            ensure!(f.len() >= 4, "グループ一覧の形式が不正です");
            group_token(f[0])?;
            result.push(Group {
                name: f[0].into(),
                high: article_number(f[1])?,
                low: article_number(f[2])?,
                ..Default::default()
            });
        }
        Ok(result)
    }
    pub fn select_group(&mut self, name: &str) -> Result<(u64, u64)> {
        group_token(name)?;
        let text = self.expect(&format!("GROUP {name}"), 211)?;
        let fields: Vec<_> = text.split_whitespace().collect();
        ensure!(fields.len() >= 3, "GROUP応答が不正です");
        Ok((article_number(fields[1])?, article_number(fields[2])?))
    }
    pub fn overview(&mut self, start: u64, end: u64) -> Result<Vec<Article>> {
        let (code, _) = self.command(&format!("OVER {start}-{end}"))?;
        let code = if matches!(code, 500 | 501) {
            self.command(&format!("XOVER {start}-{end}"))?.0
        } else {
            code
        };
        if code == 423 {
            return Ok(Vec::new());
        }
        ensure!(code == 224, "記事概要を取得できません (NNTP {code})");
        self.block(MAX_BLOCK)?
            .iter()
            .map(|line| {
                let a = parse_overview(line)?;
                ensure!(
                    (start..=end).contains(&a.number),
                    "取得範囲外の記事番号です"
                );
                Ok(a)
            })
            .collect()
    }
    pub fn article(&mut self, id: &str) -> Result<Vec<u8>> {
        ensure!(crate::article::valid_message_id(id), "Message-IDが不正です");
        self.expect(&format!("ARTICLE {id}"), 220)?;
        let mut raw = Vec::new();
        for line in self.block(MAX_ARTICLE)? {
            raw.extend_from_slice(&line);
            raw.extend_from_slice(b"\r\n");
        }
        // A misbehaving server must not poison the cache under a different identity.
        use mailparse::MailHeaderMap;
        let (headers, _) = mailparse::parse_headers(&raw)?;
        ensure!(
            headers.get_first_value("Message-ID").as_deref() == Some(id),
            "取得した記事のMessage-IDが一致しません"
        );
        Ok(raw)
    }
}
fn token(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty() && !s.chars().any(char::is_control),
        "認証情報に空欄または制御文字があります"
    );
    Ok(())
}
fn group_token(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 512
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".+-_".contains(&b)),
        "グループ名が不正です"
    );
    Ok(())
}
fn article_number(s: &str) -> Result<u64> {
    let n: u64 = s.parse()?;
    ensure!(n <= i64::MAX as u64, "記事番号が範囲外です");
    Ok(n)
}
pub fn validate_server(s: &Server) -> Result<()> {
    ensure!(!s.name.trim().is_empty(), "表示名を入力してください");
    ensure!(
        !s.host.is_empty()
            && s.host.len() <= 253
            && s.host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b)),
        "ホスト名が不正です"
    );
    ensure!(s.port > 0, "ポート番号を入力してください");
    ensure!(
        (1..=10_000).contains(&s.batch_size),
        "取得件数は1〜10000です"
    );
    if !s.username.is_empty() {
        token(&s.username)?;
        ensure!(s.security != Security::LocalPlain, "平文では認証できません");
    }
    if s.security == Security::LocalPlain {
        ensure!(
            matches!(s.host.as_str(), "localhost" | "127.0.0.1" | "::1"),
            "平文接続はlocalhost限定です"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tls_tests {
    use super::*;
    use native_tls::{Certificate, Identity, TlsAcceptor};
    use std::{net::TcpListener, thread};

    fn fixture(starttls: bool) -> (Server, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server = Server {
            name: "Local TLS fixture".into(),
            host: "localhost".into(),
            port: listener.local_addr().unwrap().port(),
            security: if starttls {
                Security::StartTls
            } else {
                Security::Tls
            },
            username: "fixture-user".into(),
            ..Server::default()
        };
        let join = thread::spawn(move || {
            let identity = Identity::from_pkcs12(
                include_bytes!("../tests/fixtures/localhost.p12"),
                "local-test-only",
            )
            .unwrap();
            let acceptor = TlsAcceptor::new(identity).unwrap();
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let tcp = if starttls {
                let mut r = BufReader::new(tcp);
                r.get_mut().write_all(b"200 ready\r\n").unwrap();
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                assert_eq!(line, "STARTTLS\r\n");
                r.get_mut().write_all(b"382 begin TLS\r\n").unwrap();
                r.into_inner()
            } else {
                tcp
            };
            let Ok(stream) = acceptor.accept(tcp) else {
                return;
            };
            let mut r = BufReader::new(stream);
            if !starttls {
                r.get_mut().write_all(b"200 ready\r\n").unwrap();
            }
            for (command, response) in [
                (
                    "CAPABILITIES",
                    "101 caps\r\nVERSION 2\r\nAUTHINFO USER\r\n.\r\n",
                ),
                ("MODE READER", "480 authenticate\r\n"),
                ("AUTHINFO USER fixture-user", "381 password\r\n"),
                ("AUTHINFO PASS fixture-password", "281 authenticated\r\n"),
                ("CAPABILITIES", "101 caps\r\nVERSION 2\r\nREADER\r\n.\r\n"),
                ("MODE READER", "201 ready\r\n"),
                ("LIST ACTIVE", "215 list\r\nfj.test 1 1 y\r\n.\r\n"),
            ] {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                assert_eq!(line.trim_end(), command);
                r.get_mut().write_all(response.as_bytes()).unwrap();
            }
        });
        (server, join)
    }
    #[test]
    fn tls_and_starttls_authenticate_before_group_list() {
        for starttls in [false, true] {
            let (s, j) = fixture(starttls);
            let cert =
                Certificate::from_pem(include_bytes!("../tests/fixtures/localhost.pem")).unwrap();
            let connector = TlsConnector::builder()
                .add_root_certificate(cert)
                .build()
                .unwrap();
            let mut client = Client::connect_with_connector(
                &s,
                "fixture-password",
                Cancel::default(),
                connector,
            )
            .unwrap();
            assert_eq!(client.groups().unwrap()[0].name, "fj.test");
            j.join().unwrap();
        }
    }
    #[test]
    fn untrusted_certificate_is_rejected() {
        let (s, j) = fixture(false);
        assert!(Client::connect(&s, "fixture-password", Cancel::default()).is_err());
        j.join().unwrap();
    }
    #[test]
    fn trusted_certificate_with_wrong_hostname_is_rejected() {
        let (mut s, j) = fixture(false);
        s.host = "127.0.0.1".into();
        let cert =
            Certificate::from_pem(include_bytes!("../tests/fixtures/localhost.pem")).unwrap();
        let connector = TlsConnector::builder()
            .add_root_certificate(cert)
            .build()
            .unwrap();
        assert!(
            Client::connect_with_connector(&s, "fixture-password", Cancel::default(), connector)
                .is_err()
        );
        j.join().unwrap();
    }
}
