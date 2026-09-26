use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Security {
    #[default]
    Tls,
    StartTls,
    LocalPlain,
}
impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tls => "TLS (563)",
            Self::StartTls => "STARTTLS 必須",
            Self::LocalPlain => "ローカル試験用・平文",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub remember_password: bool,
    pub batch_size: u64,
}
impl Default for Server {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            host: String::new(),
            port: 563,
            security: Security::Tls,
            username: String::new(),
            remember_password: false,
            batch_size: 1000,
        }
    }
}
impl Server {
    pub fn same_identity(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.security == other.security
            && self.username == other.username
    }
}

#[derive(Clone, Debug, Default)]
pub struct Group {
    pub name: String,
    pub low: u64,
    pub high: u64,
    pub subscribed: bool,
    pub fetched_low: Option<u64>,
    pub fetched_high: Option<u64>,
    pub unread: usize,
    pub cached: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Article {
    pub number: u64,
    pub subject: String,
    pub author: String,
    pub date: String,
    pub timestamp: i64,
    pub message_id: String,
    pub references: Vec<String>,
    pub read: bool,
    pub cached: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Date,
    Subject,
    Author,
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub subject: String,
    pub author: String,
    pub date: String,
    pub groups: String,
    pub headers: String,
    pub body: String,
    pub warning: Option<String>,
}

pub fn group_matches(name: &str, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if let Some(prefix) = q.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name.contains(&q)
    }
}

/// At most one bounded batch. The cursor advances over holes, not just existing articles.
pub fn fetch_range(
    low: u64,
    high: u64,
    previous: Option<(u64, u64)>,
    limit: u64,
    older: bool,
) -> Option<(u64, u64)> {
    if high == 0 || low > high {
        return None;
    }
    let low = low.max(1);
    let count = limit.clamp(1, 10_000);
    if let Some((old_low, old_high)) = previous.filter(|(_, h)| *h <= high) {
        if older {
            let end = old_low.checked_sub(1)?.min(high);
            return (end >= low).then(|| (end.saturating_sub(count - 1).max(low), end));
        }
        let start = old_high.saturating_add(1).max(low);
        return (start <= high).then(|| (start, start.saturating_add(count - 1).min(high)));
    }
    Some((high.saturating_sub(count - 1).max(low), high))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_ranges_and_resets() {
        assert_eq!(fetch_range(1, 5000, None, 1000, false), Some((4001, 5000)));
        assert_eq!(
            fetch_range(1, 9000, Some((4001, 5000)), 1000, false),
            Some((5001, 6000))
        );
        assert_eq!(
            fetch_range(1, 9000, Some((4001, 5000)), 1000, true),
            Some((3001, 4000))
        );
        assert_eq!(
            fetch_range(1, 20, Some((4001, 5000)), 10, false),
            Some((11, 20))
        );
        assert_eq!(fetch_range(3, 2, None, 1000, false), None);
        assert_eq!(fetch_range(1, 20, Some((1, 20)), 10, false), None);
    }
    #[test]
    fn hierarchy_filter() {
        assert!(group_matches("fj.comp.lang.rust", "fj.*"));
        assert!(!group_matches("alt.fj.test", "fj.*"));
        assert!(group_matches("fj.comp.lang.rust", "rust"));
    }
}
