//! The model, as the app sees it.
//!
//! EmbeddingGemma 2 runs through ONNX Runtime in a process of its own (model_process.rs), with the
//! setup chosen for the PC (machine.rs). This is the app's side: it starts and stops that process,
//! sends it the work in batches (longest texts first, so each batch needs little padding), lets
//! the governor pace between batches, and halves a batch when the process runs out of memory.
//!
//! The process only starts when there's memory to spare for the setup. When the graphics card
//! fails, the processor takes over for the rest of the session. Failures of the engine are never
//! blamed on a file: the work waits and is tried again. Only an item that can't be prepared is.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use image::RgbImage;
use parking_lot::{Mutex, RwLock};

use crate::machine::{self, Profile, Setup};
use crate::model_process::{ModelProcess, Reply, Request, StartFailed};

const RAM_SPARE_GB: f64 = 0.5; // left free for the rest of the PC when the model starts for a search

#[derive(Debug)]
pub enum EmbedError {
    /// The model ran out of memory even for a single item. Try again later.
    GpuBusy(String),
    /// The model was let go while work was under way: the work waits until it's loaded again.
    ModelAsleep,
    /// The engine failed by itself, whatever it was given: the work is tried again later.
    Engine(String),
    /// An item that can't be prepared for the model: its file is unreadable.
    BadInput(String),
}

impl std::fmt::Display for EmbedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbedError::GpuBusy(t) => write!(f, "out of memory: {t}"),
            EmbedError::ModelAsleep => write!(f, "the model was let go"),
            EmbedError::Engine(t) => write!(f, "{t}"),
            EmbedError::BadInput(t) => write!(f, "{t}"),
        }
    }
}
impl std::error::Error for EmbedError {}

#[derive(Debug)]
pub enum LoadError {
    /// Too little free memory to start the model right now.
    NotEnoughMemory { free_gb: f64, need_gb: f64 },
    Failed(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotEnoughMemory { free_gb, need_gb } => write!(f, "{free_gb:.1} GB free, {need_gb:.1} GB needed"),
            LoadError::Failed(t) => write!(f, "{t}"),
        }
    }
}
impl std::error::Error for LoadError {}

/// What the page shows about the model.
#[derive(Clone, Debug)]
pub struct State {
    pub status: String, // not loaded | downloading | loading | ready | asleep | error
    pub error: Option<String>,
    pub on_gpu: bool,
    /// Why it runs on the processor: none, shared, failed (the card failed this session), busy.
    pub cpu_because: String,
    pub gpu_name: String,
    pub engine: String, // directml | cpu
    pub download_gb: f64,
    pub setup: Option<Setup>,
}

pub struct Embedder {
    profile: Option<Profile>,
    model: String, // "stand-in": a tiny model, for tests
    proc: RwLock<Option<Arc<ModelProcess>>>,
    load_lock: Mutex<()>,
    pub state: RwLock<State>,
    /// Set by the indexer: called with the seconds each batch took, to pace itself.
    pub pace: RwLock<Option<Box<dyn Fn(f64) + Send + Sync>>>,
    /// Set by the indexer: < 1 shrinks batches when video memory is tight.
    pub batch_scale: Mutex<f64>,
    /// The graphics card failed this session: the processor does the work until the app restarts.
    pub gpu_trouble: AtomicBool,
}

impl Embedder {
    pub fn new(profile: Option<Profile>, model: &str) -> Embedder {
        Embedder {
            profile,
            model: model.into(),
            proc: RwLock::new(None),
            load_lock: Mutex::new(()),
            state: RwLock::new(State {
                status: "not loaded".into(),
                error: None,
                on_gpu: false,
                cpu_because: String::new(),
                gpu_name: String::new(),
                engine: String::new(),
                download_gb: 0.9,
                setup: None,
            }),
            pace: RwLock::new(None),
            batch_scale: Mutex::new(1.0),
            gpu_trouble: AtomicBool::new(false),
        }
    }

    pub fn profile(&self) -> &Profile {
        self.profile.as_ref().unwrap_or_else(|| machine::profile())
    }

    pub fn ready(&self) -> bool {
        self.proc.read().as_ref().is_some_and(|p| p.alive())
    }

    pub fn status(&self) -> String {
        self.state.read().status.clone()
    }

