//! The model in a process of its own.
//!
//! The app never loads the model itself: a model process does (this same program, started with
//! `--model-process`), with the setup chosen for the PC (machine.rs). Starting it loads the model;
//! stopping it gives every byte back. Windows (a job object) holds it to its setup's memory cap
//! and processor threads, runs it below normal priority, and ends it together with the app. When
//! it crashes or runs out of memory, only it stops: the app starts it again.
//!
//! They talk over the process's standard input and output, one request at a time. Each message
//! is a frame: a JSON header, then bytes (pictures' pixels, or the vectors).
//!   app -> model: the setup (once), then {"op": "text", items, prompt}, {"op": "image", sizes}
//!                 + pixels, {"op": "query", text} or {"op": "quit"}
//!   model -> app: {"t": "status", state} and {"t": "ready"} or {"t": "failed", kind, text} while
//!                 starting; then {"t": "ok", n} + vectors or {"t": "error", kind, text} per
//!                 request; {"t": "log", text} at any time.
//!   kind: "oom" (out of memory: smaller batches help), "gpu" (the graphics card failed to start),
//!         "input" (an item can't be prepared: that file's fault) or "engine" (anything else).

use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use image::RgbImage;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::config::EMBED_DIM;
use crate::machine::Setup;

/// Running out of memory, as the engine or Windows says it (also on non-English Windows).
pub fn is_oom(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("out of memory")
        || t.contains("outofmemory")
        || t.contains("e_outofmemory")
        || t.contains("8007000e")
        || t.contains("not enough memory")
        || t.contains("bad allocation")
        || t.contains("bad_alloc")
        || t.contains("memory allocation")
}

fn clip(text: &str) -> String {
    text.chars().take(500).collect()
}

// ---- frames ------------------------------------------------------------------------------------

pub fn write_frame(w: &mut dyn Write, header: &Value, bytes: &[u8]) -> std::io::Result<()> {
    let h = serde_json::to_vec(header)?;
    w.write_all(&(h.len() as u32).to_le_bytes())?;
    w.write_all(&h)?;
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(bytes)?;
    w.flush()
}

pub fn read_frame(r: &mut dyn Read) -> std::io::Result<(Value, Vec<u8>)> {
    let mut n = [0u8; 4];
    r.read_exact(&mut n)?;
    let mut h = vec![0u8; u32::from_le_bytes(n) as usize];
    r.read_exact(&mut h)?;
    r.read_exact(&mut n)?;
    let mut bytes = vec![0u8; u32::from_le_bytes(n) as usize];
    r.read_exact(&mut bytes)?;
    let header = serde_json::from_slice(&h).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((header, bytes))
}

fn vectors_bytes(vectors: &[Vec<f32>]) -> Vec<u8> {
    vectors.iter().flat_map(|v| v.iter().flat_map(|x| x.to_le_bytes())).collect()
}

