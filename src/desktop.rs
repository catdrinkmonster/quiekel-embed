//! Desktop app: a native window (Edge WebView2, built into Windows) plus a tray icon.
//!
//! Closing the window only hides it: indexing and the folder watcher keep running in the tray
//! until you choose Quit. Launching the app again brings the existing window back.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use base64_lite::encode as b64;
use parking_lot::Mutex;
use serde_json::Value;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::config;
use crate::i18n;
use crate::server::{self, Backend, Hooks};

/// Warm paper, like the page, and its dark version: background, text, muted text.
fn colors(dark: bool) -> (&'static str, &'static str, &'static str) {
    if dark { ("#151618", "#f2f2ee", "#9b9d95") } else { ("#eeefe9", "#151515", "#6b6d65") }
}


pub fn title() -> String {
    if config::COPY_NAME.is_empty() { "Quiekel Embed".into() } else { format!("Quiekel Embed [{}]", *config::COPY_NAME) }
}

#[derive(Debug, Clone)]
enum UserEvent {
    Progress(f64),
    LoadUrl(String),
    LoadHtml(String),
    Show,
    SetDark(bool),
    Quit,
}

/// The base64 of bytes (for the splash's pictures).
mod base64_lite {
    pub fn encode(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for c in data.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        out
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#x27;")
}

/// What the window shows until the app is there: the pig and a ring that fills up as the app
/// starts (red to green; progress() moves it), or what went wrong.
fn page(message: &str, detail: &str, dark: bool) -> String {
    let pig = b64(server::static_file(if dark { "pig-dark.svg" } else { "pig.svg" }).unwrap_or_default());
    let (bg, fg, muted) = colors(dark);
    let (track, sat, light) = if dark { ("#2b2c30", 62, 56) } else { ("#d0d1c9", 72, 44) };
    let ring = r#"<svg class="ring" viewBox="0 0 20 20" aria-hidden="true"><circle class="t" cx="10" cy="10" r="8"/><circle class="a" id="a" cx="10" cy="10" r="8" pathLength="100"/></svg>"#;
    let mut body = if message.is_empty() { ring.to_string() } else { format!("<p>{}</p>", escape(message)) };
    if !detail.is_empty() {
        body += &format!("<p><code>{}</code></p>", escape(detail));
    }
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{{margin:0;height:100%;background:{bg};color:{fg};font:14px "Segoe UI Variable Text","Segoe UI",system-ui,sans-serif}}
body{{display:grid;place-items:center}} .box{{display:grid;justify-items:center;gap:20px;max-width:560px;padding:24px;text-align:center}}
img{{width:88px;height:auto}} p{{margin:0;font-weight:600}} code{{font:12px "Cascadia Mono",Consolas,monospace;color:{muted}}}
.ring{{width:40px;height:40px;transform:rotate(-90deg)}} .ring circle{{fill:none;stroke-width:2.8}} .t{{stroke:{track}}}
.a{{stroke-linecap:round;stroke-dasharray:4 100;stroke:hsl(4 {sat}% {light}%);transition:stroke-dasharray .4s ease,stroke .4s ease}}
</style><script>
function progress(share) {{
  const a = document.getElementById("a");
  if (!a) return;
  share = Math.max(.04, Math.min(1, share));
  a.style.strokeDasharray = (share * 100).toFixed(1) + " 100";
  a.style.stroke = "hsl(" + Math.round(share * 120) + " {sat}% {light}%)";
}}
</script></head><body><div class="box"><img src="data:image/svg+xml;base64,{pig}" alt="">{body}</div></body></html>"#
    )
}

