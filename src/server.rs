//! The local web server on 127.0.0.1: the page (embedded in the app) and its API, shown in the
//! desktop window or a browser.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::archive;
use crate::config::{self, EMBED_DIM};
use crate::embedder::Embedder;
use crate::extract::{has_preview, max_bytes};
use crate::filemap::FileMap;
use crate::filetypes::{self, Types};
use crate::governor::Governor;
use crate::i18n;
use crate::indexer::{Indexer, is_inside};
use crate::search::{fts_query, fuse, kinds, query_terms, typing_prefix};
use crate::store::{FileRecord, Settings, Store};
use crate::updater::{REPO_URL, Updater};

const THEMES: [&str; 3] = ["auto", "light", "dark"];
// Two files this close as a whole (their passages' means) are nearly the same file: a copy, another
// format, a version. Copies under other names and folders measured 0.98-0.99 (the name is part of
// what the model reads), different bills 0.80. Not calibrated yet: the duplicates work will set it on
// the lab's sets.
const NEAR_IDENTICAL: f32 = 0.96;
const VIEWS: [&str; 3] = ["cards", "list", "grid"]; // results with passages, one line each, thumbnails
// Opening these would run them (or, for .reg, change Windows): they open in an editor instead.
const RUNNABLE_EXTS: [&str; 29] = [
    ".bat", ".cmd", ".ps1", ".psm1", ".psd1", ".js", ".mjs", ".cjs", ".jse", ".vbs", ".vbe", ".wsf", ".wsh", ".py",
    ".pyw", ".sh", ".bash", ".zsh", ".fish", ".lua", ".rb", ".php", ".pl", ".r", ".hta", ".reg", ".ahk", ".au3", ".tcl",
];

// Only the app's own files may run or load in the page.
const SECURITY_HEADERS: [(&str, &str); 6] = [
    (
        "content-security-policy",
        "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; \
         base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-frame-options", "DENY"),
    ("cross-origin-resource-policy", "same-origin"),
    ("cross-origin-opener-policy", "same-origin"),
];

/// The page's files, inside the app.
const STATIC: [(&str, &[u8], &str); 8] = [
    ("app.js", include_bytes!("../static/app.js"), "text/javascript; charset=utf-8"),
    ("i18n.json", include_bytes!("../static/i18n.json"), "application/json"),
    ("icon.png", include_bytes!("../static/icon.png"), "image/png"),
    ("index.html", include_bytes!("../static/index.html"), "text/html; charset=utf-8"),
    ("quiekel-dark.svg", include_bytes!("../static/quiekel-dark.svg"), "image/svg+xml"),
    ("quiekel.svg", include_bytes!("../static/quiekel.svg"), "image/svg+xml"),
    ("style.css", include_bytes!("../static/style.css"), "text/css; charset=utf-8"),
    ("theme.js", include_bytes!("../static/theme.js"), "text/javascript; charset=utf-8"),
];

pub fn static_file(name: &str) -> Option<&'static [u8]> {
    STATIC.iter().find(|(n, _, _)| *n == name).map(|(_, b, _)| *b)
}

/// What differs between browser mode and the desktop app.
pub struct Hooks {
    pub app_mode: bool,
    pub pick_folder: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    pub show_window: Option<Box<dyn Fn() + Send + Sync>>,
    pub set_dark: Option<Box<dyn Fn(bool) + Send + Sync>>,
}

pub struct Backend {
    pub store: Arc<Store>,
    pub settings: Arc<Settings>,
    pub governor: Arc<Governor>,
    pub embedder: Arc<Embedder>,
    pub indexer: Arc<Indexer>,
    pub updater: Arc<Updater>,
    pub maps: Arc<FileMap>,
    pub diagnostics: Arc<crate::diagnostics::Diagnostics>,
}

/// Start everything but the server: the store, the settings, the governor, the model's side and
/// the indexer (not started yet).
pub fn build_backend(model: &str) -> anyhow::Result<Arc<Backend>> {
    lower_priority();
    archive::clear_opened(); // copies of files opened from archives last time
    let store = Arc::new(Store::open()?);
    let settings = Arc::new(Settings::new(store.clone()));
    let governor = Governor::new();
    let embedder = Arc::new(Embedder::new(None, model));
    {
        let e = embedder.clone();
        *governor.helpers.write() = Box::new(move || e.pid().into_iter().collect()); // (the model's work is the app's own)
    }
    let indexer = Indexer::new(store.clone(), embedder.clone(), governor.clone(), settings.clone());
    let s = settings.clone();
    let updater = Updater::new(Box::new(move || s.get("auto_update").as_bool().unwrap_or(false)), true);
    let maps = FileMap::new(store.clone());
    let diagnostics = crate::diagnostics::Diagnostics::start(
        embedder.clone(),
        governor.clone(),
        indexer.clone(),
        settings.get("diagnostics").as_bool().unwrap_or(false),
    );
    Ok(Arc::new(Backend { store, settings, governor, embedder, indexer, updater, maps, diagnostics }))
}

/// Indexing, and the search data read into memory: only once the server is up.
pub fn start_background(b: &Backend) {
    b.store.keep_vectors_in_memory(true);
    b.indexer.start();
    let store = b.store.clone();
    std::thread::Builder::new()
        .name("tidy-previews".into())
        .spawn(move || {
            crate::governor::set_background_priority(true);
            tidy_previews(&store, &config::thumbs_dir());
        })
        .ok();
}

/// The previews kept on disk ("{file id}-{its mtime}-{size}.jpg") of files that are gone or have
/// changed since: deleted. So are half-written ones left behind.
fn tidy_previews(store: &Store, dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut kept: Vec<(i64, i64, std::path::PathBuf)> = Vec::new();
    let mut gone = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let parsed = name.strip_suffix(".jpg").and_then(|stem| {
            let (id, rest) = stem.split_once('-')?;
            let (mtime, _size) = rest.rsplit_once('-')?;
            Some((id.parse::<i64>().ok()?, mtime.parse::<i64>().ok()?))
        });
        match parsed {
            Some((id, mtime)) => kept.push((id, mtime, e.path())),
            None => {
                // (one being written right now is left alone)
                let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() > 600));
                if old && std::fs::remove_file(e.path()).is_ok() {
                    gone += 1;
                }
            }
        }
    }
    let files = store.files_by_ids(&kept.iter().map(|k| k.0).collect::<Vec<_>>());
    for (id, mtime, path) in kept {
        if files.get(&id).is_none_or(|f| f.mtime as i64 != mtime) && std::fs::remove_file(path).is_ok() {
            gone += 1;
        }
    }
    if gone > 0 {
        log::info!("Removed {gone} previews of files that are gone or have changed");
    }
}

/// Below normal for the whole app: whatever else the PC is busy with comes first.
pub fn lower_priority() {
    use windows::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, SetPriorityClass};
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }
}

static LAUNCHING: Mutex<()> = Mutex::new(());

/// Around starting another program: Windows hands the app's lower priority down to it.
fn with_normal_priority<T>(f: impl FnOnce() -> T) -> T {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetPriorityClass, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS, SetPriorityClass,
    };
    let _one = LAUNCHING.lock();
    unsafe {
        let before = GetPriorityClass(GetCurrentProcess());
        let _ = SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS);
        let out = f();
        if before != 0 {
            let _ = SetPriorityClass(GetCurrentProcess(), PROCESS_CREATION_FLAGS(before));
        }
        out
    }
}

/// Tests: what would have been opened (nothing is).
#[cfg(test)]
pub static LAUNCHED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn shell_open(target: &str, verb: &str) -> bool {
    LAUNCHED.lock().push(format!("{verb} {target}"));
    true
}

#[cfg(not(test))]
fn shell_open(target: &str, verb: &str) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::HSTRING;
    unsafe {
        let r = ShellExecuteW(None, &HSTRING::from(verb), &HSTRING::from(target), None, None, SW_SHOWNORMAL);
        r.0 as usize > 32
    }
}

/// Open a web address in the default browser.
pub fn open_browser(url: &str) {
    shell_open(url, "open");
}

#[cfg(test)]
fn reveal(path: &str) {
    LAUNCHED.lock().push(format!("explorer /select,\"{path}\""));
}

#[cfg(not(test))]
fn reveal(path: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{path}\"")).spawn();
}

// ---- errors and replies -------------------------------------------------------------------------

pub struct ApiError {
    status: StatusCode,
    body: Value,
}