fn bytes_vectors(bytes: &[u8]) -> Vec<Vec<f32>> {
    bytes
        .chunks_exact(EMBED_DIM * 4)
        .map(|row| row.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
        .collect()
}

// ---- in the model process ------------------------------------------------------------------------

/// A tiny stand-in for the model (tests): the same vector for the same text, and a few texts that
/// act up on purpose. "!bad" can't be prepared, "!oom" runs out of memory, "!oom-over:N" too in a
/// batch of more than N, "!crash" ends the process, "!hang" never answers, "!alloc:N" takes N MB
/// first. A graphics card named "!broken..." doesn't start.
struct StandIn;

impl StandIn {
    fn vector(text: &str) -> Vec<f32> {
        let digest = sha1_smol::Sha1::from(text.as_bytes()).digest().bytes();
        let mut state = u64::from_le_bytes(digest[..8].try_into().unwrap());
        let mut next = || {
            // splitmix64
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut v: Vec<f32> = (0..EMBED_DIM)
            .map(|_| {
                let (u1, u2) = (next().max(1e-12), next());
                ((-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()) as f32
            })
            .collect();
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= n);
        v
    }

    fn embed(items: &[String]) -> std::result::Result<Vec<Vec<f32>>, (String, String)> {
        if items.iter().any(|t| t.starts_with("!bad")) {
            return Err(("input".into(), "ValueError: can't be read".into()));
        }
        let mut out = Vec::new();
        let mut _held = Vec::new();
        for text in items {
            if let Some(mb) = text.strip_prefix("!alloc:").and_then(|n| n.parse::<usize>().ok()) {
                let mut block = Vec::<u8>::new();
                if block.try_reserve_exact(mb << 20).is_err() {
                    return Err(("oom".into(), "MemoryError: out of memory".into()));
                }
                block.resize(mb << 20, 1); // touched, so it really takes the memory
                _held.push(block);
            }
            if let Some(n) = text.strip_prefix("!oom-over:").and_then(|n| n.parse::<usize>().ok()) {
                if items.len() > n {
                    return Err(("oom".into(), "MemoryError: out of memory".into()));
                }
            } else if text.starts_with("!oom") {
                return Err(("oom".into(), "MemoryError: out of memory".into()));
            }
            if text.starts_with("!crash") {
                std::process::exit(3);
            }
            if text.starts_with("!hang") {
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            out.push(Self::vector(text));
        }
        Ok(out)
    }
}

enum Model {
    Onnx(Box<crate::onnx::OnnxModel>),
    StandIn,
}

/// Its log lines go to the app's log, through the frames.
struct Forward {
    out: Arc<Mutex<BufWriter<std::io::Stdout>>>,
}

impl log::Log for Forward {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let mut out = self.out.lock();
            let _ = write_frame(&mut *out, &json!({"t": "log", "text": format!("{}: {}", record.target(), record.args())}), &[]);
        }
    }

    fn flush(&self) {}
}

fn answer(model: &mut Model, gpu: bool, header: &Value, bytes: &[u8]) -> std::result::Result<Vec<Vec<f32>>, (String, String)> {
    let op = header["op"].as_str().unwrap_or("");
    let items: Vec<String> = header["items"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let prompt = header["prompt"].as_str().unwrap_or("").to_string();
    let mut images = Vec::new();
    if op == "image" {
        let mut at = 0usize;
        for size in header["sizes"].as_array().cloned().unwrap_or_default() {
            let (w, h) = (size[0].as_u64().unwrap_or(0) as u32, size[1].as_u64().unwrap_or(0) as u32);
            let n = (w * h * 3) as usize;
            let pixels = bytes.get(at..at + n).ok_or(("input".to_string(), "picture data missing".to_string()))?;
            at += n;
            if w == 0 || h == 0 {
                return Err(("input".into(), "ValueError: an empty picture".into()));
            }
            images.push(RgbImage::from_raw(w, h, pixels.to_vec()).ok_or(("input".to_string(), "bad picture".to_string()))?);
        }
    }
    let engine_error = |e: anyhow::Error| {
        let text = clip(&format!("{e:#}"));
        // On the graphics card any failure counts as running out of memory: DirectML's errors
        // can't always even be read.
        (if gpu || is_oom(&text) { "oom" } else { "engine" }.to_string(), text)
    };
    match model {
        Model::StandIn => match op {
            "query" => StandIn::embed(&[header["text"].as_str().unwrap_or("").to_string()]),
            "image" => StandIn::embed(
                &images
                    .iter()
                    .map(|i| {
                        let sum: u64 = i.as_raw().iter().map(|&b| b as u64).sum();
                        format!("{}", sum as f64 / i.as_raw().len().max(1) as f64)
                    })
                    .collect::<Vec<_>>(),
            ),
            _ => StandIn::embed(&items),
        },
        Model::Onnx(m) => {
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match op {
                "query" => m.embed_texts(&[header["text"].as_str().unwrap_or("").to_string()], crate::onnx::QUERY_PROMPT),
                "image" => m.embed_images(&images),
                _ => m.embed_texts(&items, &prompt),
            }));
            match run {
                Ok(r) => r.map_err(engine_error),
                Err(_) => Err(engine_error(anyhow!("the engine panicked"))),
            }
        }
    }
}

