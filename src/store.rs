use crate::model::{Article, Group, Server};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Store {
    conn: Connection,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path).context("保存データを開けません")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= 1,
            "この保存データには新しいバージョンのrustVNが必要です"
        );
        conn.execute_batch("BEGIN;
            CREATE TABLE IF NOT EXISTS servers(id TEXT PRIMARY KEY, config TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS groups(server TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, name TEXT NOT NULL, low INTEGER NOT NULL, high INTEGER NOT NULL, subscribed INTEGER NOT NULL DEFAULT 0, fetched_low INTEGER, fetched_high INTEGER, PRIMARY KEY(server,name));
            CREATE TABLE IF NOT EXISTS articles(server TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, grp TEXT NOT NULL, number INTEGER NOT NULL, msgid TEXT NOT NULL, subject TEXT NOT NULL, author TEXT NOT NULL, date TEXT NOT NULL, timestamp INTEGER NOT NULL, refs TEXT NOT NULL, PRIMARY KEY(server,grp,number));
            CREATE INDEX IF NOT EXISTS article_ids ON articles(server,msgid);
            CREATE TABLE IF NOT EXISTS reads(server TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, msgid TEXT NOT NULL, PRIMARY KEY(server,msgid));
            CREATE TABLE IF NOT EXISTS bodies(server TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE, msgid TEXT NOT NULL, raw BLOB NOT NULL, text TEXT NOT NULL, accessed INTEGER NOT NULL, PRIMARY KEY(server,msgid));
            PRAGMA user_version=1; COMMIT;")?;
        Ok(Self { conn })
    }
    pub fn servers(&self) -> Result<Vec<Server>> {
        let mut q = self
            .conn
            .prepare("SELECT config FROM servers ORDER BY rowid")?;
        q.query_map([], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect()
    }
    pub fn save_server(&self, s: &Server) -> Result<()> {
        self.conn.execute("INSERT INTO servers VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET config=excluded.config",params![s.id,serde_json::to_string(s)?])?;
        Ok(())
    }
    pub fn delete_server(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM servers WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn upsert_groups(&mut self, server: &str, groups: &[Group]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for g in groups {
            tx.execute("INSERT INTO groups(server,name,low,high) VALUES(?1,?2,?3,?4) ON CONFLICT(server,name) DO UPDATE SET low=excluded.low,high=excluded.high",params![server,g.name,g.low,g.high])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn groups(&self, server: &str) -> Result<Vec<Group>> {
        let mut q=self.conn.prepare("SELECT g.name,g.low,g.high,g.subscribed,g.fetched_low,g.fetched_high,COALESCE(a.total,0),COALESCE(a.unread,0) FROM groups g LEFT JOIN (SELECT a.grp,COUNT(*) total,SUM(CASE WHEN r.msgid IS NULL THEN 1 ELSE 0 END) unread FROM articles a LEFT JOIN reads r ON r.server=a.server AND r.msgid=a.msgid WHERE a.server=?1 GROUP BY a.grp) a ON a.grp=g.name WHERE g.server=?1 ORDER BY g.name")?;
        let rows = q.query_map([server], |r| {
            Ok(Group {
                name: r.get(0)?,
                low: r.get(1)?,
                high: r.get(2)?,
                subscribed: r.get(3)?,
                fetched_low: r.get(4)?,
                fetched_high: r.get(5)?,
                cached: r.get(6)?,
                unread: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn subscribe(&self, server: &str, name: &str, subscribe: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE groups SET subscribed=?3 WHERE server=?1 AND name=?2",
            params![server, name, subscribe],
        )?;
        Ok(())
    }
    pub fn save_overview(
        &mut self,
        server: &str,
        group: &str,
        bounds: (u64, u64),
        range: Option<(u64, u64)>,
        articles: &[Article],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let previous: Option<u64> = tx
            .query_row(
                "SELECT fetched_high FROM groups WHERE server=?1 AND name=?2",
                params![server, group],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if previous.is_some_and(|h| h > bounds.1) {
            tx.execute(
                "DELETE FROM articles WHERE server=?1 AND grp=?2",
                params![server, group],
            )?;
            tx.execute(
                "UPDATE groups SET fetched_low=NULL,fetched_high=NULL WHERE server=?1 AND name=?2",
                params![server, group],
            )?;
        }
        tx.execute("INSERT INTO groups(server,name,low,high) VALUES(?1,?2,?3,?4) ON CONFLICT(server,name) DO UPDATE SET low=excluded.low,high=excluded.high",params![server,group,bounds.0,bounds.1])?;
        for a in articles {
            tx.execute("INSERT INTO articles VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(server,grp,number) DO UPDATE SET msgid=excluded.msgid,subject=excluded.subject,author=excluded.author,date=excluded.date,timestamp=excluded.timestamp,refs=excluded.refs",params![server,group,a.number,a.message_id,a.subject,a.author,a.date,a.timestamp,serde_json::to_string(&a.references)?])?;
        }
        if let Some((start, end)) = range {
            tx.execute("UPDATE groups SET fetched_low=MIN(COALESCE(fetched_low,?3),?3),fetched_high=MAX(COALESCE(fetched_high,?4),?4) WHERE server=?1 AND name=?2",params![server,group,start,end])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn articles(&self, server: &str, group: &str, search: &str) -> Result<Vec<Article>> {
        let mut q=self.conn.prepare("SELECT a.number,a.subject,a.author,a.date,a.timestamp,a.msgid,a.refs,r.msgid IS NOT NULL,b.msgid IS NOT NULL FROM articles a LEFT JOIN reads r ON r.server=a.server AND r.msgid=a.msgid LEFT JOIN bodies b ON b.server=a.server AND b.msgid=a.msgid WHERE a.server=?1 AND a.grp=?2 AND (?3='' OR instr(lower(a.subject),lower(?3))>0 OR instr(lower(a.author),lower(?3))>0 OR instr(lower(b.text),lower(?3))>0) ORDER BY a.number DESC")?;
        let rows = q.query_map(params![server, group, search], |r| {
            Ok(Article {
                number: r.get(0)?,
                subject: r.get(1)?,
                author: r.get(2)?,
                date: r.get(3)?,
                timestamp: r.get(4)?,
                message_id: r.get(5)?,
                references: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                read: r.get(7)?,
                cached: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn mark_read(&self, server: &str, id: &str, read: bool) -> Result<()> {
        if read {
            self.conn.execute(
                "INSERT OR IGNORE INTO reads VALUES(?1,?2)",
                params![server, id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM reads WHERE server=?1 AND msgid=?2",
                params![server, id],
            )?;
        }
        Ok(())
    }
    pub fn mark_group_read(&mut self, server: &str, group: &str) -> Result<()> {
        self.conn.execute("INSERT OR IGNORE INTO reads SELECT server,msgid FROM articles WHERE server=?1 AND grp=?2",params![server,group])?;
        Ok(())
    }
    pub fn body(&self, server: &str, id: &str) -> Result<Option<Vec<u8>>> {
        let raw = self
            .conn
            .query_row(
                "SELECT raw FROM bodies WHERE server=?1 AND msgid=?2",
                params![server, id],
                |r| r.get(0),
            )
            .optional()?;
        if raw.is_some() {
            self.conn.execute(
                "UPDATE bodies SET accessed=unixepoch() WHERE server=?1 AND msgid=?2",
                params![server, id],
            )?;
        }
        Ok(raw)
    }
    pub fn save_body(&mut self, server: &str, id: &str, raw: &[u8], text: &str) -> Result<()> {
        self.conn.execute("INSERT INTO bodies VALUES(?1,?2,?3,?4,unixepoch()) ON CONFLICT(server,msgid) DO UPDATE SET raw=excluded.raw,text=excluded.text,accessed=excluded.accessed",params![server,id,raw,text])?;
        self.trim_cache(1024 * 1024 * 1024)?;
        Ok(())
    }
    pub fn cache_size(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(length(raw)+length(CAST(text AS BLOB))),0) FROM bodies",
            [],
            |r| r.get(0),
        )?)
    }
    pub fn trim_cache(&mut self, limit: u64) -> Result<()> {
        if self.cache_size()? <= limit {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        let mut bytes: u64 = tx.query_row(
            "SELECT COALESCE(SUM(length(raw)+length(CAST(text AS BLOB))),0) FROM bodies",
            [],
            |r| r.get(0),
        )?;
        let victims:Vec<(String,String,u64)>=tx.prepare("SELECT server,msgid,length(raw)+length(CAST(text AS BLOB)) FROM bodies ORDER BY accessed,rowid")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (server, id, size) in victims {
            if bytes <= limit {
                break;
            }
            tx.execute(
                "DELETE FROM bodies WHERE server=?1 AND msgid=?2",
                params![server, id],
            )?;
            bytes = bytes.saturating_sub(size);
        }
        tx.commit()?;
        Ok(())
    }
    pub fn clear_bodies(&self, server: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM bodies WHERE server=?1", [server])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_and_isolates_crossposts_and_cache_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("news.db");
        let mut db = Store::open(&path).unwrap();
        let a = Server::default();
        let b = Server::default();
        db.save_server(&a).unwrap();
        db.save_server(&b).unwrap();
        let article = Article {
            number: 1,
            message_id: "<one@test>".into(),
            subject: "日本語".into(),
            ..Default::default()
        };
        for server in [&a.id, &b.id] {
            for group in ["fj.test", "fj.misc"] {
                db.save_overview(
                    server,
                    group,
                    (1, 1),
                    Some((1, 1)),
                    std::slice::from_ref(&article),
                )
                .unwrap();
            }
        }
        db.subscribe(&a.id, "fj.test", true).unwrap();
        db.mark_read(&a.id, &article.message_id, true).unwrap();
        db.save_body(&a.id, &article.message_id, b"hello", "hello")
            .unwrap();
        assert!(db.articles(&a.id, "fj.misc", "").unwrap()[0].read);
        assert!(!db.articles(&b.id, "fj.test", "").unwrap()[0].read);
        assert_eq!(db.articles(&a.id, "fj.test", "hello").unwrap().len(), 1);
        db.trim_cache(0).unwrap();
        assert!(db.body(&a.id, &article.message_id).unwrap().is_none());
        drop(db);
        let db = Store::open(&path).unwrap();
        assert!(
            db.groups(&a.id)
                .unwrap()
                .iter()
                .find(|g| g.name == "fj.test")
                .unwrap()
                .subscribed
        );
        assert!(db.articles(&a.id, "fj.test", "").unwrap()[0].read);
    }
    #[test]
    fn overview_upsert_holes_and_reset() {
        let d = tempfile::tempdir().unwrap();
        let mut db = Store::open(&d.path().join("db")).unwrap();
        let s = Server::default();
        db.save_server(&s).unwrap();
        let a = Article {
            number: 100,
            message_id: "<100>".into(),
            ..Default::default()
        };
        for _ in 0..2 {
            db.save_overview(
                &s.id,
                "fj.test",
                (1, 200),
                Some((1, 200)),
                std::slice::from_ref(&a),
            )
            .unwrap();
        }
        assert_eq!(db.articles(&s.id, "fj.test", "").unwrap().len(), 1);
        assert_eq!(db.groups(&s.id).unwrap()[0].fetched_high, Some(200));
        db.save_overview(&s.id, "fj.test", (1, 10), Some((1, 10)), &[])
            .unwrap();
        assert!(db.articles(&s.id, "fj.test", "").unwrap().is_empty());
        assert_eq!(db.groups(&s.id).unwrap()[0].fetched_high, Some(10));
    }
}
