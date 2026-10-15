//! Updates from the project's GitHub releases.
//!
//! The app asks api.github.com for its releases a minute after it starts and then once a day: one
//! anonymous request that says nothing about the PC or its files. A newer release shows a badge
//! and what's new in every version since this one. Installing (a click, or by itself while the
//! app isn't in use when "Install updates automatically" is on): the release's zip is downloaded,
//! checked against the SHA-256 GitHub lists for it, and unpacked into the data folder; this app
//! quits, the new one replaces its files, starts in the tray, and puts the old files back if
//! anything fails. Afterwards the badge shows what's new until it has been looked at.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::config;

pub const REPO_URL: &str = "https://github.com/catdrinkmonster/quiekel-embed";
const API: &str = "https://api.github.com/repos/catdrinkmonster/quiekel-embed/releases?per_page=30";
const ASSET_END: &str = "-windows-x64.zip";
const EXE: &str = "quiekel-embed.exe";
const FIRST_CHECK_AFTER_S: u64 = 60;
const CHECK_EVERY_S: u64 = 24 * 3600;
const MAX_ZIP: u64 = 800 << 20;
const IDLE_BEFORE_INSTALL_S: u64 = 30 * 60; // installing by itself: the window closed this long
const STARTED_WITHIN_S: u64 = 90; // the new version must be up by then, or the old one comes back
const HELPER_WITHIN_S: u64 = 20; // the new version's helper must run by then, or this app stays
// Marks in the app's folder while an update hands over: the update waits (UPDATING), the new
// version came up (STARTED); in the unpacked release: its helper runs (HELPER_UP).
const UPDATING: &str = ".updating";
const STARTED: &str = ".started";
const HELPER_UP: &str = ".helper-up";

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let t = text.trim();
    let t = t.strip_prefix('v').unwrap_or(t);
    let mut parts = t.split('.');
    let v = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    if parts.next().is_some() {
        return None;
    }
    Some(v)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!((parse_version(candidate), parse_version(current)), (Some(n), Some(c)) if n > c)
}

