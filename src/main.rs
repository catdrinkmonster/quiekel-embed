//! Quiekel Embed: search your files by what's in them, locally.
//!
//! One program: the desktop app (no arguments, or --hidden to start in the tray), the browser
//! version (--serve), and the model's own process (--model-process, started by the app).

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod archive;
mod config;
mod desktop;
mod diagnostics;
mod documents;
mod download;
mod embedder;
mod extract;
mod filemap;
mod filetypes;
mod governor;
mod i18n;
mod indexer;
mod machine;
mod mail;
mod model_process;
mod onnx;
mod paged;
mod pictures;
#[cfg(test)]
mod reading_tests;
mod search;
mod server;
mod shortcuts;
mod store;
mod updater;

use std::io::Write;
use std::time::Duration;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::Deserialize;

/// The app's log: the log file in its data folder, and the console when there is one.
struct Logger {
    file: Mutex<Option<std::fs::File>>,
    console: bool,
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        if m.target().starts_with("quiekel_embed") { m.level() <= log::Level::Info } else { m.level() <= log::Level::Warn }
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let line = format!(
            "{} {} {}: {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f"),
            r.level(),
            r.target().replace("::", "."),
            r.args()
        );
        if let Some(f) = self.file.lock().as_mut() {
            let _ = f.write_all(line.as_bytes());
        }
        if self.console {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

fn start_logging(console: bool) {
    let path = config::log_path();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5_000_000) {
        let _ = std::fs::rename(&path, path.with_extension("log.1")); // keep one old log, start fresh
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    let _ = log::set_boxed_logger(Box::new(Logger { file: Mutex::new(file), console }));
    log::set_max_level(log::LevelFilter::Info);
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));
}

/// A console for the browser version, when started from one.
fn attach_console() -> bool {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS).is_ok() }
}

/// Browser mode: the server, and the page in the default browser.
fn serve(open_browser: bool, model: &str) -> Result<()> {
    let url = server::url();
    if server::port_in_use() {
        println!("Quiekel Embed is already running: {url}");
        if open_browser {
            server::open_browser(&url);
        }
        return Ok(());
    }
    let backend = server::build_backend(model)?;
    let hooks = server::Hooks {
        app_mode: false,
        pick_folder: Box::new(|title| desktop::pick_folder(0, title)),
        show_window: None,
        quit_app: None,
        set_dark: None,
    };
    let srv = server::Server::new(backend.clone(), hooks);
    let (tx, rx) = std::sync::mpsc::channel();
    let serving = std::thread::Builder::new().name("server".into()).spawn(move || server::serve(srv, tx))?;
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => anyhow::bail!("the local server did not start: {e}"),
        Err(_) => anyhow::bail!("the local server did not start"),
    }
    server::start_background(&backend);
    println!("\n  Quiekel Embed running at {url}  (Ctrl+C to stop)\n");
    log::info!("Server up at {url}");
    if open_browser {
        std::thread::sleep(Duration::from_secs(1));
        server::open_browser(&url);
    }
    let _ = serving.join();
    Ok(())
}

#[derive(Deserialize)]
struct Check {
    gpu: bool,
    #[serde(default)]
    device: i32,
    #[serde(default)]
    threads: usize,
    #[serde(default)]
    texts: Vec<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    batches: Vec<Vec<String>>,
    #[serde(default)]
    repeat_query: usize,
    #[serde(default)]
    late_query: bool,
}

/// One query again and again: milliseconds each.
fn time_query(model: &mut onnx::OnnxModel, query: &Option<String>, n: usize) -> Result<Vec<f64>> {
    let mut ms = Vec::new();
    if let Some(q) = query {
        for _ in 0..n {
            let t = std::time::Instant::now();
            model.embed_texts(std::slice::from_ref(q), onnx::QUERY_PROMPT)?;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    Ok(ms)
}

#[derive(Deserialize)]
struct Script {
    gpu: bool,
    #[serde(default)]
    device: i32,
    #[serde(default)]
    threads: usize,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Step {
    Query { text: String, n: usize },
    Batches { batches: Vec<Vec<String>> },
    Shape { b: usize, words: usize, n: usize },
    Images { paths: Vec<String> },
    Describe,
    /// The vision encoder alone, `batch` pictures per call; then their text part in one call.
    Vision { paths: Vec<String>, batch: usize },
    /// Pictures as indexing embeds them (several at once) and one at a time: times and how alike.
    Pictures { paths: Vec<String> },
}

/// This process's graphics memory on a card, in MB: on the card, the card's budget for it, and in
/// shared system memory.
fn video_memory(device: i32) -> serde_json::Value {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
        DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
    };
    use windows::core::Interface;
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return serde_json::Value::Null };
        let Ok(adapter) = factory.EnumAdapters1(device as u32) else { return serde_json::Value::Null };
        let Ok(adapter) = adapter.cast::<IDXGIAdapter3>() else { return serde_json::Value::Null };
        let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        let mut shared = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        let _ = adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local);
        let _ = adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut shared);
        let mb = |b: u64| b >> 20;
        let mut p = windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX2 {
            cb: std::mem::size_of::<windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            ..Default::default()
        };
        let _ = windows::Win32::System::ProcessStatus::GetProcessMemoryInfo(
            windows::Win32::System::Threading::GetCurrentProcess(),
            &mut p as *mut _ as *mut windows::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS,
            p.cb,
        );
        serde_json::json!({
            "local": mb(local.CurrentUsage), "budget": mb(local.Budget), "shared": mb(shared.CurrentUsage),
            "ram": mb(p.WorkingSetSize as u64), "ram_peak": mb(p.PeakWorkingSetSize as u64), "ram_private": mb(p.PrivateWorkingSetSize as u64), "commit": mb(p.PrivateUsage as u64),
        })
    }
}