/// Title bar in the app's colours on Windows 11 (silently ignored on older versions).
fn style_title_bar(hwnd: isize, dark: bool) {
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
    // COLORREF is 0x00BBGGRR: caption like the sidebar, text, and the window border.
    let (caption, text, border): (u32, u32, u32) =
        if dark { (0x001F1C1B, 0x00EEF2F2, 0x00363230) } else { (0x00E0E7E5, 0x00151515, 0x00C9D1D0) };
    let hwnd = HWND(hwnd as *mut _);
    unsafe {
        for (attr, value) in [(20u32, dark as u32), (35, caption), (36, text), (34, border)] {
            let v = value;
            let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attr as i32), &v as *const u32 as *const _, 4);
        }
        // Redraw the frame now.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
    }
}

/// The theme to open in: the saved setting, or the Windows app theme for "auto".
fn starts_dark() -> bool {
    let theme = rusqlite::Connection::open_with_flags(config::db_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()
        .and_then(|db| db.query_row("SELECT value FROM meta WHERE key = 'setting.theme'", [], |r| r.get::<_, String>(0)).ok())
        .and_then(|v| serde_json::from_str::<String>(&v).ok())
        .unwrap_or_else(|| "auto".into());
    if theme == "light" || theme == "dark" {
        return theme == "dark";
    }
    apps_use_dark()
}

fn apps_use_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 1u32;
    let mut size = 4u32;
    unsafe {
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        );
        r.is_ok() && value == 0
    }
}

/// The saved language, read straight from the database (the splash shows before the app loads).
fn stored_language() -> String {
    let setting = rusqlite::Connection::open_with_flags(config::db_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .ok()
        .and_then(|db| db.query_row("SELECT value FROM meta WHERE key = 'setting.language'", [], |r| r.get::<_, String>(0)).ok())
        .and_then(|v| serde_json::from_str::<String>(&v).ok())
        .unwrap_or_default();
    i18n::resolve(&setting)
}

static INSTANCE: AtomicIsize = AtomicIsize::new(0); // the mutex below, held for as long as the app runs

/// Whether this is the app's only copy in this Windows session. A copy with a data folder of its
/// own has a mutex of its own.
fn first_copy() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    let mut name = config::APP_ID.to_string();
    if !config::COPY_NAME.is_empty() {
        let digest = sha1_smol::Sha1::from(config::DATA_DIR.to_string_lossy().to_lowercase().as_bytes()).digest().to_string();
        name += &format!(".{}", &digest[..12]);
    }
    unsafe {
        match CreateMutexW(None, false, &HSTRING::from(format!("Local\\{name}"))) {
            Ok(h) => {
                let already = GetLastError() == ERROR_ALREADY_EXISTS;
                INSTANCE.store(h.0 as isize, Ordering::SeqCst);
                !already
            }
            Err(_) => true,
        }
    }
}

fn show_running_instance() -> bool {
    unsafe {
        // Let the running instance take focus: this launch is the user's own action.
        let _ = AllowSetForegroundWindow(u32::MAX);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).build().into();
    agent
        .post(&format!("{}/api/show", server::url()))
        .header("x-quiekel-embed", "1")
        .send_empty()
        .ok()
        .and_then(|mut r| r.body_mut().read_json::<Value>().ok())
        .is_some_and(|v| v["ok"] == true)
}

/// The native folder picker, owned by the window (or free-standing in browser mode).
pub fn pick_folder(owner: isize, title: &str) -> Option<String> {
    let title = title.to_string();
    std::thread::spawn(move || unsafe {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
        use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(&HSTRING::from(title.as_str()));
        let parent = if owner != 0 { Some(HWND(owner as *mut _)) } else { None };
        dialog.Show(parent).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
        path
    })
    .join()
    .ok()
    .flatten()
}

