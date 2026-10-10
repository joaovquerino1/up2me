//! Thin wrapper around `yt-dlp` (extraction/download) and `ffmpeg` (merge/convert).

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Tools {
    pub ytdlp: PathBuf,
    pub ffmpeg: Option<PathBuf>,
}

impl Tools {
    /// Prefers the copies embedded in the app bundle; falls back to Homebrew/PATH when run from `cargo run`.
    pub fn locate() -> Result<Self> {
        // 1) Tools embedded in the app bundle (Contents/Resources/bin). A newer yt-dlp
        //    downloaded by the in-app updater (Application Support) wins over the bundled one.
        if let Some(bin) = std::env::current_exe().ok().and_then(|e| e.parent().map(|m| m.join("../Resources/bin"))) {
            let bundled = bin.join("yt-dlp/yt-dlp_macos");
            if bundled.is_file() {
                let ffmpeg = Some(bin.join("ffmpeg")).filter(|p| p.is_file());
                return Ok(Self { ytdlp: user_ytdlp().unwrap_or(bundled), ffmpeg });
            }
        }
        // 2) System installs (dev builds): Homebrew on Apple Silicon / Intel, then PATH.
        let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from).to_vec();
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        let find = |name: &str| dirs.iter().map(|d| d.join(name)).find(|p| p.is_file());
        let ytdlp = user_ytdlp()
            .or_else(|| find("yt-dlp"))
            .ok_or_else(|| anyhow!("yt-dlp not found. Run scripts/fetch-tools.sh or: brew install yt-dlp"))?;
        Ok(Self { ytdlp, ffmpeg: find("ffmpeg") })
    }
}

// ---------------------------------------------------------------- yt-dlp self-update

/// Where updated yt-dlp copies live (the signed app bundle itself is never modified).
pub fn user_bin_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Library/Application Support/up2me/bin")
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

/// yt-dlp versions look like 2026.08.19 (optionally .1 for re-releases).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    version_key(candidate) > version_key(current)
}

fn installed_versions() -> Vec<(Vec<u64>, PathBuf)> {
    let mut v: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(user_bin_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let ver = name.strip_prefix("yt-dlp-")?.to_owned();
            e.path().join("yt-dlp_macos").is_file().then(|| (version_key(&ver), e.path()))
        })
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

fn user_ytdlp() -> Option<PathBuf> {
    installed_versions().pop().map(|(_, dir)| dir.join("yt-dlp_macos"))
}

pub fn current_version(tools: &Tools) -> Option<String> {
    let out = Command::new(&tools.ytdlp).arg("--version").stdin(Stdio::null()).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!v.is_empty()).then_some(v)
}