    pub fn set_status(&self, s: &str) {
        self.state.write().status = s.into();
    }

    pub fn on_gpu(&self) -> bool {
        self.state.read().on_gpu
    }


    /// The graphics card would take the model, when it's free.
    pub fn could_use_gpu(&self) -> bool {
        !self.gpu_trouble.load(Ordering::SeqCst) && machine::choose(self.profile(), true, None).gpu
    }

    pub fn device(&self) -> String {
        if !self.ready() {
            return String::new();
        }
        let s = self.state.read();
        if s.on_gpu { format!("{} (DirectML)", s.gpu_name) } else { "CPU".into() }
    }

    /// The model process, while it runs (the governor counts its work as the app's own).
    pub fn pid(&self) -> Option<u32> {
        self.proc.read().as_ref().filter(|p| p.alive()).map(|p| p.pid())
    }

    pub fn memory_mb(&self) -> f64 {
        self.proc.read().as_ref().map(|p| p.memory_mb()).unwrap_or(0.0)
    }

    // ---- starting and stopping --------------------------------------------------------------

    /// Start the model process; on the processor when prefer_gpu is false (e.g. a game is
    /// running). NotEnoughMemory unless spare_gb stays free besides what it takes.
    pub fn load(&self, prefer_gpu: bool, spare_gb: Option<f64>) -> Result<(), LoadError> {
        let spare = spare_gb.unwrap_or(RAM_SPARE_GB);
        {
            let _one = self.load_lock.lock();
            if self.ready() {
                return Ok(());
            }
            self.state.write().error = None;
            let use_gpu = prefer_gpu && !self.gpu_trouble.load(Ordering::SeqCst);
            let setup = machine::choose(self.profile(), use_gpu, None);
            let result = match self.start(setup.clone(), spare) {
                Err(LoadError::Failed(e)) if setup.gpu => {
                    log::warn!("The graphics card didn't take the model ({e}): using the processor");
                    self.gpu_trouble.store(true, Ordering::SeqCst);
                    self.start(machine::choose(self.profile(), false, None), spare)
                }
                other => other,
            };
            match result {
                Err(e @ LoadError::NotEnoughMemory { .. }) => {
                    self.set_status("asleep"); // (starts once there's room)
                    log::info!("Model waits for memory ({e})");
                    return Err(e);
                }
                Err(LoadError::Failed(e)) => {
                    let mut s = self.state.write();
                    s.status = "error".into();
                    s.error = Some(e.clone());
                    log::error!("Model failed to load: {e}");
                    return Err(LoadError::Failed(e));
                }
                Ok(()) => {}
            }
            let because = if self.on_gpu() {
                String::new()
            } else if self.gpu_trouble.load(Ordering::SeqCst) {
                "failed".into()
            } else {
                let w = machine::without_gpu(self.profile());
                if w.is_empty() { "busy".into() } else { w.to_string() }
            };
            self.state.write().cpu_because = because;
        }
        Ok(())
    }

    fn start(&self, mut setup: Setup, spare_gb: f64) -> Result<(), LoadError> {
        let (need, free) = (setup.ram_needed_gb + spare_gb, machine::free_ram_gb_on(self.profile()));
        if free < need {
            return Err(LoadError::NotEnoughMemory { free_gb: free, need_gb: need });
        }
        if self.model != "onnx" {
            setup.model = self.model.clone();
        }
        {
            let mut s = self.state.write();
            s.download_gb = if setup.gpu { 0.9 } else { 0.5 }; // (half precision for a card, 8-bit otherwise)
            s.status = "loading".into();
        }
        let proc = ModelProcess::start(setup.clone(), &|state| self.set_status(state))
            .map_err(|e: StartFailed| LoadError::Failed(e.text))?;
        *self.proc.write() = Some(Arc::new(proc));
        {
            let mut s = self.state.write();
            s.on_gpu = setup.gpu;
            s.gpu_name = setup.card.clone();
            s.engine = if setup.gpu { "directml".into() } else { "cpu".into() };
            s.status = "ready".into();
            s.setup = Some(setup.clone());
        }
        log::info!("Model ready on {} ({} setup, {} threads)", self.device(), setup.name, setup.threads);
        Ok(())
    }