/// A fullscreen app (a game, a film) is in front: it covers its whole screen.
fn fullscreen_in_front() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    unsafe {
        let front = GetForegroundWindow();
        if front.0.is_null() || front == GetShellWindow() || front == GetDesktopWindow() {
            return false;
        }
        let mut r = RECT::default();
        if GetWindowRect(front, &mut r).is_err() {
            return false;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(MonitorFromWindow(front, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            return false;
        }
        let m = info.rcMonitor;
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }
}

// ---- the tray icon -------------------------------------------------------------------------------

const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: u32 = 1;
const ID_UPDATE: u32 = 2;
const ID_PAUSE: u32 = 3;
const ID_AUTOSTART: u32 = 4;
const ID_QUIT: u32 = 5;

struct Tray {
    hwnd: AtomicIsize,
    icon: AtomicIsize,
    backend: OnceLock<Arc<Backend>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    lang: Mutex<String>,
    title: Mutex<String>,
    taskbar_created: u32,
}

static TRAY: OnceLock<Tray> = OnceLock::new();

fn tray() -> &'static Tray {
    TRAY.get().expect("the tray")
}

fn wide<const N: usize>(text: &str) -> [u16; N] {
    let mut out = [0u16; N];
    for (i, c) in text.encode_utf16().take(N - 1).enumerate() {
        out[i] = c;
    }
    out
}

