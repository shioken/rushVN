use crate::{
    article::decode_article,
    model::{Article, Document, Group, Server, fetch_range},
    nntp::{Cancel, Client},
    store::Store,
};
use anyhow::{Context, Result};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender, channel},
};
use zeroize::Zeroizing;

pub enum Action {
    Load,
    SaveServer(Server, Zeroizing<String>),
    DeleteServer(String),
    Groups {
        server: Server,
        password: Zeroizing<String>,
        network: bool,
    },
    Articles {
        server: Server,
        password: Zeroizing<String>,
        group: String,
        search: String,
        network: bool,
        older: bool,
    },
    Body {
        server: Server,
        password: Zeroizing<String>,
        group: String,
        id: String,
        network: bool,
        charset: Option<String>,
    },
    Read {
        server: String,
        group: String,
        id: Option<String>,
        read: bool,
    },
    Subscribe {
        server: String,
        group: String,
        value: bool,
    },
    ClearCache(String),
}
pub enum Update {
    Servers(Vec<Server>, String),
    Groups(Vec<Group>),
    Articles(Vec<Article>, Vec<Group>, String),
    Body(Document, Vec<u8>, String),
    Changed(Vec<Group>),
}
pub struct Event {
    pub id: u64,
    pub result: Result<Update, String>,
}
pub struct Request {
    pub id: u64,
    pub action: Action,
    pub cancel: Cancel,
}

pub fn start(path: PathBuf, ctx: eframe::egui::Context) -> (Sender<Request>, Receiver<Event>) {
    let (tx, requests) = channel::<Request>();
    let (events, rx) = channel();
    std::thread::spawn(move || {
        let mut store = match Store::open(&path) {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(Event {
                    id: 0,
                    result: Err(format!("保存データを開けません: {e:#}")),
                });
                ctx.request_repaint();
                return;
            }
        };
        while let Ok(request) = requests.recv() {
            let result =
                execute(&mut store, request.action, &request.cancel).map_err(|e| format!("{e:#}"));
            if events
                .send(Event {
                    id: request.id,
                    result,
                })
                .is_err()
            {
                break;
            }
            ctx.request_repaint();
        }
    });
    (tx, rx)
}

fn password(server: &Server, input: Zeroizing<String>) -> Result<Zeroizing<String>> {
    if !input.is_empty() || server.username.is_empty() {
        return Ok(input);
    }
    if server.remember_password {
        return Ok(Zeroizing::new(
            keyring::Entry::new("rushVN", &server.id)?
                .get_password()
                .context("保存済みパスワードを取得できません。設定で入力してください")?,
        ));
    }
    anyhow::bail!("設定でパスワードを入力してください（セッション内のみ保持）")
}
fn connect(server: &Server, input: Zeroizing<String>, cancel: &Cancel) -> Result<Client> {
    Client::connect(server, &password(server, input)?, cancel.clone())
}