    /// Stop the model process: all its memory (and video memory) is given back. The next search
    /// or file to index starts it again.
    pub fn unload(&self, why: &str) {
        let proc = {
            let _one = self.load_lock.lock();
            let Some(proc) = self.proc.write().take() else { return };
            let mut s = self.state.write();
            s.on_gpu = false;
            s.engine = String::new();
            s.status = "asleep".into();
            proc
        };
        proc.stop();
        log::info!("Model unloaded ({why})");
    }

    /// The graphics card keeps failing: stop, and start on the processor from now on.
    pub fn avoid_gpu(&self, why: &str) {
        if self.gpu_trouble.swap(true, Ordering::SeqCst) {
            return;
        }
        log::info!("Using the processor from now on ({why})");
        if self.on_gpu() {
            self.unload(why);
        }
    }

    // ---- the work ------------------------------------------------------------------------------

    fn ask(&self, request: Request) -> Result<Reply, EmbedError> {
        let Some(proc) = self.proc.read().clone() else { return Err(EmbedError::ModelAsleep) };
        match proc.ask(request) {
            Ok(r) => Ok(r),
            Err(gone) => {
                let current = self.proc.read().as_ref().is_some_and(|p| Arc::ptr_eq(p, &proc));
                if !current {
                    return Err(EmbedError::ModelAsleep);
                }
                // It crashed, or Windows held it to its memory cap: started again for the next try.
                {
                    let _one = self.load_lock.lock();
                    let mut slot = self.proc.write();
                    if slot.as_ref().is_some_and(|p| Arc::ptr_eq(p, &proc)) {
                        *slot = None;
                        let mut s = self.state.write();
                        s.on_gpu = false;
                        s.engine = String::new();
                        s.status = "asleep".into();
                    }
                }
                if proc.setup.gpu {
                    self.gpu_trouble.store(true, Ordering::SeqCst); // (on the graphics card: the processor takes over)
                }
                proc.stop();
                log::warn!("The model process stopped: {gone}");
                Err(EmbedError::Engine(gone.0))
            }
        }
    }

    fn batch(&self, kind: &str) -> usize {
        match &self.state.read().setup {
            Some(s) => {
                if kind == "text" { s.text_batch } else { s.image_batch }
            }
            None => {
                if kind == "text" { 8 } else { 1 }
            }
        }
    }

    /// Embed in batches, longest first so each batch needs little padding. A batch that runs out of
    /// memory is halved; a single item that does gives GpuBusy.
    fn embed_items(&self, texts: Option<&[String]>, images: Option<&[RgbImage]>, prompt: &str) -> Result<Vec<Vec<f32>>, EmbedError> {
        let n = texts.map(|t| t.len()).or(images.map(|i| i.len())).unwrap_or(0);
        let mut out = vec![Vec::new(); n];
        if n == 0 {
            return Ok(out);
        }
        let mut order: Vec<usize> = (0..n).collect();
        if let Some(t) = texts {
            // (Python's len(): characters; a stable sort keeps the order of equal lengths)
            order.sort_by_key(|&i| std::cmp::Reverse(t[i].chars().count()));
        }
        let base = self.batch(if texts.is_some() { "text" } else { "image" });
        let mut size = ((base as f64 * *self.batch_scale.lock()) as usize).max(1);
        let mut i = 0;
        while i < order.len() {
            let end = (i + size).min(order.len());
            let started = Instant::now();
            let reply = match (texts, images) {
                (Some(t), _) => {
                    let batch: Vec<String> = order[i..end].iter().map(|&j| t[j].clone()).collect();
                    self.ask(Request::Texts(&batch, prompt))?
                }
                (_, Some(im)) => {
                    let batch: Vec<RgbImage> = order[i..end].iter().map(|&j| im[j].clone()).collect();
                    self.ask(Request::Images(&batch))?
                }
                _ => unreachable!(),
            };
            match reply {
                Reply::Ok(vectors) => {
                    for (k, v) in order[i..end].iter().zip(vectors) {
                        out[*k] = v;
                    }
                    i = end;
                    if let Some(pace) = self.pace.read().as_ref() {
                        pace(started.elapsed().as_secs_f64());
                    }
                }
                Reply::Error { kind, text } => {
                    if kind == "input" {
                        return Err(EmbedError::BadInput(text));
                    }
                    if kind != "oom" {
                        return Err(EmbedError::Engine(text));
                    }
                    if size == 1 {
                        return Err(EmbedError::GpuBusy(text));
                    }
                    size = (size / 2).max(1);
                    log::info!("Memory tight: batch size now {size} ({})", text.chars().take(120).collect::<String>());
                }
            }
        }
        Ok(out)
    }

