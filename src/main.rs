mod engine;
mod i18n;

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Stroke, vec2};
use engine::{
    CancelHandle, DateFilter, DurationFilter, Event, Filters, Format, Info, Item, Quality, Request, Sort, SubMode, Tools,
};
use i18n::Lang;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
const RESULTS_PAGE: usize = 24;
const OLDEST_LIMIT: usize = 30;

// ------------------------------------------------------------------ settings

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    lang: Lang,
    out_dir: PathBuf,
    format: Format,
    quality: Quality,
    auto_update: bool,
    /// Downloads running at once; the rest wait in the queue.
    max_parallel: u8,
    /// Save into a sub-folder per channel.
    by_channel: bool,
    /// Browser to read cookies from ("" = off), for age-restricted / sign-in videos.
    cookies_browser: String,
    /// Unix seconds of the last successful yt-dlp update check.
    last_update_check: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lang: Lang::Pt,
            out_dir: home().join("Downloads"),
            format: Format::Video,
            quality: Quality::Best,
            auto_update: true,
            max_parallel: 3,
            by_channel: false,
            cookies_browser: String::new(),
            last_update_check: 0,
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Shows the home folder as `~` so UI text stays short and doesn't expose the user name.
fn pretty_path(p: &std::path::Path) -> String {
    match p.strip_prefix(home()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn settings_path() -> PathBuf {
    home().join("Library/Application Support/up2me/settings.json")
}

impl Settings {
    fn load() -> Self {
        std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    fn save(&self) {
        let p = settings_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }
}

// ------------------------------------------------------------------ state

enum Msg {
    Results { epoch: u64, result: Result<Vec<Item>, String> },
    Playlist { epoch: u64, result: Result<(String, Vec<Item>), String> },
    Date { epoch: u64, id: String, date: Option<String> },
    Info { url: String, result: Result<Info, String> },
    Job { id: u64, event: Event },
    Update(UpdateMsg),
}

enum UpdateMsg {
    Version(Option<String>),
    Checking,
    Updating(String),
    UpToDate,
    Installed { version: String, ytdlp: PathBuf },
    Failed(String),
}

#[derive(Clone, PartialEq)]
enum UpdateState {
    Idle,
    Checking,
    Updating(String),
    UpToDate,
    Updated(String),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum UpdateMode {
    VersionOnly,
    Check,
    Reinstall,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Search,
    Downloads,
    Settings,
}

#[derive(PartialEq)]
enum JobState {
    Queued,
    Running,
    Processing,
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

struct Job {
    id: u64,
    title: String,
    thumb: Option<String>,
    kind: String,
    state: JobState,
    fraction: f32,
    speed: Option<f64>,
    eta: Option<f64>,
    cancel: CancelHandle,
    /// Kept so a queued, failed or cancelled job can be (re)started. `None` for history entries.
    req: Option<Request>,
}

#[derive(Serialize, Deserialize, Clone)]
struct HistoryEntry {
    title: String,
    thumb: Option<String>,
    kind: String,
    path: PathBuf,
}

fn history_path() -> PathBuf {
    home().join("Library/Application Support/up2me/history.json")
}

/// Finished downloads from previous sessions (entries whose file is gone are dropped).
fn load_history() -> Vec<HistoryEntry> {
    let all: Vec<HistoryEntry> =
        std::fs::read(history_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    all.into_iter().filter(|e| e.path.exists()).collect()
}

fn save_history(jobs: &[Job]) {
    let entries: Vec<HistoryEntry> = jobs
        .iter()
        .filter_map(|j| match &j.state {
            JobState::Done(path) => Some(HistoryEntry { title: j.title.clone(), thumb: j.thumb.clone(), kind: j.kind.clone(), path: path.clone() }),
            _ => None,
        })
        .take(100)
        .collect();
    let p = history_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&entries) {
        let _ = std::fs::write(p, json);
    }
}

enum JobAction {
    Cancel(u64),
    Retry(u64),
    Remove(u64),
    CancelAll,
}

enum SearchState {
    Idle,
    Loading,
    Error(String),
}

enum InfoState {
    Loading,
    Ready(Info),
    Error(#[allow(dead_code)] String),
}

#[derive(Clone)]
struct Options {
    format: Format,
    quality: Quality,
    audio: Option<String>,
    subs_on: bool,
    sub_mode: SubMode,
    sub_langs: BTreeSet<String>,
    sub_filter: String,
}

impl Options {
    fn from_settings(s: &Settings) -> Self {
        Self {
            format: s.format,
            quality: s.quality,
            audio: None,
            subs_on: false,
            sub_mode: SubMode::File,
            sub_langs: BTreeSet::new(),
            sub_filter: String::new(),
        }
    }
}

struct Selected {
    item: Item,
    info: InfoState,
    opts: Options,
}

enum Action {
    Search,
    LoadMore,
    Select(Item),
    Quick(Item),
    DownloadSelected,
    DownloadAll,
    CloseSelected,
    Paste,
    UseClip,
    DismissClip,
}

struct Debug {
    shot: Option<PathBuf>,
    start: Instant,
    requested: bool,
    select_first: bool,
    did_select: bool,
    /// Test hook: queue the first N playlist videos.
    dl_all: Option<usize>,
    shot_after: Option<f32>,
}

struct App {
    settings: Settings,
    saved: Settings,
    tools: Result<Arc<Tools>, String>,
    tab: Tab,
    query: String,
    filters: Filters,
    results: Vec<Item>,
    search: SearchState,
    searched_query: String,
    limit: usize,
    epoch: u64,
    dates_pending: usize,
    selected: Option<Selected>,
    jobs: Vec<Job>,
    next_id: u64,
    toast: Option<(String, Instant)>,
    update: UpdateState,
    pending_update_check: bool,
    /// Title of the playlist currently listed (None for normal searches).
    playlist: Option<String>,
    history_dirty: bool,
    clip: Option<String>,
    clip_seen: String,
    was_focused: bool,
    ytdlp_version: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    debug: Debug,
}

// ------------------------------------------------------------------ helpers

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // System UI font first (looks native), then CJK/Korean fallbacks for titles in other scripts.
    let primary = ["/System/Library/Fonts/SFNS.ttf"];
    let fallbacks = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
    ];
    for (i, path) in primary.iter().chain(fallbacks.iter()).enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = format!("sys{i}");
        fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = fonts.families.entry(family.clone()).or_default();
            if i < primary.len() && family == egui::FontFamily::Proportional {
                list.insert(0, name.clone());
            } else {
                list.push(name.clone());
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn apply_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 8.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.visuals.selection.bg_fill = ACCENT;
        s.visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        s.visuals.hyperlink_color = ACCENT;
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = 9.0.into();
        }
        s.visuals.window_corner_radius = 14.0.into();
        s.visuals.menu_corner_radius = 10.0.into();
        use egui::TextStyle::*;
        s.text_styles.insert(Body, FontId::proportional(14.5));
        s.text_styles.insert(Button, FontId::proportional(14.5));
        s.text_styles.insert(Heading, FontId::proportional(22.0));
        s.text_styles.insert(Small, FontId::proportional(12.0));
    });
}

fn fmt_duration(secs: f64) -> String {
    let s = secs as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn fmt_speed(bps: f64) -> String {
    if bps >= 1e6 { format!("{:.1} MB/s", bps / 1e6) } else { format!("{:.0} KB/s", bps / 1e3) }
}

fn fmt_views(l: Lang, n: u64) -> String {
    let f = |v: f64, unit: &str| {
        let s = if v < 10.0 { format!("{v:.1}") } else { format!("{v:.0}") };
        let s = if l == Lang::Pt { s.replace('.', ",") } else { s };
        format!("{s}{unit}")
    };
    let n = n as f64;
    let body = if n >= 1e9 {
        f(n / 1e9, l.t(" bi", "B"))
    } else if n >= 1e6 {
        f(n / 1e6, l.t(" mi", "M"))
    } else if n >= 1e3 {
        f(n / 1e3, l.t(" mil", "K"))
    } else {
        format!("{n:.0}")
    };
    format!("{body} {}", l.t("visualizações", "views"))
}

fn fmt_date(l: Lang, d: &str) -> String {
    if d.len() != 8 {
        return d.to_owned();
    }
    match l {
        Lang::Pt => format!("{}/{}/{}", &d[6..8], &d[4..6], &d[0..4]),
        Lang::En => format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8]),
    }
}

fn format_label(l: Lang, f: Format) -> &'static str {
    match f {
        Format::Video => l.t("Vídeo (MP4)", "Video (MP4)"),
        Format::Mp3 => l.t("Áudio (MP3)", "Audio (MP3)"),
        Format::M4a => l.t("Áudio (M4A)", "Audio (M4A)"),
    }
}