impl Tray {
    /// Translate into the current language (the setting can change while running).
    fn tr(&self, key: &str, params: &[(&str, Value)]) -> String {
        if let Some(b) = self.backend.get() {
            *self.lang.lock() = i18n::resolve(&b.settings.str("language"));
        }
        i18n::t(&self.lang.lock(), key, params)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(self.hwnd.load(Ordering::SeqCst) as *mut _),
            uID: 1,
            ..Default::default()
        }
    }

    fn add(&self) {
        let mut d = self.data();
        d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        d.uCallbackMessage = WM_TRAY;
        d.hIcon = HICON(self.icon.load(Ordering::SeqCst) as *mut _);
        d.szTip = wide::<128>(&self.title.lock());
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
        }
    }

    fn set_title(&self, text: &str) {
        *self.title.lock() = text.chars().take(120).collect();
        let mut d = self.data();
        d.uFlags = NIF_TIP;
        d.szTip = wide::<128>(text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    fn notify(&self, text: &str) {
        if fullscreen_in_front() {
            return; // (nothing pops up over a game or a film; the page still shows it)
        }
        let mut d = self.data();
        d.uFlags = NIF_INFO;
        d.szInfo = wide::<256>(text);
        d.szInfoTitle = wide::<64>(&title());
        d.dwInfoFlags = NIIF_INFO;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }

    fn remove(&self) {
        let d = self.data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }

    fn send(&self, e: UserEvent) {
        let _ = self.proxy.lock().send_event(e);
    }

    fn update_version(&self) -> Option<String> {
        let b = self.backend.get()?;
        let latest = b.updater.latest.lock();
        latest.as_ref().and_then(|l| l["version"].as_str().map(String::from))
    }

    fn menu(&self) {
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        let b = self.backend.get();
        let ready = b.is_some();
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let item = |m: HMENU, id: u32, text: &str, checked: bool, enabled: bool| {
                let mut flags = MF_STRING;
                if checked {
                    flags |= MF_CHECKED;
                }
                if !enabled {
                    flags |= MF_GRAYED;
                }
                let _ = AppendMenuW(m, flags, id as usize, &HSTRING::from(text));
            };
            item(menu, ID_OPEN, &self.tr("tray.open", &[]), false, true);
            if let Some(v) = self.update_version() {
                item(menu, ID_UPDATE, &self.tr("tray.update", &[("version", Value::from(v))]), false, true);
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let paused = b.is_some_and(|b| b.indexer.is_paused());
            item(menu, ID_PAUSE, &self.tr("tray.pause", &[]), paused, ready);
            item(menu, ID_AUTOSTART, &self.tr("tray.autostart", &[]), crate::shortcuts::autostart_enabled(), true);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            item(menu, ID_QUIT, &self.tr("tray.quit", &[]), false, true);
            let _ = SetMenuDefaultItem(menu, ID_OPEN, 0);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let chosen = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY, pt.x, pt.y, Some(0), hwnd, None);
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            self.command(chosen.0 as u32);
        }
    }

    fn command(&self, id: u32) {
        let b = self.backend.get().cloned();
        match id {
            ID_OPEN | ID_UPDATE => self.send(UserEvent::Show),
            ID_PAUSE => {
                if let Some(b) = b {
                    if b.indexer.is_paused() { b.indexer.resume() } else { b.indexer.pause() }
                }
            }
            ID_AUTOSTART => {
                std::thread::spawn(|| {
                    if let Err(e) = crate::shortcuts::set_autostart(!crate::shortcuts::autostart_enabled()) {
                        log::error!("Could not change autostart: {e}");
                    }
                });
            }
            ID_QUIT => self.send(UserEvent::Quit),
            _ => {}
        }
    }

    fn keep_title_fresh(&self) {
        let mut last = String::new();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Some(b) = self.backend.get() else { continue };
            let p = b.indexer.progress();
            let d = b.governor.decision();
            let state = p.get("state").and_then(|v| v.as_str()).unwrap_or("");
            let working = state == "indexing" || state == "scanning";
            let note = p.get("note").filter(|n| n.is_object());
            let params = |m: &serde_json::Map<String, Value>| -> Vec<(String, Value)> { m.iter().map(|(k, v)| (k.clone(), v.clone())).collect() };
            let tr_map = |key: &str, m: &serde_json::Map<String, Value>| {
                let owned = params(m);
                let borrowed: Vec<(&str, Value)> = owned.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                self.tr(key, &borrowed)
            };
            let mut text = if p.get("paused").and_then(|v| v.as_bool()).unwrap_or(false) {
                self.tr("tray.title.paused", &[])
            } else if state == "loading-model" {
                self.tr("tray.title.starting", &[])
            } else if working && (d.duty == 0.0 || note.is_some()) {
                let reason = match note {
                    Some(n) => tr_map(n["key"].as_str().unwrap_or(""), n["params"].as_object().unwrap_or(&serde_json::Map::new())),
                    None => tr_map(&format!("gov.{}", d.code), &d.params),
                };
                self.tr("tray.title.waiting", &[("reason", Value::from(reason))])
            } else if working {
                let folder = p.get("folder").and_then(|v| v.as_str()).unwrap_or("");
                let name = server::split_path(folder).1;
                let total = p.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
                let done = p.get("done").and_then(|v| v.as_i64()).unwrap_or(0);
                let pct = if total > 0 { format!(" {} %", done * 100 / total) } else { String::new() };
                let mut t = self.tr("tray.title.indexing", &[("name", Value::from(name)), ("pct", Value::from(pct))]);
                if d.level == "throttled" {
                    t += &format!(" ({})", tr_map(&format!("gov.{}", d.code), &d.params));
                }
                t
            } else {
                self.tr("tray.title.idle", &[])
            };
            if !config::COPY_NAME.is_empty() {
                text = format!("[{}] {text}", *config::COPY_NAME);
            }
            if text != last {
                self.set_title(&text);
                last = text;
            }
        }
    }

    /// The first time the window is closed, explain where the app went.
    fn hint_once(&self) {
        let flag = config::DATA_DIR.join(".tray-hint-shown");
        if flag.exists() {
            return;
        }
        self.notify(&self.tr("tray.hint", &[]));
        let _ = std::fs::write(flag, b"");
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(t) = TRAY.get() {
        if msg == WM_TRAY {
            match lparam.0 as u32 {
                WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU => t.menu(), // (Open is its first item)
                _ => {}
            }
            return LRESULT(0);
        }
        if msg == t.taskbar_created && msg != 0 {
            t.add(); // Explorer restarted: the icon comes back
            return LRESULT(0);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn make_tray(proxy: EventLoopProxy<UserEvent>) -> &'static Tray {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("QuiekelEmbedTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .unwrap_or_default();
        let icon = LoadImageW(
            Some(instance.into()),
            PCWSTR(1 as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .map(|h| h.0 as isize)
        .unwrap_or(0);
        let t = Tray {
            hwnd: AtomicIsize::new(hwnd.0 as isize),
            icon: AtomicIsize::new(icon),
            backend: OnceLock::new(),
            proxy: Mutex::new(proxy),
            lang: Mutex::new(stored_language()), // until the backend (and its settings) is up
            title: Mutex::new(title()),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        };
        let _ = TRAY.set(t);
        let t = tray();
        t.add();
        std::thread::Builder::new().name("tray-title".into()).spawn(|| tray().keep_title_fresh()).ok();
        t
    }
}

// ---- the app -----------------------------------------------------------------------------------------

fn now_s() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1).max(1)
}

pub fn main(hidden: bool) -> Result<()> {
    log::info!("Desktop app starting");
    unsafe {
        // Own taskbar identity.
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(config::APP_ID));
    }
    if !first_copy() {
        // Running already, maybe still starting: show that window instead. Started with Windows:
        // nothing to show.
        log::info!("Already running: showing that window");
        let deadline = Instant::now() + Duration::from_secs(30);
        while !hidden && !show_running_instance() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
        return Ok(());
    }
    if server::port_in_use() {
        if !show_running_instance() {
            server::open_browser(&server::url()); // the running copy is the browser version
        }
        return Ok(());
    }
    let dark = Arc::new(AtomicBool::new(starts_dark())); // the page reports changes; the title bar follows
    // Since when the window has been closed (0: open), for updates that install while it is.
    let hidden_since = Arc::new(AtomicU64::new(if hidden { now_s() } else { 0 }));
    // WebView2's own background until a page paints: never white.
    let (bg, _, _) = colors(dark.load(Ordering::SeqCst));
    // SAFETY: before any other thread of this app starts.
    unsafe { std::env::set_var("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", format!("FF{}", bg.trim_start_matches('#').to_uppercase())) };
    let rgb = u32::from_str_radix(bg.trim_start_matches('#'), 16).unwrap_or(0xeeefe9);
    let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let icon = {
        use tao::platform::windows::IconExtWindows;
        tao::window::Icon::from_resource(1, None).ok()
    };
    let window = WindowBuilder::new()
        .with_title(title())
        .with_inner_size(LogicalSize::new(1280.0, 840.0))
        .with_min_inner_size(LogicalSize::new(760.0, 540.0))
        .with_visible(!hidden)
        .with_window_icon(icon)
        .build(&event_loop)?;
    let hwnd = {
        use tao::platform::windows::WindowExtWindows;
        window.hwnd()
    };
    style_title_bar(hwnd, dark.load(Ordering::SeqCst));
    let mut context = wry::WebContext::new(Some(config::DATA_DIR.join("webview")));
    let webview = wry::WebViewBuilder::new_with_web_context(&mut context)
        .with_html(page("", "", dark.load(Ordering::SeqCst)))
        .with_background_color((r, g, b, 255))
        .with_incognito(true)
        .with_visible(!hidden)
        .build(&window)?;
    let tray = make_tray(proxy.clone());

    // Heavy work happens here, after the window is already on screen.
    let boot_proxy = proxy.clone();
    let dark_now = dark.clone();
    let hidden_boot = hidden_since.clone();
    std::thread::Builder::new().name("boot".into()).spawn(move || {
        let progress = |share: f64| {
            let _ = boot_proxy.send_event(UserEvent::Progress(share));
        };
        log::info!("Window shown, starting server");
        progress(0.12);
        let started = (|| -> Result<()> {
            let backend = server::build_backend("onnx")?;
            progress(0.45);
            let (p1, p3) = (boot_proxy.clone(), boot_proxy.clone());
            let dark_set = dark_now.clone();
            let hooks = Hooks {
                app_mode: true,
                pick_folder: Box::new(move |title| pick_folder(hwnd, title)),
                show_window: Some(Box::new(move || {
                    let _ = p1.send_event(UserEvent::Show);
                })),
                set_dark: Some(Box::new(move |on| {
                    dark_set.store(on, Ordering::SeqCst);
                    let _ = p3.send_event(UserEvent::SetDark(on));
                })),
            };
            {
                let quit = boot_proxy.clone();
                let hidden = hidden_boot.clone();
                *backend.updater.hooks.lock() = Some(crate::updater::Hooks {
                    quit: Box::new(move || {
                        let _ = quit.send_event(UserEvent::Quit);
                    }),
                    hidden_for: Box::new(move || match hidden.load(Ordering::SeqCst) {
                        0 => None,
                        since => Some(Duration::from_secs(now_s().saturating_sub(since))),
                    }),
                });
                let t = tray;
                *backend.updater.on_found.lock() = Some(Box::new(move |release| {
                    let version = release["version"].as_str().unwrap_or("").to_string();
                    t.notify(&t.tr("tray.update_found", &[("version", Value::from(version))]));
                }));
            }
            let _ = tray.backend.set(backend.clone());
            progress(0.75);
            let srv = server::Server::new(backend.clone(), hooks);
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new().name("server".into()).spawn(move || server::serve(srv, tx))?;
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => anyhow::bail!("the local server did not start: {e}"),
                Err(_) => anyhow::bail!("the local server did not start"),
            }
            server::start_background(&backend);
            progress(1.0);
            std::thread::sleep(Duration::from_millis(350)); // (the ring fills up before the app replaces it)
            let _ = boot_proxy.send_event(UserEvent::LoadUrl(server::url()));
            crate::updater::started(); // (an update waiting to see this version come up)
            log::info!("Server up, page loaded");
            Ok(())
        })();
        if let Err(e) = started {
            log::error!("Startup failed: {e:#}");
            let lang = tray.lang.lock().clone();
            let html = page(
                &i18n::t(&lang, "splash.failed", &[("error", Value::from(format!("Error: {e:#}")))]),
                &i18n::t(&lang, "splash.details", &[("path", Value::from(config::log_path().to_string_lossy().to_string()))]),
                dark_now.load(Ordering::SeqCst),
            );
            let _ = boot_proxy.send_event(UserEvent::LoadHtml(html));
        }
    })?;

    let mut styled = Some(dark.load(Ordering::SeqCst));
    // The page knows when nobody can see it (in the tray, minimized): it then barely polls, and
    // WebView2 stops drawing it.
    let mut page_shown = !hidden;
    let mut show_page = move |webview: &wry::WebView, window: &tao::window::Window| {
        let shown = window.is_visible() && !window.is_minimized();
        if shown != page_shown {
            let _ = webview.set_visible(shown);
            page_shown = shown;
        }
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                window.set_visible(false); // keep running in the tray
                show_page(&webview, &window);
                hidden_since.store(now_s(), Ordering::SeqCst);
                tray.hint_once();
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => show_page(&webview, &window), // (minimized or back)
            Event::UserEvent(e) => match e {
                UserEvent::Progress(share) => {
                    let _ = webview.evaluate_script(&format!("progress({share:.2})"));
                }
                UserEvent::LoadUrl(url) => {
                    let _ = webview.load_url(&url);
                }
                UserEvent::LoadHtml(html) => {
                    let _ = webview.load_html(&html);
                }
                UserEvent::Show => {
                    hidden_since.store(0, Ordering::SeqCst);
                    window.set_visible(true);
                    if window.is_minimized() {
                        window.set_minimized(false);
                    }
                    show_page(&webview, &window);
                    window.set_focus();
                }
                UserEvent::SetDark(on) => {
                    if styled != Some(on) {
                        style_title_bar(hwnd, on); // (redrawing the frame for nothing could flash it)
                        styled = Some(on);
                    }
                }
                UserEvent::Quit => {
                    tray.remove();
                    *control_flow = ControlFlow::Exit;
                    std::process::exit(0); // stop the server, watcher and indexer threads right away
                }
            },
            _ => {}
        }
    });
}