/// `embed-script <spec.json>`: steps through the model (queries, batches, made-up shapes,
/// pictures), with each call's milliseconds and the graphics memory after each step.
fn embed_script(spec: &str) -> Result<()> {
    let s: Script = serde_json::from_str(&std::fs::read_to_string(spec)?)?;
    let started = std::time::Instant::now();
    let mut model = onnx::OnnxModel::new(s.gpu, s.device, s.threads)?;
    let mut out = vec![serde_json::json!({"op": "load", "s": started.elapsed().as_secs_f64(), "mem": video_memory(s.device)})];
    let timed = |model: &mut onnx::OnnxModel, texts: &[String], prompt: &str| -> Result<f64> {
        let t = std::time::Instant::now();
        model.embed_texts(texts, prompt)?;
        Ok(t.elapsed().as_secs_f64() * 1000.0)
    };
    for step in &s.steps {
        let (op, ms, shapes) = match step {
            Step::Query { text, n } => {
                let ms = (0..*n).map(|_| timed(&mut model, std::slice::from_ref(text), onnx::QUERY_PROMPT)).collect::<Result<Vec<_>>>()?;
                ("query", ms, vec![])
            }
            Step::Shape { b, words, n } => {
                let texts = vec!["lorem ".repeat(*words); *b];
                let ms = (0..*n).map(|_| timed(&mut model, &texts, "")).collect::<Result<Vec<_>>>()?;
                let longest = model.tokens(&texts, "")?.iter().map(Vec::len).max().unwrap_or(0);
                ("shape", ms, vec![serde_json::json!([*b, longest.div_ceil(64) * 64])])
            }
            Step::Batches { batches } => {
                let mut ms = Vec::new();
                let mut shapes = Vec::new();
                for b in batches {
                    ms.push(timed(&mut model, b, "")?);
                    let longest = model.tokens(b, "")?.iter().map(Vec::len).max().unwrap_or(0);
                    shapes.push(serde_json::json!([b.len(), longest.div_ceil(64) * 64]));
                }
                ("batches", ms, shapes)
            }
            Step::Images { paths } => {
                let mut ms = Vec::new();
                for path in paths {
                    let img = image::open(path).with_context(|| path.clone())?.to_rgb8();
                    let pic = pictures::thumbnail(&img, 1024);
                    let t = std::time::Instant::now();
                    model.embed_images(std::slice::from_ref(&pic))?;
                    ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                ("images", ms, vec![])
            }
            Step::Describe => {
                out.push(serde_json::json!({"op": "describe", "io": model.describe()?}));
                continue;
            }
            Step::Pictures { paths } => {
                let pics: Vec<image::RgbImage> = paths
                    .iter()
                    .map(|p| Ok(pictures::thumbnail(&image::open(p).with_context(|| p.clone())?.to_rgb8(), 1024)))
                    .collect::<Result<_>>()?;
                let t = std::time::Instant::now();
                let together = model.embed_images(&pics)?;
                let together_ms = t.elapsed().as_secs_f64() * 1000.0;
                let t = std::time::Instant::now();
                let alone: Vec<Vec<f32>> = pics.iter().map(|p| model.embed_images(std::slice::from_ref(p)).map(|mut v| v.remove(0))).collect::<Result<_>>()?;
                let alone_ms = t.elapsed().as_secs_f64() * 1000.0;
                let cos: Vec<f32> = together.iter().zip(&alone).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x * y).sum()).collect();
                let min = cos.iter().copied().fold(1.0f32, f32::min);
                out.push(serde_json::json!({"op": "pictures", "n": pics.len(), "together_ms": together_ms, "alone_ms": alone_ms, "min_cosine": min, "mem": video_memory(s.device)}));
                continue;
            }
            Step::Vision { paths, batch } => {
                let pics: Vec<image::RgbImage> = paths
                    .iter()
                    .map(|p| Ok(pictures::thumbnail(&image::open(p).with_context(|| p.clone())?.to_rgb8(), 1024)))
                    .collect::<Result<_>>()?;
                let (mut vision_ms, mut text_ms) = (Vec::new(), Vec::new());
                for chunk in pics.chunks((*batch).max(1)) {
                    let t = std::time::Instant::now();
                    let (features, softs) = model.vision_batch(chunk)?;
                    vision_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    let t = std::time::Instant::now();
                    model.picture_texts(&softs, features)?;
                    text_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
                out.push(serde_json::json!({"op": "vision", "batch": batch, "vision_ms": vision_ms, "text_ms": text_ms, "mem": video_memory(s.device)}));
                continue;
            }
        };
        out.push(serde_json::json!({"op": op, "ms": ms, "shapes": shapes, "mem": video_memory(s.device)}));
    }
    println!("{}", serde_json::Value::Array(out));
    Ok(())
}