fn valid_tag(tag: &str) -> bool {
    tag.starts_with('v') && parse_version(tag).is_some() && tag[1..].chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Where releases come from: GitHub, or (for trying updates on this PC) a server on this PC.
#[derive(Clone)]
pub struct Source {
    pub api: String,
    pub pages: String,     // a release's page starts so
    pub downloads: String, // and its files
}

impl Source {
    pub fn github() -> Source {
        Source {
            api: API.into(),
            pages: format!("{REPO_URL}/releases/"),
            downloads: format!("{REPO_URL}/releases/download/"),
        }
    }

    /// QUIEKEL_EMBED_UPDATES=http://127.0.0.1:<port> serves releases from this PC (only there).
    pub fn from_env() -> Source {
        match std::env::var("QUIEKEL_EMBED_UPDATES") {
            Ok(base) if base.starts_with("http://127.0.0.1:") && !base.contains('@') => {
                let base = base.trim_end_matches('/');
                Source { api: format!("{base}/releases"), pages: format!("{base}/"), downloads: format!("{base}/download/") }
            }
            _ => Source::github(),
        }
    }
}

/// A newer release, as the page shows it, with the file that installs it (if it has one).
pub fn parse_release(data: &Value, current: &str, src: &Source) -> Option<Value> {
    let tag = data["tag_name"].as_str().unwrap_or("");
    let url = data["html_url"].as_str().unwrap_or("");
    if data["draft"].as_bool().unwrap_or(false)
        || data["prerelease"].as_bool().unwrap_or(false)
        || !valid_tag(tag)
        || !url.starts_with(&src.pages)
        || !is_newer(tag, current)
    {
        return None;
    }
    let cut = |v: &Value, n: usize| v.as_str().unwrap_or("").chars().take(n).collect::<String>();
    let title = data["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(tag).chars().take(120).collect::<String>();
    let asset = data["assets"].as_array().and_then(|assets| {
        assets.iter().find_map(|a| {
            let name = a["name"].as_str()?;
            let link = a["browser_download_url"].as_str()?;
            let sha256 = a["digest"].as_str()?.strip_prefix("sha256:")?.to_ascii_lowercase();
            let size = a["size"].as_u64()?;
            let ok = name.ends_with(ASSET_END)
                && link.starts_with(&format!("{}{tag}/", src.downloads))
                && sha256.len() == 64
                && sha256.chars().all(|c| c.is_ascii_hexdigit())
                && size > 0
                && size <= MAX_ZIP;
            ok.then(|| json!({"url": link, "size": size, "sha256": sha256}))
        })
    });
    Some(json!({
        "version": &tag[1..], "tag": tag, "title": title,
        "notes": cut(&data["body"], 8000), "published": cut(&data["published_at"], 10), "url": url,
        "asset": asset,
    }))
}

/// The newer releases in a list from the API, newest first.
pub fn newer_releases(list: &Value, current: &str, src: &Source) -> Vec<Value> {
    let mut out: Vec<Value> = list.as_array().into_iter().flatten().filter_map(|r| parse_release(r, current, src)).collect();
    out.sort_by(|a, b| {
        parse_version(b["version"].as_str().unwrap_or("")).cmp(&parse_version(a["version"].as_str().unwrap_or("")))
    });
    out.dedup_by(|a, b| a["version"] == b["version"]);
    out
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn update_dir() -> PathBuf {
    config::DATA_DIR.join("update")
}

/// Where the app runs from, if it can replace itself there (not a build while developing).
pub fn install_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    let s = dir.to_string_lossy().to_lowercase();
    if s.contains("\\target\\") || dir.join("Cargo.toml").exists() || dir.parent().is_some_and(|p| p.join("Cargo.toml").exists()) {
        return None;
    }
    Some(dir)
}

/// What the app needs from the window, to update without getting in the way.
pub struct Hooks {
    pub quit: Box<dyn Fn() + Send + Sync>,
    pub hidden_for: Box<dyn Fn() -> Option<Duration> + Send + Sync>, // the window closed this long
}

pub struct Updater {
    pub latest: Mutex<Option<Value>>,
    releases: Mutex<Vec<Value>>,
    last_check: Mutex<Option<f64>>,
    error: Mutex<Option<Value>>, // {"key", "params"}: the page translates it
    stage: Mutex<Value>,         // {"state": idle | downloading | ready | installing | failed, ...}
    lock: Mutex<()>,
    source: Source,
    auto: Box<dyn Fn() -> bool + Send + Sync>,
    pub hooks: Mutex<Option<Hooks>>,
    pub on_found: Mutex<Option<Box<dyn Fn(&Value) + Send + Sync>>>,
}

impl Updater {
    /// `auto`: the setting "Install updates automatically". `checks`: ask GitHub at all (off in tests).
    pub fn new(auto: Box<dyn Fn() -> bool + Send + Sync>, checks: bool) -> Arc<Updater> {
        let u = Arc::new(Updater {
            latest: Mutex::new(None),
            releases: Mutex::new(Vec::new()),
            last_check: Mutex::new(None),
            error: Mutex::new(None),
            stage: Mutex::new(json!({"state": "idle"})),
            lock: Mutex::new(()),
            source: Source::from_env(),
            auto,
            hooks: Mutex::new(None),
            on_found: Mutex::new(None),
        });
        if checks {
            let me = u.clone();
            std::thread::Builder::new()
                .name("updater".into())
                .spawn(move || {
                    me.tidy();
                    std::thread::sleep(Duration::from_secs(FIRST_CHECK_AFTER_S));
                    let mut last = Instant::now() - Duration::from_secs(CHECK_EVERY_S);
                    loop {
                        if last.elapsed().as_secs() >= CHECK_EVERY_S {
                            me.check();
                            last = Instant::now();
                        }
                        me.install_by_itself();
                        std::thread::sleep(Duration::from_secs(60));
                    }
                })
                .ok();
        }
        u
    }

    pub fn can_apply(&self) -> bool {
        install_dir().is_some() && self.hooks.lock().is_some()
    }

    pub fn status(&self) -> Value {
        json!({
            "current": current_version(), "latest": *self.latest.lock(), "releases": *self.releases.lock(),
            "last_check": *self.last_check.lock(), "error": *self.error.lock(), "stage": *self.stage.lock(),
            "can_apply": self.can_apply(), "whats_new": whats_new(), "last_result": last_result(),
            "repo_url": REPO_URL,
        })
    }

    pub fn check(&self) {
        let _one = self.lock.lock();
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).build().into();
        let reply = agent
            .get(&self.source.api)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", &format!("quiekel-embed/{}", current_version()))
            .call();
        let mut data = None;
        match reply {
            Ok(mut r) => {
                *self.error.lock() = None;
                data = r.body_mut().with_config().limit(4_000_000).read_json::<Value>().ok();
            }
            Err(ureq::Error::StatusCode(404)) => *self.error.lock() = None, // no release yet
            Err(ureq::Error::StatusCode(code)) => {
                *self.error.lock() = Some(json!({"key": "upd.http_error", "params": {"status": code}}));
            }
            Err(e) => {
                *self.error.lock() = Some(json!({"key": "upd.unreachable", "params": {}}));
                log::info!("Update check failed: {e}");
            }
        }
        *self.last_check.lock() = Some(now());
        if self.error.lock().is_some() {
            return;
        }
        let releases = data.as_ref().map(|d| newer_releases(d, current_version(), &self.source)).unwrap_or_default();
        let found = releases.first().cloned();
        {
            let latest = self.latest.lock();
            if let Some(f) = &found {
                if latest.as_ref().is_none_or(|l| l["tag"] != f["tag"]) {
                    log::info!("Update available: {}", f["tag"]);
                    if let Some(cb) = self.on_found.lock().as_ref() {
                        cb(f);
                    }
                }
            }
        }
        *self.latest.lock() = found;
        *self.releases.lock() = releases;
    }

    fn set_stage(&self, v: Value) {
        *self.stage.lock() = v;
    }

    /// Download, check and unpack the newest release: the folder with its files.
    pub fn prepare(&self) -> Result<(String, PathBuf)> {
        let latest = self.latest.lock().clone().context("no newer release")?;
        let version = latest["version"].as_str().context("version")?.to_string();
        let asset = latest["asset"].clone();
        let (url, size, sha256) = (
            asset["url"].as_str().context("this release has no file for Windows")?.to_string(),
            asset["size"].as_u64().context("size")?,
            asset["sha256"].as_str().context("checksum")?.to_string(),
        );
        let dir = update_dir();
        let unpacked = dir.join(&version);
        if unpacked.join(".complete").exists() {
            return Ok((version, unpacked));
        }
        std::fs::create_dir_all(&dir)?;
        let zip_path = dir.join(format!("{version}.zip"));
        self.set_stage(json!({"state": "downloading", "version": version, "done": 0, "total": size}));
        download(&url, &zip_path, size, &sha256, &|done| {
            self.set_stage(json!({"state": "downloading", "version": version, "done": done, "total": size}));
        })?;
        unpack(&zip_path, &unpacked)?;
        let _ = std::fs::remove_file(&zip_path);
        std::fs::write(unpacked.join(".complete"), b"")?;
        // What's new, for the new version to show without asking GitHub again.
        let notes: Vec<Value> = self.releases.lock().clone();
        std::fs::write(dir.join("notes.json"), serde_json::to_vec(&json!({"from": current_version(), "to": version, "releases": notes}))?)?;
        self.set_stage(json!({"state": "ready", "version": version}));
        Ok((version, unpacked))
    }

    /// Hand over to the new version: it waits for this app to quit, then replaces its files.
    pub fn install(&self, version: &str, unpacked: &Path) -> Result<()> {
        let install = install_dir().context("this copy can't replace itself")?;
        let hooks = self.hooks.lock();
        let hooks = hooks.as_ref().context("only the desktop app can install updates")?;
        let helper = unpacked.join(EXE);
        anyhow::ensure!(helper.is_file(), "the release has no {EXE}");
        self.set_stage(json!({"state": "installing", "version": version}));
        log::info!("Installing {version}");
        use std::os::windows::process::CommandExt;
        let up = unpacked.join(HELPER_UP);
        let _ = std::fs::remove_file(&up);
        let mut helper = std::process::Command::new(&helper)
            .arg("--finish-update")
            .arg(unpacked)
            .arg(&install)
            .arg(std::process::id().to_string())
            .arg(current_version())
            .current_dir(unpacked)
            .creation_flags(0x0800_0000 | 0x0000_0200) // no window, a process group of its own
            .spawn()
            .context("starting the new version")?;
        // This app quits only once the new version's helper runs: a broken download mustn't leave
        // the app closed.
        let end = Instant::now() + Duration::from_secs(HELPER_WITHIN_S);
        while !up.exists() {
            if let Ok(Some(status)) = helper.try_wait() {
                bail!("the new version ended at once ({status})");
            }
            if Instant::now() > end {
                let _ = helper.kill();
                bail!("the new version didn't start");
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        (hooks.quit)();
        Ok(())
    }

    /// Download and install the newest release (the page's Update button).
    pub fn apply(self: &Arc<Self>) -> Result<()> {
        anyhow::ensure!(self.can_apply(), "only the installed desktop app can update itself");
        let me = self.clone();
        std::thread::Builder::new().name("update".into()).spawn(move || {
            let r = me.prepare().and_then(|(v, dir)| me.install(&v, &dir));
            if let Err(e) = r {
                log::error!("Update failed: {e:#}");
                me.set_stage(json!({"state": "failed", "error": format!("{e:#}")}));
            }
        })?;
        Ok(())
    }

    /// "Install updates automatically": download as soon as there's a release; install once the
    /// window has been closed a while (the app is in the tray, not in use).
    fn install_by_itself(&self) {
        if !(self.auto)() || !self.can_apply() || self.latest.lock().is_none() {
            return;
        }
        let ready = match self.prepare() {
            Ok(r) => r,
            Err(e) => {
                log::info!("Update not downloaded: {e:#}");
                return;
            }
        };
        let idle = self.hooks.lock().as_ref().and_then(|h| (h.hidden_for)()).is_some_and(|d| d.as_secs() >= IDLE_BEFORE_INSTALL_S);
        if idle {
            if let Err(e) = self.install(&ready.0, &ready.1) {
                log::error!("Update failed: {e:#}");
                self.set_stage(json!({"state": "failed", "error": format!("{e:#}")}));
            }
        }
    }

    /// Leftovers of earlier updates: unpacked versions not newer than this one, old files.
    fn tidy(&self) {
        if let Ok(entries) = std::fs::read_dir(update_dir()) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if e.path().is_dir() && !is_newer(&name, current_version()) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        if let Some(dir) = install_dir() {
            tidy_install(&dir);
        }
    }
}

/// The old version's files and the marks in the app's folder go, unless an update is handing
/// over right now (the old files are its way back: it removes them itself once this version runs).
fn tidy_install(dir: &Path) {
    let updating = dir.join(UPDATING);
    let fresh = std::fs::metadata(&updating)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() < 10 * 60));
    if fresh {
        return;
    }
    let _ = std::fs::remove_file(updating); // (one left over from an update that broke off)
    let _ = std::fs::remove_file(dir.join(STARTED));
    let _ = std::fs::remove_file(dir.join(HELPER_UP)); // (copied along by an earlier version)
    remove_old(dir);
}

/// The releases this version brought, until the page has shown them (it saves "seen_version").
pub fn whats_new() -> Value {
    let Ok(raw) = std::fs::read(update_dir().join("notes.json")) else { return Value::Null };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else { return Value::Null };
    if v["to"].as_str() != Some(current_version()) {
        return Value::Null;
    }
    v
}

/// How the last update went, once: {"ok", "version", "error"}.
fn last_result() -> Value {
    std::fs::read(update_dir().join("result.json")).ok().and_then(|r| serde_json::from_slice(&r).ok()).unwrap_or(Value::Null)
}

fn write_result(v: Value) {
    let _ = std::fs::create_dir_all(update_dir());
    let _ = std::fs::write(update_dir().join("result.json"), v.to_string());
}

/// Streams the file to `target`, refusing more than `size` bytes or a different SHA-256.
fn download(url: &str, target: &Path, size: u64, sha256: &str, progress: &dyn Fn(u64)) -> Result<()> {
    use sha2::Digest;
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(1800))).build().into();
    let mut r = agent.get(url).header("User-Agent", &format!("quiekel-embed/{}", current_version())).call()?;
    let part = target.with_extension("part");
    let mut out = std::fs::File::create(&part)?;
    let mut reader = r.body_mut().with_config().limit(size + 1).reader();
    let (mut hash, mut done, mut buf) = (sha2::Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > size {
            bail!("the download is bigger than the release says");
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        if last.elapsed() > Duration::from_millis(300) {
            progress(done);
            last = Instant::now();
        }
    }
    out.sync_all()?;
    drop(out);
    let got: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != size || got != sha256 {
        let _ = std::fs::remove_file(&part);
        bail!("the download doesn't match the release (size or checksum)");
    }
    std::fs::rename(&part, target)?;
    Ok(())
}