fn quality_label(l: Lang, q: Quality) -> &'static str {
    match q {
        Quality::Best => l.t("Melhor disponível", "Best available"),
        Quality::P2160 => "2160p (4K)",
        Quality::P1440 => "1440p",
        Quality::P1080 => "1080p (Full HD)",
        Quality::P720 => "720p (HD)",
        Quality::P480 => "480p",
    }
}

fn sort_label(l: Lang, s: Sort) -> &'static str {
    match s {
        Sort::Relevance => l.t("Relevância", "Relevance"),
        Sort::Newest => l.t("Mais recentes", "Newest"),
        Sort::Oldest => l.t("Mais antigos", "Oldest"),
        Sort::MostViewed => l.t("Mais visualizados", "Most viewed"),
        Sort::LeastViewed => l.t("Menos visualizados", "Least viewed"),
        Sort::Longest => l.t("Mais longos", "Longest"),
        Sort::Shortest => l.t("Mais curtos", "Shortest"),
        Sort::Rating => l.t("Melhor avaliados", "Top rated"),
    }
}

fn date_label(l: Lang, d: DateFilter) -> &'static str {
    match d {
        DateFilter::Any => l.t("Qualquer data", "Any time"),
        DateFilter::Hour => l.t("Última hora", "Last hour"),
        DateFilter::Today => l.t("Hoje", "Today"),
        DateFilter::Week => l.t("Esta semana", "This week"),
        DateFilter::Month => l.t("Este mês", "This month"),
        DateFilter::Year => l.t("Este ano", "This year"),
    }
}

fn duration_label(l: Lang, d: DurationFilter) -> &'static str {
    match d {
        DurationFilter::Any => l.t("Qualquer duração", "Any length"),
        DurationFilter::Short => l.t("Menos de 4 min", "Under 4 min"),
        DurationFilter::Medium => l.t("4 a 20 min", "4 to 20 min"),
        DurationFilter::Long => l.t("Mais de 20 min", "Over 20 min"),
    }
}

/// Video id from watch?v=, youtu.be/, /shorts/ or /embed/ links.
fn video_id(url: &str) -> Option<String> {
    let id: String = if let Some(i) = url.find("v=") {
        url[i + 2..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
    } else {
        let rest = ["youtu.be/", "/shorts/", "/embed/"].iter().find_map(|m| url.find(m).map(|i| &url[i + m.len()..]))?;
        rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
    };
    (id.len() == 11).then_some(id)
}

fn is_url(q: &str) -> bool {
    let q = q.trim();
    q.starts_with("http://") || q.starts_with("https://") || q.starts_with("youtu.be/") || q.contains("youtube.com/")
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).strong().color(Color32::WHITE)).fill(ACCENT).corner_radius(9.0)
}

fn chip(ui: &mut egui::Ui, on: &mut bool, text: &str) {
    let btn = if *on {
        egui::Button::new(RichText::new(text.to_owned()).color(Color32::WHITE)).fill(ACCENT).stroke(Stroke::new(1.0, ACCENT))
    } else {
        egui::Button::new(text.to_owned()).fill(Color32::TRANSPARENT).stroke(Stroke::new(1.0, Color32::from_gray(100)))
    };
    if ui.add(btn.corner_radius(16.0).min_size(vec2(0.0, 28.0))).clicked() {
        *on = !*on;
    }
}

fn card<R>(ui: &mut egui::Ui, selected: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let stroke = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    let width = ui.available_width();
    egui::Frame::new()
        .fill(ui.visuals().widgets.noninteractive.weak_bg_fill)
        .stroke(stroke)
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(width - 26.0);
            add(ui)
        })
        .inner
}

fn thumbnail(ui: &mut egui::Ui, url: Option<&str>, size: egui::Vec2, duration: Option<f64>) -> egui::Response {
    let resp = match url {
        Some(u) => ui.add(egui::Image::new(u.to_owned()).fit_to_exact_size(size).corner_radius(8.0).sense(Sense::click())),
        None => {
            let (_, r) = ui.allocate_exact_size(size, Sense::click());
            ui.painter().rect_filled(r.rect, 8.0, ui.visuals().extreme_bg_color);
            r
        }
    };
    if let Some(d) = duration {
        let galley = ui.painter().layout_no_wrap(fmt_duration(d), FontId::proportional(11.5), Color32::WHITE);
        let pos = resp.rect.right_bottom() - galley.size() - vec2(10.0, 8.0);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(pos - vec2(5.0, 2.0), galley.size() + vec2(10.0, 4.0)),
            4.0,
            Color32::from_black_alpha(205),
        );
        ui.painter().galley(pos, galley, Color32::WHITE);
    }
    resp
}

fn open_path(path: &std::path::Path, reveal: bool) {
    let mut cmd = std::process::Command::new("open");
    if reveal && path.is_file() {
        cmd.arg("-R");
    }
    let _ = cmd.arg(path).spawn();
}