pub fn execute(store: &mut Store, action: Action, cancel: &Cancel) -> Result<Update> {
    cancel.check()?;
    match action {
        Action::Load => Ok(Update::Servers(
            store.servers()?,
            "保存済みデータを読み込みました".into(),
        )),
        Action::SaveServer(mut s, pw) => {
            crate::nntp::validate_server(&s)?;
            let old_remember = store
                .servers()?
                .iter()
                .any(|old| old.id == s.id && old.remember_password);
            if s.username.is_empty() {
                s.remember_password = false;
            }
            anyhow::ensure!(
                !s.remember_password || old_remember || !pw.is_empty(),
                "保存するパスワードを入力してください"
            );
            let mut message = "接続設定を保存しました".to_owned();
            if s.remember_password {
                if !pw.is_empty()
                    && keyring::Entry::new("rushVN", &s.id)
                        .and_then(|e| e.set_password(&pw))
                        .is_err()
                {
                    s.remember_password = false;
                    message="資格情報ストアを利用できないため、パスワードはこのセッションのみ保持します".into();
                }
            } else if old_remember && let Ok(entry) = keyring::Entry::new("rushVN", &s.id) {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => (),
                    Err(_) => {
                        message="設定を保存しました。資格情報ストアの古いパスワードを削除できませんでした".into();
                    }
                }
            }
            store.save_server(&s)?;
            Ok(Update::Servers(store.servers()?, message))
        }
        Action::DeleteServer(id) => {
            let mut message = "サーバー設定と保存記事を削除しました".to_owned();
            if store
                .servers()?
                .iter()
                .any(|old| old.id == id && old.remember_password)
                && let Ok(entry) = keyring::Entry::new("rushVN", &id)
            {
                match entry.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => (),
                    Err(_) => message.push_str("。資格情報ストアの削除は失敗しました"),
                }
            }
            store.delete_server(&id)?;
            Ok(Update::Servers(store.servers()?, message))
        }
        Action::Groups {
            server,
            password,
            network,
        } => {
            if network {
                let mut client = connect(&server, password, cancel)?;
                let groups = client.groups()?;
                cancel.check()?;
                store.upsert_groups(&server.id, &groups)?;
            }
            Ok(Update::Groups(store.groups(&server.id)?))
        }
        Action::Articles {
            server,
            password,
            group,
            search,
            network,
            older,
        } => {
            let mut message = "保存済みの記事概要".to_owned();
            if network {
                let previous = store
                    .groups(&server.id)?
                    .into_iter()
                    .find(|g| g.name == group)
                    .and_then(|g| g.fetched_low.zip(g.fetched_high));
                let mut client = connect(&server, password, cancel)?;
                let bounds = client.select_group(&group)?;
                let range = fetch_range(bounds.0, bounds.1, previous, server.batch_size, older);
                let articles = if let Some((start, end)) = range {
                    client.overview(start, end)?
                } else {
                    Vec::new()
                };
                cancel.check()?;
                store.save_overview(&server.id, &group, bounds, range, &articles)?;
                message = match range {
                    Some((a, b)) => format!("記事番号 {a}–{b} を確認・{} 件取得", articles.len()),
                    None => "この方向に追加で取得する記事はありません".into(),
                };
            }
            Ok(Update::Articles(
                store.articles(&server.id, &group, &search)?,
                store.groups(&server.id)?,
                message,
            ))
        }
        Action::Body {
            server,
            password,
            group: _,
            id,
            network,
            charset,
        } => {
            let raw = if let Some(raw) = store.body(&server.id, &id)? {
                raw
            } else {
                anyhow::ensure!(
                    network,
                    "本文は未取得です。オンラインにして「本文を取得」を押してください"
                );
                let mut client = connect(&server, password, cancel)?;
                let raw = client.article(&id)?;
                cancel.check()?;
                // Preserve raw data even if MIME decoding fails.
                let decoded = decode_article(&raw, charset.as_deref()).ok();
                store.save_body(
                    &server.id,
                    &id,
                    &raw,
                    decoded.as_ref().map(|d| d.body.as_str()).unwrap_or(""),
                )?;
                raw
            };
            let doc = decode_article(&raw, charset.as_deref()).unwrap_or_else(|e| Document {
                warning: Some(format!(
                    "本文を解析できません: {e}。「元データ」で確認できます。"
                )),
                ..Default::default()
            });
            Ok(Update::Body(doc, raw, id))
        }
        Action::Read {
            server,
            group,
            id,
            read,
        } => {
            if let Some(id) = id {
                store.mark_read(&server, &id, read)?;
            } else {
                store.mark_group_read(&server, &group)?;
            }
            Ok(Update::Changed(store.groups(&server)?))
        }
        Action::Subscribe {
            server,
            group,
            value,
        } => {
            store.subscribe(&server, &group, value)?;
            Ok(Update::Changed(store.groups(&server)?))
        }
        Action::ClearCache(id) => {
            store.clear_bodies(&id)?;
            Ok(Update::Changed(store.groups(&id)?))
        }
    }
}