/// Unpacks the release's zip into `dir` (nothing outside it); it must hold the app.
pub fn unpack(zip_path: &Path, dir: &Path) -> Result<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { bail!("a file in the release points outside it") };
        let path = dir.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut (&mut f).take(MAX_ZIP), &mut std::fs::File::create(&path)?)?;
    }
    anyhow::ensure!(dir.join(EXE).is_file(), "the release has no {EXE}");
    Ok(())
}

const OLD: &str = ".old-version";

/// Every file below `dir` (not the unpacked updates' own folders).
fn files_below(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

fn remove_old(dir: &Path) {
    for p in files_below(dir) {
        if p.to_string_lossy().ends_with(OLD) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Replaces the app's files in `install` with the ones in `new`: the old ones are renamed first
/// (so they can come back) and only removed once the new version runs.
pub fn swap(new: &Path, install: &Path) -> Result<()> {
    let old: Vec<PathBuf> = files_below(install).into_iter().filter(|p| !p.to_string_lossy().ends_with(OLD)).collect();
    let mut renamed: Vec<PathBuf> = Vec::new();
    let mut copied: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<()> {
        for p in &old {
            let to = PathBuf::from(format!("{}{OLD}", p.display()));
            let _ = std::fs::remove_file(&to);
            rename_retrying(p, &to)?;
            renamed.push(p.clone());
        }
        for p in files_below(new) {
            let rel = p.strip_prefix(new)?;
            if [".complete", HELPER_UP].contains(&rel.to_string_lossy().as_ref()) {
                continue; // (the unpacked release's own marks)
            }
            let to = install.join(rel);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&p, &to).with_context(|| format!("copying {}", rel.display()))?;
            copied.push(to);
        }
        Ok(())
    })();
    if let Err(e) = result {
        undo(&copied, &renamed);
        return Err(e);
    }
    Ok(())
}

fn undo(copied: &[PathBuf], renamed: &[PathBuf]) {
    for p in copied {
        let _ = std::fs::remove_file(p);
    }
    for p in renamed {
        let _ = std::fs::rename(format!("{}{OLD}", p.display()), p);
    }
}

/// Puts the old version back after a failed start.
pub fn restore(install: &Path) {
    let old: Vec<PathBuf> = files_below(install).into_iter().filter(|p| p.to_string_lossy().ends_with(OLD)).collect();
    let originals: Vec<PathBuf> = old.iter().map(|p| PathBuf::from(p.to_string_lossy().trim_end_matches(OLD))).collect();
    for p in files_below(install) {
        if !p.to_string_lossy().ends_with(OLD) {
            let _ = std::fs::remove_file(&p);
        }
    }
    for (from, to) in old.iter().zip(&originals) {
        let _ = std::fs::rename(from, to);
    }
}

/// Windows keeps a file busy a moment after its process ended (or a virus scan reads it).
fn rename_retrying(from: &Path, to: &Path) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() < end => {
                log::info!("Waiting to replace {}: {e}", from.display());
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => return Err(e).with_context(|| format!("replacing {}", from.display())),
        }
    }
}