// ------------------------------------------------------------------ app logic

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        setup_fonts(&cc.egui_ctx);
        apply_style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let settings = Settings::load();
        let env = |k: &str| std::env::var(k).ok();
        let mut app = Self {
            saved: settings.clone(),
            settings,
            tools: Tools::locate().map(Arc::new).map_err(|e| e.to_string()),
            tab: match env("UP2ME_TAB").as_deref() {
                Some("downloads") => Tab::Downloads,
                Some("settings") => Tab::Settings,
                _ => Tab::Search,
            },
            query: String::new(),
            filters: Filters::default(),
            results: Vec::new(),
            search: SearchState::Idle,
            searched_query: String::new(),
            limit: RESULTS_PAGE,
            epoch: 0,
            dates_pending: 0,
            selected: None,
            jobs: Vec::new(),
            next_id: 1,
            toast: None,
            update: UpdateState::Idle,
            pending_update_check: false,
            playlist: None,
            history_dirty: false,
            clip: env("UP2ME_CLIP"),
            clip_seen: String::new(),
            was_focused: false,
            ytdlp_version: None,
            tx,
            rx,
            debug: Debug {
                shot: env("UP2ME_SHOT").map(PathBuf::from),
                start: Instant::now(),
                requested: false,
                select_first: env("UP2ME_SELECT").is_some(),
                did_select: false,
                dl_all: env("UP2ME_DLALL").and_then(|n| n.parse().ok()),
                shot_after: env("UP2ME_SHOT_AFTER").and_then(|n| n.parse().ok()),
            },
        };
        engine::set_cookies_browser(Some(app.settings.cookies_browser.clone()));
        for e in load_history() {
            let id = app.next_id;
            app.next_id += 1;
            app.jobs.push(Job {
                id,
                title: e.title,
                thumb: e.thumb,
                kind: e.kind,
                state: JobState::Done(e.path),
                fraction: 1.0,
                speed: None,
                eta: None,
                cancel: CancelHandle::default(),
                req: None,
            });
        }
        let due = app.settings.auto_update && now_secs().saturating_sub(app.settings.last_update_check) > 24 * 3600;
        let mode = if env("UP2ME_UPDATE").as_deref() == Some("reinstall") {
            UpdateMode::Reinstall
        } else if due {
            UpdateMode::Check
        } else {
            UpdateMode::VersionOnly
        };
        app.start_update(mode, &cc.egui_ctx);
        // Debug/test hooks (no effect unless the env vars are set).
        if let Some(s) = env("UP2ME_SORT") {
            app.filters.sort = Sort::ALL.iter().copied().find(|x| format!("{x:?}") == s).unwrap_or_default();
        }
        if let Some(url) = env("UP2ME_DL") {
            let mut opts = Options::from_settings(&app.settings);
            opts.format = match env("UP2ME_FMT").as_deref() {
                Some("mp3") => Format::Mp3,
                Some("m4a") => Format::M4a,
                _ => Format::Video,
            };
            opts.quality = Quality::P480;
            opts.audio = env("UP2ME_AUDIO");
            if let Some(subs) = env("UP2ME_SUBS") {
                opts.subs_on = true;
                opts.sub_langs = subs.split(',').map(String::from).collect();
                opts.sub_mode = match env("UP2ME_SUBMODE").as_deref() {
                    Some("embed") => SubMode::Embed,
                    Some("only") => SubMode::Only,
                    _ => SubMode::File,
                };
            }
            if let Some(dir) = env("UP2ME_OUT") {
                app.settings.out_dir = PathBuf::from(dir);
                app.saved.out_dir = app.settings.out_dir.clone(); // never persist test folders
            }
            let item = Item { id: video_id(&url).unwrap_or_default(), url, title: String::new(), channel: None, duration: None, views: None, upload_date: None, live: false };
            app.enqueue(&item, &opts, None, &cc.egui_ctx);
            app.tab = Tab::Downloads;
        }
        if env("UP2ME_CC").is_some() {
            app.filters.creative_commons = true;
        }
        if let Some(q) = env("UP2ME_QUERY") {
            app.query = q;
            app.run_query(&cc.egui_ctx);
        }
        app
    }

    fn lang(&self) -> Lang {
        self.settings.lang
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn run_query(&mut self, ctx: &egui::Context) {
        let q = self.query.trim().to_owned();
        if q.is_empty() {
            return;
        }
        if is_url(&q) {
            let url = if q.starts_with("http") { q } else { format!("https://{q}") };
            if engine::is_playlist_url(&url) {
                self.start_playlist(url, ctx);
                return;
            }
            let item = Item {
                id: video_id(&url).unwrap_or_default(),
                url,
                title: String::new(),
                channel: None,
                duration: None,
                views: None,
                upload_date: None,
                live: false,
            };
            self.select(item, ctx);
        } else {
            self.limit = if self.filters.sort == Sort::Oldest { OLDEST_LIMIT } else { RESULTS_PAGE };
            self.start_search(ctx);
        }
    }

    fn start_playlist(&mut self, url: String, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        self.epoch += 1;
        self.search = SearchState::Loading;
        self.searched_query = self.query.trim().to_owned();
        self.dates_pending = 0;
        let (epoch, tx, ctx) = (self.epoch, self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = engine::fetch_playlist(&tools, &url, 300).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Playlist { epoch, result });
            ctx.request_repaint();
        });
    }

    fn start_search(&mut self, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        self.epoch += 1;
        self.search = SearchState::Loading;
        self.searched_query = self.query.trim().to_owned();
        self.dates_pending = 0;
        let (epoch, q, f, limit, tx, ctx) =
            (self.epoch, self.searched_query.clone(), self.filters, self.limit, self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = engine::search(&tools, &q, &f, limit).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Results { epoch, result });
            ctx.request_repaint();
        });
    }

    /// "Oldest first" needs upload dates, which YouTube's search listing doesn't include:
    /// fetch them per video with a few parallel workers.
    fn enrich_dates(&mut self, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        let ids: VecDeque<String> = self.results.iter().filter(|i| i.upload_date.is_none()).map(|i| i.id.clone()).collect();
        self.dates_pending = ids.len();
        let queue = Arc::new(Mutex::new(ids));
        for _ in 0..6 {
            let (queue, tools, tx, ctx, epoch) = (queue.clone(), tools.clone(), self.tx.clone(), ctx.clone(), self.epoch);
            std::thread::spawn(move || {
                while let Some(id) = { let next = queue.lock().unwrap().pop_front(); next } {
                    let date = engine::fetch_upload_date(&tools, &id);
                    let _ = tx.send(Msg::Date { epoch, id, date });
                    ctx.request_repaint();
                }
            });
        }
    }

    /// Reads the current yt-dlp version and, depending on `mode`, checks GitHub for a newer
    /// release and installs it in the background.
    fn start_update(&mut self, mode: UpdateMode, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        if matches!(self.update, UpdateState::Checking | UpdateState::Updating(_)) {
            return;
        }
        if mode != UpdateMode::VersionOnly {
            self.update = UpdateState::Checking;
        }
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let send = |m: UpdateMsg| {
                let _ = tx.send(Msg::Update(m));
                ctx.request_repaint();
            };
            let current = engine::current_version(&tools);
            send(UpdateMsg::Version(current.clone()));
            if mode == UpdateMode::VersionOnly {
                return;
            }
            send(UpdateMsg::Checking);
            let latest = match engine::latest_version() {
                Ok(v) => v,
                Err(e) => return send(UpdateMsg::Failed(e.to_string())),
            };
            let newer = current.as_deref().is_none_or(|c| engine::is_newer(&latest, c));
            if !newer && mode != UpdateMode::Reinstall {
                return send(UpdateMsg::UpToDate);
            }
            send(UpdateMsg::Updating(latest.clone()));
            match engine::install_ytdlp(&latest) {
                Ok(ytdlp) => send(UpdateMsg::Installed { version: latest, ytdlp }),
                Err(e) => send(UpdateMsg::Failed(e.to_string())),
            }
        });
    }

    fn select(&mut self, item: Item, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        let url = item.url.clone();
        self.selected = Some(Selected { item, info: InfoState::Loading, opts: Options::from_settings(&self.settings) });
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = engine::fetch_info(&tools, &url).map_err(|e| e.to_string());
            let _ = tx.send(Msg::Info { url, result });
            ctx.request_repaint();
        });
    }

    /// Adds a download to the queue; `pump_queue` starts it when a slot is free.
    fn enqueue(&mut self, item: &Item, opts: &Options, subdir: Option<String>, ctx: &egui::Context) {
        self.enqueue_quiet(item, opts, subdir);
        let l = self.lang();
        self.toast(l.t("Adicionado em Downloads", "Added to Downloads"));
        ctx.request_repaint();
    }

    fn enqueue_quiet(&mut self, item: &Item, opts: &Options, subdir: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;
        let l = self.lang();
        let subs_only = opts.subs_on && opts.sub_mode == SubMode::Only && !opts.sub_langs.is_empty();
        let kind = if subs_only {
            l.t("Legendas", "Subtitles").to_owned()
        } else {
            match opts.format {
                Format::Video => format!("MP4 · {}", quality_label(l, opts.quality)),
                Format::Mp3 => "MP3".into(),
                Format::M4a => "M4A".into(),
            }
        };
        let req = Request {
            url: item.url.clone(),
            format: opts.format,
            quality: opts.quality,
            out_dir: self.settings.out_dir.clone(),
            audio_lang: opts.audio.clone(),
            sub_langs: if opts.subs_on { opts.sub_langs.iter().cloned().collect() } else { Vec::new() },
            sub_mode: opts.sub_mode,
            subdir,
            by_channel: self.settings.by_channel,
        };
        self.jobs.insert(
            0,
            Job {
                id,
                title: if item.title.is_empty() { item.url.clone() } else { item.title.clone() },
                thumb: item.thumb(),
                kind,
                state: JobState::Queued,
                fraction: 0.0,
                speed: None,
                eta: None,
                cancel: CancelHandle::default(),
                req: Some(req),
            },
        );
    }

    /// Starts queued jobs (oldest first) while fewer than `max_parallel` are running.
    fn pump_queue(&mut self, ctx: &egui::Context) {
        let Ok(tools) = self.tools.clone() else { return };
        let max = self.settings.max_parallel.clamp(1, 8) as usize;
        let mut active = self.jobs.iter().filter(|j| matches!(j.state, JobState::Running | JobState::Processing)).count();
        for i in (0..self.jobs.len()).rev() {
            if active >= max {
                break;
            }
            if self.jobs[i].state != JobState::Queued {
                continue;
            }
            let Some(req) = self.jobs[i].req.clone() else { continue };
            self.jobs[i].state = JobState::Running;
            active += 1;
            let (id, cancel) = (self.jobs[i].id, self.jobs[i].cancel.clone());
            let (tools, tx, ctx) = (tools.clone(), self.tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                engine::download(&tools, &req, &cancel, |event| {
                    let _ = tx.send(Msg::Job { id, event });
                    ctx.request_repaint();
                });
            });
        }
    }

    fn download_all(&mut self, ctx: &egui::Context) {
        let items: Vec<Item> = self.visible().into_iter().cloned().collect();
        let opts = Options::from_settings(&self.settings);
        let folder = self.playlist.clone();
        for it in &items {
            self.enqueue_quiet(it, &opts, folder.clone());
        }
        let l = self.lang();
        self.toast(format!("{} {}", items.len(), l.t("vídeos adicionados à fila", "videos added to the queue")));
        ctx.request_repaint();
    }

    fn apply_job_action(&mut self, a: JobAction) {
        match a {
            JobAction::Cancel(id) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) {
                    match j.state {
                        JobState::Queued => j.state = JobState::Cancelled,
                        JobState::Running | JobState::Processing => j.cancel.cancel(),
                        _ => {}
                    }
                }
            }
            JobAction::CancelAll => {
                for j in &mut self.jobs {
                    match j.state {
                        JobState::Queued => j.state = JobState::Cancelled,
                        JobState::Running | JobState::Processing => j.cancel.cancel(),
                        _ => {}
                    }
                }
            }
            JobAction::Retry(id) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id && j.req.is_some()) {
                    j.state = JobState::Queued;
                    j.fraction = 0.0;
                    j.speed = None;
                    j.eta = None;
                    j.cancel = CancelHandle::default();
                }
            }
            JobAction::Remove(id) => {
                self.jobs.retain(|j| j.id != id);
                self.history_dirty = true;
            }
        }
    }

    fn check_clipboard(&mut self) {
        if self.debug.shot.is_some() {
            return;
        }
        let Ok(out) = std::process::Command::new("pbpaste").output() else { return };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if text.len() < 300 && !text.contains('\n') && is_url(&text) && text != self.clip_seen && text != self.query.trim() {
            self.clip = Some(text);
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|i| i.focused);
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Results { epoch, result } if epoch == self.epoch => match result {
                    Ok(items) => {
                        self.results = items;
                        self.playlist = None;
                        self.search = SearchState::Idle;
                        if self.filters.sort == Sort::Oldest {
                            self.enrich_dates(ctx);
                        }
                    }
                    Err(e) => self.search = SearchState::Error(e),
                },
                Msg::Results { .. } => {}
                Msg::Playlist { epoch, result } if epoch == self.epoch => match result {
                    Ok((title, items)) => {
                        self.results = items;
                        self.playlist = Some(title);
                        self.search = SearchState::Idle;
                    }
                    Err(e) => self.search = SearchState::Error(e),
                },
                Msg::Playlist { .. } => {}
                Msg::Date { epoch, id, date } if epoch == self.epoch => {
                    self.dates_pending = self.dates_pending.saturating_sub(1);
                    if let Some(it) = self.results.iter_mut().find(|i| i.id == id) {
                        it.upload_date = date;
                    }
                }
                Msg::Date { .. } => {}
                Msg::Info { url, result } => {
                    if let Some(sel) = self.selected.as_mut().filter(|s| s.item.url == url) {
                        match result {
                            Ok(info) => {
                                if sel.item.title.is_empty() {
                                    sel.item.title = info.title.clone();
                                }
                                if sel.item.channel.is_none() {
                                    sel.item.channel = info.uploader.clone();
                                }
                                if sel.item.duration.is_none() {
                                    sel.item.duration = info.duration;
                                }
                                if sel.item.views.is_none() {
                                    sel.item.views = info.view_count;
                                }
                                sel.item.upload_date = info.upload_date.clone().or(sel.item.upload_date.take());
                                if sel.item.id.is_empty() {
                                    // Direct link: recover the id from the thumbnail URL for the preview.
                                    if let Some(id) = info.thumbnail.as_deref().and_then(|t| t.split("/vi/").nth(1)).and_then(|t| t.split('/').next()) {
                                        sel.item.id = id.to_owned();
                                    }
                                }
                                sel.info = InfoState::Ready(info);
                            }
                            Err(e) => sel.info = InfoState::Error(e),
                        }
                    }
                }
                Msg::Update(m) => match m {
                    UpdateMsg::Version(v) => self.ytdlp_version = v,
                    UpdateMsg::Checking => self.update = UpdateState::Checking,
                    UpdateMsg::Updating(v) => self.update = UpdateState::Updating(v),
                    UpdateMsg::UpToDate => {
                        self.update = UpdateState::UpToDate;
                        self.settings.last_update_check = now_secs();
                    }
                    UpdateMsg::Installed { version, ytdlp } => {
                        if let Ok(t) = &self.tools {
                            // New downloads use the new copy; running ones keep their own.
                            self.tools = Ok(Arc::new(Tools { ytdlp, ffmpeg: t.ffmpeg.clone() }));
                        }
                        self.ytdlp_version = Some(version.clone());
                        self.update = UpdateState::Updated(version);
                        self.settings.last_update_check = now_secs();
                        let l = self.settings.lang;
                        self.toast(l.t("yt-dlp atualizado", "yt-dlp updated"));
                    }
                    UpdateMsg::Failed(e) => self.update = UpdateState::Failed(e),
                },
                Msg::Job { id, event } => {
                    let l = self.settings.lang;
                    let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) else { continue };
                    match event {
                        Event::Title(t) => job.title = t,
                        Event::Progress { fraction, speed, eta } => {
                            job.fraction = fraction;
                            job.speed = speed;
                            job.eta = eta;
                            if fraction >= 1.0 {
                                job.state = JobState::Processing;
                            }
                        }
                        Event::Processing => job.state = JobState::Processing,
                        Event::Done(p) => {
                            job.fraction = 1.0;
                            job.state = JobState::Done(p);
                            self.history_dirty = true;
                            if !focused {
                                let script = format!(
                                    "display notification {:?} with title \"up2me\"",
                                    format!("{}: {}", l.t("Concluído", "Finished"), job.title)
                                );
                                let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
                            }
                        }
                        Event::Failed(e) => job.state = JobState::Failed(e),
                        Event::Cancelled => job.state = JobState::Cancelled,
                    }
                }
            }
        }
    }

    /// Client-side filtering/sorting of the fetched results.
    fn visible(&self) -> Vec<&Item> {
        let f = &self.filters;
        let mut v: Vec<&Item> = self
            .results
            .iter()
            .filter(|i| !(f.hide_shorts && i.duration.is_some_and(|d| d < 61.0)))
            .filter(|i| f.min_views == 0 || i.views.unwrap_or(0) >= f.min_views)
            .collect();
        match f.sort {
            Sort::Oldest => v.sort_by(|a, b| match (&a.upload_date, &b.upload_date) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }),
            Sort::LeastViewed => v.sort_by_key(|i| i.views.unwrap_or(u64::MAX)),
            Sort::Longest => v.sort_by(|a, b| b.duration.unwrap_or(0.0).total_cmp(&a.duration.unwrap_or(0.0))),
            Sort::Shortest => v.sort_by(|a, b| a.duration.unwrap_or(f64::MAX).total_cmp(&b.duration.unwrap_or(f64::MAX))),
            _ => {}
        }
        v
    }

    // -------------------------------------------------------------- UI pieces

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let l = self.lang();
        let active = self.jobs.iter().filter(|j| matches!(j.state, JobState::Running | JobState::Processing)).count();
        egui::Frame::new().inner_margin(egui::Margin::symmetric(18, 12)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::Image::new(egui::include_image!("../assets/icon-1024.png")).fit_to_exact_size(vec2(34.0, 34.0)));
                ui.label(RichText::new("up2me").size(21.0).strong());
                ui.add_space(16.0);
                let downloads = if active > 0 {
                    format!("{} ({active})", l.t("Downloads", "Downloads"))
                } else {
                    l.t("Downloads", "Downloads").to_owned()
                };
                for (tab, label) in [
                    (Tab::Search, l.t("Buscar", "Search").to_owned()),
                    (Tab::Downloads, downloads),
                    (Tab::Settings, l.t("Ajustes", "Settings").to_owned()),
                ] {
                    let text = RichText::new(label).size(15.0);
                    let text = if self.tab == tab { text.strong().color(Color32::WHITE) } else { text };
                    let btn = egui::Button::new(text)
                        .fill(if self.tab == tab { ACCENT } else { Color32::TRANSPARENT })
                        .corner_radius(18.0)
                        .min_size(vec2(0.0, 32.0));
                    if ui.add(btn).clicked() {
                        self.tab = tab;
                    }
                }
            });
        });
    }

    fn search_tab(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let l = self.lang();
        // --- link found on the clipboard
        if let Some(url) = self.clip.clone() {
            egui::Frame::new()
                .fill(ACCENT.gamma_multiply(0.16))
                .stroke(Stroke::new(1.0, ACCENT))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(l.t("Link na área de transferência:", "Link on the clipboard:"));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(l.t("Ignorar", "Dismiss")).clicked() {
                                actions.push(Action::DismissClip);
                            }
                            if ui.add(primary_button(l.t("Abrir", "Open"))).clicked() {
                                actions.push(Action::UseClip);
                            }
                            ui.add(egui::Label::new(RichText::new(&url).weak()).truncate());
                        });
                    });
                });
        }
        // --- search bar
        ui.horizontal(|ui| {
            let w = ui.available_width() - 230.0;
            let resp = ui.add_sized(
                [w.max(200.0), 40.0],
                egui::TextEdit::singleline(&mut self.query)
                    .font(FontId::proportional(16.0))
                    .margin(egui::Margin::symmetric(14, 10))
                    .hint_text(l.t("Pesquise no YouTube ou cole um link…", "Search YouTube or paste a link…")),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                actions.push(Action::Search);
            }
            if ui.add_sized([70.0, 40.0], egui::Button::new(l.t("Colar", "Paste"))).clicked() {
                actions.push(Action::Paste);
            }
            let label = if is_url(&self.query) { l.t("Abrir link", "Open link") } else { l.t("Buscar", "Search") };
            if ui.add_enabled(!self.query.trim().is_empty(), primary_button(label).min_size(vec2(100.0, 40.0))).clicked() {
                actions.push(Action::Search);
            }
        });

        // --- filters
        let f = &mut self.filters;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("sort").selected_text(sort_label(l, f.sort)).width(160.0).show_ui(ui, |ui| {
                for s in Sort::ALL {
                    ui.selectable_value(&mut f.sort, s, sort_label(l, s));
                }
            });
            egui::ComboBox::from_id_salt("date").selected_text(date_label(l, f.date)).width(140.0).show_ui(ui, |ui| {
                for d in DateFilter::ALL {
                    ui.selectable_value(&mut f.date, d, date_label(l, d));
                }
            });
            egui::ComboBox::from_id_salt("dur").selected_text(duration_label(l, f.duration)).width(150.0).show_ui(ui, |ui| {
                for d in DurationFilter::ALL {
                    ui.selectable_value(&mut f.duration, d, duration_label(l, d));
                }
            });
            let views_text = match f.min_views {
                0 => l.t("Qualquer nº de views", "Any views").to_owned(),
                n => format!("≥ {}", fmt_views(l, n)),
            };
            egui::ComboBox::from_id_salt("views").selected_text(views_text).width(170.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut f.min_views, 0, l.t("Qualquer nº de views", "Any views"));
                for n in [10_000u64, 100_000, 1_000_000, 10_000_000] {
                    ui.selectable_value(&mut f.min_views, n, format!("≥ {}", fmt_views(l, n)));
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            chip(ui, &mut f.hd, "HD");
            chip(ui, &mut f.uhd, "4K");
            chip(ui, &mut f.captions, l.t("Com legendas", "Subtitles (CC)"));
            chip(ui, &mut f.creative_commons, "Creative Commons");
            chip(ui, &mut f.hide_shorts, l.t("Ocultar Shorts (< 1 min)", "Hide Shorts (< 1 min)"));
            if *f != Filters::default() && ui.link(l.t("Limpar filtros", "Clear filters")).clicked() {
                *f = Filters::default();
            }
        });
        ui.add_space(2.0);

        if let Err(e) = &self.tools {
            ui.colored_label(Color32::LIGHT_RED, e);
            return;
        }

        // --- status + results
        match &self.search {
            SearchState::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(l.t("Buscando…", "Searching…"));
                });
                return;
            }
            SearchState::Error(e) => {
                ui.colored_label(Color32::LIGHT_RED, e);
                return;
            }
            SearchState::Idle => {}
        }
        if self.results.is_empty() {
            ui.add_space(70.0);
            ui.vertical_centered(|ui| {
                if self.searched_query.is_empty() {
                    ui.label(RichText::new(l.t("O que você quer baixar?", "What do you want to download?")).size(24.0).strong());
                    ui.add_space(4.0);
                    ui.weak(l.t(
                        "Pesquise por um termo ou cole o link de um vídeo. Use os filtros para refinar.",
                        "Search for a term or paste a video link. Use the filters to refine.",
                    ));
                } else {
                    ui.label(RichText::new(l.t("Nenhum resultado", "No results")).size(20.0).strong());
                    ui.weak(l.t("Tente outros termos ou remova alguns filtros.", "Try other terms or remove some filters."));
                }
            });
            return;
        }

        let visible: Vec<Item> = self.visible().into_iter().cloned().collect();
        ui.horizontal(|ui| {
            if let Some(title) = &self.playlist {
                ui.label(RichText::new(format!("{}: {}", l.t("Playlist", "Playlist"), if title.is_empty() { "?" } else { title })).strong());
            }
            let noun = if self.playlist.is_some() { l.t("vídeos", "videos") } else { l.t("resultados", "results") };
            ui.weak(format!("{} {noun}", visible.len()));
            if self.playlist.is_some() && !visible.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(primary_button(&format!("{} ({})", l.t("Baixar todos", "Download all"), visible.len()))).clicked() {
                        actions.push(Action::DownloadAll);
                    }
                });
            }
            if self.dates_pending > 0 {
                ui.spinner();
                ui.weak(format!("{} ({})", l.t("Ordenando por data…", "Sorting by date…"), self.dates_pending));
            }
        });
        let selected_url = self.selected.as_ref().map(|s| s.item.url.clone());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for item in &visible {
                let is_sel = selected_url.as_deref() == Some(item.url.as_str());
                card(ui, is_sel, |ui| {
                    ui.horizontal(|ui| {
                        if thumbnail(ui, item.thumb().as_deref(), vec2(176.0, 99.0), item.duration).clicked() {
                            actions.push(Action::Select(item.clone()));
                        }
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(RichText::new(&item.title).strong().size(15.5)).truncate())
                                .on_hover_text(&item.title);
                            let mut meta: Vec<String> = Vec::new();
                            if let Some(c) = &item.channel {
                                meta.push(c.clone());
                            }
                            if item.live {
                                meta.push(l.t("AO VIVO", "LIVE").to_owned());
                            }
                            if let Some(v) = item.views {
                                meta.push(fmt_views(l, v));
                            }
                            if let Some(d) = &item.upload_date {
                                meta.push(fmt_date(l, d));
                            }
                            ui.add(egui::Label::new(RichText::new(meta.join("  ·  ")).weak()).truncate());
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                if ui.add(primary_button(l.t("Baixar", "Download")).min_size(vec2(90.0, 30.0))).clicked() {
                                    actions.push(Action::Quick(item.clone()));
                                }
                                if ui.button(l.t("Opções…", "Options…")).clicked() {
                                    actions.push(Action::Select(item.clone()));
                                }
                            });
                        });
                    });
                });
                ui.add_space(2.0);
            }
            ui.add_space(6.0);
            if self.filters.sort != Sort::Oldest && self.playlist.is_none() {
                ui.vertical_centered(|ui| {
                    if ui.button(l.t("Mostrar mais resultados", "Show more results")).clicked() {
                        actions.push(Action::LoadMore);
                    }
                });
            }
            ui.add_space(14.0);
        });
    }

    fn options_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let l = self.lang();
        let Some(sel) = self.selected.as_mut() else { return };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(l.t("Opções de download", "Download options")).size(17.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new("×").size(18.0)).min_size(vec2(32.0, 28.0))).clicked() {
                        actions.push(Action::CloseSelected);
                    }
                });
            });
            ui.add_space(4.0);

            let w = ui.available_width();
            thumbnail(ui, sel.item.thumb().as_deref(), vec2(w, w * 9.0 / 16.0), sel.item.duration);
            if sel.item.title.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(l.t("Carregando informações…", "Loading info…"));
                });
            } else {
                ui.label(RichText::new(&sel.item.title).strong().size(15.5));
                let mut meta: Vec<String> = Vec::new();
                if let Some(c) = &sel.item.channel {
                    meta.push(c.clone());
                }
                if let Some(v) = sel.item.views {
                    meta.push(fmt_views(l, v));
                }
                if let Some(d) = &sel.item.upload_date {
                    meta.push(fmt_date(l, d));
                }
                ui.weak(meta.join("  ·  "));
            }
            ui.separator();

            let o = &mut sel.opts;
            // --- format
            ui.label(RichText::new(l.t("Formato", "Format")).strong());
            ui.horizontal(|ui| {
                for f in Format::ALL {
                    ui.selectable_value(&mut o.format, f, format_label(l, f));
                }
            });
            if o.format == Format::Video {
                ui.label(RichText::new(l.t("Qualidade", "Quality")).strong());
                egui::ComboBox::from_id_salt("q").selected_text(quality_label(l, o.quality)).width(ui.available_width()).show_ui(ui, |ui| {
                    for q in Quality::ALL {
                        ui.selectable_value(&mut o.quality, q, quality_label(l, q));
                    }
                });
                if let InfoState::Ready(info) = &sel.info {
                    let h = info.max_height();
                    if h > 0 {
                        ui.weak(format!("{} {h}p", l.t("Resolução máxima deste vídeo:", "Max resolution for this video:")));
                    }
                }
            }

            // --- audio track
            if let InfoState::Ready(info) = &sel.info {
                let tracks = info.audio_tracks();
                if tracks.len() > 1 {
                    ui.add_space(4.0);
                    ui.label(RichText::new(l.t("Faixa de áudio", "Audio track")).strong());
                    let current = o
                        .audio
                        .as_ref()
                        .and_then(|a| tracks.iter().find(|t| &t.lang == a).map(|t| t.label.clone()))
                        .unwrap_or_else(|| l.t("Padrão (original)", "Default (original)").to_owned());
                    egui::ComboBox::from_id_salt("audio").selected_text(current).width(ui.available_width()).show_ui(ui, |ui| {
                        ui.selectable_value(&mut o.audio, None, l.t("Padrão (original)", "Default (original)"));
                        for t in &tracks {
                            ui.selectable_value(&mut o.audio, Some(t.lang.clone()), format!("{}  ({})", t.label, t.lang));
                        }
                    });
                    ui.weak(format!("{} {}", tracks.len(), l.t("faixas disponíveis", "tracks available")));
                }
            }

            // --- subtitles
            ui.add_space(4.0);
            ui.separator();
            ui.checkbox(&mut o.subs_on, RichText::new(l.t("Baixar legendas", "Download subtitles")).strong());
            if o.subs_on {
                match &sel.info {
                    InfoState::Loading => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.weak(l.t("Carregando idiomas…", "Loading languages…"));
                        });
                    }
                    InfoState::Error(_) => {
                        ui.weak(l.t("Não foi possível listar as legendas.", "Couldn't list the subtitles."));
                    }
                    InfoState::Ready(info) => {
                        let langs = info.sub_langs();
                        if langs.is_empty() {
                            ui.weak(l.t("Este vídeo não tem legendas.", "This video has no subtitles."));
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                ui.selectable_value(&mut o.sub_mode, SubMode::File, l.t("Arquivo .srt", ".srt file"));
                                if o.format == Format::Video {
                                    ui.selectable_value(&mut o.sub_mode, SubMode::Embed, l.t("Embutir no vídeo", "Embed in video"));
                                }
                                ui.selectable_value(&mut o.sub_mode, SubMode::Only, l.t("Só legendas", "Subtitles only"));
                            });
                            if o.format != Format::Video && o.sub_mode == SubMode::Embed {
                                o.sub_mode = SubMode::File;
                            }
                            ui.add(
                                egui::TextEdit::singleline(&mut o.sub_filter)
                                    .hint_text(l.t("Filtrar idiomas…", "Filter languages…"))
                                    .desired_width(f32::INFINITY),
                            );
                            ui.horizontal_wrapped(|ui| {
                                for (label, prefix) in [("Português", "pt"), ("English", "en"), ("Español", "es")] {
                                    if ui.small_button(label).clicked() {
                                        // Manual track preferred; fall back to the auto-caption variant.
                                        let pick = langs
                                            .iter()
                                            .filter(|s| s.code == prefix || s.code.starts_with(&format!("{prefix}-")))
                                            .min_by_key(|s| (s.auto, s.code.len()));
                                        if let Some(p) = pick {
                                            o.sub_langs.insert(p.code.clone());
                                        }
                                    }
                                }
                                if !o.sub_langs.is_empty() && ui.small_button(l.t("Limpar", "Clear")).clicked() {
                                    o.sub_langs.clear();
                                }
                            });
                            let filter = o.sub_filter.to_lowercase();
                            egui::Frame::new()
                                .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                                .corner_radius(8.0)
                                .inner_margin(6.0)
                                .show(ui, |ui| {
                                    egui::ScrollArea::vertical().max_height(170.0).auto_shrink([false, true]).id_salt("subs").show(ui, |ui| {
                                        for s in langs.iter().filter(|s| {
                                            filter.is_empty() || s.name.to_lowercase().contains(&filter) || s.code.to_lowercase().contains(&filter)
                                        }) {
                                            let mut on = o.sub_langs.contains(&s.code);
                                            let label = if s.auto {
                                                format!("{} · {} ({})", s.name, s.code, l.t("automática", "auto"))
                                            } else {
                                                format!("{} · {}", s.name, s.code)
                                            };
                                            if ui.checkbox(&mut on, label).changed() {
                                                if on {
                                                    o.sub_langs.insert(s.code.clone());
                                                } else {
                                                    o.sub_langs.remove(&s.code);
                                                }
                                            }
                                        }
                                    });
                                });
                            if o.sub_langs.is_empty() {
                                ui.weak(l.t("Marque ao menos um idioma.", "Tick at least one language."));
                            } else {
                                ui.weak(format!("{}: {}", l.t("Selecionadas", "Selected"), o.sub_langs.iter().cloned().collect::<Vec<_>>().join(", ")));
                            }
                        }
                    }
                }
            }

            // --- destination + go
            ui.add_space(4.0);
            ui.separator();
            ui.label(RichText::new(l.t("Salvar em", "Save to")).strong());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(RichText::new(pretty_path(&self.settings.out_dir)).weak()).truncate());
            });
            if ui.button(l.t("Alterar pasta…", "Change folder…")).clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_directory(&self.settings.out_dir).pick_folder() {
                    self.settings.out_dir = dir;
                }
            }
            ui.add_space(8.0);
            let o = &sel.opts;
            let needs_langs = o.subs_on && o.sub_mode == SubMode::Only && o.sub_langs.is_empty();
            let go = ui.add_enabled(!needs_langs, primary_button(l.t("Baixar", "Download")).min_size(vec2(ui.available_width(), 42.0)));
            if go.clicked() {
                actions.push(Action::DownloadSelected);
            }
            ui.add_space(10.0);
        });
    }

    fn downloads_tab(&mut self, ui: &mut egui::Ui) {
        let l = self.lang();
        let mut actions: Vec<JobAction> = Vec::new();
        let busy = self.jobs.iter().filter(|j| matches!(j.state, JobState::Queued | JobState::Running | JobState::Processing)).count();
        ui.horizontal(|ui| {
            ui.label(RichText::new(l.t("Downloads", "Downloads")).size(20.0).strong());
            if busy > 0 {
                ui.weak(format!("{busy} {}", l.t("em andamento ou na fila", "active or queued")));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link(l.t("Limpar concluídos", "Clear finished")).clicked() {
                    self.jobs.retain(|j| matches!(j.state, JobState::Queued | JobState::Running | JobState::Processing));
                    self.history_dirty = true;
                }
                if ui.link(l.t("Abrir pasta", "Open folder")).clicked() {
                    open_path(&self.settings.out_dir, false);
                }
                if busy > 0 && ui.link(l.t("Cancelar todos", "Cancel all")).clicked() {
                    actions.push(JobAction::CancelAll);
                }
            });
        });
        if self.jobs.is_empty() {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(l.t("Nada por aqui ainda", "Nothing here yet")).size(20.0).strong());
                ui.weak(l.t("Os downloads aparecem aqui com progresso e velocidade.", "Downloads show up here with progress and speed."));
            });
            return;
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for job in &self.jobs {
                card(ui, false, |ui| {
                    ui.horizontal(|ui| {
                        thumbnail(ui, job.thumb.as_deref(), vec2(128.0, 72.0), None);
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(RichText::new(&job.title).strong().size(15.0)).truncate());
                            match &job.state {
                                JobState::Queued => {
                                    ui.horizontal(|ui| {
                                        ui.weak(format!("{}  ·  {}", job.kind, l.t("Na fila", "Queued")));
                                        if ui.small_button(l.t("Cancelar", "Cancel")).clicked() {
                                            actions.push(JobAction::Cancel(job.id));
                                        }
                                    });
                                }
                                JobState::Running | JobState::Processing => {
                                    let processing = job.state == JobState::Processing;
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::ProgressBar::new(job.fraction)
                                                .desired_width(ui.available_width() - 100.0)
                                                .fill(ACCENT)
                                                .animate(processing),
                                        );
                                        if ui.button(l.t("Cancelar", "Cancel")).clicked() {
                                            actions.push(JobAction::Cancel(job.id));
                                        }
                                    });
                                    let mut s = format!("{}  ·  ", job.kind);
                                    if processing {
                                        s += l.t("Processando…", "Processing…");
                                    } else {
                                        s += &format!("{:.0}%", job.fraction * 100.0);
                                        if let Some(sp) = job.speed {
                                            s += &format!("  ·  {}", fmt_speed(sp));
                                        }
                                        if let Some(eta) = job.eta {
                                            s += &format!("  ·  {} {}", fmt_duration(eta), l.t("restantes", "left"));
                                        }
                                    }
                                    ui.weak(s);
                                }
                                JobState::Done(path) => {
                                    ui.colored_label(Color32::from_rgb(80, 200, 120), format!("✔ {}  ·  {}", l.t("Concluído", "Done"), job.kind));
                                    ui.horizontal(|ui| {
                                        if ui.small_button(l.t("Mostrar no Finder", "Show in Finder")).clicked() {
                                            open_path(path, true);
                                        }
                                        if !path.is_dir() && ui.small_button(l.t("Abrir", "Open")).clicked() {
                                            open_path(path, false);
                                        }
                                        if ui.small_button(l.t("Remover", "Remove")).clicked() {
                                            actions.push(JobAction::Remove(job.id));
                                        }
                                    });
                                }
                                JobState::Failed(e) => {
                                    ui.colored_label(Color32::LIGHT_RED, format!("× {e}"));
                                    ui.weak(l.t(
                                        "Se o erro persistir, verifique atualizações do yt-dlp em Ajustes.",
                                        "If this keeps happening, check for yt-dlp updates in Settings.",
                                    ));
                                    ui.horizontal(|ui| {
                                        if job.req.is_some() && ui.small_button(l.t("Tentar novamente", "Try again")).clicked() {
                                            actions.push(JobAction::Retry(job.id));
                                        }
                                        if ui.small_button(l.t("Remover", "Remove")).clicked() {
                                            actions.push(JobAction::Remove(job.id));
                                        }
                                    });
                                }
                                JobState::Cancelled => {
                                    ui.weak(l.t("Cancelado", "Cancelled"));
                                    ui.horizontal(|ui| {
                                        if job.req.is_some() && ui.small_button(l.t("Tentar novamente", "Try again")).clicked() {
                                            actions.push(JobAction::Retry(job.id));
                                        }
                                        if ui.small_button(l.t("Remover", "Remove")).clicked() {
                                            actions.push(JobAction::Remove(job.id));
                                        }
                                    });
                                }
                            }
                        });
                    });
                });
                ui.add_space(2.0);
            }
        });
        for a in actions {
            self.apply_job_action(a);
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let l = self.lang();
        ui.label(RichText::new(l.t("Ajustes", "Settings")).size(20.0).strong());
        let s = &mut self.settings;
        card(ui, false, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(l.t("Idioma", "Language")).strong());
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.lang, Lang::Pt, "Português (Brasil)");
                ui.selectable_value(&mut s.lang, Lang::En, "English");
            });
        });
        card(ui, false, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(l.t("Pasta de downloads", "Download folder")).strong());
            ui.add(egui::Label::new(RichText::new(pretty_path(&s.out_dir)).weak()).truncate());
            ui.horizontal(|ui| {
                if ui.button(l.t("Alterar…", "Change…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_directory(&s.out_dir).pick_folder() {
                        s.out_dir = dir;
                    }
                }
                if ui.button(l.t("Abrir no Finder", "Open in Finder")).clicked() {
                    open_path(&s.out_dir, false);
                }
            });
        });
        card(ui, false, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(l.t("Padrões", "Defaults")).strong());
            ui.horizontal(|ui| {
                for f in Format::ALL {
                    ui.selectable_value(&mut s.format, f, format_label(l, f));
                }
            });
            egui::ComboBox::from_id_salt("defq").selected_text(quality_label(l, s.quality)).show_ui(ui, |ui| {
                for q in Quality::ALL {
                    ui.selectable_value(&mut s.quality, q, quality_label(l, q));
                }
            });
        });
        card(ui, false, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(l.t("Downloads", "Downloads")).strong());
            ui.horizontal(|ui| {
                ui.label(l.t("Simultâneos:", "At once:"));
                egui::ComboBox::from_id_salt("parallel").selected_text(s.max_parallel.to_string()).width(60.0).show_ui(ui, |ui| {
                    for n in 1..=6u8 {
                        ui.selectable_value(&mut s.max_parallel, n, n.to_string());
                    }
                });
                ui.weak(l.t("os demais esperam na fila", "the rest wait in the queue"));
            });
            ui.checkbox(&mut s.by_channel, l.t("Organizar em subpastas por canal", "Organize into sub-folders by channel"));
            ui.horizontal(|ui| {
                ui.label(l.t("Cookies do navegador:", "Browser cookies:"));
                let shown = match s.cookies_browser.as_str() {
                    "" => l.t("Nenhum", "None").to_owned(),
                    b => b[..1].to_uppercase() + &b[1..],
                };
                egui::ComboBox::from_id_salt("cookies").selected_text(shown).width(120.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.cookies_browser, String::new(), l.t("Nenhum", "None"));
                    for (id, name) in [("safari", "Safari"), ("chrome", "Chrome"), ("firefox", "Firefox"), ("brave", "Brave"), ("edge", "Edge")] {
                        ui.selectable_value(&mut s.cookies_browser, id.to_owned(), name);
                    }
                });
            });
            ui.weak(l.t(
                "Para vídeos com restrição de idade ou que pedem login. O macOS pode pedir permissão (o Safari exige Acesso Total ao Disco).",
                "For age-restricted or sign-in videos. macOS may ask for permission (Safari needs Full Disk Access).",
            ));
        });
        let mut check_now = false;
        card(ui, false, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(l.t("Ferramentas", "Tools")).strong());
            match &self.tools {
                Ok(t) => {
                    let ver = self.ytdlp_version.clone().unwrap_or_else(|| "…".into());
                    ui.label(format!("yt-dlp {ver}"));
                    ui.weak(t.ytdlp.display().to_string());
                    match &t.ffmpeg {
                        Some(f) => ui.weak(format!("ffmpeg: {}", f.display())),
                        None => ui.colored_label(
                            Color32::GOLD,
                            l.t("ffmpeg não encontrado. Instale com: brew install ffmpeg", "ffmpeg not found. Install with: brew install ffmpeg"),
                        ),
                    };
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let busy = matches!(self.update, UpdateState::Checking | UpdateState::Updating(_));
                        if ui.add_enabled(!busy, egui::Button::new(l.t("Verificar atualização", "Check for updates"))).clicked() {
                            check_now = true;
                        }
                        match &self.update {
                            UpdateState::Checking => {
                                ui.spinner();
                                ui.weak(l.t("Verificando…", "Checking…"));
                            }
                            UpdateState::Updating(v) => {
                                ui.spinner();
                                ui.weak(format!("{} {v}…", l.t("Baixando yt-dlp", "Downloading yt-dlp")));
                            }
                            UpdateState::UpToDate => {
                                ui.colored_label(Color32::from_rgb(80, 200, 120), l.t("✔ Já está na versão mais recente", "✔ Already up to date"));
                            }
                            UpdateState::Updated(v) => {
                                ui.colored_label(Color32::from_rgb(80, 200, 120), format!("✔ {} {v}", l.t("Atualizado para", "Updated to")));
                            }
                            UpdateState::Failed(e) => {
                                ui.colored_label(Color32::LIGHT_RED, format!("× {e}"));
                            }
                            UpdateState::Idle => {}
                        }
                    });
                    ui.checkbox(
                        &mut s.auto_update,
                        l.t("Verificar atualizações automaticamente (1x por dia)", "Check for updates automatically (once a day)"),
                    );
                    ui.weak(l.t(
                        "O YouTube muda com frequência; manter o yt-dlp em dia evita erros de download.",
                        "YouTube changes often; keeping yt-dlp current avoids download errors.",
                    ));
                }
                Err(e) => {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
            }
        });
        if check_now {
            self.pending_update_check = true;
        }
        ui.weak(format!("up2me {}", env!("CARGO_PKG_VERSION")));
    }

    fn debug_tick(&mut self, ctx: &egui::Context) {
        if self.debug.select_first && !self.debug.did_select {
            if let Some(first) = self.results.first().cloned() {
                self.debug.did_select = true;
                self.select(first, ctx);
                if let Some(sel) = self.selected.as_mut() {
                    sel.opts.subs_on = std::env::var("UP2ME_SUBSON").is_ok();
                }
            }
        }
        if let Some(n) = self.debug.dl_all {
            if self.playlist.is_some() {
                self.results.truncate(n);
                self.debug.dl_all = None;
                self.download_all(ctx);
            }
        }
        let Some(path) = self.debug.shot.clone() else { return };
        let elapsed = self.debug.start.elapsed().as_secs_f32();
        let loaded = !matches!(self.search, SearchState::Loading)
            && self.dates_pending == 0
            && !matches!(self.update, UpdateState::Checking | UpdateState::Updating(_))
            && self.jobs.iter().all(|j| !matches!(j.state, JobState::Running | JobState::Processing));
        let info_ready = !self.debug.select_first || self.selected.as_ref().is_some_and(|s| !matches!(s.info, InfoState::Loading));
        let by_time = self.debug.shot_after.is_some_and(|t| elapsed > t);
        let ready = self.debug.shot_after.is_none() && loaded && info_ready && elapsed > 6.0;
        if !self.debug.requested && elapsed > 2.0 && (ready || by_time || elapsed > 120.0) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            self.debug.requested = true;
        }
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Screenshot { image, .. } = e {
                    let (w, h) = (image.width() as u32, image.height() as u32);
                    let raw: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                    match image::RgbaImage::from_raw(w, h, raw) {
                        Some(img) => {
                            if let Err(e) = img.save(&path) {
                                eprintln!("screenshot save failed: {e}");
                            }
                        }
                        None => eprintln!("screenshot has unexpected size {w}x{h}"),
                    }
                    std::process::exit(0);
                }
            }
        });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump(&ctx);
        self.pump_queue(&ctx);
        self.debug_tick(&ctx);
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            self.check_clipboard();
        }
        self.was_focused = focused;
        let l = self.lang();
        let mut actions: Vec<Action> = Vec::new();

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.selected.is_some() {
            actions.push(Action::CloseSelected);
        }

        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));

        if self.tab == Tab::Search && self.selected.is_some() {
            egui::Panel::right("options")
                .exact_size(360.0)
                .resizable(false)
                .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::same(16)))
                .show(ui, |ui| self.options_panel(ui, &mut actions));
        }

        let before = self.filters;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(18, 8))).show(ui, |ui| {
            match self.tab {
                Tab::Search => self.search_tab(ui, &mut actions),
                Tab::Downloads => self.downloads_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            }
        });
        // Server-side filters changed → search again.
        if self.filters.server_part() != before.server_part() && self.playlist.is_none() && !self.searched_query.is_empty() && self.query.trim() == self.searched_query {
            actions.push(Action::Search);
        }
        // Sorting by oldest uses a smaller page (dates cost one request per video).
        if self.filters.sort != before.sort && self.filters.sort == Sort::Oldest && !self.results.is_empty() {
            actions.push(Action::Search);
        }

        if std::mem::take(&mut self.pending_update_check) {
            self.start_update(UpdateMode::Check, &ctx);
        }

        for a in actions {
            match a {
                Action::Search => self.run_query(&ctx),
                Action::LoadMore => {
                    self.limit += RESULTS_PAGE;
                    self.start_search(&ctx);
                }
                Action::Select(item) => self.select(item, &ctx),
                Action::Quick(item) => {
                    let opts = Options::from_settings(&self.settings);
                    let folder = self.playlist.clone();
                    self.enqueue(&item, &opts, folder, &ctx);
                }
                Action::DownloadSelected => {
                    if let Some(sel) = self.selected.take() {
                        let folder = self.playlist.clone();
                        self.enqueue(&sel.item, &sel.opts, folder, &ctx);
                    }
                }
                Action::DownloadAll => self.download_all(&ctx),
                Action::UseClip => {
                    if let Some(url) = self.clip.take() {
                        self.clip_seen = url.clone();
                        self.query = url;
                        self.run_query(&ctx);
                    }
                }
                Action::DismissClip => {
                    if let Some(url) = self.clip.take() {
                        self.clip_seen = url;
                    }
                }
                Action::CloseSelected => self.selected = None,
                Action::Paste => {
                    if let Ok(out) = std::process::Command::new("pbpaste").output() {
                        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                        if !text.is_empty() {
                            self.query = text;
                            self.run_query(&ctx);
                        }
                    }
                }
            }
        }

        // Toast
        if let Some((text, at)) = &self.toast {
            if at.elapsed() < Duration::from_millis(2600) {
                egui::Area::new(egui::Id::new("toast")).anchor(Align2::CENTER_BOTTOM, [0.0, -20.0]).show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).corner_radius(18.0).show(ui, |ui| {
                        ui.label(RichText::new(format!("✔  {text}")).strong());
                    });
                });
                ctx.request_repaint_after(Duration::from_millis(300));
            } else {
                self.toast = None;
            }
        }
        let _ = l;

        if self.history_dirty {
            save_history(&self.jobs);
            self.history_dirty = false;
        }
        if self.settings != self.saved {
            engine::set_cookies_browser(Some(self.settings.cookies_browser.clone()));
            self.settings.save();
            self.saved = self.settings.clone();
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("up2me")
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-1024.png")).expect("valid icon"))
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("up2me", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