    /// Embed passages already formatted as 'title: ... | text: ...'.
    pub fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_items(Some(texts), None, "")
    }

    pub fn images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_items(None, Some(images), "")
    }

    pub fn query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        match self.ask(Request::Query(text))? {
            Reply::Ok(mut v) if !v.is_empty() => Ok(v.remove(0)),
            Reply::Ok(_) => Err(EmbedError::Engine("no vector".into())),
            Reply::Error { kind, text } => Err(if kind == "oom" { EmbedError::GpuBusy(text) } else { EmbedError::Engine(text) }),
        }
    }
}

/// The model as the indexer uses it.
pub trait Model: Send + Sync {
    fn ready(&self) -> bool;
    fn status(&self) -> String;
    fn set_status(&self, status: &str);
    /// Why it failed to start, for the page.
    fn error(&self) -> Option<String>;
    fn load(&self, prefer_gpu: bool, spare_gb: Option<f64>) -> Result<(), LoadError>;
    fn unload(&self, why: &str);
    fn avoid_gpu(&self, why: &str);
    fn on_gpu(&self) -> bool;
    /// The graphics card's memory budget Windows gives the model is used up (another app needs it).
    fn over_budget(&self) -> bool;
    fn could_use_gpu(&self) -> bool;
    /// Called with the seconds each batch took, to pace the work.
    fn set_pace(&self, pace: Box<dyn Fn(f64) + Send + Sync>);
    /// < 1 shrinks batches when video memory is tight.
    fn set_batch_scale(&self, scale: f64);
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
    fn images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

impl Model for Embedder {
    fn ready(&self) -> bool {
        Embedder::ready(self)
    }
    fn status(&self) -> String {
        Embedder::status(self)
    }
    fn set_status(&self, status: &str) {
        Embedder::set_status(self, status)
    }
    fn error(&self) -> Option<String> {
        self.state.read().error.clone()
    }
    fn load(&self, prefer_gpu: bool, spare_gb: Option<f64>) -> Result<(), LoadError> {
        Embedder::load(self, prefer_gpu, spare_gb)
    }
    fn unload(&self, why: &str) {
        Embedder::unload(self, why)
    }
    fn avoid_gpu(&self, why: &str) {
        Embedder::avoid_gpu(self, why)
    }
    fn on_gpu(&self) -> bool {
        Embedder::on_gpu(self)
    }
    fn over_budget(&self) -> bool {
        self.proc.read().as_ref().is_some_and(|p| p.over_budget())
    }
    fn could_use_gpu(&self) -> bool {
        Embedder::could_use_gpu(self)
    }
    fn set_pace(&self, pace: Box<dyn Fn(f64) + Send + Sync>) {
        *self.pace.write() = Some(pace);
    }
    fn set_batch_scale(&self, scale: f64) {
        *self.batch_scale.lock() = scale;
    }
    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Embedder::documents(self, texts)
    }
    fn images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Embedder::images(self, images)
    }
}

#[cfg(test)]
mod tests {
    //! These start a real model process with the stand-in model, from the app's own build
    //! (target\release or target\debug: build it first).
    use super::*;
    use crate::config::EMBED_DIM;
    use crate::machine::Card;