/// An error the page can translate: an i18n key, its parameters, and English for logs.
fn fail(status: u16, code: &str, params: Value) -> ApiError {
    let key = format!("err.{code}");
    let p = match &params {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let message = i18n::t_obj("en", &key, &p);
    ApiError {
        status: StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        body: json!({"detail": {"key": key, "params": params, "message": message}}),
    }
}

fn detail(status: StatusCode, text: &str) -> ApiError {
    ApiError { status, body: json!({"detail": text}) }
}

fn json_reply(v: &Value) -> Response {
    let mut r = Response::new(Body::from(serde_json::to_vec(v).unwrap_or_default()));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

fn ok() -> Result<Response, ApiError> {
    Ok(json_reply(&json!({"ok": true})))
}

fn etag_of(bytes: &[u8]) -> String {
    format!("\"{}\"", &sha1_smol::Sha1::from(bytes).digest().to_string()[..16])
}

type Query = HashMap<String, String>;

/// A request that doesn't fit (a body, number or field the API can't read): 422 with the reason.
/// The model couldn't do it (busy, out of memory): try again in a moment.
fn model_failed(e: crate::embedder::EmbedError) -> ApiError {
    log::info!("Comparing a file failed: {e}");
    fail(503, "model_busy", json!({}))
}

fn unreadable(what: impl std::fmt::Display) -> ApiError {
    detail(StatusCode::UNPROCESSABLE_ENTITY, &what.to_string())
}

fn query_int(q: &Query, key: &str) -> Result<Option<i64>, ApiError> {
    q.get(key).map(|v| v.trim().parse::<i64>().map_err(|_| unreadable(format!("{key}: not a whole number")))).transpose()
}

/// `page`, for the picture of a document's page (from 1).
fn page_of(q: &Query) -> Result<Option<u32>, ApiError> {
    Ok(query_int(q, "page")?.filter(|&n| n >= 1).map(|n| n.min(100_000) as u32))
}

fn path_int(name: &str, v: &str) -> Result<i64, ApiError> {
    v.trim().parse::<i64>().map_err(|_| unreadable(format!("{name}: not a whole number")))
}

/// A JSON body, as the type the route takes.
fn body_json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(unreadable)
}

// ---- the server -----------------------------------------------------------------------------------

pub struct Server {
    pub b: Arc<Backend>,
    pub hooks: Hooks,
    pick_lock: Mutex<()>,
    allowed_hosts: [String; 2],
}

#[derive(Deserialize)]
struct FolderIn {
    path: String,
}

#[derive(Deserialize)]
struct WatchIn {
    watch: bool,
}

#[derive(Deserialize)]
struct ExampleIn {
    reason: String,
    index: i64,
}

#[derive(Deserialize)]
struct LeaveOutIn {
    path: String,
    out: bool,
}

#[derive(Deserialize)]
struct ThemeIn {
    dark: bool,
}

#[derive(Deserialize, Default)]
struct SettingsIn {
    language: Option<String>,
    theme: Option<String>,
    auto_update: Option<bool>,
    seen_version: Option<String>,
    autostart: Option<bool>,
    view_files: Option<String>,
    view_images: Option<String>,
    kinds_off: Option<Vec<String>>,
    formats_off: Option<Vec<String>>,
    problems_seen: Option<i64>,
    hidden_files: Option<bool>,
    program_folders: Option<bool>,
    diagnostics: Option<bool>,
}

impl Server {
    pub fn new(b: Arc<Backend>, hooks: Hooks) -> Arc<Server> {
        let port = *config::PORT;
        Arc::new(Server {
            b,
            hooks,
            pick_lock: Mutex::new(()),
            allowed_hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
        })
    }

    fn lang(&self) -> String {
        i18n::resolve(&self.b.settings.str("language"))
    }

    /// What the app holds in memory, for the Performance setting's explanation.
    fn memory_view(&self) -> Value {
        let app = crate::model_process::process_memory_mb(std::process::id());
        json!({
            "app_mb": app.round() as i64,
            "model_mb": self.b.embedder.memory_mb().round() as i64,
            "vectors_mb": match self.b.store.vectors_mb() {
                mb if mb > 0.0 => json!((mb * 10.0).round() / 10.0),
                _ => json!(0), // (on disk)
            },
        })
    }

    fn settings_view(&self) -> Value {
        let mut v = self.b.settings.all();
        v.insert(
            "autostart".into(),
            if self.hooks.app_mode { json!(crate::shortcuts::autostart_enabled()) } else { Value::Null },
        );
        v.insert("language_resolved".into(), json!(self.lang()));
        v.insert("language_system".into(), json!(i18n::system_language()));
        v.insert("languages".into(), Value::Object(i18n::languages()));
        Value::Object(v)
    }

    fn status(&self) -> Value {
        let paused = self.b.indexer.paused_folders();
        let missing = self.b.indexer.missing_folders();
        let e = &self.b.embedder;
        let state = e.state.read().clone();
        let update = self.b.updater.status();
        let folders: Vec<Value> = self
            .b
            .store
            .folders()
            .into_iter()
            .map(|f| {
                let id = f.id;
                let mut v = serde_json::to_value(f).unwrap_or_default();
                v["paused"] = json!(paused.contains(&id));
                v["missing"] = json!(missing.contains(&id));
                v
            })
            .collect();
        json!({
            "model": {
                "id": config::MODEL_ID, "status": state.status, "error": state.error,
                "on_gpu": state.on_gpu, "cpu_because": state.cpu_because, "gpu_name": state.gpu_name,
                "engine": state.engine, "download_gb": state.download_gb,
                "setup": state.setup.as_ref().map(|s| json!({"name": s.name, "threads": s.threads})),
                "dim": EMBED_DIM,
            },
            "progress": self.b.indexer.progress(),
            "resources": {"decision": self.b.governor.decision(), "metrics": self.b.governor.metrics()},
            "settings": self.settings_view(),
            "memory": self.memory_view(),
            "folders": folders,
            "update": update,
            "app_mode": self.hooks.app_mode,
        })
    }

    fn update_settings(&self, body: SettingsIn) -> Result<Response, ApiError> {
        let s = &self.b.settings;
        if let Some(lang) = &body.language {
            if lang != "auto" && !i18n::languages().contains_key(lang) {
                return Err(fail(400, "unknown_language", json!({"lang": lang})));
            }
            let _ = s.update("language", json!(lang));
        }
        if let Some(theme) = &body.theme {
            if !THEMES.contains(&theme.as_str()) {
                return Err(fail(400, "unknown_theme", json!({"theme": theme})));
            }
            let _ = s.update("theme", json!(theme));
        }
        if let Some(v) = body.auto_update {
            let _ = s.update("auto_update", json!(v));
        }
        if let Some(v) = body.diagnostics {
            let _ = s.update("diagnostics", json!(v));
            self.b.diagnostics.set_on(v);
        }
        if let Some(v) = &body.seen_version {
            if crate::updater::parse_version(v).is_some() {
                let _ = s.update("seen_version", json!(v));
                crate::updater::forget_result(); // (the page has shown how the update went)
            }
        }
        if let Some(v) = body.problems_seen {
            let _ = s.update("problems_seen", json!(v.max(0)));
        }
        for (key, view) in [("view_files", &body.view_files), ("view_images", &body.view_images)] {
            if let Some(view) = view {
                if !VIEWS.contains(&view.as_str()) {
                    return Err(fail(400, "unknown_view", json!({"view": view})));
                }
                let _ = s.update(key, json!(view));
            }
        }
        let before = Types::of(s).fingerprint();
        if let Some(off) = &body.kinds_off {
            if let Some(bad) = off.iter().find(|v| !filetypes::CATEGORIES.contains(&v.as_str())) {
                return Err(fail(400, "unknown_type", json!({"type": bad.chars().take(20).collect::<String>()})));
            }
            let mut clean: Vec<&str> = filetypes::CATEGORIES.iter().copied().filter(|c| off.iter().any(|v| v == c)).collect();
            clean.dedup();
            let _ = s.update("kinds_off", json!(clean));
        }
        if let Some(formats) = &body.formats_off {
            let mut clean: Vec<String> = formats.iter().filter_map(|f| filetypes::format_key(f)).collect();
            clean.sort();
            clean.dedup();
            clean.truncate(200);
            let _ = s.update("formats_off", json!(clean));
        }
        for (key, value) in [
            ("hidden_files", body.hidden_files),
            ("program_folders", body.program_folders),
        ] {
            if let Some(v) = value {
                let _ = s.update(key, json!(v));
            }
        }
        if Types::of(s).fingerprint() != before {
            for f in self.b.store.folders() {
                self.b.indexer.enqueue_scan(f.id); // pick up, or forget, what's searched now
            }
        }
        if let Some(on) = body.autostart {
            if !self.hooks.app_mode {
                return Err(fail(400, "autostart_app_only", json!({})));
            }
            let _ = crate::shortcuts::set_autostart(on);
        }
        Ok(json_reply(&self.settings_view()))
    }

    fn present(&self, results: Vec<crate::search::Result>) -> Value {
        let ids: Vec<i64> = results.iter().map(|r| r.file_id).collect();
        let files = self.b.store.files_by_ids(&ids);
        let key = |r: &crate::search::Result| (r.file_id << crate::store::FTS_SHIFT) | r.chunk;
        let pages = self.b.store.pages(&results.iter().map(key).collect::<Vec<_>>());
        let out: Vec<Value> = results
            .into_iter()
            .filter_map(|r| {
                let f = files.get(&r.file_id)?;
                let page = pages.get(&key(&r)).copied();
                let (dir, name) = split_path(&f.path);
                let mut v = serde_json::to_value(&r).ok()?;
                let m = v.as_object_mut()?;
                m.insert("path".into(), json!(f.path));
                m.insert("name".into(), json!(name));
                m.insert("dir".into(), json!(dir));
                m.insert("kind".into(), json!(f.kind));
                m.insert("mtime".into(), json!(f.mtime));
                m.insert("size".into(), json!(f.size));
                m.insert("preview".into(), json!(has_preview(&f.path, &f.kind)));
                if let Some(page) = page {
                    m.insert("page".into(), json!(page)); // (the page the passage starts on)
                }
                Some(v)
            })
            .collect();
        Value::Array(out)
    }

    /// Nearest chunks, optionally only from files inside the folder `under`.
    fn vector_hits(&self, vector: &[f32], n: usize, kind: &str, folder: Option<i64>, under: Option<&str>) -> Vec<crate::search::VectorHit> {
        let ids = under.filter(|u| !u.is_empty()).map(|u| self.b.store.file_ids_under(u));
        self.b.store.search(vector, n, kinds(kind), folder, ids.as_deref())
    }

    fn search(&self, q: &Query) -> Result<Response, ApiError> {
        let text = q.get("q").ok_or_else(|| unreadable("q: missing"))?.trim().to_string();
        let kind = q.get("kind").map(String::as_str).unwrap_or("all");
        let folder = query_int(q, "folder")?;
        let under = q.get("under").map(String::as_str);
        let limit = query_int(q, "limit")?.unwrap_or(30).min(100).max(0) as usize;
        let e = &self.b.embedder;
        if text.is_empty() {
            return Ok(json_reply(&json!({"results": [], "semantic": e.ready()})));
        }
        self.b.indexer.wake_model(); // if it went to sleep; this search uses keywords meanwhile
        let query_vec = if e.ready() {
            match e.query(&text) {
                Ok(v) => Some(v),
                Err(err) => {
                    log::info!("Search without the model: {err}");
                    None
                }
            }
        } else {
            None
        };
        let hits = match &query_vec {
            Some(v) => self.vector_hits(v, limit * 6, kind, folder, under),
            None => Vec::new(),
        };
        // Words most passages hold can't rank anything: left out, unless keywords are all there is.
        let terms = query_terms(&text);
        let common = if query_vec.is_some() { self.b.store.common_terms(&terms, typing_prefix(&terms)) } else { Default::default() };
        let keyword_hits = match fts_query(&terms, &common) {
            Some(m) => self.b.store.keyword_search(&m, limit * 4, kinds(kind), folder, under),
            None => Vec::new(),
        };
        let store = self.b.store.clone();
        let similarity = query_vec.clone().map(|v| move |ids: &[i64]| store.similarities(&v, ids));
        let results = fuse(&hits, &keyword_hits, &text, limit, similarity.as_ref().map(|f| f as &dyn Fn(&[i64]) -> HashMap<i64, f32>));
        // The likely ones first, then the rest (the page shows those on request).
        let Value::Array(items) = self.present(results) else { unreachable!() };
        let flags = crate::search::likely(
            &items.iter().map(|r| (r["similarity"].as_f64(), r["exact"] == true, r["kind"] == "image")).collect::<Vec<_>>(),
        );
        let likely = flags.iter().filter(|x| **x).count();
        let (top, rest): (Vec<(Value, bool)>, Vec<(Value, bool)>) = items.into_iter().zip(flags).partition(|(_, f)| *f);
        let items: Vec<Value> = top.into_iter().chain(rest).map(|(v, _)| v).collect();
        Ok(json_reply(&json!({"results": items, "likely": likely, "semantic": e.ready()})))
    }

    /// Files like this one, by meaning (the files' own vectors: no model needed), or like it and
    /// like the words in `q` too (the two vectors' average; the model turns the words into one).
    /// Files nearly the same as a whole (copies, other formats, versions) come first, marked
    /// "near". With the result: the file itself, for the page to show what's compared with.
    fn similar(&self, id: i64, q: &Query) -> Result<Response, ApiError> {
        let kind = q.get("kind").map(String::as_str).unwrap_or("all");
        let folder = query_int(q, "folder")?;
        let limit = query_int(q, "limit")?.unwrap_or(30).clamp(0, 100) as usize;
        let reference = self.mean_vector(id)?;
        let words = q.get("q").map(|s| s.trim()).filter(|s| !s.is_empty());
        let mut v = reference.clone();
        let mut with_words = false;
        if let Some(text) = words {
            self.b.indexer.wake_model();
            if let Some(w) = self.b.embedder.ready().then(|| self.b.embedder.query(text).ok()).flatten() {
                let mix: Vec<f32> = v.iter().zip(&w).map(|(a, b)| a + b).collect();
                let norm = crate::store::dot(&mix, &mix).sqrt().max(1e-9);
                v = mix.into_iter().map(|x| x / norm).collect();
                with_words = true;
            }
        }
        let hits: Vec<_> = self
            .vector_hits(&v, limit * 6, kind, folder, q.get("under").map(String::as_str))
            .into_iter()
            .filter(|h| h.file_id != id)
            .collect();
        let results = fuse(&hits, &[], "", limit, None);
        // (with words added, it's a search of its own: no "nearly the same" part)
        let near: HashSet<i64> = results
            .iter()
            .filter(|r| !with_words && self.mean_vector(r.file_id).is_ok_and(|m| crate::store::dot(&m, &reference) >= NEAR_IDENTICAL))
            .map(|r| r.file_id)
            .collect();
        let Value::Array(mut items) = self.present(results) else { unreachable!() };
        for item in &mut items {
            if item["file_id"].as_i64().is_some_and(|f| near.contains(&f)) {
                item["near"] = json!(true);
            }
        }
        items.sort_by_key(|item| item["near"] != true); // (stable: each part keeps its order)
        let me = crate::search::Result {
            file_id: id, chunk: 0, snippet: String::new(), similarity: None, score: 0.0, keyword: false, exact: false,
        };
        let reference = self.present(vec![me]).as_array().and_then(|a| a.first().cloned()).unwrap_or(Value::Null);
        Ok(json_reply(&json!({"results": items, "reference": reference, "semantic": true, "words": with_words})))
    }

    /// Files like one that doesn't have to be indexed: dropped on the window or pasted (the bytes,
    /// `name` its file name). It's read as indexing reads it, in a temp folder that's removed right
    /// after, and its passages or picture go to the model; files are compared with their mean.
    fn similar_to(&self, q: &Query, body: &[u8]) -> Result<Response, ApiError> {
        let name = q.get("name").map(|n| crate::config::file_name(n).trim().to_string()).filter(|n| !n.is_empty());
        let name = name.unwrap_or_else(|| "pasted.png".into());
        if body.is_empty() {
            return Err(fail(400, "empty_file", json!({})));
        }
        let e = &self.b.embedder;
        if !e.ready() {
            self.b.indexer.wake_model();
            return Err(fail(503, "model_waking", json!({})));
        }
        let copy = archive::temp_copy(&name, body).map_err(|err| detail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()))?;
        let read = crate::extract::extract(&copy.file, body.len() as u64, None, None);
        drop(copy);
        let ex = read.map_err(|err| fail(415, "not_comparable", json!({"why": crate::extract::problem(&err)})))?;
        // (enough of it to say what it's about: a long document's first passages, a scan's pages)
        let texts: Vec<String> = ex.chunks.iter().filter_map(|c| c.text.clone()).take(24).collect();
        let pictures: Vec<&image::RgbImage> = ex.chunks.iter().filter_map(|c| c.image.as_ref()).take(6).collect();
        let mut vectors = if texts.is_empty() { Vec::new() } else { e.documents(&texts).map_err(model_failed)? };
        if !pictures.is_empty() {
            vectors.extend(e.images(&pictures).map_err(model_failed)?);
        }
        if vectors.is_empty() {
            return Err(fail(415, "not_comparable", json!({})));
        }
        let mut mean = vec![0f32; EMBED_DIM];
        for v in &vectors {
            mean.iter_mut().zip(v).for_each(|(m, x)| *m += x);
        }
        let norm = crate::store::dot(&mean, &mean).sqrt().max(1e-9);
        let mean: Vec<f32> = mean.into_iter().map(|x| x / norm).collect();
        // (words typed too: like this file and like the words, as for an indexed file)
        let mut v = mean.clone();
        if let Some(w) = q.get("q").map(|s| s.trim()).filter(|s| !s.is_empty()).and_then(|text| e.query(text).ok()) {
            let mix: Vec<f32> = v.iter().zip(&w).map(|(a, b)| a + b).collect();
            let norm = crate::store::dot(&mix, &mix).sqrt().max(1e-9);
            v = mix.into_iter().map(|x| x / norm).collect();
        }
        let kind = q.get("kind").map(String::as_str).unwrap_or("all");
        let limit = query_int(q, "limit")?.unwrap_or(30).clamp(0, 100) as usize;
        let hits = self.vector_hits(&v, limit * 6, kind, None, q.get("under").map(String::as_str));
        let results = fuse(&hits, &[], "", limit, None);
        let near: HashSet<i64> = results
            .iter()
            .filter(|r| self.mean_vector(r.file_id).is_ok_and(|m| crate::store::dot(&m, &mean) >= NEAR_IDENTICAL))
            .map(|r| r.file_id)
            .collect();
        let Value::Array(mut items) = self.present(results) else { unreachable!() };
        for item in &mut items {
            if item["file_id"].as_i64().is_some_and(|f| near.contains(&f)) {
                item["near"] = json!(true);
            }
        }
        items.sort_by_key(|item| item["near"] != true);
        let reference = json!({"file_id": 0, "name": name, "path": "", "dir": "", "kind": ex.kind, "size": body.len(),
                               "mtime": 0, "preview": false});
        Ok(json_reply(&json!({"results": items, "reference": reference, "semantic": true})))
    }

    fn mean_vector(&self, file_id: i64) -> Result<Vec<f32>, ApiError> {
        let vecs = self.b.store.file_vectors(file_id);
        if vecs.is_empty() {
            return Err(fail(404, "no_vectors", json!({})));
        }
        let n = vecs.len() / EMBED_DIM;
        let mut mean = vec![0f32; EMBED_DIM];
        for r in 0..n {
            for j in 0..EMBED_DIM {
                mean[j] += vecs[r * EMBED_DIM + j];
            }
        }
        mean.iter_mut().for_each(|x| *x /= n as f32);
        let norm = crate::store::dot(&mean, &mean).sqrt();
        let norm = if norm > 0.0 { norm } else { 1.0 };
        Ok(mean.into_iter().map(|x| x / norm).collect())
    }

    fn indexed_file(&self, file_id: i64) -> Result<FileRecord, ApiError> {
        match self.b.store.file(file_id) {
            Some(f) if archive::exists(&f.path) => Ok(f),
            _ => Err(fail(404, "file_not_found", json!({}))),
        }
    }

    /// JPEG preview of an image, or of a document's first page, kept on disk.
    /// A file's picture (`page`: of a document, that page), cached in thumbs\.
    fn rendered(&self, file_id: i64, size: u32, quality: u8, page: Option<u32>) -> Result<Response, ApiError> {
        let f = self.indexed_file(file_id)?;
        let dir = config::thumbs_dir();
        let _ = std::fs::create_dir_all(&dir);
        let at = page.filter(|&n| n > 1).map(|n| format!("-p{n}")).unwrap_or_default();
        let out = dir.join(format!("{file_id}-{}-{size}{at}.jpg", f.mtime as i64));
        if !out.exists() {
            let made = (|| -> anyhow::Result<()> {
                let img = if archive::split(&f.path).is_none() {
                    crate::pictures::picture_at(std::path::Path::new(&f.path), page, size).map_err(|e| anyhow::anyhow!("{e}"))?
                } else {
                    let copy = archive::extracted(&f.path, max_bytes(&f.kind, crate::config::file_name(&f.path)))?;
                    crate::pictures::picture_at(&copy.file, page, size).map_err(|e| anyhow::anyhow!("{e}"))?
                };
                let tmp = out.with_extension("jpg.tmp");
                {
                    let mut file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
                    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, quality).encode_image(&img)?;
                }
                std::fs::rename(&tmp, &out)?;
                Ok(())
            })();
            if let Err(e) = made {
                log::info!("No preview for {}: {e}", f.path);
                return Err(fail(415, "no_preview", json!({})));
            }
        }
        let bytes = std::fs::read(&out).map_err(|_| fail(415, "no_preview", json!({})))?;
        let mut r = Response::new(Body::from(bytes));
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
        r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=86400"));
        Ok(r)
    }

    fn open_file(&self, file_id: i64) -> Result<Response, ApiError> {
        let f = self.indexed_file(file_id)?;
        let mut path = f.path.clone();
        if archive::split(&path).is_some() {
            // inside an archive: open a copy of just that file
            path = archive::open_copy(&path, max_bytes(&f.kind, crate::config::file_name(&path)))
                .map_err(|_| fail(404, "file_not_found", json!({})))?
                .to_string_lossy()
                .to_string();
        }
        with_normal_priority(|| {
            if RUNNABLE_EXTS.contains(&crate::config::suffix(&path).as_str()) {
                // Scripts and web pages would *run* when opened: show them in an editor instead.
                if !shell_open(&path, "edit") {
                    let _ = std::process::Command::new("notepad.exe").arg(&path).spawn();
                }
            } else {
                shell_open(&path, "open");
            }
        });
        ok()
    }

    fn add_folder(&self, body: FolderIn) -> Result<Response, ApiError> {
        let raw = body.path.trim().trim_matches('"').to_string();
        if raw.is_empty() {
            return Err(fail(400, "empty_path", json!({})));
        }
        let expanded = match raw.strip_prefix('~') {
            Some(rest) => format!("{}{rest}", std::env::var("USERPROFILE").unwrap_or_default()),
            None => raw.clone(),
        };
        let path = std::fs::canonicalize(&expanded)
            .map(config::strip_verbatim)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| std::path::absolute(&expanded).map(|p| p.to_string_lossy().to_string()).unwrap_or(expanded.clone()));
        if !std::path::Path::new(&path).is_dir() {
            return Err(fail(400, "not_a_folder", json!({"path": path})));
        }
        let mut inner = Vec::new();
        for f in self.b.store.folders() {
            if is_inside(&path, &f.path) {
                return Err(fail(400, "already_covered", json!({"path": f.path})));
            }
            if is_inside(&f.path, &path) {
                inner.push(f);
            }
        }
        let id = self.b.store.add_folder(&path).map_err(|e| detail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        // Indexed folders inside it become part of it, with everything indexed in them.
        for f in &inner {
            self.b.indexer.forget_folder(f.id);
            self.b.store.take_over(id, f.id).map_err(|e| detail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        }
        self.b.indexer.enqueue_scan(id);
        self.b.indexer.watcher().sync();
        Ok(json_reply(&json!({"id": id, "path": path, "took_over": inner.iter().map(|f| f.path.clone()).collect::<Vec<_>>()})))
    }

    fn known_folder(&self, id: i64) -> Result<(), ApiError> {
        if self.b.store.folder(id).is_none() { Err(fail(404, "unknown_folder", json!({}))) } else { Ok(()) }
    }

    /// The API: method and path to a reply.
    fn route(&self, method: &Method, path: &str, q: &Query, headers: &HeaderMap, body: &[u8]) -> Result<Response, ApiError> {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let get = method == Method::GET || method == Method::HEAD;
        let post = method == Method::POST;
        let b = &self.b;
        match parts.as_slice() {
            [""] if get => {
                // The saved theme goes into the page, so theme.js can pick light or dark before
                // anything is drawn: no white flash when the app opens in dark mode.
                let mode = b.settings.str("theme");
                let mode = if THEMES.contains(&mode.as_str()) { mode } else { "auto".into() };
                let page = String::from_utf8_lossy(static_file("index.html").unwrap_or_default())
                    .replacen("<html ", &format!("<html data-theme-mode=\"{mode}\" "), 1);
                let mut r = Response::new(Body::from(page));
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
                r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                Ok(r)
            }
            ["static", name] if get => {
                let Some((_, bytes, kind)) = STATIC.iter().find(|(n, _, _)| n == name) else {
                    return Err(detail(StatusCode::NOT_FOUND, "Not Found"));
                };
                let tag = etag_of(bytes);
                if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
                    let mut r = Response::new(Body::empty());
                    *r.status_mut() = StatusCode::NOT_MODIFIED;
                    r.headers_mut().insert(header::ETAG, HeaderValue::from_str(&tag).unwrap());
                    return Ok(r);
                }
                let mut r = Response::new(Body::from(*bytes));
                r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
                r.headers_mut().insert(header::ETAG, HeaderValue::from_str(&tag).unwrap());
                Ok(r)
            }
            ["api", "status"] if get => Ok(json_reply(&self.status())),
            ["api", "diagnostics"] if get => Ok(json_reply(&b.diagnostics.view(&b.embedder))),
            ["api", "settings"] if post => self.update_settings(body_json(body)?),
            ["api", "update", "check"] if post => {
                b.updater.check();
                Ok(json_reply(&b.updater.status()))
            }
            ["api", "update", "apply"] if post => {
                if !b.updater.can_apply() {
                    return Err(fail(400, "update_app_only", json!({})));
                }
                if b.updater.latest.lock().is_none() {
                    b.updater.check();
                }
                b.updater.apply().map_err(|e| fail(409, "update_failed", json!({"error": format!("{e:#}")})))?;
                ok()
            }
            ["api", "update", "open"] if post => {
                let url = b.updater.latest.lock().as_ref().and_then(|l| l["url"].as_str().map(String::from));
                open_browser(&url.unwrap_or_else(|| REPO_URL.to_string()));
                ok()
            }
            ["api", "show"] if post => match &self.hooks.show_window {
                Some(show) => {
                    show();
                    ok()
                }
                None => Ok(json_reply(&json!({"ok": false}))),
            },
            ["api", "report-bug"] if post => {
                let body = format!(
                    "**What happened?**\n\n\n**What did you expect?**\n\n\n---\nQuiekel Embed {} on Windows",
                    crate::updater::current_version()
                );
                let encoded: String = form_urlencoded::Serializer::new(String::new()).append_pair("body", &body).finish();
                open_browser(&format!("{REPO_URL}/issues/new?{encoded}"));
                ok()
            }
            ["api", "window", "theme"] if post => {
                let t: ThemeIn = body_json(body)?;
                if let Some(set) = &self.hooks.set_dark {
                    set(t.dark);
                }
                ok()
            }
            ["api", "folders"] if post => self.add_folder(body_json(body)?),
            ["api", "folders", "pick"] if post => {
                let Some(_one) = self.pick_lock.try_lock() else { return Err(fail(409, "picker_open", json!({}))) };
                let picked = (self.hooks.pick_folder)(&i18n::t(&self.lang(), "folders.picker_title", &[]));
                Ok(json_reply(&json!({"path": picked})))
            }
            ["api", "folders", id] if method == Method::DELETE => {
                let id = path_int("folder_id", id)?;
                self.known_folder(id)?;
                b.indexer.remove_folder(id).map_err(|e| detail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
                ok()
            }
            ["api", "folders", id, action] if post && ["rescan", "pause", "resume"].contains(action) => {
                let id = path_int("folder_id", id)?;
                self.known_folder(id)?;
                match *action {
                    "rescan" => b.indexer.enqueue_scan(id), // (also tries the unreadable files again)
                    "pause" => b.indexer.pause_folder(id),
                    _ => b.indexer.resume_folder(id),
                }
                ok()
            }
            ["api", "folders", id, "errors", "seen"] if post => {
                let id = path_int("folder_id", id)?;
                self.known_folder(id)?;
                b.store.errors_seen(id);
                ok()
            }
            ["api", "folders", id, "watch"] if post => {
                let id = path_int("folder_id", id)?;
                let w: WatchIn = body_json(body)?;
                self.known_folder(id)?;
                b.store.set_watch(id, w.watch);
                b.indexer.watcher().sync();
                if w.watch {
                    b.indexer.enqueue_scan(id); // pick up what changed while unwatched
                }
                ok()
            }
            ["api", "folders", id, "leave-out"] if post => {
                let id = path_int("folder_id", id)?;
                let l: LeaveOutIn = body_json(body)?;
                let Some(folder) = b.store.folder(id) else { return Err(fail(404, "unknown_folder", json!({}))) };
                if !is_inside(&l.path, &folder.path) || crate::store::normcase(l.path.trim_end_matches('\\')) == crate::store::normcase(folder.path.trim_end_matches('\\')) {
                    return Err(fail(400, "not_inside", json!({})));
                }
                b.store.set_left_out(id, &l.path, l.out).map_err(|e| detail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
                if !l.out {
                    b.indexer.enqueue_scan(id); // (it's read again)
                }
                ok()
            }
            ["api", "folders", id, "locate"] if post => {
                let id = path_int("folder_id", id)?;
                let f: FolderIn = body_json(body)?;
                self.known_folder(id)?;
                let to = std::fs::canonicalize(f.path.trim().trim_matches('"'))
                    .map(config::strip_verbatim)
                    .map(|p| p.to_string_lossy().to_string())
                    .map_err(|_| fail(400, "not_a_folder", json!({"path": f.path})))?;
                if !std::path::Path::new(&to).is_dir() {
                    return Err(fail(400, "not_a_folder", json!({"path": to})));
                }
                for other in b.store.folders().into_iter().filter(|o| o.id != id) {
                    if is_inside(&to, &other.path) || is_inside(&other.path, &to) {
                        return Err(fail(400, "already_covered", json!({"path": other.path})));
                    }
                }
                b.store.relocate(id, &to).map_err(|e| detail(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
                b.indexer.enqueue_scan(id);
                b.indexer.watcher().sync();
                Ok(json_reply(&json!({"id": id, "path": to})))
            }
            ["api", "folders", id, "errors"] if get => Ok(json_reply(&json!(b.store.errors(path_int("folder_id", id)?, 500)))),
            ["api", "folders", id, "details"] if get => {
                let id = path_int("folder_id", id)?;
                self.known_folder(id)?;
                let mut d = b.store.details(id, 5);
                d["errors"] = json!(b.store.errors(id, 500));
                d["left_out"] = json!(b.store.left_out(id));
                Ok(json_reply(&d))
            }
            ["api", "folders", id, "reveal"] if post => {
                let id = path_int("folder_id", id)?;
                let ex: ExampleIn = body_json(body)?;
                let report = b.store.details(id, 5)["report"].clone();
                let examples: Vec<String> = report[&ex.reason]["examples"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if ex.index < 0 || ex.index as usize >= examples.len() {
                    return Err(fail(404, "file_not_found", json!({})));
                }
                let example = &examples[ex.index as usize];
                let target = archive::split(example).map(|(disk, _)| disk).unwrap_or_else(|| example.clone()); // in an archive: show the archive
                if !std::path::Path::new(&target).exists() {
                    return Err(fail(404, "file_not_found", json!({})));
                }
                with_normal_priority(|| reveal(&target));
                ok()
            }
            ["api", "filetypes"] if get => Ok(json_reply(&filetypes::menu(&Types::of(&b.settings), &b.store.format_counts()))),
            ["api", "pause"] if post => {
                b.indexer.pause();
                ok()
            }
            ["api", "resume"] if post => {
                b.indexer.resume();
                ok()
            }
            ["api", "retry-model"] if post => {
                b.indexer.retry_model();
                ok()
            }
            ["api", "search"] if get => self.search(q),
            ["api", "dirs"] if get => {
                if let Some(find) = q.get("find").filter(|f| !f.trim().is_empty()) {
                    return Ok(json_reply(&json!({"find": find, "dirs": b.store.find_dirs(find, 30)})));
                }
                let path = q.get("path").cloned().unwrap_or_default();
                if path.is_empty() {
                    let dirs: Vec<Value> = b
                        .store
                        .folders()
                        .into_iter()
                        .map(|f| {
                            let name = split_path(&f.path).1;
                            json!({"path": f.path, "name": if name.is_empty() { f.path.clone() } else { name },
                                   "count": f.indexed + f.pending})
                        })
                        .collect();
                    return Ok(json_reply(&json!({"path": "", "dirs": dirs})));
                }
                Ok(json_reply(&json!({"path": path, "dirs": b.store.subdirs(&path)})))
            }
            ["api", "map"] if get => {
                // Kept ready as JSON; the page asks again every few seconds while the map updates,
                // and gets "not modified" until it changed.
                let (body, tag) = b.maps.reply();
                let mut r = if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
                    let mut r = Response::new(Body::empty());
                    *r.status_mut() = StatusCode::NOT_MODIFIED;
                    r
                } else {
                    let mut r = Response::new(Body::from(body));
                    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                    r
                };
                r.headers_mut().insert(header::ETAG, HeaderValue::from_str(&tag).unwrap_or(HeaderValue::from_static("\"\"")));
                r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                Ok(r)
            }
            ["api", "map", "vector", id] if get => {
                let id = path_int("file_id", id)?;
                let Some(f) = b.store.file(id) else { return Err(fail(404, "file_not_found", json!({}))) };
                let v = self.mean_vector(id)?;
                let rounded: Vec<f64> = v.iter().map(|x| (*x as f64 * 10000.0).round() / 10000.0).collect();
                Ok(json_reply(&json!({
                    "vector": rounded,
                    "file": {"mtime": f.mtime, "size": f.size, "kind": f.kind, "preview": has_preview(&f.path, &f.kind),
                             "passage": b.store.first_passage(id).chars().take(600).collect::<String>()},
                })))
            }
            ["api", "map", "related", id] if get => {
                let v = self.mean_vector(path_int("file_id", id)?)?;
                Ok(json_reply(&b.maps.related(&v)))
            }
            ["api", "map", "dimension", index] if get => {
                let index = path_int("index", index)?;
                if !(0..EMBED_DIM as i64).contains(&index) {
                    return Err(fail(404, "file_not_found", json!({})));
                }
                Ok(json_reply(&b.maps.dimension(index as usize)))
            }
            ["api", "similar", id] if get => self.similar(path_int("file_id", id)?, q),
            ["api", "similar-to"] if post => self.similar_to(q, body),
            ["api", "thumb", id] if get => self.rendered(path_int("file_id", id)?, 360, 82, page_of(q)?),
            ["api", "preview", id] if get => self.rendered(path_int("file_id", id)?, 1600, 88, page_of(q)?),
            ["api", "open", id] if post => self.open_file(path_int("file_id", id)?),
            ["api", "reveal", id] if post => {
                let f = self.indexed_file(path_int("file_id", id)?)?;
                let target = archive::split(&f.path).map(|(disk, _)| disk).unwrap_or(f.path); // an archive: show the archive
                with_normal_priority(|| reveal(&target));
                ok()
            }
            _ => {
                if known_path(&parts) {
                    Err(detail(StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed"))
                } else {
                    Err(detail(StatusCode::NOT_FOUND, "Not Found"))
                }
            }
        }
    }
}


/// A path the API has, for some method (wrong method: 405 rather than 404).
fn known_path(parts: &[&str]) -> bool {
    matches!(
        parts,
        [""] | ["api", "status" | "diagnostics" | "settings" | "pause" | "resume" | "retry-model" | "search" | "dirs" | "map" | "filetypes" | "show" | "report-bug" | "folders" | "similar-to"]
            | ["api", "update", "check" | "apply" | "open"]
            | ["api", "window", "theme"]
            | ["api", "folders", _]
            | ["api", "folders", _, "rescan" | "pause" | "resume" | "watch" | "errors" | "details" | "reveal" | "leave-out" | "locate"]
            | ["api", "folders", _, "errors", "seen"]
            | ["api", "map", "vector" | "related" | "dimension", _]
            | ["api", "similar" | "thumb" | "preview" | "open" | "reveal", _]
    )
}

/// (folder, name) of a path.
pub fn split_path(path: &str) -> (String, String) {
    match path.rfind('\\') {
        Some(i) => {
            let dir = &path[..i];
            let dir = if dir.ends_with(':') { format!("{dir}\\") } else { dir.to_string() };
            (dir, path[i + 1..].to_string())
        }
        None => (".".into(), path.to_string()),
    }
}

async fn handle(State(server): State<Arc<Server>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    // Blocks DNS rebinding (Host), other web pages (Sec-Fetch-Site, which browsers always send and
    // pages can't fake), and form posts from them (custom header).
    let host = parts.headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    let site = parts.headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).unwrap_or("same-origin");
    let custom = parts.headers.get("x-quiekel-embed").and_then(|v| v.to_str().ok());
    let forbidden = !server.allowed_hosts.iter().any(|h| h == host)
        || !(site == "same-origin" || site == "none")
        || (parts.method != Method::GET && parts.method != Method::HEAD && custom != Some("1"));
    let mut response = if forbidden {
        let mut r = Response::new(Body::from("Forbidden"));
        *r.status_mut() = StatusCode::FORBIDDEN;
        r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
        r
    } else {
        // (up to 64 MB: a file dropped on the window to find ones like it)
        let bytes = axum::body::to_bytes(body, 64 * 1024 * 1024).await.unwrap_or_default();
        let query: Query = parts
            .uri
            .query()
            .map(|q| form_urlencoded::parse(q.as_bytes()).map(|(k, v)| (k.into_owned(), v.into_owned())).collect())
            .unwrap_or_default();
        let path = percent_encoding::percent_decode_str(parts.uri.path()).decode_utf8_lossy().into_owned();
        let method = parts.method.clone();
        let headers = parts.headers.clone();
        let s = server.clone();
        let p = path.clone();
        let result = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.route(&method, &p, &query, &headers, &bytes)))
        })
        .await;
        let mut r = match result {
            Ok(Ok(Ok(r))) => r,
            Ok(Ok(Err(e))) => {
                let mut r = json_reply(&e.body);
                *r.status_mut() = e.status;
                r
            }
            other => {
                let text = match other {
                    Ok(Err(p)) => p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default(),
                    Err(e) => e.to_string(),
                    Ok(Ok(_)) => String::new(),
                };
                log::error!("Request failed: {path}: {text}");
                let mut r = json_reply(&json!({"detail": format!("Error: {text}")}));
                *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
                r
            }
        };
        if path.starts_with("/static/") {
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache")); // revalidate, so updates show up
        }
        if parts.method == Method::HEAD {
            *r.body_mut() = Body::empty();
        }
        r
    };
    for (k, v) in SECURITY_HEADERS {
        response.headers_mut().insert(k, HeaderValue::from_static(v));
    }
    response
}