/// The model process: load the model with the setup the app sends, then answer requests.
pub fn serve() -> Result<()> {
    let mut input = BufReader::new(std::io::stdin().lock());
    let out = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let send = |header: Value, bytes: &[u8]| -> std::io::Result<()> {
        let mut w = out.lock();
        write_frame(&mut *w, &header, bytes)
    };
    let (header, _) = read_frame(&mut input)?;
    let setup: Setup = serde_json::from_value(header)?;
    let _ = log::set_boxed_logger(Box::new(Forward { out: out.clone() }));
    log::set_max_level(log::LevelFilter::Info);
    let made: Result<Model> = if setup.model == "stand-in" {
        if setup.gpu && setup.card.starts_with("!broken") {
            Err(anyhow!("DirectML didn't start on this graphics card"))
        } else {
            Ok(Model::StandIn)
        }
    } else {
        send(json!({"t": "status", "state": if crate::onnx::cached(setup.gpu) { "loading" } else { "downloading" }}), &[])?;
        std::panic::catch_unwind(|| crate::onnx::OnnxModel::new(setup.gpu, setup.device, setup.threads))
            .unwrap_or_else(|_| Err(anyhow!("the engine panicked while loading")))
            .map(|m| Model::Onnx(Box::new(m)))
    };
    let mut model = match made {
        Ok(m) => m,
        Err(e) => {
            let text = clip(&format!("{e:#}"));
            let kind = if is_oom(&text) { "oom" } else if setup.gpu { "gpu" } else { "engine" };
            send(json!({"t": "failed", "kind": kind, "text": text}), &[])?;
            return Ok(());
        }
    };
    send(json!({"t": "ready"}), &[])?;
    loop {
        let Ok((header, bytes)) = read_frame(&mut input) else { return Ok(()) }; // the app is gone
        if header["op"] == "quit" {
            return Ok(());
        }
        match answer(&mut model, setup.gpu, &header, &bytes) {
            Ok(vectors) => {
                // On the graphics card: how much of its memory this process uses, and may.
                let vram = setup.gpu.then(|| crate::machine::video_memory(setup.device as u32)).flatten();
                send(json!({"t": "ok", "n": vectors.len(), "vram": vram.map(|(used, budget)| [used, budget])}), &vectors_bytes(&vectors))?
            }
            Err((kind, text)) => send(json!({"t": "error", "kind": kind, "text": text}), &[])?,
        }
    }
}

// ---- in the app ------------------------------------------------------------------------------------

/// The model process isn't there anymore (it crashed, ran out of memory, or was stopped).
#[derive(Debug)]
pub struct Gone(pub String);

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for Gone {}

#[derive(Debug)]
pub struct StartFailed {
    pub kind: String,
    pub text: String,
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.text)
    }
}
impl std::error::Error for StartFailed {}

/// An answer to a request.
pub enum Reply {
    Ok(Vec<Vec<f32>>),
    Error { kind: String, text: String },
}