    fn exe() -> Option<std::path::PathBuf> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        ["release", "debug"].iter().map(|d| root.join(d).join("quiekel-embed.exe")).find(|p| p.is_file())
    }

    fn pc(ram: f64, cores: usize, cards: Vec<Card>, threads_cap: Option<usize>) -> Profile {
        Profile { ram_gb: ram, cores, cards, free_cap_gb: None, threads_cap, simulated: true }
    }

    fn processor() -> Profile {
        pc(16.0, 8, vec![], None) // batches of 8
    }

    fn light() -> Profile {
        pc(8.0, 4, vec![], Some(2)) // batches of 4, at most 1 GB
    }

    fn started(p: Profile) -> Option<Arc<Embedder>> {
        let exe = exe()?;
        // SAFETY: the tests that start a model all set the same value.
        unsafe { std::env::set_var("QUIEKEL_EMBED_MODEL_EXE", exe) };
        let e = Arc::new(Embedder::new(Some(p), "stand-in"));
        e.load(true, None).expect("the stand-in starts");
        Some(e)
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn each(e: &Embedder, items: &[String]) -> Vec<Vec<f32>> {
        items.iter().map(|t| e.documents(std::slice::from_ref(t)).unwrap().remove(0)).collect()
    }

    #[test]
    fn results_come_back_in_input_order_though_batches_are_sorted() {
        let Some(e) = started(light()) else { return };
        let items: Vec<String> = [5, 1, 9, 3, 7, 2, 8].iter().map(|n| "a".repeat(*n)).collect();
        let paced = Arc::new(Mutex::new(0));
        let counter = paced.clone();
        *e.pace.write() = Some(Box::new(move |_| *counter.lock() += 1));
        let out = e.documents(&items).unwrap();
        assert_eq!(*paced.lock(), 2); // two batches of up to 4: the governor gets a say after each
        *e.pace.write() = None;
        assert_eq!(out.len(), 7);
        assert!(out.iter().all(|v| v.len() == EMBED_DIM && (v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4));
        assert_eq!(out, each(&e, &items));
        e.unload("test over");
    }

    #[test]
    fn batches_shrink_when_memory_runs_out() {
        let Some(e) = started(processor()) else { return };
        let mut items = texts(&["!oom-over:2"]);
        items.extend((0..7).map(|i| format!("note {i}")));
        assert_eq!(e.documents(&items).unwrap(), each(&e, &items));
        assert!(matches!(e.documents(&texts(&["!oom"])), Err(EmbedError::GpuBusy(_))));
        e.unload("test over");
    }

    #[test]
    fn windows_holds_the_model_to_its_memory_cap() {
        let Some(e) = started(light()) else { return };
        assert!(matches!(e.documents(&texts(&["!alloc:2000"])), Err(EmbedError::GpuBusy(_)))); // 2 GB, capped at 1 GB
        assert!(e.ready());
        assert_eq!(e.documents(&texts(&["!alloc:200"])).unwrap().len(), 1);
        e.unload("test over");
    }

    #[test]
    fn an_item_that_cant_be_prepared_is_that_files_fault() {
        let Some(e) = started(processor()) else { return };
        assert!(matches!(e.documents(&texts(&["!bad one"])), Err(EmbedError::BadInput(_))));
        assert!(e.ready());
        e.unload("test over");
    }

    #[test]
    fn a_crash_is_the_engines_fault_and_it_starts_again() {
        let Some(e) = started(processor()) else { return };
        assert!(matches!(e.documents(&texts(&["!crash"])), Err(EmbedError::Engine(_))));
        assert!(!e.ready() && e.status() == "asleep");
        e.load(true, None).unwrap();
        assert_eq!(e.documents(&texts(&["back"])).unwrap().len(), 1);
        e.unload("test over");
    }

    #[test]
    fn letting_the_model_go_between_batches_is_no_error() {
        let Some(e) = started(light()) else { return };
        let weak = Arc::downgrade(&e);
        *e.pace.write() = Some(Box::new(move |_| {
            if let Some(e) = weak.upgrade() {
                e.unload("a game started");
            }
        }));
        let items: Vec<String> = (0..8).map(|i| format!("note {i}")).collect();
        assert!(matches!(e.documents(&items), Err(EmbedError::ModelAsleep)));
        assert_eq!(e.status(), "asleep");
    }

    #[test]
    fn stopping_ends_the_model_process() {
        let Some(e) = started(processor()) else { return };
        let pid = e.pid().unwrap();
        e.unload("not used");
        assert!(e.pid().is_none());
        // Ended: all its memory is back (Windows may take a moment to let go of a process that's gone).
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while crate::model_process::process_memory_mb(pid) > 0.0 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(crate::model_process::process_memory_mb(pid), 0.0);
    }

    #[test]
    fn the_model_only_starts_with_memory_to_spare() {
        if exe().is_none() {
            return;
        }
        let mut p = processor();
        p.free_cap_gb = Some(0.8);
        let e = Embedder::new(Some(p), "stand-in");
        assert!(matches!(e.load(true, None), Err(LoadError::NotEnoughMemory { .. })));
        assert!(e.status() == "asleep" && !e.ready());
    }

    #[test]
    fn a_graphics_card_that_wont_start_hands_over_to_the_processor() {
        let card = pc(16.0, 8, vec![Card { index: 0, name: "!broken card".into(), own_vram: 8_000_000_000 }], None);
        let Some(e) = started(card) else { return };
        let s = e.state.read().clone();
        assert_eq!((s.setup.unwrap().name.as_str(), s.engine.as_str()), ("processor", "cpu"));
        assert!(e.gpu_trouble.load(Ordering::SeqCst));
        e.unload("test over");
    }

    #[test]
    fn a_crash_on_the_graphics_card_hands_over_to_the_processor() {
        let card = pc(16.0, 8, vec![Card { index: 0, name: "Card".into(), own_vram: 8_000_000_000 }], None);
        let Some(e) = started(card) else { return };
        assert!(e.on_gpu());
        assert!(matches!(e.documents(&texts(&["!crash"])), Err(EmbedError::Engine(_))));
        e.load(true, None).unwrap();
        let s = e.state.read().clone();
        assert_eq!((s.setup.unwrap().name.as_str(), s.on_gpu), ("processor", false));
        e.unload("test over");
    }

    #[test]
    fn search_queries_are_answered_and_nothing_is_nothing() {
        let Some(e) = started(processor()) else { return };
        let v = e.query("rent contract").unwrap();
        assert_eq!(v.len(), EMBED_DIM);
        assert!((v.iter().map(|x| x * x).sum::<f32>().sqrt() - 1.0).abs() < 1e-4);
        assert!(e.documents(&[]).unwrap().is_empty());
        e.unload("test over");
    }

    #[test]
    fn the_model_runs_below_normal_priority_with_its_setups_threads() {
        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetPriorityClass, GetProcessAffinityMask, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let Some(e) = started(light()) else { return };
        let pid = e.pid().unwrap();
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).unwrap();
            assert_eq!(GetPriorityClass(h), BELOW_NORMAL_PRIORITY_CLASS.0);
            let (mut mask, mut system) = (0usize, 0usize);
            GetProcessAffinityMask(h, &mut mask, &mut system).unwrap();
            assert_eq!(mask, 0b11); // (a pretended PC with two threads for the model)
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
        assert!(e.memory_mb() > 0.0);
        e.unload("test over");
    }

    #[test]
    fn the_page_learns_why_the_model_is_on_the_processor() {
        if exe().is_none() {
            return;
        }
        let cases = [
            (pc(16.0, 8, vec![], None), true, "none"),
            (pc(8.0, 4, vec![Card { index: 0, name: "Built-in".into(), own_vram: 128_000_000 }], None), true, "shared"),
            (pc(16.0, 8, vec![Card { index: 0, name: "Card".into(), own_vram: 8_000_000_000 }], None), false, "busy"),
            (pc(16.0, 8, vec![Card { index: 0, name: "!broken card".into(), own_vram: 8_000_000_000 }], None), true, "failed"),
        ];
        for (p, prefer_gpu, why) in cases {
            unsafe { std::env::set_var("QUIEKEL_EMBED_MODEL_EXE", exe().unwrap()) };
            let e = Embedder::new(Some(p), "stand-in");
            e.load(prefer_gpu, None).unwrap();
            let s = e.state.read().clone();
            assert_eq!((s.on_gpu, s.cpu_because.as_str()), (false, why));
            assert_eq!(e.could_use_gpu(), why == "busy"); // (only a busy card is worth going back to)
            e.unload("test over");
        }
    }

    #[test]
    fn a_model_process_that_hangs_is_ended() {
        let Some(e) = started(processor()) else { return };
        crate::model_process::REQUEST_TIMEOUT_S.store(1, Ordering::SeqCst);
        let r = e.documents(&texts(&["!alloc:1", "!hang"]));
        crate::model_process::REQUEST_TIMEOUT_S.store(900, Ordering::SeqCst);
        assert!(matches!(r, Err(EmbedError::Engine(_))));
        assert!(!e.ready());
    }
}