/// `embed-check <spec.json>`: vectors of texts, a query and pictures, as JSON (to compare).
fn embed_check(spec: &str) -> Result<()> {
    let c: Check = serde_json::from_str(&std::fs::read_to_string(spec)?)?;
    let started = std::time::Instant::now();
    let mut model = onnx::OnnxModel::new(c.gpu, c.device, c.threads)?;
    let loaded = started.elapsed().as_secs_f64();
    let t0 = std::time::Instant::now();
    let texts = if c.texts.is_empty() { Vec::new() } else { model.embed_texts(&c.texts, "")? };
    let text_s = t0.elapsed().as_secs_f64();
    let query = match &c.query {
        Some(q) if !c.late_query => Some(model.embed_texts(std::slice::from_ref(q), onnx::QUERY_PROMPT)?.remove(0)),
        _ => None,
    };
    // A search's query again and again: how long one takes once the model is warm.
    let mut query_ms = Vec::new();
    if let Some(q) = c.query.as_ref().filter(|_| !c.late_query) {
        for _ in 0..c.repeat_query {
            let t = std::time::Instant::now();
            model.embed_texts(std::slice::from_ref(q), onnx::QUERY_PROMPT)?;
            query_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let mut pics = Vec::new();
    for path in &c.images {
        let img = image::open(path).with_context(|| path.clone())?.to_rgb8();
        pics.push(pictures::thumbnail(&img, 1024));
    }
    let mut after = serde_json::Map::new();
    let t2 = std::time::Instant::now();
    for b in &c.batches {
        model.embed_texts(b, "")?;
    }
    let batches_s = t2.elapsed().as_secs_f64();
    after.insert("query_after_batches_ms".into(), serde_json::json!(time_query(&mut model, &c.query, c.repeat_query)?));
    let t1 = std::time::Instant::now();
    let images = model.embed_images(&pics)?;
    let image_s = t1.elapsed().as_secs_f64();
    after.insert("query_after_images_ms".into(), serde_json::json!(time_query(&mut model, &c.query, c.repeat_query)?));
    println!(
        "{}",
        serde_json::json!({
            "texts": texts, "query": query, "images": images,
            "load_s": loaded, "text_s": text_s, "image_s": image_s, "batches_s": batches_s, "query_ms": query_ms,
            "after": after,
            "tokens": model.tokens(&c.texts, "")?.iter().map(Vec::len).collect::<Vec<_>>(),
        })
    );
    Ok(())
}

/// `extract-check <file>...`: what the app reads from files, as JSON (to compare).
fn extract_check(paths: &[String]) -> Result<()> {
    let mut out = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let v = match extract::extract(path, size, None, None) {
            Ok(ex) => serde_json::json!({
                "path": p, "kind": ex.kind,
                "chunks": ex.chunks.iter().map(|c| serde_json::json!({
                    "snippet": c.snippet, "text": c.text,
                    "image": c.image.as_ref().map(|i| [i.width(), i.height()]),
                    "pixels": c.image.as_ref().map(|i| sha1_smol::Sha1::from(i.as_raw()).digest().to_string()),
                })).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({"path": p, "error": extract::problem(&e)}),
        };
        out.push(v);
    }
    println!("{}", serde_json::Value::Array(out));
    Ok(())
}

/// `walk-check <container>...`: what's found inside containers, as JSON (to compare).
fn walk_check(paths: &[String]) -> Result<()> {
    let types = filetypes::Types::new(&[], &Default::default());
    let mut out = Vec::new();
    for p in paths {
        let mtime = std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut items = Vec::new();
        archive::walk(p, mtime, &|n| types.inside(n), true, types.max_entries(), &mut |item| {
            let bytes = if item.problem.is_none() {
                archive::read(&item.path, 50_000_000).ok().map(|b| sha1_smol::Sha1::from(&b).digest().to_string())
            } else {
                None
            };
            items.push(serde_json::json!({
                "path": item.path, "name": item.name, "size": item.size, "mtime": item.mtime,
                "problem": item.problem, "sha1": bytes,
            }));
            true
        });
        out.push(serde_json::json!({"path": p, "items": items}));
    }
    println!("{}", serde_json::Value::Array(out));
    Ok(())
}

/// `logic-check <names.json>`: file-type decisions and query handling, as JSON (to compare).
fn logic_check(path: &str) -> Result<()> {
    let input: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let types = filetypes::Types::new(&[], &Default::default());
    let names: Vec<serde_json::Value> = input["names"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|n| {
            let n = n.as_str().unwrap_or("");
            let kind = extract::kind_of(n);
            serde_json::json!({
                "name": n, "kind": kind, "why_not": extract::why_not(n), "container": archive::container_type(n),
                "inside": types.inside(n), "types_kind": types.kind(n, None), "decide": types.decide(n, None).err(),
                "skip_file": types.skip_file(n, false, false), "skip_dir": types.skip_dir(n, false, false, ""),
                "max_bytes": extract::max_bytes(kind.unwrap_or("text"), n),
                "preview": extract::has_preview(n, kind.unwrap_or("text")),
            })
        })
        .collect();
    let queries: Vec<serde_json::Value> = input["queries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|q| {
            let q = q.as_str().unwrap_or("");
            let terms = search::query_terms(q);
            serde_json::json!({
                "q": q, "terms": terms, "fts": search::fts_query(&terms), "identifier": search::looks_like_identifier(q),
                "normalized": search::normalize(q),
            })
        })
        .collect();
    let texts: Vec<serde_json::Value> = input["texts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|t| serde_json::json!(extract::chunk_text(t.as_str().unwrap_or(""))))
        .collect();
    println!("{}", serde_json::json!({"names": names, "queries": queries, "chunks": texts}));
    Ok(())
}

/// `picture-check <file> <size>...`: previews as the server makes them (dimensions and pixels).
fn picture_check(args: &[String]) -> Result<()> {
    let mut out = Vec::new();
    for pair in args.chunks(2) {
        let size: u32 = pair.get(1).and_then(|s| s.parse().ok()).unwrap_or(360);
        let v = match pictures::picture(std::path::Path::new(&pair[0]), size) {
            Ok(i) => serde_json::json!({"path": pair[0], "size": [i.width(), i.height()],
                                        "pixels": sha1_smol::Sha1::from(i.as_raw()).digest().to_string()}),
            Err(e) => serde_json::json!({"path": pair[0], "error": extract::problem(&e)}),
        };
        out.push(v);
    }
    println!("{}", serde_json::Value::Array(out));
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    match args.get(1).map(String::as_str) {
        Some("--model-process") => return model_process::serve(),
        Some("embed-check") => {
            attach_console();
            return embed_check(args.get(2).context("spec file")?);
        }
        Some("embed-script") => {
            attach_console();
            return embed_script(args.get(2).context("spec file")?);
        }
        Some("extract-check") => {
            attach_console();
            return extract_check(&args[2..]);
        }
        Some("walk-check") => {
            attach_console();
            return walk_check(&args[2..]);
        }
        Some("picture-check") => {
            attach_console();
            return picture_check(&args[2..]);
        }
        Some("logic-check") => {
            attach_console();
            return logic_check(args.get(2).context("input file")?);
        }
        Some("--finish-update") => {
            start_logging(false);
            let r = updater::finish(&args[2..]);
            if let Err(e) = &r {
                log::error!("Update failed: {e:#}");
            }
            return r;
        }
        Some("--shortcuts") => {
            attach_console();
            return shortcuts::install(has("--remove"));
        }
        _ => {}
    }
    if has("--serve") {
        let console = attach_console();
        start_logging(console);
        let model = if has("--stand-in") { "stand-in" } else { "onnx" };
        let result = serve(!has("--no-browser"), model);
        if let Err(e) = &result {
            log::error!("{e:#}");
        }
        return result;
    }
    start_logging(false);
    let result = desktop::main(has("--hidden"));
    if let Err(e) = &result {
        log::error!("{e:#}");
    }
    result
}