/// Serve until the process ends. `started` is told once the port is taken (or why it wasn't).
pub fn serve(server: Arc<Server>, started: std::sync::mpsc::Sender<Result<(), String>>) {
    let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(4).max_blocking_threads(40).enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = started.send(Err(e.to_string()));
            return;
        }
    };
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind((config::HOST, *config::PORT)).await {
            Ok(l) => l,
            Err(e) => {
                let _ = started.send(Err(e.to_string()));
                return;
            }
        };
        let _ = started.send(Ok(()));
        // Small replies go out at once (else Windows holds them back for its delayed ACK).
        use axum::serve::ListenerExt;
        let listener = listener.tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        });
        let app = axum::Router::new().fallback(handle).with_state(server);
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("The server stopped: {e}");
        }
    });
}

pub fn url() -> String {
    format!("http://{}:{}", config::HOST, *config::PORT)
}

pub fn port_in_use() -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], *config::PORT)),
        std::time::Duration::from_millis(300),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(split_path(r"C:\a\b.txt"), (r"C:\a".to_string(), "b.txt".to_string()));
        assert_eq!(split_path(r"C:\b.txt"), (r"C:\".to_string(), "b.txt".to_string()));
        assert!(known_path(&["api", "folders", "3", "rescan"]));
        assert!(!known_path(&["api", "nope"]));
        assert!(static_file("index.html").is_some());
    }

    // ---- the app's HTTP API on a fresh, empty index (the model is never loaded) ----------------

    use crate::store::{Entry, VectorRow};

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: Vec<u8>,
    }

    impl Reply {
        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
        }
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.body).into_owned()
        }
        fn header(&self, name: &str) -> &str {
            self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
        }
    }

    struct Client {
        server: Arc<Server>,
        rt: tokio::runtime::Runtime,
        dir: std::path::PathBuf,
    }

    impl Drop for Client {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn client() -> Client {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("quiekel-rs-api-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open_at(&dir.join("state.db")).unwrap());
        let settings = Arc::new(Settings::new(store.clone()));
        let governor = Governor::fixed();
        let embedder = Arc::new(Embedder::new(None, "onnx")); // never loaded: these tests don't need the model
        let indexer = Indexer::new(store.clone(), embedder.clone(), governor.clone(), settings.clone());
        let updater = Updater::new(Box::new(|| false), false);
        let maps = FileMap::new(store.clone());
        let diagnostics = crate::diagnostics::Diagnostics::start(embedder.clone(), governor.clone(), indexer.clone(), false);
        let b = Arc::new(Backend { store, settings, governor, embedder, indexer, updater, maps, diagnostics });
        let hooks = Hooks { app_mode: false, pick_folder: Box::new(|_| None), show_window: None, set_dark: None };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Client { server: Server::new(b, hooks), rt, dir }
    }

    const APP: (&str, &str) = ("x-quiekel-embed", "1");

    impl Client {
        fn b(&self) -> &Backend {
            &self.server.b
        }

        fn send(&self, method: Method, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Reply {
            let mut req = Request::builder().method(method).uri(path);
            if !headers.iter().any(|(k, _)| *k == "host") {
                req = req.header("host", format!("127.0.0.1:{}", *config::PORT));
            }
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            let req = match body {
                Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())),
                None => req.body(Body::empty()),
            };
            let resp = self.rt.block_on(handle(State(self.server.clone()), req.unwrap()));
            let (status, headers) = (resp.status(), resp.headers().clone());
            let body = self.rt.block_on(axum::body::to_bytes(resp.into_body(), usize::MAX)).unwrap().to_vec();
            Reply { status, headers, body }
        }

        fn get(&self, path: &str) -> Reply {
            self.send(Method::GET, path, &[], None)
        }

        fn post(&self, path: &str, body: Value) -> Reply {
            self.send(Method::POST, path, &[APP], Some(body))
        }

        fn status(&self) -> Value {
            self.get("/api/status").json()
        }

        fn settings(&self) -> Value {
            self.status()["settings"].clone()
        }
    }

    fn entry(folder_id: i64, path: &str, kind: &str, status: &str, chunks: i64, error: Option<&str>) -> Entry {
        Entry {
            folder_id, path: path.into(), size: 1, mtime: 1.0, kind: kind.into(), status: status.into(), chunks,
            error: error.map(String::from), search_name: path.rsplit('\\').next().unwrap().into(),
        }
    }

    fn row(folder_id: i64, path: &str, chunk: i64, kind: &str, text: &str, vector: Vec<f32>) -> VectorRow {
        VectorRow { path: path.into(), folder_id, chunk, kind: kind.into(), text: text.into(), vector, page: None }
    }

    // ---- who may ask --------------------------------------------------------------------------------

    #[test]
    fn pages_get_strict_security_headers() {
        let c = client();
        let r = c.get("/");
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.header("content-security-policy").contains("script-src 'self'"));
        assert_eq!(r.header("x-frame-options"), "DENY");
        assert_eq!(r.header("cross-origin-resource-policy"), "same-origin");
    }

    #[test]
    fn other_hosts_and_sites_are_refused() {
        let c = client();
        assert_eq!(c.send(Method::GET, "/api/status", &[("host", "evil.example")], None).status, StatusCode::FORBIDDEN);
        // Another web page (even another localhost port) trying to read or embed our data:
        assert_eq!(c.send(Method::GET, "/api/status", &[("sec-fetch-site", "cross-site")], None).status, StatusCode::FORBIDDEN);
        assert_eq!(c.send(Method::GET, "/api/thumb/1", &[("sec-fetch-site", "same-site")], None).status, StatusCode::FORBIDDEN);
        assert_eq!(c.send(Method::GET, "/api/status", &[("sec-fetch-site", "same-origin")], None).status, StatusCode::OK);
    }

    #[test]
    fn changes_need_the_app_header() {
        let c = client();
        assert_eq!(c.send(Method::POST, "/api/pause", &[], None).status, StatusCode::FORBIDDEN); // e.g. a form post from a web page
        assert_eq!(c.send(Method::POST, "/api/pause", &[APP], None).status, StatusCode::OK);
    }

    #[test]
    fn updates_only_install_from_the_desktop_app() {
        assert_eq!(client().send(Method::POST, "/api/update/apply", &[APP], None).status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_folder_around_indexed_ones_takes_them_over_and_a_moved_one_can_be_found_again() {
        let c = client();
        let base = c.dir.join("disk");
        let (parent, child, moved) = (base.join("Docs"), base.join("Docs").join("Taxes"), base.join("Docs moved"));
        std::fs::create_dir_all(child.join("2024")).unwrap();
        std::fs::create_dir_all(moved.join("Taxes").join("2024")).unwrap();
        let store = &c.b().store;
        let canon = |p: &std::path::Path| config::strip_verbatim(std::fs::canonicalize(p).unwrap()).to_string_lossy().to_string();
        let r = c.post("/api/folders", json!({"path": child.to_string_lossy()}));
        let child_id = r.json()["id"].as_i64().unwrap();
        let file = format!(r"{}\2024\bill.pdf", canon(&child));
        let entry = crate::store::Entry {
            folder_id: child_id, path: file.clone(), size: 1, mtime: 1.0, kind: "doc".into(), status: "indexed".into(),
            chunks: 0, error: None, search_name: "bill.pdf".into(),
        };
        store.save_files(&[entry], &[]).unwrap();
        // Adding the folder around it: one folder, the file indexed in it, nothing read again.
        let r = c.post("/api/folders", json!({"path": parent.to_string_lossy()}));
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.json()["took_over"], json!([canon(&child)]));
        let folders = store.folders();
        assert_eq!((folders.len(), folders[0].indexed), (1, 1));
        let parent_id = folders[0].id;
        assert_eq!(store.signature(&file).unwrap().1, 1);
        // A folder inside the one there is already covered.
        assert_eq!(c.post("/api/folders", json!({"path": child.to_string_lossy()})).json()["detail"]["key"], "err.already_covered");
        // Left out: its files leave the index; back in: the next scan reads them.
        let r = c.post(&format!("/api/folders/{parent_id}/leave-out"), json!({"path": canon(&child), "out": true}));
        assert_eq!(r.status, StatusCode::OK);
        assert!(store.signature(&file).is_none());
        assert_eq!(c.get(&format!("/api/folders/{parent_id}/details")).json()["left_out"], json!([canon(&child)]));
        c.post(&format!("/api/folders/{parent_id}/leave-out"), json!({"path": canon(&child), "out": false}));
        assert!(store.left_out(parent_id).is_empty());
        // Moved: pointed at its new place, what's indexed moves along.
        let entry = crate::store::Entry {
            folder_id: parent_id, path: file.clone(), size: 1, mtime: 1.0, kind: "doc".into(), status: "indexed".into(),
            chunks: 0, error: None, search_name: "bill.pdf".into(),
        };
        store.save_files(&[entry], &[]).unwrap();
        let r = c.post(&format!("/api/folders/{parent_id}/locate"), json!({"path": moved.to_string_lossy()}));
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(store.folders()[0].path, canon(&moved));
        assert!(store.signature(&file).is_none());
        assert!(store.signature(&format!(r"{}\Taxes\2024\bill.pdf", canon(&moved))).is_some());
    }

    #[test]
    fn errors_come_as_translation_keys() {
        let r = client().post("/api/folders", json!({"path": "  "}));
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        let detail = &r.json()["detail"];
        assert_eq!((detail["key"].as_str(), detail["message"].as_str()), (Some("err.empty_path"), Some("Enter a folder path.")));
    }

    #[test]
    fn requests_that_dont_fit_get_422_with_the_reason() {
        let c = client();
        let r = c.post("/api/folders", json!({"folder": "C:/x"})); // (it's "path")
        assert!(r.status == StatusCode::UNPROCESSABLE_ENTITY && r.json()["detail"].as_str().unwrap().contains("path"));
        assert_eq!(c.post("/api/settings", json!({"diagnostics": "very"})).status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(c.get("/api/folders/abc/details").status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(c.get("/api/search?q=x&limit=lots").status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(c.get("/api/search").status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(c.post("/api/settings", json!({"theme": "dark", "unknown": 1})).status, StatusCode::OK); // (others are left alone)
    }

    #[test]
    fn previews_of_files_gone_or_changed_are_tidied_away() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\p").unwrap();
        let entry = crate::store::Entry {
            folder_id: folder, path: r"C:\p\a.jpg".into(), size: 1, mtime: 100.0, kind: "image".into(), status: "indexed".into(),
            chunks: 0, error: None, search_name: "a.jpg".into(),
        };
        store.save_files(&[entry], &[]).unwrap();
        let id = store.signature(r"C:\p\a.jpg").unwrap().0;
        let dir = std::env::temp_dir().join(format!("quiekel-rs-previews-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = [format!("{id}-100-360.jpg"), format!("{id}-100-1600.jpg"), format!("{id}-99-360.jpg"), "999999-5-360.jpg".into(), "7-x.jpg.tmp".into()];
        for n in &names {
            std::fs::write(dir.join(n), b"jpeg").unwrap();
        }
        tidy_previews(store, &dir);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        // The file as it is now keeps its previews; the changed and the gone file's go. (A file still
        // being written is left alone.)
        assert_eq!(left, [format!("{id}-100-1600.jpg"), format!("{id}-100-360.jpg"), "7-x.jpg.tmp".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_language_setting() {
        let c = client();
        let r = c.post("/api/settings", json!({"language": "de"}));
        assert!(r.status == StatusCode::OK && r.json()["language_resolved"] == "de");
        assert_eq!(c.settings()["language"], "de");
        let r = c.post("/api/settings", json!({"language": "klingon"}));
        assert!(r.status == StatusCode::BAD_REQUEST && r.json()["detail"]["key"] == "err.unknown_language");
    }

    // ---- settings ---------------------------------------------------------------------------------

    #[test]
    fn the_defaults_install_nothing_by_themselves() {
        let s = client().settings();
        assert_eq!(s["auto_update"], false); // (updates are looked for; installed when asked)
        assert_eq!((&s["kinds_off"], &s["hidden_files"], &s["program_folders"]), (&json!([]), &json!(false), &json!(false)));
        assert_eq!((&s["view_files"], &s["view_images"]), (&json!("cards"), &json!("grid")));
    }

    #[test]
    fn diagnostics_are_off_until_switched_on() {
        let c = client();
        let d = c.get("/api/diagnostics").json();
        assert!(d["on"] == false && d["samples"].as_array().unwrap().is_empty());
        assert_eq!(c.post("/api/settings", json!({"diagnostics": true})).status, StatusCode::OK);
        assert!(c.get("/api/diagnostics").json()["on"] == true && c.settings()["diagnostics"] == true);
    }

    #[test]
    fn updates_install_only_in_the_installed_desktop_app() {
        let c = client();
        let r = c.post("/api/update/apply", json!({}));
        assert!(r.status == StatusCode::BAD_REQUEST && r.json()["detail"]["key"] == "err.update_app_only");
        assert_eq!(c.status()["update"]["can_apply"], false);
        assert_eq!(c.post("/api/settings", json!({"auto_update": true, "seen_version": "1.2.3"})).status, StatusCode::OK);
        assert!(c.settings()["auto_update"] == true && c.settings()["seen_version"] == "1.2.3");
        assert_eq!(c.post("/api/settings", json!({"seen_version": "nonsense"})).status, StatusCode::OK);
        assert_eq!(c.settings()["seen_version"], "1.2.3"); // (only versions are kept)
    }

    #[test]
    fn result_views_are_remembered_per_kind() {
        let c = client();
        assert_eq!(c.post("/api/settings", json!({"view_files": "list"})).status, StatusCode::OK);
        assert_eq!(c.post("/api/settings", json!({"view_images": "cards"})).status, StatusCode::OK);
        let s = c.settings();
        assert_eq!((&s["view_files"], &s["view_images"]), (&json!("list"), &json!("cards")));
        let r = c.post("/api/settings", json!({"view_files": "carousel"}));
        assert!(r.status == StatusCode::BAD_REQUEST && r.json()["detail"]["key"] == "err.unknown_view");
    }

    #[test]
    fn a_bug_report_opens_githubs_form_with_only_the_version() {
        let c = client();
        LAUNCHED.lock().clear();
        assert_eq!(c.send(Method::POST, "/api/report-bug", &[APP], None).status, StatusCode::OK);
        let opened = LAUNCHED.lock().clone();
        assert!(opened.len() == 1 && opened[0].starts_with("open https://github.com/catdrinkmonster/quiekel-embed/issues/new?body="));
        assert!(opened[0].contains("Quiekel+Embed") && !opened[0].contains("Users"));
    }

    #[test]
    fn the_page_opens_in_the_saved_theme() {
        let c = client();
        assert!(c.get("/").text().contains(r#"data-theme-mode="auto""#));
        c.post("/api/settings", json!({"theme": "dark"}));
        assert!(c.get("/").text().contains(r#"data-theme-mode="dark""#));
    }

    #[test]
    fn the_badge_remembers_what_was_seen() {
        let c = client();
        assert_eq!(c.post("/api/settings", json!({"problems_seen": 3})).status, StatusCode::OK);
        assert_eq!(c.settings()["problems_seen"], 3);
    }

    #[test]
    fn performance_follows_the_pcs_resources_not_a_setting() {
        let c = client();
        assert!(c.settings().get("perf_mode").is_none() && c.settings().get("modes").is_none());
        assert!(c.b().store.keeps_vectors_in_memory()); // (the search data stays in memory)
        let status = c.status();
        assert!(status["resources"]["metrics"].get("ram_free_gb").is_some() && status["memory"].get("app_mb").is_some());
        assert_eq!(c.send(Method::POST, "/api/pause", &[APP], None).status, StatusCode::OK); // the one control
        assert_eq!(c.status()["progress"]["paused"], true);
        assert_eq!(c.send(Method::POST, "/api/resume", &[APP], None).status, StatusCode::OK);
        assert_eq!(c.status()["progress"]["paused"], false);
    }

    #[test]
    fn the_file_types_menu() {
        let c = client();
        let menu = c.get("/api/filetypes").json();
        let keys: Vec<&str> = menu["categories"].as_array().unwrap().iter().map(|k| k["key"].as_str().unwrap()).collect();
        assert_eq!(keys, ["documents", "pictures", "text", "mail", "archives"]);
        assert!(menu["categories"].as_array().unwrap().iter().all(|k| k["on"] == true));
        assert!(menu["rules"] == menu["defaults"]);

        let r = c.post("/api/settings", json!({"kinds_off": ["archives", "pictures", "archives"], "hidden_files": true}));
        assert_eq!(r.status, StatusCode::OK);
        let s = r.json();
        assert_eq!((&s["kinds_off"], &s["hidden_files"]), (&json!(["pictures", "archives"]), &json!(true)));
        let on: Vec<bool> = c.get("/api/filetypes").json()["categories"].as_array().unwrap().iter().map(|k| k["on"] == true).collect();
        assert_eq!(on, [true, false, true, true, false]);
        let bad = c.post("/api/settings", json!({"kinds_off": ["../evil"]}));
        assert!(bad.status == StatusCode::BAD_REQUEST && bad.json()["detail"]["key"] == "err.unknown_type");
    }

    // ---- folders --------------------------------------------------------------------------------

    #[test]
    fn unreadable_files_warn_once_and_stay_listed() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\pics").unwrap();
        store.save_files(&[entry(folder, r"C:\pics\a.jpg", "image", "error", 0, Some("!damaged_image"))], &[]).unwrap();
        let warnings = || {
            let f = c.status()["folders"][0].clone();
            (f["errors"].as_i64().unwrap(), f["unreadable"].as_i64().unwrap())
        };
        assert_eq!(warnings(), (1, 1));
        assert_eq!(c.send(Method::POST, &format!("/api/folders/{folder}/errors/seen"), &[APP], None).status, StatusCode::OK);
        assert_eq!(warnings(), (0, 1));
        let rows = c.get(&format!("/api/folders/{folder}/errors")).json();
        assert_eq!(rows, json!([{"id": rows[0]["id"].clone(), "path": r"C:\pics\a.jpg", "error": "!damaged_image", "seen": true}]));
        assert_eq!(c.send(Method::POST, &format!("/api/folders/{folder}/rescan"), &[APP], None).status, StatusCode::OK);
        assert_eq!(warnings().0, 0); // trying again doesn't bring the warning back
        assert_eq!(c.send(Method::POST, "/api/folders/999/errors/seen", &[APP], None).status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn folder_details_say_why_files_are_not_searched() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\docs").unwrap();
        let mut empty = entry(folder, r"C:\docs\leer.txt", "text", "skipped", 0, Some("!empty"));
        empty.size = 0;
        store.save_files(&[empty], &[]).unwrap();
        let mut report = filetypes::Report::default();
        report.add("media", r"C:\docs\a.mp4", Some(".mp4"), 2);
        store.mark_scanned(folder, Some(&report));
        let d = c.get(&format!("/api/folders/{folder}/details")).json();
        assert_eq!(d["report"]["media"]["n"], 2);
        let skipped: Vec<(String, i64, String)> = d["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["reason"].as_str().unwrap().into(), s["n"].as_i64().unwrap(), s["examples"][0]["path"].as_str().unwrap().into()))
            .collect();
        assert_eq!(skipped, [("!empty".to_string(), 1, r"C:\docs\leer.txt".to_string())]);
        assert_eq!(d["errors"], json!([]));
        assert_eq!(c.status()["folders"][0]["unsearched"], 3);
        assert_eq!(c.get("/api/folders/999/details").status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn files_left_out_are_shown_by_where_the_details_name_them() {
        let c = client();
        let store = &c.b().store;
        let docs = c.dir.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let folder = store.add_folder(&docs.to_string_lossy()).unwrap();
        let video = docs.join("urlaub.mp4").to_string_lossy().to_string();
        std::fs::write(&video, b"x").unwrap();
        let mut report = filetypes::Report::default();
        report.add("media", &video, Some(".mp4"), 1);
        store.mark_scanned(folder, Some(&report));
        LAUNCHED.lock().clear();
        let reveal = |reason: &str, index: i64| c.post(&format!("/api/folders/{folder}/reveal"), json!({"reason": reason, "index": index})).status;
        assert_eq!(reveal("media", 0), StatusCode::OK);
        assert_eq!(*LAUNCHED.lock(), [format!("explorer /select,\"{video}\"")]);
        for (reason, index) in [("media", 1), ("type", 0), ("media", -1)] {
            assert_eq!(reveal(reason, index), StatusCode::NOT_FOUND, "{reason} {index}"); // only what the details name
        }
    }

    #[test]
    fn a_folder_can_be_paused_on_its_own() {
        let c = client();
        let fid = c.b().store.add_folder(&c.dir.to_string_lossy()).unwrap();
        let paused = || c.status()["folders"].as_array().unwrap().iter().map(|f| f["paused"].clone()).collect::<Vec<_>>();
        assert_eq!(c.send(Method::POST, &format!("/api/folders/{fid}/pause"), &[APP], None).status, StatusCode::OK);
        assert_eq!(paused(), [json!(true)]);
        assert_eq!(c.send(Method::POST, &format!("/api/folders/{fid}/resume"), &[APP], None).status, StatusCode::OK);
        assert_eq!(paused(), [json!(false)]);
        assert_eq!(c.send(Method::POST, "/api/folders/999/pause", &[APP], None).status, StatusCode::NOT_FOUND);
    }

    // ---- the map and similar files ---------------------------------------------------------------

    #[test]
    fn an_unchanged_map_is_not_sent_again() {
        let c = client();
        let mut r = c.get("/api/map");
        for _ in 0..100 {
            if r.json()["status"] == "ready" {
                break; // (an empty index: laid out at once)
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            r = c.get("/api/map");
        }
        let tag = r.header("etag").to_string();
        let again = c.send(Method::GET, "/api/map", &[("if-none-match", &tag)], None);
        assert!(again.status == StatusCode::NOT_MODIFIED && again.body.is_empty());
    }

    #[test]
    fn a_search_puts_the_likely_files_first_and_says_how_many() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\data").unwrap();
        let unit = |k: usize| (0..EMBED_DIM).map(|j| if j == k { 1.0 } else { 0.0 }).collect::<Vec<f32>>();
        for (i, text) in ["the rent is due", "a letter from the landlord about the rent", "holiday photos"].iter().enumerate() {
            let path = format!(r"C:\data\f{i}.txt");
            store.save_files(&[entry(folder, &path, "text", "indexed", 1, None)], &[row(folder, &path, 0, "text", text, unit(i))]).unwrap();
        }
        let reply = c.get("/api/search?q=landlord%20rent").json();
        let first = reply["results"][0]["name"].as_str();
        assert_eq!((first, reply["likely"].as_u64()), (Some("f1.txt"), Some(1))); // (all words; the model asleep)
    }

    #[test]
    fn similar_files_work_while_the_model_sleeps() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\data").unwrap();
        for i in 0..3 {
            let mut v: Vec<f32> = (0..EMBED_DIM).map(|k| ((k * (i + 3)) % 17) as f32 - 8.0).collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            let path = format!(r"C:\data\f{i}.txt");
            store.save_files(&[entry(folder, &path, "text", "indexed", 1, None)], &[row(folder, &path, 0, "text", "t", v)]).unwrap();
        }
        assert!(!c.b().embedder.ready());
        let r = c.get(&format!("/api/similar/{}", store.signature(r"C:\data\f0.txt").unwrap().0));
        assert!(r.status == StatusCode::OK && r.json()["results"].as_array().unwrap().len() == 2);
    }

    #[test]
    fn the_map_card_gets_a_fingerprint_and_a_preview() {
        let c = client();
        let store = &c.b().store;
        let folder = store.add_folder(r"C:\data").unwrap();
        let save = |path: &str, kind: &str, texts: &[&str]| {
            let mut e = entry(folder, path, kind, "indexed", texts.len() as i64, None);
            (e.size, e.mtime) = (7, 5.5);
            let rows: Vec<VectorRow> =
                texts.iter().enumerate().map(|(i, t)| row(folder, path, i as i64, kind, t, vec![i as f32 + 1.0; EMBED_DIM])).collect();
            store.save_files(&[e], &rows).unwrap();
        };
        save(r"C:\data\notes.txt", "text", &["first passage", "second passage"]);
        save(r"C:\data\scan.PDF", "document", &["page one"]);
        save(r"C:\data\IMG_1.jpg", "image", &[""]);
        let ids: HashMap<String, i64> =
            store.file_signatures(folder).into_iter().map(|(p, sig)| (p.rsplit('\\').next().unwrap().to_string(), sig.0)).collect();

        let notes = c.get(&format!("/api/map/vector/{}", ids["notes.txt"])).json();
        let v: Vec<f64> = notes["vector"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert_eq!(v.len(), EMBED_DIM);
        assert!((v.iter().map(|x| x * x).sum::<f64>().sqrt() - 1.0).abs() < 1e-3); // the mean of its passages, normalized
        assert_eq!(notes["file"], json!({"mtime": 5.5, "size": 7, "kind": "text", "preview": false, "passage": "first passage"}));

        assert_eq!(c.get(&format!("/api/map/vector/{}", ids["scan.PDF"])).json()["file"]["preview"], true);
        let photo = c.get(&format!("/api/map/vector/{}", ids["IMG_1.jpg"])).json()["file"].clone();
        assert!(photo["preview"] == true && photo["passage"] == "");
        assert_eq!(c.get("/api/map/vector/999").status, StatusCode::NOT_FOUND);
    }
}