/// What the app asks.
pub enum Request<'a> {
    Texts(&'a [String], &'a str),
    Images(&'a [RgbImage]),
    Query(&'a str),
}

// No answer to a request for this long: the process hangs, and is ended. Not while starting: the
// download may be slow.
pub static REQUEST_TIMEOUT_S: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(900);

/// The program that runs the model: this one (tests name the app's own build instead).
fn model_exe() -> std::io::Result<std::path::PathBuf> {
    match std::env::var_os("QUIEKEL_EMBED_MODEL_EXE") {
        Some(p) => Ok(p.into()),
        None => std::env::current_exe(),
    }
}

struct Channel {
    stdin: BufWriter<ChildStdin>,
    frames: mpsc::Receiver<std::io::Result<(Value, Vec<u8>)>>,
}

pub struct ModelProcess {
    pub setup: Setup,
    child: Mutex<Option<Child>>,
    channel: Mutex<Option<Channel>>, // one request at a time
    job: Mutex<Option<isize>>,
    pid: u32,
    dead: std::sync::atomic::AtomicBool,
    vram: Mutex<Option<(f64, f64)>>, // (in use, budget) in MB, as of the last answer
}

/// A job object holding the process: below normal priority, its memory cap, its processor threads,
/// and ended when the app ends. Returns the job's handle (closing it ends the process).
fn job(process: windows::Win32::Foundation::HANDLE, memory_cap_gb: Option<f64>, threads: Option<usize>) -> Result<isize> {
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;
    unsafe {
        let job = CreateJobObjectW(None, None)?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let basic = &mut limits.BasicLimitInformation;
        basic.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
        basic.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS.0;
        if let Some(cap) = memory_cap_gb.filter(|c| *c > 0.0) {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            limits.ProcessMemoryLimit = (cap * 1e9) as usize;
        }
        if let Some(t) = threads.filter(|t| *t > 0) {
            basic.LimitFlags |= JOB_OBJECT_LIMIT_AFFINITY;
            basic.Affinity = (1usize << t.min(crate::machine::logical_cpus())) - 1;
        }
        let set = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if let Err(e) = set.and_then(|_| AssignProcessToJobObject(job, process)) {
            let _ = windows::Win32::Foundation::CloseHandle(job);
            return Err(e.into());
        }
        Ok(job.0 as isize)
    }
}

fn resume(process: windows::Win32::Foundation::HANDLE) -> Result<()> {
    type NtResume = unsafe extern "system" fn(*mut std::ffi::c_void) -> i32;
    unsafe {
        let ntdll = libloading::Library::new("ntdll.dll")?;
        let f: libloading::Symbol<NtResume> = ntdll.get(b"NtResumeProcess\0")?;
        let status = f(process.0);
        if status != 0 {
            return Err(anyhow!("NtResumeProcess failed ({status:#x})"));
        }
    }
    Ok(())
}

impl ModelProcess {
    /// Start the process and wait until its model is loaded (or StartFailed).
    pub fn start(setup: Setup, on_status: &dyn Fn(&str)) -> std::result::Result<ModelProcess, StartFailed> {
        use std::os::windows::io::AsRawHandle;
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x4;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL: u32 = 0x4000;
        let failed = |kind: &str, text: String| StartFailed { kind: kind.into(), text: clip(&text) };
        let exe = model_exe().map_err(|e| failed("engine", e.to_string()))?;
        let mut child = Command::new(exe)
            .arg("--model-process")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW | BELOW_NORMAL)
            .spawn()
            .map_err(|e| failed("engine", e.to_string()))?;
        let handle = windows::Win32::Foundation::HANDLE(child.as_raw_handle());
        let job = match job(handle, setup.memory_cap_gb, setup.threads_cap) {
            Ok(j) => j,
            Err(e) => {
                let _ = child.kill(); // (still suspended: it never ran)
                return Err(failed("engine", e.to_string()));
            }
        };
        if let Err(e) = resume(handle) {
            let _ = child.kill();
            return Err(failed("engine", e.to_string()));
        }
        let stdin = BufWriter::new(child.stdin.take().unwrap());
        let stdout: ChildStdout = child.stdout.take().unwrap();
        // A thread reads its frames, so waiting for one can time out.
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("model-replies".into())
            .spawn(move || {
                let mut r = BufReader::with_capacity(1 << 20, stdout);
                loop {
                    let frame = read_frame(&mut r);
                    let end = frame.is_err();
                    if tx.send(frame).is_err() || end {
                        return;
                    }
                }
            })
            .map_err(|e| failed("engine", e.to_string()))?;
        let pid = child.id();
        let p = ModelProcess {
            setup: setup.clone(),
            child: Mutex::new(Some(child)),
            channel: Mutex::new(Some(Channel { stdin, frames: rx })),
            job: Mutex::new(Some(job)),
            pid,
            dead: std::sync::atomic::AtomicBool::new(false),
            vram: Mutex::new(None),
        };
        let started = (|| -> std::result::Result<(), StartFailed> {
            let mut ch = p.channel.lock();
            let ch = ch.as_mut().unwrap();
            let header = serde_json::to_value(&setup).unwrap_or_default();
            write_frame(&mut ch.stdin, &header, &[]).map_err(|e| failed("engine", e.to_string()))?;
            loop {
                let (msg, _) = p.receive(ch, None).map_err(|e| failed("engine", e.0))?;
                match msg["t"].as_str() {
                    Some("status") => on_status(msg["state"].as_str().unwrap_or("")),
                    Some("ready") => {
                        log::info!("Model process {} ready ({} setup)", p.pid, setup.name);
                        return Ok(());
                    }
                    Some("failed") => {
                        return Err(failed(msg["kind"].as_str().unwrap_or("engine"), msg["text"].as_str().unwrap_or("").to_string()));
                    }
                    _ => {}
                }
            }
        })();
        match started {
            Ok(()) => Ok(p),
            Err(e) => {
                p.stop();
                Err(e)
            }
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn alive(&self) -> bool {
        if self.dead.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        match self.child.lock().as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// The next message that isn't a log line. Gone when the process ends, or when it doesn't
    /// answer in time (it's ended then).
    fn receive(&self, ch: &mut Channel, timeout: Option<Duration>) -> std::result::Result<(Value, Vec<u8>), Gone> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let wait = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(3600),
            };
            let frame = match ch.frames.recv_timeout(wait) {
                Ok(Ok(f)) => f,
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Err(self.gone()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        if let Some(c) = self.child.lock().as_mut() {
                            let _ = c.kill();
                        }
                        return Err(self.gone());
                    }
                    continue;
                }
            };
            if frame.0["t"] == "log" {
                log::info!("Model process: {}", frame.0["text"].as_str().unwrap_or(""));
                continue;
            }
            return Ok(frame);
        }
    }

    fn gone(&self) -> Gone {
        self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
        let code = self.child.lock().as_mut().and_then(|c| {
            for _ in 0..20 {
                if let Ok(Some(s)) = c.try_wait() {
                    return s.code();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            None
        });
        Gone(format!("the model process ended (exit code {})", code.map(|c| c.to_string()).unwrap_or("None".into())))
    }

    /// A request and its answer. Gone if the process went away meanwhile.
    pub fn ask(&self, request: Request) -> std::result::Result<Reply, Gone> {
        let mut guard = self.channel.lock();
        if !self.alive() {
            return Err(Gone("the model process isn't running".into()));
        }
        let ch = guard.as_mut().ok_or_else(|| Gone("the model process isn't running".into()))?;
        let (header, bytes) = match request {
            Request::Texts(items, prompt) => (json!({"op": "text", "items": items, "prompt": prompt}), Vec::new()),
            Request::Query(text) => (json!({"op": "query", "text": text}), Vec::new()),
            Request::Images(images) => {
                let sizes: Vec<[u32; 2]> = images.iter().map(|i| [i.width(), i.height()]).collect();
                let mut bytes = Vec::with_capacity(images.iter().map(|i| i.as_raw().len()).sum());
                for i in images {
                    bytes.extend_from_slice(i.as_raw());
                }
                (json!({"op": "image", "sizes": sizes}), bytes)
            }
        };
        if let Err(e) = write_frame(&mut ch.stdin, &header, &bytes) {
            self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(Gone(clip(&e.to_string())));
        }
        let timeout = Duration::from_secs(REQUEST_TIMEOUT_S.load(std::sync::atomic::Ordering::SeqCst));
        let (msg, bytes) = self.receive(ch, Some(timeout))?;
        if let Some([used, budget]) = msg["vram"].as_array().and_then(|v| Some([v.first()?.as_f64()?, v.get(1)?.as_f64()?])) {
            *self.vram.lock() = Some((used, budget));
        }
        Ok(match msg["t"].as_str() {
            Some("ok") => Reply::Ok(bytes_vectors(&bytes)),
            _ => Reply::Error {
                kind: msg["kind"].as_str().unwrap_or("engine").to_string(),
                text: msg["text"].as_str().unwrap_or("").to_string(),
            },
        })
    }

    /// The graphics card's memory budget Windows gives the process is used up: another app needs it.
    pub fn over_budget(&self) -> bool {
        self.vram.lock().is_some_and(|(used, budget)| budget > 0.0 && used > budget)
    }

    /// Ask it to end, end it if it doesn't, and give everything back.
    pub fn stop(&self) {
        // (a batch under way finishes first)
        let guard = self.channel.try_lock_for(Duration::from_secs(15));
        let mut child = self.child.lock();
        if let Some(c) = child.as_mut() {
            if matches!(c.try_wait(), Ok(None)) && !self.dead.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(mut g) = guard {
                    if let Some(ch) = g.as_mut() {
                        let _ = write_frame(&mut ch.stdin, &json!({"op": "quit"}), &[]);
                    }
                    *g = None;
                }
                let deadline = Instant::now() + Duration::from_secs(5);
                while Instant::now() < deadline && matches!(c.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                if matches!(c.try_wait(), Ok(None)) {
                    let _ = c.kill();
                }
            }
            let _ = c.wait();
        }
        *child = None;
        if let Some(j) = self.job.lock().take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(j as *mut _)); // (ends whatever is left)
            }
        }
        self.dead.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Its memory (working set), as Task Manager shows it.
    pub fn memory_mb(&self) -> f64 {
        if !self.alive() {
            return 0.0;
        }
        process_memory_mb(self.pid)
    }
}

impl Drop for ModelProcess {
    fn drop(&mut self) {
        if let Some(j) = self.job.lock().take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(j as *mut _));
            }
        }
    }
}

/// A process's working set in MB (0 if it can't be read).
pub fn process_memory_mb(pid: u32) -> f64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return 0.0 };
        let mut c = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
        let ok = GetProcessMemoryInfo(h, &mut c, c.cb).is_ok();
        let _ = windows::Win32::Foundation::CloseHandle(h);
        if ok { c.WorkingSetSize as f64 / 1e6 } else { 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"t": "ok", "n": 1}), &[1, 2, 3]).unwrap();
        let (h, b) = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!((h["n"].as_i64(), b), (Some(1), vec![1, 2, 3]));
        let v = StandIn::vector("hello");
        assert_eq!(v, StandIn::vector("hello"));
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
        assert!(is_oom("Not enough memory resources are available"));
        assert_eq!(StandIn::embed(&["!bad".into()]).unwrap_err().0, "input");
        assert_eq!(StandIn::embed(&["a".into(), "!oom-over:1".into()]).unwrap_err().0, "oom");
    }
}