fn wait_for_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, timeout.as_millis() as u32);
            let _ = CloseHandle(h);
        }
    }
}

/// The new version, run from where it was unpacked: replaces the old files once the old app has
/// quit, starts the new app in the tray and checks it comes up; else puts the old one back.
pub fn finish(args: &[String]) -> Result<()> {
    let [unpacked, install, pid, from] = args else { bail!("usage: --finish-update <unpacked> <install> <pid> <from>") };
    let (unpacked, install) = (PathBuf::from(unpacked), PathBuf::from(install));
    let _ = std::fs::write(unpacked.join(HELPER_UP), b""); // (the old app quits once it sees this)
    let to = current_version();
    wait_for_exit(pid.parse().unwrap_or(0), Duration::from_secs(60));
    let (started, updating) = (install.join(STARTED), install.join(UPDATING));
    let _ = std::fs::remove_file(&started);
    if let Err(e) = swap(&unpacked, &install) {
        write_result(json!({"ok": false, "version": to, "error": format!("{e:#}")}));
        start_app(&install);
        return Err(e);
    }
    let _ = std::fs::write(&updating, b"");
    start_app(&install);
    let end = Instant::now() + Duration::from_secs(STARTED_WITHIN_S);
    while Instant::now() < end {
        if started.exists() {
            let _ = std::fs::remove_file(&started);
            let _ = std::fs::remove_file(&updating);
            remove_old(&install);
            write_result(json!({"ok": true, "version": to, "from": from}));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // The new version didn't come up: the old one comes back.
    log::error!("Version {to} did not start; going back to {from}");
    kill_app(&install);
    restore(&install); // (the marks go with the new files)
    write_result(json!({"ok": false, "version": to, "error": "the new version did not start"}));
    start_app(&install);
    Ok(())
}

/// The app started fine: tells an update waiting for it so.
pub fn started() {
    if let Some(dir) = install_dir() {
        mark_started(&dir);
    }
}

fn mark_started(dir: &Path) {
    if dir.join(UPDATING).exists() {
        let _ = std::fs::write(dir.join(STARTED), b"");
    }
}

/// The page has shown how the last update went.
pub fn forget_result() {
    let _ = std::fs::remove_file(update_dir().join("result.json"));
}

fn start_app(install: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new(install.join(EXE))
        .arg("--hidden")
        .current_dir(install)
        .creation_flags(0x0000_0200)
        .spawn();
}

/// Ends the app and its model process: every process run from `install` (never this one, which
/// runs from where the update was unpacked).
fn kill_app(install: &Path) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
    };
    let dir = install.to_string_lossy().to_lowercase();
    let me = std::process::id();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            if e.th32ProcessID != me {
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, false, e.th32ProcessID) {
                    let mut buf = [0u16; 1024];
                    let mut len = buf.len() as u32;
                    if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok() {
                        let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                        if path.starts_with(&format!("{dir}\\")) {
                            let _ = TerminateProcess(h, 1);
                        }
                    }
                    let _ = CloseHandle(h);
                }
            }
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    std::thread::sleep(Duration::from_secs(2));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src() -> Source {
        Source::github()
    }

    fn release(tag: &str, with_asset: bool) -> Value {
        let mut r = json!({"tag_name": tag, "html_url": format!("{REPO_URL}/releases/tag/{tag}"), "name": "", "body": format!("notes {tag}")});
        if with_asset {
            r["assets"] = json!([{
                "name": format!("Quiekel-Embed-{}{ASSET_END}", &tag[1..]),
                "browser_download_url": format!("{REPO_URL}/releases/download/{tag}/Quiekel-Embed-{}{ASSET_END}", &tag[1..]),
                "size": 1234, "digest": format!("sha256:{}", "ab".repeat(32)),
            }]);
        }
        r
    }

    #[test]
    fn releases() {
        assert!(is_newer("v0.13.0", "0.12.0"));
        assert!(!is_newer("v0.12.0", "0.12.0"));
        assert_eq!(parse_release(&release("v9.0.0", false), "0.12.0", &src()).unwrap()["version"], "9.0.0");
        let bad = json!({"tag_name": "v9.0.0-rc1", "html_url": format!("{REPO_URL}/releases/x")});
        assert!(parse_release(&bad, "0.12.0", &src()).is_none());
        let elsewhere = json!({"tag_name": "v9.0.0", "html_url": "https://example.org/releases/v9.0.0"});
        assert!(parse_release(&elsewhere, "0.12.0", &src()).is_none());
    }

    #[test]
    fn every_newer_release_is_listed_newest_first_with_its_file() {
        let list = json!([release("v1.1.0", true), release("v1.3.0", true), release("v0.9.0", true), release("v1.2.0", false)]);
        let found = newer_releases(&list, "1.0.0", &src());
        let versions: Vec<&str> = found.iter().map(|r| r["version"].as_str().unwrap()).collect();
        assert_eq!(versions, ["1.3.0", "1.2.0", "1.1.0"]);
        assert_eq!(found[0]["asset"]["sha256"], "ab".repeat(32));
        assert!(found[1]["asset"].is_null()); // (no file for Windows: notes only)
        // A file from somewhere else, or without a checksum, isn't one to install.
        let mut odd = release("v2.0.0", true);
        odd["assets"][0]["browser_download_url"] = json!("https://example.org/x-windows-x64.zip");
        assert!(parse_release(&odd, "1.0.0", &src()).unwrap()["asset"].is_null());
        let mut unsigned = release("v2.0.0", true);
        unsigned["assets"][0]["digest"] = Value::Null;
        assert!(parse_release(&unsigned, "1.0.0", &src()).unwrap()["asset"].is_null());
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("quiekel-rs-upd-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn a_release_unpacks_only_into_its_folder_and_must_hold_the_app() {
        let d = temp("unpack");
        let good = d.join("good.zip");
        write_zip(&good, &[(EXE, b"new app"), ("onnxruntime.dll", b"lib"), ("sub/x.dll", b"x")]);
        unpack(&good, &d.join("good")).unwrap();
        assert!(d.join("good").join("sub").join("x.dll").is_file());
        let evil = d.join("evil.zip");
        write_zip(&evil, &[(EXE, b"app"), ("../../outside.txt", b"no")]);
        assert!(unpack(&evil, &d.join("evil")).is_err());
        assert!(!d.join("outside.txt").exists());
        let empty = d.join("empty.zip");
        write_zip(&empty, &[("readme.txt", b"no app")]);
        assert!(unpack(&empty, &d.join("empty")).is_err());
    }

    #[test]
    fn the_new_files_replace_the_old_ones_and_the_old_ones_can_come_back() {
        let d = temp("swap");
        let (install, new) = (d.join("install"), d.join("new"));
        std::fs::create_dir_all(install.join("sub")).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(install.join(EXE), b"old app").unwrap();
        std::fs::write(install.join("gone.dll"), b"only in the old version").unwrap();
        std::fs::write(install.join("sub").join("a.dll"), b"old a").unwrap();
        std::fs::write(new.join(EXE), b"new app").unwrap();
        std::fs::write(new.join("added.dll"), b"new").unwrap();
        std::fs::write(new.join(".complete"), b"").unwrap();
        std::fs::write(new.join(HELPER_UP), b"").unwrap();
        swap(&new, &install).unwrap();
        assert_eq!(std::fs::read(install.join(EXE)).unwrap(), b"new app");
        assert!(install.join("added.dll").is_file() && !install.join("gone.dll").exists());
        assert!(!install.join(".complete").exists() && !install.join(HELPER_UP).exists()); // (marks stay behind)
        // The new version didn't start: everything as it was.
        restore(&install);
        assert_eq!(std::fs::read(install.join(EXE)).unwrap(), b"old app");
        assert!(install.join("gone.dll").is_file() && !install.join("added.dll").exists());
        assert_eq!(std::fs::read(install.join("sub").join("a.dll")).unwrap(), b"old a");
        // It started: the old files go.
        swap(&new, &install).unwrap();
        remove_old(&install);
        assert!(files_below(&install).iter().all(|p| !p.to_string_lossy().ends_with(OLD)));
    }

    #[test]
    fn the_handover_marks_and_the_old_files_last_as_long_as_an_update_waits() {
        let d = temp("marks");
        std::fs::write(d.join(EXE), b"new").unwrap();
        std::fs::write(d.join(format!("{EXE}{OLD}")), b"old").unwrap();
        // An ordinary start: no update waits for it, so no mark, and leftovers go.
        mark_started(&d);
        assert!(!d.join(STARTED).exists());
        tidy_install(&d);
        assert!(!d.join(format!("{EXE}{OLD}")).exists());
        // While an update waits: the mark, and the old files stay (they're its way back).
        std::fs::write(d.join(format!("{EXE}{OLD}")), b"old").unwrap();
        std::fs::write(d.join(UPDATING), b"").unwrap();
        mark_started(&d);
        tidy_install(&d);
        assert!(d.join(STARTED).exists() && d.join(format!("{EXE}{OLD}")).exists());
        // An update that broke off long ago doesn't hold them for good.
        let stale = std::time::SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options().write(true).open(d.join(UPDATING)).unwrap().set_modified(stale).unwrap();
        tidy_install(&d);
        assert!(!d.join(UPDATING).exists() && !d.join(STARTED).exists() && !d.join(format!("{EXE}{OLD}")).exists());
        assert_eq!(std::fs::read(d.join(EXE)).unwrap(), b"new");
    }

    #[test]
    fn a_download_must_match_its_checksum_and_size() {
        use sha2::Digest;
        let d = temp("download");
        let body = b"the release zip".to_vec();
        let sha: String = sha2::Sha256::digest(&body).iter().map(|b| format!("{b:02x}")).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = body.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut s = stream.unwrap();
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len());
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(&served).unwrap();
            }
        });
        let url = format!("http://127.0.0.1:{port}/download/v9.0.0/x-windows-x64.zip");
        download(&url, &d.join("ok.zip"), body.len() as u64, &sha, &|_| {}).unwrap();
        assert_eq!(std::fs::read(d.join("ok.zip")).unwrap(), body);
        assert!(download(&url, &d.join("bad.zip"), body.len() as u64, &"00".repeat(32), &|_| {}).is_err());
        assert!(!d.join("bad.zip").exists() && !d.join("bad.part").exists());
    }
}
