use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};
use rustvn::{
    article::{article_order, decode_article},
    model::{Article, Document, Group, Security, Server, Sort, group_matches},
    nntp::Cancel,
    store::Store,
    worker::{self, Action, Event, Request, Update},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Preferences {
    server: String,
    group: String,
    threaded: bool,
    sort: Sort,
    font_size: f32,
    wrap: bool,
    subscribed_only: bool,
    group_filter: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            server: String::new(),
            group: String::new(),
            threaded: true,
            sort: Sort::Date,
            font_size: 15.0,
            wrap: true,
            subscribed_only: false,
            group_filter: "fj.*".into(),
        }
    }
}
pub struct NewsApp {
    tx: Sender<Request>,
    rx: Receiver<Event>,
    sequence: u64,
    cancel: Cancel,
    cancellable: bool,
    busy: bool,
    servers: Vec<Server>,
    groups: Vec<Group>,
    articles: Vec<Article>,
    order: Vec<(usize, usize)>,
    collapsed: HashSet<String>,
    prefs: Preferences,
    selected: Option<String>,
    document: Option<Document>,
    raw: Vec<u8>,
    passwords: HashMap<String, Zeroizing<String>>,
    online: bool,
    status: String,
    error: Option<String>,
    settings: bool,
    draft: Server,
    draft_password: Zeroizing<String>,
    delete_confirm: bool,
    search: String,
    charset: usize,
    body_mode: usize,
    demo: bool,
    initial_restore: bool,
    scroll_to: Option<usize>,
    screenshot: Option<PathBuf>,
    screenshot_sent: bool,
    started: Instant,
    font_warning: bool,
    pending_close: bool,
}
impl NewsApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        path: PathBuf,
        demo: bool,
        screenshot: Option<PathBuf>,
    ) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let font_warning = !load_japanese_font(&cc.egui_ctx);
        let prefs = cc
            .storage
            .and_then(|s| eframe::get_value(s, "preferences"))
            .unwrap_or_default();
        let (tx, rx) = worker::start(path, cc.egui_ctx.clone());
        let mut app = Self {
            tx,
            rx,
            sequence: 0,
            cancel: Cancel::default(),
            cancellable: true,
            busy: false,
            servers: vec![],
            groups: vec![],
            articles: vec![],
            order: vec![],
            collapsed: HashSet::new(),
            prefs,
            selected: None,
            document: None,
            raw: vec![],
            passwords: HashMap::new(),
            online: false,
            status: "起動しています…".into(),
            error: None,
            settings: false,
            draft: Server::default(),
            draft_password: Zeroizing::new(String::new()),
            delete_confirm: false,
            search: String::new(),
            charset: 0,
            body_mode: 0,
            demo,
            initial_restore: true,
            scroll_to: None,
            screenshot,
            screenshot_sent: false,
            started: Instant::now(),
            font_warning,
            pending_close: false,
        };
        app.submit(Action::Load, "保存データを読み込み中…");
        app
    }
    fn server(&self) -> Option<Server> {
        self.servers
            .iter()
            .find(|s| s.id == self.prefs.server)
            .cloned()
    }
    fn password(&self) -> Zeroizing<String> {
        self.passwords
            .get(&self.prefs.server)
            .cloned()
            .unwrap_or_else(|| Zeroizing::new(String::new()))
    }
    fn submit(&mut self, action: Action, label: &str) {
        if self.cancellable {
            self.cancel.cancel();
        }
        self.sequence += 1;
        self.cancel = Cancel::default();
        self.cancellable = matches!(
            &action,
            Action::Load | Action::Groups { .. } | Action::Articles { .. } | Action::Body { .. }
        );
        self.busy = true;
        self.status = label.into();
        self.error = None;
        if self
            .tx
            .send(Request {
                id: self.sequence,
                action,
                cancel: self.cancel.clone(),
            })
            .is_err()
        {
            self.busy = false;
            self.error =
                Some("バックグラウンド処理が終了しました。アプリを再起動してください".into());
        }
    }
    fn load_groups(&mut self, network: bool) {
        if let Some(server) = self.server() {
            self.submit(
                Action::Groups {
                    server,
                    password: self.password(),
                    network,
                },
                if network {
                    "接続・認証とグループ一覧を取得中…"
                } else {
                    "保存済みグループを読み込み中…"
                },
            );
        }
    }
    fn load_articles(&mut self, network: bool, older: bool) {
        if let Some(server) = self.server()
            && !self.prefs.group.is_empty()
        {
            self.submit(
                Action::Articles {
                    server,
                    password: self.password(),
                    group: self.prefs.group.clone(),
                    search: self.search.clone(),
                    network,
                    older,
                },
                if network {
                    "記事概要を取得中…"
                } else {
                    "保存済みの記事を検索中…"
                },
            );
        }
    }
    fn rebuild(&mut self) {
        self.order = article_order(
            &self.articles,
            self.prefs.threaded,
            self.prefs.sort,
            &self.collapsed,
        );
    }
    fn select_group(&mut self, name: String) {
        self.prefs.group = name;
        self.articles.clear();
        self.order.clear();
        self.selected = None;
        self.document = None;
        self.raw.clear();
        self.search.clear();
        self.collapsed.clear();
        self.load_articles(false, false);
    }
    fn select_article(&mut self, id: String) {
        self.selected = Some(id);
        self.document = None;
        self.raw.clear();
        self.charset = 0;
        self.body_mode = 0;
        self.load_body();
    }
    fn load_body(&mut self) {
        if let (Some(server), Some(id)) = (self.server(), self.selected.clone()) {
            let charset = charset_label(self.charset).map(str::to_owned);
            self.submit(
                Action::Body {
                    server,
                    password: self.password(),
                    group: self.prefs.group.clone(),
                    id,
                    network: self.online && !self.demo,
                    charset,
                },
                "本文を読み込み中…",
            );
        }
    }
    fn mark_read(&mut self, read: bool) {
        if let Some(id) = self.selected.clone() {
            for a in &mut self.articles {
                if a.message_id == id {
                    a.read = read;
                }
            }
            self.submit(
                Action::Read {
                    server: self.prefs.server.clone(),
                    group: self.prefs.group.clone(),
                    id: Some(id),
                    read,
                },
                "既読状態を保存中…",
            );
        }
    }
    fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            if event.id != self.sequence && event.id != 0 {
                continue;
            }
            self.busy = false;
            match event.result {
                Err(e) => {
                    self.status = "処理を完了できませんでした".into();
                    self.error = Some(e);
                }
                Ok(Update::Servers(servers, message)) => {
                    self.servers = servers;
                    self.status = message;
                    if self.server().is_none() {
                        self.prefs.server = self
                            .servers
                            .first()
                            .map(|s| s.id.clone())
                            .unwrap_or_default();
                        self.prefs.group.clear();
                        self.groups.clear();
                        self.articles.clear();
                        self.order.clear();
                        self.selected = None;
                        self.document = None;
                        self.raw.clear();
                    }
                    if self.servers.is_empty() {
                        self.settings = true;
                        self.groups.clear();
                        self.articles.clear();
                        self.order.clear();
                        self.document = None;
                    } else {
                        self.load_groups(false);
                    }
                }
                Ok(Update::Groups(groups)) => {
                    self.groups = groups;
                    self.status =
                        format!("{} グループ · 未読数は取得済み記事のみ", self.groups.len());
                    if self.initial_restore {
                        self.initial_restore = false;
                        if self.demo && self.prefs.group.is_empty() {
                            self.prefs.group = "fj.comp.lang.rust".into();
                        }
                        self.load_articles(false, false);
                    }
                }
                Ok(Update::Articles(articles, groups, message)) => {
                    self.articles = articles;
                    self.groups = groups;
                    self.status = message;
                    self.rebuild();
                    if self.demo
                        && self.selected.is_none()
                        && let Some(a) = self.articles.last()
                    {
                        self.select_article(a.message_id.clone());
                    }
                }
                Ok(Update::Body(doc, raw, id)) => {
                    self.document = Some(doc);
                    self.raw = raw;
                    for a in &mut self.articles {
                        if a.message_id == id {
                            a.cached = true;
                        }
                    }
                    self.status = "本文を表示しました · キャッシュ保存済み".into();
                    // Mark only after a successfully decoded document has reached the UI.
                    if self.document.as_ref().is_some_and(|d| !d.body.is_empty()) {
                        self.mark_read(true);
                    }
                }
                Ok(Update::Changed(groups)) => {
                    self.groups = groups;
                    self.status = "保存しました".into();
                }
            }
        }
    }
    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("rustVN")
                        .strong()
                        .size(23.0)
                        .color(Color32::from_rgb(28, 75, 105)),
                );
                ui.label(RichText::new("READ NEWS").small().color(Color32::DARK_GRAY));
                ui.separator();
                if ui
                    .add_enabled(
                        !self.busy && !self.demo && self.server().is_some(),
                        egui::Button::new("接続・一覧取得"),
                    )
                    .clicked()
                {
                    self.online = true;
                    self.load_groups(true);
                }
                if ui
                    .add_enabled(
                        !self.busy && !self.demo && self.online && !self.prefs.group.is_empty(),
                        egui::Button::new("新着を取得  R"),
                    )
                    .clicked()
                {
                    self.load_articles(true, false);
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("接続設定"))
                    .clicked()
                {
                    self.draft = self.server().unwrap_or_default();
                    self.draft_password = self.password();
                    self.settings = true;
                    self.delete_confirm = false;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.busy && self.cancellable && ui.button("キャンセル").clicked() {
                        self.cancel.cancel();
                        self.sequence += 1;
                        self.busy = false;
                        self.status = "キャンセルしました".into();
                    }
                    if self.busy {
                        ui.spinner();
                    }
                    let mut offline = !self.online;
                    if ui
                        .add_enabled(!self.demo, egui::Checkbox::new(&mut offline, "オフライン"))
                        .changed()
                    {
                        self.online = !offline;
                        if offline && self.cancellable {
                            self.cancel.cancel();
                            self.sequence += 1;
                            self.busy = false;
                        }
                    }
                });
            });
            ui.add_space(5.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if self.demo {
                        "DEMO"
                    } else if self.online {
                        "オンライン取得可"
                    } else {
                        "OFFLINE"
                    })
                    .strong()
                    .color(Color32::from_rgb(35, 99, 102)),
                );
                ui.separator();
                ui.label(&self.status);
            });
        });
        if let Some(error) = self.error.clone() {
            egui::TopBottomPanel::top("error").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::from_rgb(158, 48, 39), error);
                    if ui.small_button("閉じる").clicked() {
                        self.error = None;
                    }
                });
            });
        }
        if self.font_warning {
            egui::TopBottomPanel::top("font_warning").show(ctx, |ui| {
                ui.label(
                    "Japanese font not found. Set RUSTVN_FONT to a Japanese .ttf/.otf/.ttc file.",
                );
            });
        }
    }
    fn groups_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("groups")
            .default_width(250.0)
            .width_range(180.0..=480.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("ニュースグループ").strong().size(16.0));
                ui.add_space(5.0);
                let old = self.prefs.server.clone();
                egui::ComboBox::from_id_salt("server")
                    .selected_text(
                        self.server()
                            .map(|s| s.name)
                            .unwrap_or_else(|| "接続先を追加".into()),
                    )
                    .width(ui.available_width() - 10.0)
                    .show_ui(ui, |ui| {
                        for s in &self.servers {
                            ui.selectable_value(&mut self.prefs.server, s.id.clone(), &s.name);
                        }
                    });
                if old != self.prefs.server {
                    self.online = false;
                    self.prefs.group.clear();
                    self.groups.clear();
                    self.articles.clear();
                    self.order.clear();
                    self.document = None;
                    self.selected = None;
                    self.load_groups(false);
                }
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.prefs.group_filter)
                        .id(egui::Id::new("group_filter_input"))
                        .hint_text("fj.* / グループ名を検索")
                        .desired_width(f32::INFINITY),
                );
                ui.checkbox(&mut self.prefs.subscribed_only, "購読中のみ");
                ui.small("★ 購読中    数字: 取得済みの未読");
                ui.separator();
                let visible: Vec<_> = self
                    .groups
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| {
                        (!self.prefs.subscribed_only || g.subscribed)
                            && group_matches(&g.name, &self.prefs.group_filter)
                    })
                    .map(|(i, _)| i)
                    .collect();
                if visible.is_empty() {
                    ui.add_space(15.0);
                    ui.label(if self.groups.is_empty() {
                        "接続設定を保存し、\n「接続・一覧取得」を押してください。"
                    } else {
                        "該当するグループはありません。"
                    });
                }
                let mut choose = None;
                egui::ScrollArea::vertical()
                    .id_salt("group_list")
                    .show_rows(ui, 26.0, visible.len(), |ui, range| {
                        for row in range {
                            let g = &self.groups[visible[row]];
                            let label = format!(
                                "{} {}  {}",
                                if g.subscribed { "★" } else { " " },
                                g.name,
                                if g.unread > 0 {
                                    g.unread.to_string()
                                } else {
                                    String::new()
                                }
                            );
                            if ui
                                .selectable_label(self.prefs.group == g.name, label)
                                .clicked()
                            {
                                choose = Some(g.name.clone());
                            }
                        }
                    });
                if let Some(name) = choose {
                    self.select_group(name);
                }
            });
    }
    fn list_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("articles").resizable(true).default_height(310.0).min_height(180.0).max_height(ctx.content_rect().height()-240.0).show(ctx,|ui|{
            ui.horizontal(|ui|{
                ui.heading(if self.prefs.group.is_empty(){"グループを選択"}else{&self.prefs.group});
                if let Some(g)=self.groups.iter().find(|g|g.name==self.prefs.group).cloned(){
                    if ui.add_enabled(!self.busy,egui::Button::new(if g.subscribed{"★ 購読中"}else{"☆ 購読する"})).clicked(){self.submit(Action::Subscribe{server:self.prefs.server.clone(),group:g.name,value:!g.subscribed},"購読状態を保存中…");}
                    ui.small(format!("取得済み {} / 未読 {}",g.cached,g.unread));
                }
            });
            ui.horizontal_wrapped(|ui|{
                let changed=ui.checkbox(&mut self.prefs.threaded,"スレッド").changed();
                let old=self.prefs.sort;
                egui::ComboBox::from_id_salt("sort").selected_text(match self.prefs.sort{Sort::Date=>"日時順",Sort::Subject=>"件名順",Sort::Author=>"投稿者順"}).show_ui(ui,|ui|{ui.selectable_value(&mut self.prefs.sort,Sort::Date,"日時順");ui.selectable_value(&mut self.prefs.sort,Sort::Subject,"件名順");ui.selectable_value(&mut self.prefs.sort,Sort::Author,"投稿者順");});
                if changed||old!=self.prefs.sort{self.rebuild();}
                if ui.add_enabled(!self.busy&&self.online&&!self.demo&&!self.prefs.group.is_empty(),egui::Button::new("古い記事を取得")).clicked(){self.load_articles(true,true);}
                if ui.add_enabled(!self.busy&&!self.articles.is_empty(),egui::Button::new("取得済みを既読に")).clicked(){for a in &mut self.articles{a.read=true;}self.submit(Action::Read{server:self.prefs.server.clone(),group:self.prefs.group.clone(),id:None,read:true},"既読状態を保存中…");}
            });
            ui.horizontal(|ui|{
                let response=ui.add(egui::TextEdit::singleline(&mut self.search).id(egui::Id::new("article_search_input")).hint_text("件名・投稿者・保存済み本文を検索").desired_width(310.0));
                if (response.lost_focus()&&ui.input(|i|i.key_pressed(egui::Key::Enter)))||ui.button("検索").clicked(){self.load_articles(false,false);}
                if !self.search.is_empty()&&ui.small_button("解除").clicked(){self.search.clear();self.load_articles(false,false);}
                ui.small("本文未取得の記事は本文検索の対象外");
            });
            ui.separator();
            if self.articles.is_empty(){ui.add_space(14.0);ui.label(if self.prefs.group.is_empty(){"左の一覧からニュースグループを選んでください。"}else if !self.search.is_empty(){"検索条件に一致する保存記事はありません。"}else{"保存された記事概要がありません。オンラインで「新着を取得」を押してください。"});return;}
            let mut select=None;let mut toggle=None;
            let mut table=TableBuilder::new(ui).id_salt("article_table").striped(true).resizable(true).cell_layout(egui::Layout::left_to_right(egui::Align::Center)).column(Column::remainder().at_least(160.0)).column(Column::initial(150.0).range(70.0..=350.0)).column(Column::initial(205.0).range(100.0..=350.0));
            if let Some(row)=self.scroll_to.take(){table=table.scroll_to_row(row,None);}
            table.header(25.0,|mut header|{header.col(|ui|{ui.strong("状態 / 件名");});header.col(|ui|{ui.strong("投稿者");});header.col(|ui|{ui.strong("日時");});}).body(|body|{
                body.rows(27.0,self.order.len(),|mut row|{
                    let(index,depth)=self.order[row.index()];let a=&self.articles[index];row.set_selected(self.selected.as_deref()==Some(&a.message_id));
                    row.col(|ui|{ui.add_space(depth as f32*12.0);if self.prefs.threaded&&ui.small_button(if self.collapsed.contains(&a.message_id){"+"}else{"−"}).clicked(){toggle=Some(a.message_id.clone());}
                        let title=format!("{} {}{}",if a.read{"○"}else{"●"},if a.cached{"▣ "}else{""},if a.subject.is_empty(){"（件名なし）"}else{&a.subject});
                        let text=if a.read{RichText::new(title)}else{RichText::new(title).strong()};if ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click())).on_hover_text(&a.subject).clicked(){select=Some(a.message_id.clone());}
                    });row.col(|ui|{ui.add(egui::Label::new(&a.author).truncate()).on_hover_text(&a.author);});row.col(|ui|{ui.add(egui::Label::new(&a.date).truncate()).on_hover_text(&a.date);});
                    if row.response().clicked(){select=Some(a.message_id.clone());}
                });
            });
            if let Some(id)=toggle{if !self.collapsed.remove(&id){self.collapsed.insert(id);}self.rebuild();}
            if let Some(id)=select{self.select_article(id);}
        });
    }
    fn body_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.selected.is_some() && !self.busy,
                        egui::Button::new("本文を取得 / 再表示"),
                    )
                    .clicked()
                {
                    self.load_body();
                }
                if ui
                    .add_enabled(
                        self.selected.is_some() && !self.busy,
                        egui::Button::new("未読に戻す  U"),
                    )
                    .clicked()
                {
                    self.mark_read(false);
                }
                if ui.button("次の未読  N").clicked() {
                    self.navigate(true, 1);
                }
                ui.separator();
                ui.selectable_value(&mut self.body_mode, 0, "本文");
                ui.selectable_value(&mut self.body_mode, 1, "ヘッダー");
                ui.selectable_value(&mut self.body_mode, 2, "元データ");
            });
            ui.horizontal_wrapped(|ui| {
                let previous = self.charset;
                egui::ComboBox::from_id_salt("charset")
                    .selected_text(charset_name(self.charset))
                    .show_ui(ui, |ui| {
                        for n in 0..5 {
                            ui.selectable_value(&mut self.charset, n, charset_name(n));
                        }
                    });
                if previous != self.charset && !self.raw.is_empty() {
                    match decode_article(&self.raw, charset_label(self.charset)) {
                        Ok(doc) => self.document = Some(doc),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                ui.checkbox(&mut self.prefs.wrap, "折り返す");
                ui.add(egui::Slider::new(&mut self.prefs.font_size, 12.0..=24.0).text("文字"));
                if let Some(doc) = &self.document
                    && ui.small_button("コピー").clicked()
                {
                    ctx.copy_text(match self.body_mode {
                        1 => doc.headers.clone(),
                        2 => String::from_utf8_lossy(&self.raw).into_owned(),
                        _ => doc.body.clone(),
                    });
                }
            });
            ui.separator();
            if let Some(doc) = &self.document {
                let overview = self
                    .articles
                    .iter()
                    .find(|a| Some(&a.message_id) == self.selected.as_ref());
                ui.label(
                    RichText::new(if doc.subject.is_empty() {
                        overview.map(|a| a.subject.as_str()).unwrap_or("記事")
                    } else {
                        &doc.subject
                    })
                    .strong()
                    .size(19.0),
                );
                ui.small(format!("{}  ·  {}", doc.author, doc.date));
                if !doc.groups.is_empty() {
                    ui.small(&doc.groups);
                }
                ui.add_space(8.0);
                if let Some(warning) = &doc.warning {
                    ui.colored_label(Color32::from_rgb(147, 87, 24), warning);
                }
                let text = match self.body_mode {
                    1 => doc.headers.clone(),
                    2 => String::from_utf8_lossy(&self.raw).into_owned(),
                    _ => doc.body.clone(),
                };
                let mut layout = egui::text::LayoutJob::default();
                let mut line_count = 0;
                for line in text.split_inclusive('\n').take(20000) {
                    line_count += 1;
                    layout.append(
                        line,
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(self.prefs.font_size),
                            color: if self.body_mode == 0 && line.trim_start().starts_with('>') {
                                Color32::from_rgb(42, 113, 104)
                            } else {
                                Color32::from_rgb(36, 43, 52)
                            },
                            ..Default::default()
                        },
                    );
                }
                if line_count == 20000 {
                    ui.colored_label(
                        Color32::DARK_RED,
                        "画面表示は先頭20,000行までです。元データは保持しています。",
                    );
                }
                egui::ScrollArea::both()
                    .id_salt(("body", self.selected.clone(), self.body_mode))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        layout.wrap.max_width = if self.prefs.wrap {
                            ui.available_width()
                        } else {
                            f32::INFINITY
                        };
                        ui.add(egui::Label::new(layout).selectable(true).wrap_mode(
                            if self.prefs.wrap {
                                egui::TextWrapMode::Wrap
                            } else {
                                egui::TextWrapMode::Extend
                            },
                        ));
                    });
            } else {
                ui.add_space(35.0);
                ui.vertical_centered(|ui| {
                    ui.heading("記事を読む");
                    ui.add_space(10.0);
                    ui.label(if self.selected.is_some() {
                        "本文が未取得、または読み込み中です。"
                    } else {
                        "上の記事一覧から読みたい記事を選んでください。"
                    });
                    ui.add_space(8.0);
                    ui.small("N 次の未読  ·  J / K 記事移動  ·  R 新着取得");
                });
            }
        });
    }
    fn navigate_group(&mut self, step: i32) {
        let groups: Vec<_> = self
            .groups
            .iter()
            .filter(|g| {
                (!self.prefs.subscribed_only || g.subscribed)
                    && group_matches(&g.name, &self.prefs.group_filter)
            })
            .map(|g| g.name.clone())
            .collect();
        if groups.is_empty() {
            return;
        }
        let current = groups
            .iter()
            .position(|g| *g == self.prefs.group)
            .map(|i| i as i32)
            .unwrap_or(if step > 0 { -1 } else { 0 });
        self.select_group(
            groups[(current + step).rem_euclid(groups.len() as i32) as usize].clone(),
        );
    }
    fn navigate(&mut self, unread: bool, step: i32) {
        if self.order.is_empty() {
            return;
        }
        let current = self
            .order
            .iter()
            .position(|(i, _)| Some(&self.articles[*i].message_id) == self.selected.as_ref());
        let count = self.order.len() as i32;
        for distance in 1..=count {
            let index = (current
                .map(|i| i as i32)
                .unwrap_or(if step > 0 { -1 } else { 0 })
                + distance * step)
                .rem_euclid(count) as usize;
            let a = &self.articles[self.order[index].0];
            if !unread || !a.read {
                let id = a.message_id.clone();
                self.scroll_to = Some(index);
                self.select_article(id);
                return;
            }
        }
        self.status = "表示中の記事に未読はありません".into();
    }
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut remove = false;
        egui::Window::new("接続設定").open(&mut open).collapsible(false).resizable(false).default_width(450.0).show(ctx,|ui|{
            ui.label("接続先とアカウントを設定します。");
            if ui.add_enabled(!self.busy,egui::Button::new("＋ 別のサーバーを追加")).clicked(){self.draft=Server::default();self.draft_password=Zeroizing::new(String::new());}
            egui::Grid::new("server_settings").num_columns(2).spacing([14.0,10.0]).show(ui,|ui|{
                ui.label("表示名");ui.text_edit_singleline(&mut self.draft.name);ui.end_row();
                ui.label("ホスト");ui.text_edit_singleline(&mut self.draft.host);ui.end_row();
                ui.label("接続方式");egui::ComboBox::from_id_salt("security").selected_text(self.draft.security.label()).show_ui(ui,|ui|{for mode in [Security::Tls,Security::StartTls,Security::LocalPlain]{if ui.selectable_value(&mut self.draft.security,mode,mode.label()).changed(){self.draft.port=if mode==Security::Tls{563}else{119};}}});ui.end_row();
                ui.label("ポート");ui.add(egui::DragValue::new(&mut self.draft.port).range(1..=65535));ui.end_row();
                ui.label("ユーザー名");ui.text_edit_singleline(&mut self.draft.username);ui.end_row();
                ui.label("パスワード");ui.add(egui::TextEdit::singleline(&mut *self.draft_password).password(true));ui.end_row();
                ui.label("資格情報");ui.checkbox(&mut self.draft.remember_password,"OSの資格情報ストアに保存");ui.end_row();
                ui.label("1回の取得件数");ui.add(egui::DragValue::new(&mut self.draft.batch_size).range(1..=10000));ui.end_row();
            });
            ui.add_space(10.0);ui.small("パスワードは設定ファイルに保存しません。未保存の場合は起動ごとに入力してください。");
            ui.small("ホスト・ポート・認証ユーザーを変更すると別の接続先として保存し、以前の記事を保持します。");
            ui.horizontal(|ui|{if ui.add_enabled(!self.busy,egui::Button::new("設定を保存")).clicked(){save=true;}
if ui.button("閉じる").clicked(){self.settings=false;}});
            if self.servers.iter().any(|s|s.id==self.draft.id){ui.separator();ui.checkbox(&mut self.delete_confirm,"この接続先と保存記事を削除する");if ui.add_enabled(!self.busy&&self.delete_confirm,egui::Button::new("削除を実行")).clicked(){remove=true;}}
            if !self.prefs.server.is_empty(){ui.separator();if ui.add_enabled(!self.busy,egui::Button::new("このサーバーの本文キャッシュを削除")).clicked(){self.submit(Action::ClearCache(self.prefs.server.clone()),"本文キャッシュを削除中…");self.document=None;self.raw.clear();for a in &mut self.articles{a.cached=false;}}ui.small("購読と既読状態、記事概要は残ります。");}
        });
        if !open {
            self.settings = false;
        }
        if save {
            if let Err(e) = rustvn::nntp::validate_server(&self.draft) {
                self.error = Some(e.to_string());
                return;
            }
            if self
                .servers
                .iter()
                .any(|s| s.id == self.draft.id && !s.same_identity(&self.draft))
            {
                self.draft.id = uuid::Uuid::new_v4().to_string();
            }
            if self.prefs.server != self.draft.id {
                self.prefs.group.clear();
                self.groups.clear();
                self.articles.clear();
                self.order.clear();
                self.selected = None;
                self.document = None;
                self.raw.clear();
            }
            self.prefs.server = self.draft.id.clone();
            self.passwords
                .insert(self.draft.id.clone(), self.draft_password.clone());
            self.online = false;
            self.submit(
                Action::SaveServer(self.draft.clone(), self.draft_password.clone()),
                "接続設定を保存中…",
            );
            self.settings = false;
        }
        if remove {
            self.passwords.remove(&self.draft.id);
            self.submit(
                Action::DeleteServer(self.draft.id.clone()),
                "接続先を削除中…",
            );
            self.settings = false;
        }
    }
}
impl eframe::App for NewsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
        if ctx.input(|i| i.viewport().close_requested()) && self.busy && !self.cancellable {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_close = true;
        }
        if self.pending_close && !self.busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let typing = ctx.memory(|m| {
            m.has_focus(egui::Id::new("group_filter_input"))
                || m.has_focus(egui::Id::new("article_search_input"))
        });
        if !typing && !self.settings {
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowDown)) {
                self.navigate_group(1);
            }
            if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowUp)) {
                self.navigate_group(-1);
            }
            if let Some(id) = self.selected.clone() {
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
                    self.collapsed.insert(id.clone());
                    self.rebuild();
                }
                if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
                    self.collapsed.remove(&id);
                    self.rebuild();
                }
            }
            if ctx.input(|i| i.key_pressed(egui::Key::N)) {
                self.navigate(true, 1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::J)) {
                self.navigate(false, 1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::K)) {
                self.navigate(false, -1);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::U)) && !self.busy {
                self.mark_read(false);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::R)) && self.online && !self.busy && !self.demo
            {
                self.load_articles(true, false);
            }
        }
        self.toolbar(ctx);
        self.groups_panel(ctx);
        self.list_panel(ctx);
        self.body_panel(ctx);
        self.settings_window(ctx);
        if self.screenshot.is_some()
            && !self.screenshot_sent
            && !self.busy
            && self.started.elapsed() > Duration::from_secs(2)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.screenshot_sent = true;
        }
        if let Some(path) = &self.screenshot {
            let captured = ctx.input(|i| {
                i.events.iter().find_map(|event| {
                    if let egui::Event::Screenshot { image, .. } = event {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(image) = captured {
                let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                if let Err(e) = image::save_buffer(
                    path,
                    &bytes,
                    image.width() as u32,
                    image.height() as u32,
                    image::ColorType::Rgba8,
                ) {
                    eprintln!("Screenshot failed: {e}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "preferences", &self.prefs);
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cancel.cancel();
    }
}
fn charset_label(n: usize) -> Option<&'static str> {
    match n {
        1 => Some("utf-8"),
        2 => Some("iso-2022-jp"),
        3 => Some("shift_jis"),
        4 => Some("euc-jp"),
        _ => None,
    }
}
fn charset_name(n: usize) -> &'static str {
    match n {
        1 => "UTF-8",
        2 => "ISO-2022-JP",
        3 => "Shift_JIS",
        4 => "EUC-JP",
        _ => "文字コード: 自動",
    }
}
fn load_japanese_font(ctx: &egui::Context) -> bool {
    let mut candidates: Vec<PathBuf> = std::env::var_os("RUSTVN_FONT")
        .or_else(|| std::env::var_os("RUSHVN_FONT"))
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Ok(dir) = std::fs::read_dir("/System/Library/Fonts") {
        for entry in dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.contains("W3") && (name.contains("ヒラ") || name.contains("Hiragino")) {
                candidates.push(entry.path());
            }
        }
    }
    candidates.extend(
        [
            "C:/Windows/Fonts/meiryo.ttc",
            "C:/Windows/Fonts/YuGothM.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        ]
        .map(PathBuf::from),
    );
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("japanese".into(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push("japanese".into());
            }
            ctx.set_fonts(fonts);
            return true;
        }
    }
    false
}

pub fn seed_demo(path: &Path) -> anyhow::Result<()> {
    let mut db = Store::open(path)?;
    let s = Server {
        id: "demo".into(),
        name: "デモ · 保存済みのサンプル".into(),
        ..Server::default()
    };
    db.save_server(&s)?;
    let groups = [
        "fj.comp.lang.rust",
        "fj.comp.misc",
        "fj.rec.travel",
        "fj.sci.astronomy",
        "fj.test",
    ];
    for g in groups {
        db.upsert_groups(
            &s.id,
            &[Group {
                name: g.into(),
                low: 1,
                high: 3,
                ..Default::default()
            }],
        )?;
        db.subscribe(&s.id, g, true)?;
    }
    let samples = [
        (
            "Rustでニュースリーダーを作ろう",
            "高橋 <takahashi@example.invalid>",
            "",
            "久しぶりに、ニュースグループを開いてみました。\n\nグループを選び、件名を眺めて、気になった記事を読む。\nそんなWinVNの操作感を、Rustで少しずつ形にしています。\n\nまずは読むことから。\n日本語の記事、スレッド、既読と未読を大切にしたいですね。\n\n※ これは動作確認用の架空の記事です。",
        ),
        (
            "Re: Rustでニュースリーダーを作ろう",
            "佐藤 <sato@example.invalid>",
            "<demo-1@rustvn.invalid>",
            "> まずは読むことから。\n\n賛成です。取得した記事をオフラインでも読めると便利ですね。\n日本語の文字コードにも対応してほしいです。",
        ),
        (
            "日本語表示と文字コードの確認",
            "鈴木 <suzuki@example.invalid>",
            "",
            "UTF-8 / ISO-2022-JP / Shift_JIS / EUC-JP\n\nひらがな、カタカナ、漢字。\n引用や改行を保ったまま、読みやすい本文を表示します。",
        ),
    ];
    for (i, (subject, author, refs, body)) in samples.iter().enumerate() {
        let id = format!("<demo-{}@rustvn.invalid>", i + 1);
        let raw = format!(
            "From: {author}\r\nSubject: {subject}\r\nDate: Sat, 26 Sep 2026 10:0{i}:00 +0900\r\nNewsgroups: fj.comp.lang.rust\r\nMessage-ID: {id}\r\nReferences: {refs}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
        );
        let a = Article {
            number: i as u64 + 1,
            subject: (*subject).into(),
            author: (*author).into(),
            date: format!("2026-09-26 10:0{i}"),
            timestamp: i as i64,
            message_id: id.clone(),
            references: rustvn::article::message_ids(refs),
            ..Default::default()
        };
        db.save_overview(&s.id, "fj.comp.lang.rust", (1, 3), Some((1, 3)), &[a])?;
        db.save_body(&s.id, &id, raw.as_bytes(), body)?;
    }
    Ok(())
}