fn curl(args: &[&str]) -> Result<String> {
    let out = Command::new("/usr/bin/curl")
        .args(["-fsSL", "--retry", "2", "--max-time", "600"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("failed to run curl")?;
    if !out.status.success() {
        bail!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Latest yt-dlp release tag, read from the redirect of /releases/latest (no API rate limits).
pub fn latest_version() -> Result<String> {
    let url = curl(&["-o", "/dev/null", "-w", "%{url_effective}", "https://github.com/yt-dlp/yt-dlp/releases/latest"])?;
    let tag = url.rsplit('/').next().unwrap_or("").trim().to_owned();
    if tag.is_empty() || !tag.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        bail!("unexpected release tag: {tag}");
    }
    Ok(tag)
}

/// Downloads yt-dlp `tag`, verifies its SHA-256, test-runs it and installs it into a versioned
/// folder. Running downloads keep using their old copy; new ones pick up the new path.
pub fn install_ytdlp(tag: &str) -> Result<PathBuf> {
    if tag.is_empty() || !tag.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        bail!("invalid version {tag}");
    }
    let bin = user_bin_dir();
    let part = bin.join(format!(".yt-dlp-{tag}.part"));
    let _ = std::fs::remove_dir_all(&part);
    std::fs::create_dir_all(&part).context("cannot create update folder")?;
    let result = (|| -> Result<PathBuf> {
        let base = format!("https://github.com/yt-dlp/yt-dlp/releases/download/{tag}");
        let zip = part.join("yt-dlp_macos.zip");
        curl(&["-o", &zip.to_string_lossy(), &format!("{base}/yt-dlp_macos.zip")])?;
        let sums = curl(&[&format!("{base}/SHA2-256SUMS")])?;
        let want = sums
            .lines()
            .find(|l| l.trim_end().ends_with(" yt-dlp_macos.zip"))
            .and_then(|l| l.split_whitespace().next())
            .ok_or_else(|| anyhow!("checksum not published"))?
            .to_owned();
        let got = Command::new("/usr/bin/shasum").args(["-a", "256"]).arg(&zip).output().context("shasum failed")?;
        let got = String::from_utf8_lossy(&got.stdout).split_whitespace().next().unwrap_or("").to_owned();
        if want != got {
            bail!("checksum mismatch, update aborted");
        }
        let unpacked = part.join("x");
        let st = Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(&unpacked).status().context("ditto failed")?;
        if !st.success() {
            bail!("could not unpack the update");
        }
        let exe = unpacked.join("yt-dlp_macos");
        let test = Tools { ytdlp: exe.clone(), ffmpeg: None };
        if current_version(&test).as_deref() != Some(tag) {
            bail!("the downloaded yt-dlp did not start correctly");
        }
        let dest = bin.join(format!("yt-dlp-{tag}"));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&unpacked, &dest).context("cannot install the update")?;
        Ok(dest.join("yt-dlp_macos"))
    })();
    let _ = std::fs::remove_dir_all(&part);
    let exe = result?;
    // Keep the two newest copies (an in-flight download may still use the previous one).
    let all = installed_versions();
    for (_, dir) in all.iter().take(all.len().saturating_sub(2)) {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(exe)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Format {
    Video,
    Mp3,
    M4a,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Video, Format::Mp3, Format::M4a];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Quality {
    Best,
    P2160,
    P1440,
    P1080,
    P720,
    P480,
}

impl Quality {
    pub const ALL: [Quality; 6] =
        [Quality::Best, Quality::P2160, Quality::P1440, Quality::P1080, Quality::P720, Quality::P480];
    fn height(self) -> Option<u32> {
        match self {
            Quality::Best => None,
            Quality::P2160 => Some(2160),
            Quality::P1440 => Some(1440),
            Quality::P1080 => Some(1080),
            Quality::P720 => Some(720),
            Quality::P480 => Some(480),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Info {
    pub title: String,
    #[serde(default)]
    pub uploader: Option<String>,
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default)]
    pub thumbnail: Option<String>,
    #[serde(default)]
    pub view_count: Option<u64>,
    #[serde(default)]
    pub upload_date: Option<String>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    subtitles: BTreeMap<String, Vec<RawSub>>,
    #[serde(default)]
    automatic_captions: BTreeMap<String, Vec<RawSub>>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawFormat {
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    format_note: Option<String>,
    #[serde(default)]
    vcodec: Option<String>,
    #[serde(default)]
    acodec: Option<String>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawSub {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct AudioTrack {
    pub lang: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct SubLang {
    pub code: String,
    pub name: String,
    pub auto: bool,
}

impl Info {
    /// Distinct audio languages; empty or single-entry when the video has just one track.
    pub fn audio_tracks(&self) -> Vec<AudioTrack> {
        let strip = |n: &str| {
            for suf in [", ultralow", ", low", ", medium", ", high"] {
                if let Some(x) = n.strip_suffix(suf) {
                    return x.to_owned();
                }
            }
            n.to_owned()
        };
        let mut out: Vec<AudioTrack> = Vec::new();
        for f in &self.formats {
            let audio_only = f.vcodec.as_deref() == Some("none") && f.acodec.as_deref().is_some_and(|a| a != "none");
            let Some(lang) = f.language.clone().filter(|_| audio_only) else { continue };
            let note = f.format_note.clone().unwrap_or_else(|| lang.clone());
            // Prefer the descriptive "Português - dubbed" / "original" name over "Portuguese, low".
            let descriptive = note.contains("dubbed") || note.contains("original") || note.contains("default");
            match out.iter_mut().find(|t| t.lang == lang) {
                Some(t) => {
                    if descriptive && !(t.label.contains("dubbed") || t.label.contains("original") || t.label.contains("default")) {
                        t.label = note;
                    }
                }
                None => out.push(AudioTrack { lang, label: strip(&note) }),
            }
        }
        out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        out
    }

    /// Manual subtitles first, then automatic captions that have no manual equivalent.
    pub fn sub_langs(&self) -> Vec<SubLang> {
        let name_of = |code: &str, v: &Vec<RawSub>| v.iter().find_map(|s| s.name.clone()).unwrap_or_else(|| code.to_owned());
        let mut out: Vec<SubLang> = self
            .subtitles
            .iter()
            .filter(|(c, _)| c.as_str() != "live_chat")
            .map(|(c, v)| SubLang { code: c.clone(), name: name_of(c, v), auto: false })
            .collect();
        for (c, v) in &self.automatic_captions {
            if !self.subtitles.contains_key(c) {
                out.push(SubLang { code: c.clone(), name: name_of(c, v), auto: true });
            }
        }
        out
    }

    pub fn max_height(&self) -> u32 {
        self.formats.iter().filter_map(|f| f.height).max().unwrap_or(0)
    }
}

pub fn fetch_info(tools: &Tools, url: &str) -> Result<Info> {
    let out = Command::new(&tools.ytdlp)
        .args(["-J", "--no-playlist", "--no-warnings", "--", url])
        .stdin(Stdio::null())
        .output()
        .context("failed to run yt-dlp")?;
    if !out.status.success() {
        bail!("{}", last_error_line(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(serde_json::from_slice(&out.stdout).context("unexpected yt-dlp output")?)
}

/// Upload date (YYYYMMDD) for one video; used to sort search results "oldest first".
pub fn fetch_upload_date(tools: &Tools, id: &str) -> Option<String> {
    let out = Command::new(&tools.ytdlp)
        .args(["--skip-download", "--no-playlist", "--no-warnings", "--print", "upload_date", "--"])
        .arg(format!("https://www.youtube.com/watch?v={id}"))
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let d = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit())).then_some(d)
}

// ---------------------------------------------------------------- search

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    #[default]
    Relevance,
    Newest,
    Oldest,
    MostViewed,
    LeastViewed,
    Longest,
    Shortest,
    Rating,
}

impl Sort {
    pub const ALL: [Sort; 8] = [
        Sort::Relevance,
        Sort::Newest,
        Sort::Oldest,
        Sort::MostViewed,
        Sort::LeastViewed,
        Sort::Longest,
        Sort::Shortest,
        Sort::Rating,
    ];
    fn server_code(self) -> u8 {
        match self {
            Sort::Rating => 1,
            Sort::Newest => 2,
            Sort::MostViewed => 3,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DateFilter {
    #[default]
    Any,
    Hour,
    Today,
    Week,
    Month,
    Year,
}

impl DateFilter {
    pub const ALL: [DateFilter; 6] =
        [DateFilter::Any, DateFilter::Hour, DateFilter::Today, DateFilter::Week, DateFilter::Month, DateFilter::Year];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DurationFilter {
    #[default]
    Any,
    Short,
    Medium,
    Long,
}

impl DurationFilter {
    pub const ALL: [DurationFilter; 4] =
        [DurationFilter::Any, DurationFilter::Short, DurationFilter::Medium, DurationFilter::Long];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Filters {
    pub sort: Sort,
    pub date: DateFilter,
    pub duration: DurationFilter,
    pub hd: bool,
    pub uhd: bool,
    pub captions: bool,
    pub creative_commons: bool,
    /// Client-side: hide videos under a minute (Shorts).
    pub hide_shorts: bool,
    /// Client-side minimum view count (0 = off).
    pub min_views: u64,
}

impl Filters {
    /// Parts that change what YouTube returns (a change means a new search).
    pub fn server_part(&self) -> (u8, DateFilter, DurationFilter, bool, bool, bool, bool) {
        let sort = if self.sort == Sort::Oldest { 0 } else { self.sort.server_code() };
        (sort, self.date, self.duration, self.hd, self.uhd, self.captions, self.creative_commons)
    }

    /// YouTube's `sp` search parameter: a tiny protobuf, base64-encoded.
    fn sp(&self) -> String {
        let mut inner: Vec<u8> = Vec::new();
        let date = match self.date {
            DateFilter::Any => 0,
            DateFilter::Hour => 1,
            DateFilter::Today => 2,
            DateFilter::Week => 3,
            DateFilter::Month => 4,
            DateFilter::Year => 5,
        };
        if date > 0 {
            inner.extend([0x08, date]);
        }
        inner.extend([0x10, 1]); // type: video
        match self.duration {
            DurationFilter::Any => {}
            DurationFilter::Short => inner.extend([0x18, 1]),
            DurationFilter::Long => inner.extend([0x18, 2]),
            DurationFilter::Medium => inner.extend([0x18, 3]),
        }
        if self.hd {
            inner.extend([0x20, 1]);
        }
        if self.captions {
            inner.extend([0x28, 1]);
        }
        if self.creative_commons {
            inner.extend([0x30, 1]);
        }
        if self.uhd {
            inner.extend([0x70, 1]);
        }
        let mut msg: Vec<u8> = Vec::new();
        let sort = if self.sort == Sort::Oldest { 0 } else { self.sort.server_code() };
        if sort > 0 {
            msg.extend([0x08, sort]);
        }
        msg.extend([0x12, inner.len() as u8]);
        msg.extend(inner);
        base64(&msg).replace('=', "%3D")
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct Item {
    pub url: String,
    pub id: String,
    pub title: String,
    pub channel: Option<String>,
    pub duration: Option<f64>,
    pub views: Option<u64>,
    /// YYYYMMDD, only known after enrichment or a full info fetch.
    pub upload_date: Option<String>,
    pub live: bool,
}

impl Item {
    pub fn thumb(&self) -> Option<String> {
        (!self.id.is_empty()).then(|| format!("https://i.ytimg.com/vi/{}/mqdefault.jpg", self.id))
    }
}

pub fn search(tools: &Tools, query: &str, filters: &Filters, limit: usize) -> Result<Vec<Item>> {
    let url = format!("https://www.youtube.com/results?search_query={}&sp={}", percent_encode(query), filters.sp());
    let out = Command::new(&tools.ytdlp)
        .args(["--flat-playlist", "-J", "--no-warnings", "--playlist-end"])
        .arg(limit.to_string())
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .context("failed to run yt-dlp")?;
    if !out.status.success() {
        bail!("{}", last_error_line(&String::from_utf8_lossy(&out.stderr)));
    }
    #[derive(Deserialize)]
    struct Entry {
        id: Option<String>,
        title: Option<String>,
        channel: Option<String>,
        uploader: Option<String>,
        duration: Option<f64>,
        view_count: Option<u64>,
        live_status: Option<String>,
    }
    #[derive(Deserialize)]
    struct Playlist {
        #[serde(default)]
        entries: Vec<Option<Entry>>,
    }
    let pl: Playlist = serde_json::from_slice(&out.stdout).context("unexpected yt-dlp output")?;
    Ok(pl
        .entries
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let id = e.id?;
            if id.len() != 11 {
                return None; // channels / playlists
            }
            Some(Item {
                url: format!("https://www.youtube.com/watch?v={id}"),
                title: e.title.unwrap_or_else(|| id.clone()),
                id,
                channel: e.channel.or(e.uploader),
                duration: e.duration,
                views: e.view_count,
                upload_date: None,
                live: matches!(e.live_status.as_deref(), Some("is_live" | "is_upcoming")),
            })
        })
        .collect())
}

fn last_error_line(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .map(|l| l.trim().trim_start_matches("ERROR: ").to_owned())
        .unwrap_or_else(|| "yt-dlp failed".into())
}

#[derive(Debug)]
pub enum Event {
    Title(String),
    Progress { fraction: f32, speed: Option<f64>, eta: Option<f64> },
    Processing,
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

/// Handle used to cancel a running download (kills yt-dlp and its ffmpeg children).
#[derive(Clone, Default)]
pub struct CancelHandle(Arc<Mutex<CancelState>>);

#[derive(Default)]
struct CancelState {
    pid: Option<i32>,
    cancelled: bool,
}

impl CancelHandle {
    pub fn cancel(&self) {
        let mut s = self.0.lock().unwrap();
        s.cancelled = true;
        if let Some(pid) = s.pid {
            // yt-dlp runs in its own process group, so this also takes down ffmpeg.
            unsafe { libc::killpg(pid, libc::SIGTERM) };
        }
    }
    fn is_cancelled(&self) -> bool {
        self.0.lock().unwrap().cancelled
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SubMode {
    /// Save .srt files next to the video.
    #[default]
    File,
    /// Embed into the MP4 (video only).
    Embed,
    /// Subtitles only, no media.
    Only,
}

pub struct Request {
    pub url: String,
    pub format: Format,
    pub quality: Quality,
    pub out_dir: PathBuf,
    /// Preferred audio-track language (e.g. "pt-BR"); `None` = YouTube's default/original.
    pub audio_lang: Option<String>,
    pub sub_langs: Vec<String>,
    pub sub_mode: SubMode,
}

fn build_args(tools: &Tools, req: &Request) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--no-playlist".into(),
        "--newline".into(),
        "--progress".into(),
        "--no-warnings".into(),
        "--embed-metadata".into(),
        "--concurrent-fragments".into(),
        "4".into(),
        "--progress-template".into(),
        "download:PROG|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s".into(),
        "--print".into(),
        "before_dl:TITLE|%(title)s".into(),
        "--print".into(),
        "after_move:FILE|%(filepath)s".into(),
        "-o".into(),
        req.out_dir.join("%(title).180B [%(id)s].%(ext)s").to_string_lossy().into_owned(),
    ];
    if let Some(ff) = &tools.ffmpeg {
        a.push("--ffmpeg-location".into());
        a.push(ff.to_string_lossy().into_owned());
    }
    let ba = match &req.audio_lang {
        Some(l) => format!("ba[language={l}]/ba"),
        None => "ba".to_owned(),
    };
    let subs_only = req.sub_mode == SubMode::Only && !req.sub_langs.is_empty();
    if subs_only {
        a.push("--skip-download".into());
    } else {
        match req.format {
            Format::Video => {
                // Prefer H.264/AAC (plays everywhere on macOS, hardware-decoded), cap by height.
                let res = req.quality.height().map_or("res".to_owned(), |h| format!("res:{h}"));
                a.extend([
                    "-f".into(),
                    format!("bv*+{ba}/b"),
                    "-S".into(),
                    format!("{res},vcodec:h264,acodec:m4a"),
                    "--merge-output-format".into(),
                    "mp4".into(),
                ]);
            }
            Format::Mp3 => {
                a.extend(["-f".into(), format!("{ba}/b"), "-x".into(), "--audio-format".into(), "mp3".into(), "--audio-quality".into(), "0".into()]);
                a.extend(["--embed-thumbnail", "--convert-thumbnails", "jpg"].map(String::from));
            }
            Format::M4a => {
                a.extend(["-f".into(), format!("{ba}[ext=m4a]/{ba}/b"), "-x".into(), "--audio-format".into(), "m4a".into()]);
                a.extend(["--embed-thumbnail", "--convert-thumbnails", "jpg"].map(String::from));
            }
        }
    }
    if !req.sub_langs.is_empty() {
        a.extend(["--write-subs", "--write-auto-subs", "--sub-format", "srt/vtt/best", "--convert-subs", "srt"].map(String::from));
        a.push("--sub-langs".into());
        a.push(req.sub_langs.join(","));
        if req.sub_mode == SubMode::Embed && req.format == Format::Video {
            a.push("--embed-subs".into());
        }
    }
    a.push("--".into());
    a.push(req.url.clone());
    a
}

/// Runs a download to completion, reporting through `emit`. Blocking: call from a worker thread.
pub fn download(tools: &Tools, req: &Request, cancel: &CancelHandle, emit: impl Fn(Event)) {
    let result = run(tools, req, cancel, &emit);
    match result {
        Ok(Some(path)) => emit(Event::Done(path)),
        Ok(None) => emit(Event::Cancelled),
        Err(e) => emit(Event::Failed(e.to_string())),
    }
}

fn run(tools: &Tools, req: &Request, cancel: &CancelHandle, emit: &impl Fn(Event)) -> Result<Option<PathBuf>> {
    std::fs::create_dir_all(&req.out_dir).context("cannot create output folder")?;
    let mut child: Child = Command::new(&tools.ytdlp)
        .args(build_args(tools, req))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("failed to start yt-dlp")?;

    {
        let mut s = cancel.0.lock().unwrap();
        s.pid = Some(child.id() as i32);
        if s.cancelled {
            unsafe { libc::killpg(child.id() as i32, libc::SIGTERM) };
        }
    }

    let mut stderr = child.stderr.take().unwrap();
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stderr.read_to_string(&mut buf);
        buf
    });

    let mut file: Option<PathBuf> = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(t) = line.strip_prefix("TITLE|") {
            emit(Event::Title(t.to_owned()));
        } else if let Some(p) = line.strip_prefix("FILE|") {
            file = Some(PathBuf::from(p));
        } else if let Some(p) = line.strip_prefix("PROG|") {
            let f: Vec<&str> = p.split('|').collect();
            let num = |i: usize| f.get(i).and_then(|s| s.trim().parse::<f64>().ok());
            let total = num(1).or(num(2));
            if let (Some(done), Some(total)) = (num(0), total) {
                if total > 0.0 {
                    emit(Event::Progress {
                        fraction: (done / total).clamp(0.0, 1.0) as f32,
                        speed: num(3),
                        eta: num(4),
                    });
                }
            }
        } else if line.starts_with("[Merger]") || line.starts_with("[ExtractAudio]") || line.starts_with("[VideoRemuxer]") {
            emit(Event::Processing);
        }
    }

    let status = child.wait()?;
    let stderr = stderr_thread.join().unwrap_or_default();
    if cancel.is_cancelled() {
        return Ok(None);
    }
    if !status.success() {
        bail!("{}", last_error_line(&stderr));
    }
    if req.sub_mode == SubMode::Only && !req.sub_langs.is_empty() {
        return Ok(Some(req.out_dir.clone()));
    }
    if req.sub_mode == SubMode::Embed && req.format == Format::Video && !req.sub_langs.is_empty() {
        // Subtitles are inside the MP4 now; drop the loose .srt copies.
        if let Some(f) = &file {
            for lang in &req.sub_langs {
                let _ = std::fs::remove_file(f.with_extension(format!("{lang}.srt")));
            }
        }
    }
    file.filter(|p| Path::new(p).exists()).map(Some).ok_or_else(|| anyhow!("download finished but the file was not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("2026.09.01", "2026.08.19"));
        assert!(is_newer("2026.08.19.1", "2026.08.19"));
        assert!(is_newer("2027.01.01", "2026.12.31"));
        assert!(!is_newer("2026.08.19", "2026.08.19"));
        assert!(!is_newer("2026.08.18", "2026.08.19"));
    }

    #[test]
    fn rejects_bad_tags() {
        assert!(install_ytdlp("../evil").is_err());
        assert!(install_ytdlp("").is_err());
    }
}
