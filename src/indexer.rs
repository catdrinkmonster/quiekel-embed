//! Background indexing: one worker thread, a job queue, and a file watcher.
//!
//! The worker runs at Windows "background" priority (low CPU, disk and memory priority) and asks
//! the resource governor how fast it may go after every unit of work. When there is nothing to
//! do, or video memory runs out, it hands the GPU back.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use parking_lot::{Condvar, Mutex};
use serde_json::{Map, Value, json};

use crate::archive;
use crate::config::{self, file_name};
use crate::embedder::{EmbedError, LoadError, Model};
use crate::extract::{self, Chunk, ReadError, extract, max_bytes, problem};
use crate::filetypes::{Report, Types};
use crate::governor::{Governor, set_background_priority};
use crate::store::{ContainerRow, Entry, Settings, Store, VectorRow, normcase};

const BACK_TO_GPU_S: f64 = 300.0; // the model moves back to a free graphics card at most this often
const GPU_NEEDED_GB: f64 = 2.0; // free video memory needed to put the model on the GPU
// Memory: when it stays almost full this long, the model is let go, whatever the setting. To
// index, it only starts again with this much free besides what it takes.
const RAM_RELEASE_AFTER_S: f64 = 30.0;
const INDEX_SPARE_GB: f64 = 1.2;
// Files read and decoded ahead, so the GPU never waits for the CPU. Fewer when memory is tight.
const PREFETCH_FILES: usize = 32;
const PREFETCH_TIGHT: usize = 8;
const MAX_READERS: usize = 8; // files read at once, at most

/// Indexing stops here (switched off, the folder removed or paused): what isn't saved yet stays
/// pending for the next scan.
#[derive(Debug)]
struct Stop;

/// A file found: (path, size, mtime).
type Found = (String, i64, f64);

/// Where indexing spends its time, summed since the app started: reading files (all readers
/// together), waiting for read files, the model, saving. Microseconds (a small file is read in
/// well under a millisecond), and what was done.
#[derive(Default)]
struct Pipeline {
    read_us: AtomicU64,
    files: AtomicU64,
    wait_us: AtomicU64,
    embed_us: AtomicU64,
    texts: AtomicU64,
    pictures: AtomicU64,
    save_us: AtomicU64,
}

fn add_time(counter: &AtomicU64, since: Instant) {
    counter.fetch_add(since.elapsed().as_micros() as u64, Ordering::Relaxed);
}

enum Job {
    Scan(i64),
    Index(i64, String, Vec<Found>),
    Remove(Vec<String>),
}

/// A flag that threads can wait for.
struct Flag {
    set: Mutex<bool>,
    cv: Condvar,
}

impl Flag {
    fn new() -> Flag {
        Flag { set: Mutex::new(false), cv: Condvar::new() }
    }
    fn set(&self) {
        *self.set.lock() = true;
        self.cv.notify_all();
    }
    fn clear(&self) {
        *self.set.lock() = false;
    }
    fn wait(&self) {
        let mut s = self.set.lock();
        while !*s {
            self.cv.wait(&mut s);
        }
    }
}

fn since(t: Option<Instant>) -> f64 {
    t.map(|t| t.elapsed().as_secs_f64()).unwrap_or(f64::INFINITY)
}

/// (hidden, system), as Windows marks them.
fn attributes(meta: &std::fs::Metadata) -> (bool, bool) {
    use std::os::windows::fs::MetadataExt;
    let a = meta.file_attributes();
    (a & 0x2 != 0, a & 0x4 != 0)
}

fn mtime_of(meta: &std::fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Why a path's folders or name rule it out (inside an archive, or from a file event).
pub fn skipped_part(types: &Types, parts: &[&str]) -> Option<&'static str> {
    let (name, folders) = parts.split_last()?;
    let mut parent = "";
    for folder in folders {
        if let Some(why) = types.skip_dir(folder, false, false, parent) {
            return Some(why);
        }
        parent = folder;
    }
    types.skip_file(name, false, false)
}

/// A file inside an archive or an email, read through its container.
fn in_container(path: &str) -> bool {
    let parts: Vec<&str> = path.split('\\').collect();
    parts[..parts.len().saturating_sub(1)].iter().any(|p| archive::container_type(p).is_some())
}

/// The known files inside containers, by container: normcase(path) -> [(path, size, mtime)].
fn by_container(signatures: &HashMap<String, (i64, i64, f64, String)>) -> HashMap<String, Vec<Found>> {
    let mut out: HashMap<String, Vec<Found>> = HashMap::new();
    for (path, (_, size, mtime, _)) in signatures {
        let parts: Vec<&str> = path.split('\\').collect();
        for k in 1..parts.len() {
            if archive::container_type(parts[k - 1]).is_some() {
                out.entry(normcase(&parts[..k].join("\\"))).or_default().push((path.clone(), *size, *mtime));
                break;
            }
        }
    }
    out
}

pub fn is_inside(path: &str, root: &str) -> bool {
    let (p, r) = (normcase(path), normcase(root.trim_end_matches(['\\', '/'])));
    p == r || p.starts_with(&format!("{r}\\"))
}

/// The part of a path below root ("a\b.txt").
fn relative<'a>(path: &'a str, root: &str) -> &'a str {
    let root = root.trim_end_matches(['\\', '/']);
    match path.get(..root.len()) {
        Some(head) if normcase(head) == normcase(root) => path[root.len()..].trim_start_matches(['\\', '/']),
        _ => path,
    }
}

/// Filename plus the folders it sits in, for keyword search ("Taxes 2024 landlord.pdf").
pub fn search_name(path: &str, root: &str) -> String {
    let rel: Vec<&str> = relative(path, root).split('\\').filter(|p| !p.is_empty() && *p != ".").collect();
    match rel.split_last() {
        Some((name, folders)) => std::iter::once(*name).chain(folders.iter().copied()).collect::<Vec<_>>().join(" "),
        None => String::new(),
    }
}

/// Title given to the model: the file name, with its folder when that adds meaning.
pub fn embed_title(path: &str, root: &str) -> String {
    let rel: Vec<&str> = relative(path, root).split('\\').filter(|p| !p.is_empty() && *p != ".").collect();
    match rel.len() {
        0 => String::new(),
        1 => rel[0].to_string(),
        n => format!("{}/{}", rel[n - 2], rel[n - 1]),
    }
}

/// Files whose chunks are embedded and saved together.
#[derive(Default)]
struct Group {
    entries: Vec<Entry>,
    chunks: Vec<Vec<Chunk>>,
    n_text: usize,
    n_image: usize,
}

impl Group {
    fn add(&mut self, mut entry: Entry, chunks: Vec<Chunk>) {
        entry.chunks = chunks.len() as i64;
        for c in &chunks {
            if c.image.is_some() {
                self.n_image += 1;
            } else {
                self.n_text += 1;
            }
        }
        self.entries.push(entry);
        self.chunks.push(chunks);
    }

    fn full(&self, scale: f64) -> bool {
        self.n_text as f64 >= config::GROUP_CHUNKS as f64 * scale || self.n_image >= 4.max((32.0 * scale) as usize)
    }

    fn embed(&self, embedder: &dyn Model) -> Result<Vec<VectorRow>, EmbedError> {
        let (mut texts, mut images, mut meta_t, mut meta_i) = (vec![], vec![], vec![], vec![]);
        for (entry, chunks) in self.entries.iter().zip(&self.chunks) {
            for (i, c) in chunks.iter().enumerate() {
                if let Some(img) = &c.image {
                    images.push(img);
                    meta_i.push((entry, i, &c.snippet));
                } else {
                    texts.push(c.text.clone().unwrap_or_default());
                    meta_t.push((entry, i, &c.snippet));
                }
            }
        }
        let mut rows = Vec::new();
        let tv = embedder.documents(&texts)?;
        let iv = embedder.images(&images)?;
        for (vecs, meta) in [(tv, meta_t), (iv, meta_i)] {
            for (v, (entry, i, snippet)) in vecs.into_iter().zip(meta) {
                rows.push(VectorRow {
                    path: entry.path.clone(),
                    folder_id: entry.folder_id,
                    chunk: i as i64,
                    kind: entry.kind.clone(),
                    text: snippet.clone(),
                    vector: v,
                });
            }
        }
        Ok(rows)
    }
}

/// Looking into archives and emails while walking a folder. One that hasn't changed since the
/// last scan isn't opened again: what's inside it is known, and so is why the rest isn't searched.
struct Containers {
    types: Types,
    fingerprint: String,
    saved: HashMap<String, ContainerRow>,  // normcase(path) -> what the last scan found
    members: HashMap<String, Vec<Found>>,  // normcase(path) -> what's known inside it
    seen: HashMap<String, ContainerRow>,   // normcase(path) -> what this scan found
}

impl Containers {
    fn new(types: Types, saved: HashMap<String, ContainerRow>, members: HashMap<String, Vec<Found>>) -> Containers {
        let fingerprint = types.fingerprint();
        Containers { types, fingerprint, saved, members, seen: HashMap::new() }
    }

    fn files(&mut self, path: &str, size: i64, mtime: f64, report: &mut Report) -> Vec<Found> {
        let key = normcase(path);
        if let Some(old) = self.saved.get(&key) {
            if old.size == size && (old.mtime - mtime).abs() <= 1e-3 && old.types == self.fingerprint {
                report.merge(&old.report);
                self.seen.insert(key.clone(), old.clone());
                return self.members.get(&key).cloned().unwrap_or_default();
            }
        }
        let types = &self.types;
        let mut found = Report::default();
        let mut out = Vec::new();
        archive::walk(
            path,
            mtime,
            &|name| types.inside(name),
            true, // (one level of archives inside archives)
            types.max_entries(),
            &mut |item| {
                if let Some(p) = item.problem {
                    found.note(p, &item.path);
                    return true;
                }
                let rest = item.path.get(path.len() + 1..).unwrap_or("");
                let parts: Vec<&str> = rest.split('\\').collect();
                if let Some(why) = skipped_part(types, &parts) {
                    found.note(why, &item.path);
                } else {
                    // (inside an archive only its name says what a file is)
                    match types.decide(&item.name, None) {
                        Ok(_) => out.push((item.path, item.size as i64, item.mtime)),
                        Err("archive") => {} // (its files come on their own)
                        Err(why) => found.add_type(why, &item.path),
                    }
                }
                true
            },
        );
        report.merge(&found);
        self.seen.insert(
            key,
            ContainerRow { path: path.to_string(), size, mtime, types: self.fingerprint.clone(), report: found },
        );
        out
    }
}

pub struct Indexer {
    store: Arc<Store>,
    pub embedder: Arc<dyn Model>,
    governor: Arc<Governor>,
    settings: Arc<Settings>,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,
    paused: AtomicBool,
    retry: Flag,
    waking: AtomicBool,
    removing: AtomicBool, // the thread that takes removed folders' files out of the index runs
    cancelled: Mutex<HashSet<i64>>,
    progress: Mutex<Map<String, Value>>,
    moved_to_gpu: Mutex<Option<Instant>>,
    vram_full_at: Mutex<Option<Instant>>, // when the graphics card last ran out of memory
    ram_low_since: Mutex<Option<Instant>>, // since when memory has been almost full
    // Folders paused on their card: their work waits, the others' goes on.
    paused_folders: Mutex<HashSet<i64>>,
    current_folder: Mutex<Option<i64>>,
    recent: Mutex<VecDeque<(Instant, usize)>>,
    cores: AtomicUsize, // the processor's cores, for reading files at once
    pipeline: Pipeline,
    pub watcher: OnceLock<Watcher>,
    me: Weak<Indexer>,
    #[cfg(test)]
    no_waiting: AtomicBool, // the waits between tries (2 s and more) take no time
    #[cfg(test)]
    fail_read: Mutex<Option<String>>, // reading a file whose path ends so fails
    #[cfg(test)]
    slow_read: std::sync::atomic::AtomicU64, // each read takes this many ms longer
    #[cfg(test)]
    reading: (AtomicUsize, AtomicUsize), // files being read now, and the most at once
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

impl Indexer {
    pub fn new(store: Arc<Store>, embedder: Arc<dyn Model>, governor: Arc<Governor>, settings: Arc<Settings>) -> Arc<Indexer> {
        let (tx, rx) = unbounded();
        let me = Arc::new_cyclic(|me: &Weak<Indexer>| Indexer {
            store,
            embedder,
            governor,
            settings,
            jobs_tx: tx,
            jobs_rx: rx,
            paused: AtomicBool::new(false),
            retry: Flag::new(),
            waking: AtomicBool::new(false),
            removing: AtomicBool::new(false),
            cancelled: Mutex::new(HashSet::new()),
            progress: Mutex::new(obj(json!({"state": "starting"}))),
            moved_to_gpu: Mutex::new(None),
            vram_full_at: Mutex::new(None),
            ram_low_since: Mutex::new(None),
            paused_folders: Mutex::new(HashSet::new()),
            current_folder: Mutex::new(None),
            recent: Mutex::new(VecDeque::with_capacity(120)),
            cores: AtomicUsize::new(crate::machine::profile().cores),
            pipeline: Pipeline::default(),
            watcher: OnceLock::new(),
            me: me.clone(),
            #[cfg(test)]
            no_waiting: AtomicBool::new(false),
            #[cfg(test)]
            fail_read: Mutex::new(None),
            #[cfg(test)]
            slow_read: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            reading: (AtomicUsize::new(0), AtomicUsize::new(0)),
        });
        let weak = Arc::downgrade(&me);
        me.embedder.set_pace(Box::new(move |_| {
            if let Some(ix) = weak.upgrade() {
                ix.pace();
            }
        }));
        let _ = me.watcher.set(Watcher::new(Arc::downgrade(&me)));
        me
    }

    fn arc(&self) -> Arc<Indexer> {
        self.me.upgrade().expect("the indexer")
    }

    pub fn watcher(&self) -> &Watcher {
        self.watcher.get().expect("the watcher")
    }

    // ---- public API (called from the server's threads) ---------------------------------------

    pub fn start(&self) {
        let me = self.arc();
        std::thread::Builder::new().name("indexer".into()).spawn(move || me.run()).ok();
        for f in self.store.folders() {
            self.enqueue_scan(f.id); // catch changes made while the app was closed
        }
        self.watcher().sync();
        self.finish_removals(); // (folders removed just before the app last closed)
    }

    pub fn enqueue_scan(&self, folder_id: i64) {
        self.cancelled.lock().remove(&folder_id);
        let _ = self.jobs_tx.send(Job::Scan(folder_id));
    }

    /// Changes the watcher saw. They're noted right away; reading the new content waits its turn.
    pub fn enqueue_paths(&self, paths: &[String]) {
        if !paths.is_empty() {
            for job in self.note_changes(paths) {
                let _ = self.jobs_tx.send(job);
            }
        }
    }

    /// The folder is gone for the page at once; its files leave the index in the background.
    pub fn remove_folder(&self, folder_id: i64) -> anyhow::Result<()> {
        self.cancelled.lock().insert(folder_id);
        self.paused_folders.lock().remove(&folder_id);
        self.store.begin_removing(folder_id);
        self.watcher().sync();
        self.finish_removals();
        Ok(())
    }

    /// Takes the files of removed folders out of the index, a step at a time, in a thread of its
    /// own (at most one), also for folders a restart interrupted.
    fn finish_removals(&self) {
        if self.store.removing().is_empty() || self.removing.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.arc();
        std::thread::Builder::new()
            .name("remove-folders".into())
            .spawn(move || {
                set_background_priority(true);
                loop {
                    let Some(id) = me.store.removing().first().copied() else { break };
                    let started = Instant::now();
                    loop {
                        match me.store.remove_folder_step(id, crate::store::REMOVE_STEP) {
                            Ok(true) => {
                                log::info!("A removed folder has left the index ({:.1} s)", started.elapsed().as_secs_f64());
                                break;
                            }
                            Ok(false) => {}
                            Err(e) => {
                                log::error!("Removing a folder's files failed: {e:#}");
                                std::thread::sleep(Duration::from_secs(5));
                            }
                        }
                    }
                }
                me.removing.store(false, Ordering::SeqCst);
                me.finish_removals(); // (one that came in just now)
            })
            .ok();
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn pause_folder(&self, folder_id: i64) {
        self.paused_folders.lock().insert(folder_id);
    }

    pub fn resume_folder(&self, folder_id: i64) {
        self.paused_folders.lock().remove(&folder_id);
        self.enqueue_scan(folder_id); // (picks up what's still pending, and what changed meanwhile)
    }

    pub fn paused_folders(&self) -> HashSet<i64> {
        self.paused_folders.lock().clone()
    }

    /// "Try again" after the model failed to load.
    pub fn retry_model(&self) {
        self.retry.set();
        if !self.embedder.ready() && self.embedder.status() == "error" {
            self.embedder.set_status("asleep");
            self.wake_model();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn progress(&self) -> Map<String, Value> {
        let mut p = self.progress.lock().clone();
        p.insert("queued".into(), json!(self.jobs_rx.len()));
        p.insert("paused".into(), json!(self.is_paused()));
        p.insert("files_per_min".into(), json!(self.files_per_min()));
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let ms = |c: &AtomicU64| n(c) / 1000;
        let t = &self.pipeline;
        p.insert("pipeline".into(), json!({
            "read_ms": ms(&t.read_us), "files": n(&t.files), "wait_ms": ms(&t.wait_us), "embed_ms": ms(&t.embed_us),
            "texts": n(&t.texts), "pictures": n(&t.pictures), "save_ms": ms(&t.save_us), "readers": self.readers(),
        }));
        p
    }

    fn set(&self, v: Value) {
        *self.progress.lock() = obj(v);
    }

    fn update(&self, v: Value) {
        let mut p = self.progress.lock();
        for (k, v) in obj(v) {
            p.insert(k, v);
        }
    }

    fn files_per_min(&self) -> Option<f64> {
        let r = self.recent.lock();
        if r.len() < 2 {
            return None;
        }
        let ((t0, n0), (t1, n1)) = (r[0], r[r.len() - 1]);
        let span = t1.duration_since(t0).as_secs_f64();
        if span < 5.0 || t1.elapsed().as_secs_f64() > 30.0 {
            return None;
        }
        Some(((n1 as f64 - n0 as f64) / span * 60.0 * 10.0).round() / 10.0)
    }

    // ---- pacing & device ----------------------------------------------------------------------

    /// Between pieces of work: wait while memory has run out, and while paused.
    fn pace(&self) {
        self.governor.pace(&|| self.is_paused() || self.folder_stopped(), Some(&|| self.manage_device()));
        self.wait_if_paused();
        // On the processor while the graphics card has room again: move now, not only after the
        // group (on the processor one group of pictures can take minutes); the group starts over
        // on the card.
        self.back_to_the_gpu();
    }

    /// The graphics card may take the model now: it has memory to spare, and didn't just run out.
    fn gpu_ok_now(&self) -> bool {
        if since(*self.vram_full_at.lock()) < BACK_TO_GPU_S {
            return false;
        }
        self.governor.metrics().vram_free_gb.is_none_or(|v| v >= GPU_NEEDED_GB)
    }

    /// The model started on the processor while the graphics card was busy: once the card is free
    /// and there's work, the model moves back to it.
    fn back_to_the_gpu(&self) {
        let e = &self.embedder;
        if e.ready() && !e.on_gpu() && since(*self.moved_to_gpu.lock()) > BACK_TO_GPU_S && e.could_use_gpu() && self.gpu_ok_now() {
            *self.moved_to_gpu.lock() = Some(Instant::now());
            e.unload("the graphics card is free again"); // (ensure_model starts it there)
        }
    }

    /// A wait between tries (in tests, none).
    fn nap(&self, d: Duration) {
        #[cfg(test)]
        if self.no_waiting.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(d);
    }

    fn wait_if_paused(&self) {
        while self.is_paused() && !self.folder_stopped() {
            self.manage_device();
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    /// The folder being indexed was removed or paused on its card.
    fn folder_stopped(&self) -> bool {
        match *self.current_folder.lock() {
            Some(f) => self.cancelled.lock().contains(&f) || self.paused_folders.lock().contains(&f),
            None => false,
        }
    }

    /// Lets the model go when memory runs out: the PC's, for a while (it may free up again), or the
    /// graphics card's (the model starts again on the processor, and moves back once the card has
    /// room). Otherwise it stays ready.
    fn manage_device(&self) {
        let e = &self.embedder;
        if !e.ready() {
            return;
        }
        if self.governor.decision().code == "ram_full" {
            let mut low = self.ram_low_since.lock();
            let start = *low.get_or_insert_with(Instant::now);
            if start.elapsed().as_secs_f64() >= RAM_RELEASE_AFTER_S {
                *low = None;
                drop(low);
                e.unload("memory almost full");
            }
            return;
        }
        *self.ram_low_since.lock() = None;
        if e.on_gpu() && (e.over_budget() || self.governor.metrics().vram_free_gb.is_some_and(|v| v < 0.3)) {
            *self.vram_full_at.lock() = Some(Instant::now());
            e.unload("GPU memory almost full");
        }
    }

    /// Load the model again after it went to sleep, in the background. Every search comes here.
    pub fn wake_model(&self) {
        let e = &self.embedder;
        if e.ready() || e.status() != "asleep" || self.waking.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.arc();
        std::thread::Builder::new()
            .name("wake-model".into())
            .spawn(move || {
                let prefer = me.gpu_ok_now();
                match me.embedder.load(prefer, None) {
                    Ok(()) | Err(LoadError::NotEnoughMemory { .. }) => {}
                    Err(e) => log::error!("Could not wake the model: {e}"),
                }
                me.waking.store(false, Ordering::SeqCst);
            })
            .ok();
    }

    /// New files to index while the model sleeps: wake it first, once no game is running and
    /// there's memory for it to spare.
    fn ensure_model(&self) -> Result<(), Stop> {
        let mut waiting = false;
        let result = (|| {
            while !self.embedder.ready() {
                if self.folder_stopped() {
                    return Err(Stop);
                }
                match self.embedder.load(self.gpu_ok_now(), Some(INDEX_SPARE_GB)) {
                    Ok(()) => {}
                    Err(LoadError::NotEnoughMemory { free_gb, .. }) => {
                        if !waiting {
                            self.update(json!({"note": {"key": "note.waiting_ram", "params": {"gb": (free_gb * 10.0).round() / 10.0}}}));
                            waiting = true;
                        }
                        self.nap(Duration::from_secs(5));
                    }
                    Err(LoadError::Failed(_)) => {
                        let message = self.embedder.error();
                        self.wait_for_retry(message);
                    }
                }
            }
            Ok(())
        })();
        if waiting {
            self.update(json!({"note": null}));
        }
        result
    }

    /// The model doesn't work: say so, and wait for "try again" (the files aren't to blame).
    fn wait_for_retry(&self, message: Option<String>) {
        let before = self.progress.lock().clone();
        self.set(json!({"state": "model-error", "message": message}));
        self.retry.clear();
        self.retry.wait();
        *self.progress.lock() = before;
    }

    // ---- worker -----------------------------------------------------------------------------------

    /// The model is made ready for searches from the start, if there's memory for it.
    fn load_at_start(&self) {
        while !self.embedder.ready() {
            self.set(json!({"state": "loading-model"}));
            match self.embedder.load(self.gpu_ok_now(), None) {
                Ok(()) => {}
                Err(LoadError::NotEnoughMemory { .. }) => {
                    self.embedder.set_status("asleep"); // (it loads once there's work and room, or a search)
                    return;
                }
                Err(LoadError::Failed(_)) => {
                    let message = self.embedder.error();
                    self.set(json!({"state": "model-error", "message": message}));
                    self.retry.clear();
                    self.retry.wait();
                }
            }
        }
    }

    fn run(&self) {
        set_background_priority(true); // whatever else runs on the PC comes first
        // (before the scans queued at start, which then read them again)
        match self.store.reread_documents() {
            Ok(0) => {}
            Ok(n) => log::info!("{n} documents are read again: their text comes out better now"),
            Err(e) => log::warn!("Couldn't look for documents to read again: {e:#}"),
        }
        self.load_at_start();
        loop {
            self.set(json!({"state": "idle"}));
            let job = match self.jobs_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(j) => j,
                Err(_) => {
                    self.manage_device();
                    continue;
                }
            };
            let me = self.arc();
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| me.run_job(job)));
            if let Err(e) = done {
                let text = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                log::error!("A job failed: {text}");
                self.set(json!({"state": "error", "message": format!("panic: {text}")}));
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }

    fn run_job(&self, job: Job) {
        let result = match job {
            Job::Scan(folder_id) => self.scan_folder(folder_id),
            Job::Index(folder_id, root, files) => self.index_noted(folder_id, &root, files),
            Job::Remove(paths) => {
                // Again, in turn: the job before may have read one of them just before it went.
                let gone: Vec<String> = paths.into_iter().filter(|p| !Path::new(p).exists()).collect();
                self.store.remove_paths(&gone)
            }
        };
        if let Err(e) = result {
            log::error!("A job failed: {e:#}");
            self.set(json!({"state": "error", "message": format!("{e:#}")}));
            self.nap(Duration::from_secs(2));
        }
    }

    /// What's searched, as Settings -> File types say.
    pub fn types(&self) -> Types {
        Types::of(&self.settings)
    }

    /// The files below root that are searched, also inside archives and emails, to `out`; note in
    /// `report` why everything else isn't.
    fn walk(&self, root: &str, folder_id: i64, report: &mut Report, types: &Types, containers: &mut Containers, out: &mut dyn FnMut(Found)) {
        let data_dir = config::DATA_DIR.to_string_lossy().to_string();
        let mut stack = vec![root.to_string()];
        while let Some(d) = stack.pop() {
            if self.cancelled.lock().contains(&folder_id) {
                return;
            }
            let here = file_name(d.trim_end_matches(['\\', '/'])).to_string();
            let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&d) {
                Ok(it) => it.flatten().collect(),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    report.note("no_access_dir", &d);
                    continue;
                }
                Err(_) => {
                    report.note("unreadable_dir", &d);
                    continue;
                }
            };
            for e in entries {
                let name = e.file_name().to_string_lossy().to_string();
                let path = format!("{}\\{}", d.trim_end_matches(['\\', '/']), name);
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    report.note("link", &path); // it could lead anywhere, or in circles
                    continue;
                }
                let Ok(meta) = e.metadata() else { continue };
                let (hidden, system) = attributes(&meta);
                if ft.is_dir() {
                    if let Some(why) = types.skip_dir(&name, hidden, system, &here) {
                        report.note(why, &path);
                    } else if !types.rule("program_folders") && Path::new(&path).join("CACHEDIR.TAG").is_file() {
                        report.note("program_folder", &path); // a cache, marked as one by the program that made it
                    } else if !is_inside(&path, &data_dir) {
                        stack.push(path);
                    }
                    continue;
                }
                if let Some(why) = types.skip_file(&name, hidden, system) {
                    report.note(why, &path);
                    continue;
                }
                let (read, inside) = match types.decide(&name, Some(&path)) {
                    Ok(_) => (true, types.inside(&name)), // (an email: read, and its attachments too)
                    Err("archive") => (false, true),
                    Err(why) => {
                        report.add_type(why, &path);
                        continue;
                    }
                };
                let (size, mtime) = (meta.len() as i64, mtime_of(&meta));
                if read {
                    out((path.clone(), size, mtime));
                }
                if inside {
                    for f in containers.files(&path, size, mtime, report) {
                        out(f);
                    }
                }
            }
        }
    }

    fn scan_folder(&self, folder_id: i64) -> anyhow::Result<()> {
        let Some(folder) = self.store.folder(folder_id) else { return Ok(()) };
        if self.cancelled.lock().contains(&folder_id) || self.paused_folders.lock().contains(&folder_id) {
            return Ok(());
        }
        let root = folder.path.clone();
        self.set(json!({"state": "scanning", "folder": root, "found": 0}));
        if !Path::new(&root).is_dir() {
            self.set(json!({"state": "error", "message": format!("Folder not found: {root}")}));
            self.nap(Duration::from_secs(2));
            return Ok(());
        }
        let types = self.types();
        let known = self.store.file_signatures(folder_id);
        let known_ci: HashMap<String, (i64, i64, f64, String)> = known.iter().map(|(p, v)| (normcase(p), v.clone())).collect();
        let mut containers = Containers::new(types.clone(), self.store.containers(folder_id), by_container(&known));
        let mut report = Report::default();
        let mut seen = HashSet::new();
        let mut todo: Vec<Found> = Vec::new();
        let mut n = 0usize;
        self.walk(&root, folder_id, &mut report, &types, &mut containers, &mut |(path, size, mtime)| {
            n += 1;
            let key = normcase(&path);
            // New or changed, not finished last time, or failed before (maybe readable now).
            let changed = match known_ci.get(&key) {
                None => true,
                Some(sig) => sig.1 != size || (sig.2 - mtime).abs() > 1e-3 || sig.3 == "pending" || sig.3 == "error",
            };
            seen.insert(key);
            if changed {
                todo.push((path, size, mtime));
            }
            if n % 500 == 0 {
                self.update(json!({"found": n}));
            }
        });
        if self.cancelled.lock().contains(&folder_id) {
            return Ok(());
        }
        let gone: Vec<i64> = known_ci.iter().filter(|(k, _)| !seen.contains(*k)).map(|(_, v)| v.0).collect();
        if !gone.is_empty() {
            self.store.remove_file_ids(&gone)?;
        }
        // Everything found is searchable by name right away; by content as indexing gets to it.
        let pending: Vec<(String, i64, f64, String, String)> = todo
            .iter()
            .map(|(p, size, mtime)| {
                let kind = types.kind(file_name(p), None).unwrap_or("text").to_string();
                (p.clone(), *size, *mtime, kind, search_name(p, &root))
            })
            .collect();
        self.store.add_pending(folder_id, &pending)?;
        self.index_files(folder_id, &root, todo, Some(types))?;
        if !self.cancelled.lock().contains(&folder_id) {
            let rows: Vec<ContainerRow> = containers.seen.into_values().collect();
            self.store.save_containers(folder_id, &rows, true)?;
            self.store.mark_scanned(folder_id, Some(&report));
        }
        Ok(())
    }

    /// Write down what the watcher saw, at once: new files are searchable by name and count as
    /// waiting, deleted ones disappear. Returns the jobs that read the new content, in turn.
    fn note_changes(&self, paths: &[String]) -> Vec<Job> {
        let folders: Vec<_> = self.store.folders().into_iter().filter(|f| f.watch != 0).collect();
        let types = self.types();
        let data_dir = config::DATA_DIR.to_string_lossy().to_string();
        let mut removed = Vec::new();
        let mut by_folder: Vec<(i64, String, Vec<Found>)> = Vec::new();
        let mut unique = HashSet::new();
        for p in paths {
            if !unique.insert(p.clone()) {
                continue;
            }
            let Some(folder) = folders.iter().find(|f| is_inside(p, &f.path)) else { continue };
            if is_inside(p, &data_dir) {
                continue;
            }
            let rel = relative(p, &folder.path);
            let parts: Vec<&str> = rel.split('\\').filter(|x| !x.is_empty()).collect();
            if !parts.is_empty() && skipped_part(&types, &parts).is_some() {
                continue;
            }
            let slot = match by_folder.iter().position(|(id, _, _)| *id == folder.id) {
                Some(i) => i,
                None => {
                    by_folder.push((folder.id, folder.path.clone(), Vec::new()));
                    by_folder.len() - 1
                }
            };
            let path = Path::new(p);
            if !path.exists() {
                removed.push(p.clone());
            } else if path.is_dir() {
                // A folder appeared (e.g. moved in): index everything below it.
                let mut report = Report::default();
                let mut containers = Containers::new(types.clone(), HashMap::new(), HashMap::new());
                let mut found = Vec::new();
                self.walk(p, folder.id, &mut report, &types, &mut containers, &mut |f| found.push(f));
                by_folder[slot].2.extend(found.into_iter().filter(|f| self.changed(f)));
            } else {
                let name = file_name(p);
                let kind = types.kind(name, Some(p));
                let inside = types.inside(name);
                if kind.is_none() && !inside {
                    continue;
                }
                let Ok(meta) = std::fs::metadata(p) else { continue };
                let (hidden, system) = attributes(&meta);
                if types.skip_file(name, hidden, system).is_some() {
                    continue;
                }
                let f = (p.clone(), meta.len() as i64, mtime_of(&meta));
                if kind.is_some() && self.changed(&f) {
                    by_folder[slot].2.push(f.clone());
                }
                if inside {
                    // An archive or an email changed: index what's new or changed inside it, and
                    // forget what's gone.
                    let mut containers = Containers::new(types.clone(), HashMap::new(), HashMap::new());
                    let inner = containers.files(p, f.1, f.2, &mut Report::default());
                    let keep: HashSet<String> = inner.iter().map(|x| normcase(&x.0)).collect();
                    let stale: Vec<i64> = self
                        .store
                        .files_inside(p)
                        .into_iter()
                        .filter(|(_, q)| !keep.contains(&normcase(q)))
                        .map(|(id, _)| id)
                        .collect();
                    if !stale.is_empty() {
                        let _ = self.store.remove_file_ids(&stale);
                    }
                    by_folder[slot].2.extend(inner.into_iter().filter(|x| self.changed(x)));
                    let rows: Vec<ContainerRow> = containers.seen.into_values().collect();
                    let _ = self.store.save_containers(folder.id, &rows, false);
                }
            }
        }
        let mut jobs = Vec::new();
        if !removed.is_empty() {
            let _ = self.store.remove_paths(&removed);
            jobs.push(Job::Remove(removed));
        }
        for (folder_id, root, mut files) in by_folder {
            // A new folder's walk and its files' own events name the same files: once each.
            let mut once = HashSet::new();
            files.retain(|f| once.insert(normcase(&f.0)));
            if !files.is_empty() {
                let pending: Vec<(String, i64, f64, String, String)> = files
                    .iter()
                    .map(|(p, size, mtime)| {
                        let kind = types.kind(file_name(p), None).unwrap_or("text").to_string();
                        (p.clone(), *size, *mtime, kind, search_name(p, &root))
                    })
                    .collect();
                let _ = self.store.add_pending(folder_id, &pending);
                jobs.push(Job::Index(folder_id, root, files));
            }
        }
        jobs
    }

    /// Read the files the watcher noted, unless they went away or a scan got to them first.
    fn index_noted(&self, folder_id: i64, root: &str, files: Vec<Found>) -> anyhow::Result<()> {
        if self.cancelled.lock().contains(&folder_id) || self.paused_folders.lock().contains(&folder_id) {
            return Ok(());
        }
        let todo: Vec<Found> = files
            .into_iter()
            .filter(|(path, size, mtime)| {
                self.store
                    .signature(path)
                    .is_some_and(|sig| sig.3 == "pending" || sig.1 != *size || (sig.2 - mtime).abs() > 1e-3)
            })
            .collect();
        self.index_files(folder_id, root, todo, None)
    }

    fn changed(&self, f: &Found) -> bool {
        match self.store.signature(&f.0) {
            None => true,
            Some(sig) => sig.1 != f.1 || (sig.2 - f.2).abs() > 1e-3,
        }
    }

    fn index_files(&self, folder_id: i64, root: &str, mut files: Vec<Found>, types: Option<Types>) -> anyhow::Result<()> {
        let mut once = HashSet::new();
        files.retain(|f| once.insert(normcase(&f.0))); // (a file saved twice in one group would clash)
        if files.is_empty() {
            return Ok(());
        }
        let types = types.unwrap_or_else(|| self.types());
        let total = files.len();
        *self.current_folder.lock() = Some(folder_id);
        self.set(json!({"state": "indexing", "folder": root, "done": 0, "total": total, "current": ""}));
        self.recent.lock().clear();
        // Reading and decoding files (CPU) runs ahead of embedding (GPU) in its own thread, so the
        // GPU isn't left waiting between groups.
        let (ready_tx, ready_rx) = bounded::<Option<(Entry, Vec<Chunk>)>>(PREFETCH_FILES);
        let stop = Arc::new(AtomicBool::new(false));
        {
            let me = self.arc();
            let (root, stop) = (root.to_string(), stop.clone());
            std::thread::Builder::new()
                .name("extract".into())
                .spawn(move || me.extract_ahead(folder_id, &root, files, types, ready_tx, stop))
                .ok();
        }
        let result = (|| -> Result<anyhow::Result<()>, Stop> {
            let mut group = Group::default();
            for done in 0..total {
                self.wait_if_paused();
                if self.folder_stopped() {
                    return Err(Stop);
                }
                let Some((entry, chunks)) = self.take(&ready_rx) else {
                    return Err(Stop); // the reader stopped early: the folder was removed
                };
                {
                    let mut r = self.recent.lock();
                    if r.len() == 120 {
                        r.pop_front();
                    }
                    r.push_back((Instant::now(), done));
                }
                let rate = self.files_per_min();
                self.update(json!({
                    "done": done, "current": entry.path,
                    "eta_s": rate.filter(|r| *r > 0.0).map(|r| ((total - done) as f64 / r * 60.0).round() as i64),
                }));
                group.add(entry, chunks);
                if group.full(self.group_scale()) {
                    if let Err(e) = self.flush(std::mem::take(&mut group))? {
                        return Ok(Err(e));
                    }
                }
            }
            if let Err(e) = self.flush(group)? {
                return Ok(Err(e));
            }
            self.update(json!({"done": total, "current": "", "eta_s": null}));
            Ok(Ok(()))
        })();
        *self.current_folder.lock() = None;
        stop.store(true, Ordering::SeqCst);
        while ready_rx.try_recv().is_ok() {} // let the reader go if it's waiting to hand over a file
        match result {
            Ok(r) => r,
            Err(Stop) => Ok(()), // (switched off, or the folder removed or paused: the rest stays pending)
        }
    }

    /// The next file from the reader. While waiting for it, hand the GPU back when video memory
    /// runs out, just as during embedding.
    fn take(&self, ready: &Receiver<Option<(Entry, Vec<Chunk>)>>) -> Option<(Entry, Vec<Chunk>)> {
        loop {
            let waiting = Instant::now();
            let got = ready.recv_timeout(Duration::from_millis(500));
            add_time(&self.pipeline.wait_us, waiting);
            match got {
                Ok(item) => return item,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return None,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
            self.wait_if_paused();
            if self.governor.duty() <= 0.0 {
                self.manage_device();
            }
        }
    }

    /// The reading side of the pipeline: files in, (entry, chunks) out, as they're ready. Files in
    /// archives and emails are read one after another, so their containers stay open; the others
    /// by as many readers as the PC has cores to spare (see readers()).
    fn extract_ahead(
        &self,
        folder_id: i64,
        root: &str,
        files: Vec<Found>,
        types: Types,
        ready: Sender<Option<(Entry, Vec<Chunk>)>>,
        stop: Arc<AtomicBool>,
    ) {
        let (inside, plain): (Vec<Found>, Vec<Found>) = files.into_iter().partition(|f| in_container(&f.0));
        let next = AtomicUsize::new(0); // the next plain file to read
        let (types, ready, stop) = (&types, &ready, &*stop);
        std::thread::scope(|s| {
            if !inside.is_empty() {
                let _ = std::thread::Builder::new().name("extract-containers".into()).spawn_scoped(s, || {
                    let _session = archive::session(); // archives stay open while their files are read one by one
                    set_background_priority(true);
                    for f in &inside {
                        if !self.read_ahead(folder_id, root, f, types, ready, stop) {
                            break;
                        }
                    }
                });
            }
            for i in 0..MAX_READERS.min(plain.len()) {
                let (plain, next) = (&plain, &next);
                let _ = std::thread::Builder::new().name(format!("extract-{i}")).spawn_scoped(s, move || {
                    set_background_priority(true);
                    loop {
                        // Only as many readers as there's room for now; the first always reads.
                        while i >= self.readers() {
                            if stop.load(Ordering::SeqCst) || next.load(Ordering::SeqCst) >= plain.len() {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                        let k = next.fetch_add(1, Ordering::SeqCst);
                        if k >= plain.len() || !self.read_ahead(folder_id, root, &plain[k], types, ready, stop) {
                            return;
                        }
                    }
                });
            }
        });
        // "No more files", even after an error, or the indexer would wait forever (unless it has
        // stopped already).
        while !stop.load(Ordering::SeqCst) {
            match ready.send_timeout(None, Duration::from_millis(250)) {
                Ok(()) | Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => break,
                Err(crossbeam_channel::SendTimeoutError::Timeout(_)) => {}
            }
        }
    }

    /// How many files are read at once: one while the model works on the processor (it needs the
    /// cores) or memory is tight; else every core but one, for the model's own work.
    fn readers(&self) -> usize {
        if !self.embedder.on_gpu() || self.group_scale() < 1.0 {
            return 1;
        }
        self.cores.load(Ordering::Relaxed).saturating_sub(1).clamp(1, MAX_READERS)
    }

    /// Reads one file and hands it to the indexer. False when reading should stop.
    fn read_ahead(
        &self,
        folder_id: i64,
        root: &str,
        (path, size, mtime): &Found,
        types: &Types,
        ready: &Sender<Option<(Entry, Vec<Chunk>)>>,
        stop: &AtomicBool,
    ) -> bool {
        // Nothing to read ahead for while indexing waits (paused, memory full, ...).
        while !stop.load(Ordering::SeqCst) && (self.is_paused() || self.governor.duty() <= 0.0) {
            std::thread::sleep(Duration::from_millis(250));
        }
        if stop.load(Ordering::SeqCst) || self.cancelled.lock().contains(&folder_id) || self.paused_folders.lock().contains(&folder_id) {
            return false;
        }
        let reading = Instant::now();
        let item = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.read(folder_id, root, path, *size, *mtime, types)))
            .unwrap_or_else(|_| {
                let mut entry = self.entry(folder_id, root, path, *size, *mtime, "text");
                entry.status = "error".into();
                entry.error = Some("Error: the reader failed".into());
                (entry, Vec::new())
            });
        add_time(&self.pipeline.read_us, reading);
        self.pipeline.files.fetch_add(1, Ordering::Relaxed);
        self.governor.pace(&|| stop.load(Ordering::SeqCst) || self.is_paused(), None); // (memory running out)
        let mut item = Some(item);
        while !stop.load(Ordering::SeqCst) {
            if ready.len() >= PREFETCH_TIGHT && self.group_scale() < 1.0 {
                std::thread::sleep(Duration::from_millis(100)); // memory is tight: hold fewer files ahead
                continue;
            }
            match ready.send_timeout(item.take().map(Some).unwrap(), Duration::from_millis(250)) {
                Ok(()) => return true,
                Err(crossbeam_channel::SendTimeoutError::Timeout(back)) => item = back,
                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return false,
            }
        }
        false
    }

    fn entry(&self, folder_id: i64, root: &str, path: &str, size: i64, mtime: f64, kind: &str) -> Entry {
        Entry {
            folder_id,
            path: path.to_string(),
            size,
            mtime,
            kind: kind.to_string(),
            status: "indexed".into(),
            chunks: 0,
            error: None,
            search_name: search_name(path, root),
        }
    }

    fn read(&self, folder_id: i64, root: &str, path: &str, size: i64, mtime: f64, types: &Types) -> (Entry, Vec<Chunk>) {
        #[cfg(test)]
        let _reading = {
            struct Reading<'a>(&'a AtomicUsize);
            impl Drop for Reading<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let now = self.reading.0.fetch_add(1, Ordering::SeqCst) + 1;
            self.reading.1.fetch_max(now, Ordering::SeqCst);
            let guard = Reading(&self.reading.0);
            std::thread::sleep(Duration::from_millis(self.slow_read.load(Ordering::SeqCst)));
            if self.fail_read.lock().as_ref().is_some_and(|end| path.ends_with(end.as_str())) {
                panic!("out of memory while reading");
            }
            guard
        };
        let name = file_name(path);
        let known = types.kind(name, None); // (None: extract looks at what's inside)
        let kind = known.unwrap_or("text");
        let mut entry = self.entry(folder_id, root, path, size, mtime, kind);
        let title = embed_title(path, root);
        let result: Result<extract::Extracted, ReadError> = if !Path::new(path).exists() && archive::split(path).is_some() {
            // inside an archive or an email
            let limit = max_bytes(kind, name);
            if size as u64 > limit {
                Err(ReadError::Skip("too_large"))
            } else {
                match archive::extracted(path, limit) {
                    Ok(copy) => extract(&copy.file, size as u64, Some(&title), known),
                    Err(e) => match e.downcast_ref::<archive::ArchiveError>() {
                        Some(archive::ArchiveError::Encrypted) => Err(ReadError::Skip("password")),
                        Some(archive::ArchiveError::TooLarge) => Err(ReadError::Skip("too_large")),
                        Some(archive::ArchiveError::NotFound) => Err(ReadError::Io(std::io::ErrorKind::NotFound.into())),
                        Some(_) => Err(ReadError::Damaged("archive")),
                        None => Err(ReadError::from(e)),
                    },
                }
            }
        } else {
            extract(Path::new(path), size as u64, Some(&title), known)
        };
        match result {
            Ok(ex) => {
                entry.kind = ex.kind;
                (entry, ex.chunks)
            }
            Err(ReadError::Skip(key)) => {
                entry.status = "skipped".into();
                entry.error = Some(format!("!{key}"));
                (entry, Vec::new())
            }
            Err(e) => {
                entry.status = "error".into();
                entry.error = Some(problem(&e));
                (entry, Vec::new())
            }
        }
    }

    fn group_scale(&self) -> f64 {
        // Fewer files (and decoded images) held in memory when RAM is tight.
        if self.governor.metrics().ram_free_gb < 2.5 { 0.25 } else { 1.0 }
    }

    fn flush(&self, mut group: Group) -> Result<anyhow::Result<()>, Stop> {
        if group.entries.is_empty() {
            return Ok(Ok(()));
        }
        let embedding = Instant::now();
        let embedded = self.embed(&group);
        add_time(&self.pipeline.embed_us, embedding);
        self.pipeline.texts.fetch_add(group.n_text as u64, Ordering::Relaxed);
        self.pipeline.pictures.fetch_add(group.n_image as u64, Ordering::Relaxed);
        let rows = match embedded? {
            Some(rows) => rows,
            None => {
                // Isolate the file that broke the batch instead of failing them all.
                let mut rows = Vec::new();
                let entries = std::mem::take(&mut group.entries);
                let chunks = std::mem::take(&mut group.chunks);
                for (mut entry, chunks) in entries.into_iter().zip(chunks) {
                    let mut single = Group::default();
                    single.add(entry.clone(), chunks);
                    match self.embed(&single)? {
                        None => {
                            entry.status = "error".into();
                            entry.chunks = 0;
                            entry.error = Some("!embed_failed".into());
                        }
                        Some(one) => rows.extend(one),
                    }
                    group.entries.push(entry);
                }
                rows
            }
        };
        let saving = Instant::now();
        let saved = self.store.save_files(&group.entries, &rows);
        add_time(&self.pipeline.save_us, saving);
        Ok(saved)
    }

    /// Embed a group, waiting out memory shortages and engine trouble. None only when a file in the
    /// group can't be prepared for the model (the caller finds which).
    fn embed(&self, group: &Group) -> Result<Option<Vec<VectorRow>>, Stop> {
        if group.chunks.iter().all(|c| c.is_empty()) {
            return Ok(Some(Vec::new())); // only skipped files: no need to touch the model
        }
        let (mut attempts, mut failures) = (0u64, 0u64);
        loop {
            if self.folder_stopped() {
                return Err(Stop);
            }
            self.back_to_the_gpu();
            self.ensure_model()?;
            let vram = self.governor.metrics().vram_free_gb;
            self.embedder.set_batch_scale(match vram {
                None => 1.0,
                Some(v) if v >= 2.0 => 1.0,
                Some(v) if v >= 1.0 => 0.5,
                Some(_) => 0.25,
            });
            let e = &self.embedder;
            let rows = match group.embed(e.as_ref()) {
                Ok(rows) => Some(rows),
                Err(EmbedError::ModelAsleep) => continue, // let go meanwhile: started again above
                Err(EmbedError::GpuBusy(_)) => {
                    attempts += 1;
                    let key = if e.on_gpu() { "note.vram_full" } else { "note.ram_full" };
                    self.update(json!({"note": {"key": key, "params": {}}}));
                    if e.on_gpu() && attempts >= 2 {
                        e.avoid_gpu("the graphics card keeps running out of memory");
                    }
                    self.nap(Duration::from_secs((10 * attempts).min(60)));
                    continue;
                }
                Err(EmbedError::Engine(err)) => {
                    failures += 1;
                    log::warn!("The model failed ({err})");
                    if failures >= 3 {
                        e.unload("it failed three times");
                        self.wait_for_retry(Some(err));
                        failures = 0;
                    } else {
                        self.nap(Duration::from_secs(10 * failures));
                    }
                    continue;
                }
                Err(EmbedError::BadInput(err)) => {
                    log::info!("A file can't be prepared for the model: {err}");
                    None
                }
            };
            self.update(json!({"note": null}));
            return Ok(rows);
        }
    }
}

// ---- the file watcher ----------------------------------------------------------------------------

const DEBOUNCE_S: f64 = 2.0;

/// Collects file-system events and hands them to the indexer after they settle.
pub struct Watcher {
    indexer: Weak<Indexer>,
    inner: Mutex<Option<notify::RecommendedWatcher>>,
    watches: Mutex<HashMap<i64, String>>,
    pending: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Watcher {
    fn new(indexer: Weak<Indexer>) -> Watcher {
        let pending: Arc<Mutex<HashMap<String, Instant>>> = Arc::new(Mutex::new(HashMap::new()));
        let p = pending.clone();
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            use notify::EventKind::*;
            match event.kind {
                Create(_) | Remove(_) => {}
                Modify(notify::event::ModifyKind::Name(_)) => {}
                Modify(_) => {
                    // A folder's "modified" fires whenever a child changes; the child event covers it.
                    if event.paths.iter().all(|x| x.is_dir()) {
                        return;
                    }
                }
                _ => return,
            }
            let now = Instant::now();
            let mut pending = p.lock();
            for path in event.paths {
                pending.insert(config::strip_verbatim(path).to_string_lossy().to_string(), now);
            }
        })
        .map_err(|e| log::error!("Cannot watch folders: {e}"))
        .ok();
        let w = Watcher { indexer: indexer.clone(), inner: Mutex::new(inner), watches: Mutex::new(HashMap::new()), pending };
        let (p, ix) = (w.pending.clone(), indexer);
        std::thread::Builder::new()
            .name("watcher".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    let ready: Vec<String> = {
                        let mut pending = p.lock();
                        let ready: Vec<String> =
                            pending.iter().filter(|(_, t)| t.elapsed().as_secs_f64() >= DEBOUNCE_S).map(|(k, _)| k.clone()).collect();
                        for k in &ready {
                            pending.remove(k);
                        }
                        ready
                    };
                    let Some(indexer) = ix.upgrade() else { return };
                    if !ready.is_empty() {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| indexer.enqueue_paths(&ready)));
                        if r.is_err() {
                            log::error!("Could not note {} changed paths", ready.len());
                        }
                    }
                }
            })
            .ok();
        w
    }

    /// Watch the folders that want it.
    pub fn sync(&self) {
        use notify::Watcher as _;
        let Some(indexer) = self.indexer.upgrade() else { return };
        let wanted: HashMap<i64, String> =
            indexer.store.folders().into_iter().filter(|f| f.watch != 0).map(|f| (f.id, f.path)).collect();
        let mut inner = self.inner.lock();
        let Some(w) = inner.as_mut() else { return };
        let mut watches = self.watches.lock();
        let gone: Vec<i64> = watches.keys().filter(|id| !wanted.contains_key(id)).copied().collect();
        for id in gone {
            if let Some(path) = watches.remove(&id) {
                let _ = w.unwatch(Path::new(&path));
            }
        }
        for (id, path) in wanted {
            if !watches.contains_key(&id) && Path::new(&path).is_dir() {
                match w.watch(Path::new(&path), notify::RecursiveMode::Recursive) {
                    Ok(()) => {
                        watches.insert(id, path);
                    }
                    Err(e) => log::error!("Cannot watch {path}: {e}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_titles() {
        assert_eq!(search_name(r"C:\Docs\Taxes\2024\landlord.pdf", r"C:\Docs"), "landlord.pdf Taxes 2024");
        assert_eq!(embed_title(r"C:\Docs\Taxes\2024\landlord.pdf", r"C:\Docs"), "2024/landlord.pdf");
        assert_eq!(embed_title(r"C:\Docs\a.txt", r"C:\Docs\"), "a.txt");
        assert!(is_inside(r"C:\Docs\x", r"c:\docs"));
        assert!(!is_inside(r"C:\Docs2\x", r"C:\Docs"));
    }

    #[test]
    fn groups_fill_up() {
        let mut g = Group::default();
        let entry = Entry {
            folder_id: 1, path: "x".into(), size: 1, mtime: 0.0, kind: "text".into(), status: "indexed".into(),
            chunks: 0, error: None, search_name: String::new(),
        };
        let chunks: Vec<Chunk> = (0..128).map(|_| Chunk { snippet: String::new(), text: Some("a".into()), image: None }).collect();
        g.add(entry, chunks);
        assert!(g.full(1.0));
        assert_eq!(g.entries[0].chunks, 128);
    }

    // ---- an indexer with a stand-in model and a governor that sees what a test sets ----------

    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::AtomicUsize;

    /// Stands in for the model: every text and picture becomes the same unit vector.
    struct Fake {
        ready: AtomicBool,
        status: Mutex<String>,
        on_gpu: AtomicBool,
        could_use_gpu: AtomicBool,
        over_budget: AtomicBool,
        pace: Mutex<Option<Box<dyn Fn(f64) + Send + Sync>>>,
        after_first: Mutex<Option<Box<dyn Fn() + Send + Sync>>>, // runs after the first item it embeds
        unloaded: Mutex<Vec<String>>,
        loads: AtomicUsize,
        preferred: Mutex<Vec<bool>>,
        failures: Mutex<VecDeque<EmbedError>>, // the first calls fail like this
        picky: Option<&'static str>,           // texts with this in them can't be prepared
        room: AtomicBool,                      // false: too little memory to start
        spare: Mutex<Option<f64>>,
        on_load: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    }

    impl Fake {
        fn new(on_gpu: bool) -> Fake {
            Fake {
                ready: AtomicBool::new(true),
                status: Mutex::new("ready".into()),
                on_gpu: AtomicBool::new(on_gpu),
                could_use_gpu: AtomicBool::new(on_gpu),
                over_budget: AtomicBool::new(false),
                pace: Mutex::new(None),
                after_first: Mutex::new(None),
                unloaded: Mutex::new(Vec::new()),
                loads: AtomicUsize::new(0),
                preferred: Mutex::new(Vec::new()),
                failures: Mutex::new(VecDeque::new()),
                picky: None,
                room: AtomicBool::new(true),
                spare: Mutex::new(None),
                on_load: Mutex::new(None),
            }
        }

        /// Not loaded (yet, or any more).
        fn asleep(self, status: &str) -> Fake {
            self.ready.store(false, Ordering::SeqCst);
            *self.status.lock() = status.into();
            self
        }

        /// Like the real model: a pause for the indexer after each item; let go meanwhile, the rest
        /// waits for it to start again.
        fn items(&self, n: usize) -> Result<(), EmbedError> {
            for k in 0..n {
                if !self.ready() {
                    return Err(EmbedError::ModelAsleep);
                }
                if k == 0 {
                    if let Some(f) = self.after_first.lock().take() {
                        f();
                    }
                }
                if let Some(p) = self.pace.lock().as_ref() {
                    p(0.0);
                }
            }
            if self.ready() { Ok(()) } else { Err(EmbedError::ModelAsleep) }
        }

        fn unloaded(&self) -> Vec<String> {
            self.unloaded.lock().clone()
        }

        fn loads(&self) -> usize {
            self.loads.load(Ordering::SeqCst)
        }

        fn preferred(&self) -> Vec<bool> {
            self.preferred.lock().clone()
        }
    }

    fn unit() -> Vec<f32> {
        vec![1.0 / (config::EMBED_DIM as f32).sqrt(); config::EMBED_DIM]
    }

    impl Model for Fake {
        fn ready(&self) -> bool {
            self.ready.load(Ordering::SeqCst)
        }
        fn status(&self) -> String {
            self.status.lock().clone()
        }
        fn set_status(&self, status: &str) {
            *self.status.lock() = status.into();
        }
        fn error(&self) -> Option<String> {
            None
        }
        fn load(&self, prefer_gpu: bool, spare_gb: Option<f64>) -> Result<(), LoadError> {
            *self.spare.lock() = spare_gb;
            if let Some(f) = self.on_load.lock().as_ref() {
                f();
            }
            if !self.room.load(Ordering::SeqCst) {
                return Err(LoadError::NotEnoughMemory { free_gb: 0.6, need_gb: 1.4 });
            }
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.preferred.lock().push(prefer_gpu);
            self.on_gpu.store(prefer_gpu && self.could_use_gpu.load(Ordering::SeqCst), Ordering::SeqCst);
            *self.status.lock() = "ready".into();
            self.ready.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn unload(&self, why: &str) {
            self.unloaded.lock().push(why.into());
            self.ready.store(false, Ordering::SeqCst);
            *self.status.lock() = "asleep".into();
        }
        fn avoid_gpu(&self, why: &str) {
            self.could_use_gpu.store(false, Ordering::SeqCst);
            self.unload(why);
        }
        fn on_gpu(&self) -> bool {
            self.on_gpu.load(Ordering::SeqCst)
        }
        fn over_budget(&self) -> bool {
            self.over_budget.load(Ordering::SeqCst)
        }
        fn could_use_gpu(&self) -> bool {
            self.could_use_gpu.load(Ordering::SeqCst)
        }
        fn set_pace(&self, pace: Box<dyn Fn(f64) + Send + Sync>) {
            *self.pace.lock() = Some(pace);
        }
        fn set_batch_scale(&self, _scale: f64) {}
        fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            if let Some(e) = self.failures.lock().pop_front() {
                return Err(e);
            }
            if self.picky.is_some_and(|p| texts.iter().any(|t| t.contains(p))) {
                return Err(EmbedError::BadInput("ValueError: broken".into()));
            }
            self.items(texts.len())?;
            Ok(vec![unit(); texts.len()])
        }
        fn images(&self, images: &[&image::RgbImage]) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.items(images.len())?;
            Ok(vec![unit(); images.len()])
        }
    }

    struct T {
        ix: Arc<Indexer>,
        e: Arc<Fake>,
        gov: Arc<Governor>,
        dir: std::path::PathBuf,
    }

    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir); // (as far as Windows lets go of the files)
        }
    }

    fn make(e: Fake) -> T {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("quiekel-rs-ix-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open_at(&dir.join("state.db")).unwrap());
        let settings = Arc::new(Settings::new(store.clone()));
        let gov = Governor::fixed();
        let e = Arc::new(e);
        let ix = Indexer::new(store, e.clone(), gov.clone(), settings);
        ix.no_waiting.store(true, Ordering::SeqCst);
        T { ix, e, gov, dir }
    }

    impl T {
        fn path(&self, rel: &str) -> String {
            self.dir.join(rel).to_string_lossy().to_string()
        }

        /// A folder of the index: (id, path).
        fn folder(&self, rel: &str) -> (i64, String) {
            let p = self.path(rel);
            std::fs::create_dir_all(&p).unwrap();
            (self.ix.store.add_folder(&p).unwrap(), p)
        }

        fn set(&self, key: &str, value: Value) {
            self.ix.settings.update(key, value).unwrap();
        }

        fn paths(&self, folder_id: i64) -> BTreeSet<String> {
            self.ix.store.file_signatures(folder_id).into_keys().collect()
        }

        /// Path -> status.
        fn found(&self, folder_id: i64) -> HashMap<String, String> {
            self.ix.store.file_signatures(folder_id).into_iter().map(|(p, v)| (p, v.3)).collect()
        }

        /// File name -> status.
        fn statuses(&self, folder_id: i64) -> BTreeMap<String, String> {
            self.found(folder_id).into_iter().map(|(p, s)| (p.rsplit(['\\', '/']).next().unwrap_or("").to_string(), s)).collect()
        }

        /// What waits in the queue, in turn (taken off it).
        fn queued(&self) -> Vec<&'static str> {
            let mut out = Vec::new();
            while let Ok(job) = self.ix.jobs_rx.try_recv() {
                out.push(match job {
                    Job::Scan(_) => "scan",
                    Job::Index(..) => "index",
                    Job::Remove(_) => "remove",
                });
            }
            out
        }

        /// What the indexer does in turn.
        fn work(&self) {
            while let Ok(job) = self.ix.jobs_rx.try_recv() {
                self.ix.run_job(job);
            }
        }

    }

    fn ago(seconds: u64) -> Instant {
        Instant::now().checked_sub(Duration::from_secs(seconds)).expect("the PC has been on for a few minutes")
    }

    fn set<const N: usize>(items: [&str; N]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// n notes in a folder, every 7th one empty (skipped): (path, size, mtime) of each.
    fn notes(folder: &str, n: usize) -> Vec<Found> {
        std::fs::create_dir_all(folder).unwrap();
        (0..n)
            .map(|i| {
                let p = format!("{folder}\\note_{i:02}.txt");
                std::fs::write(&p, if i % 7 != 0 { format!("note number {i} about apples") } else { String::new() }).unwrap();
                let meta = std::fs::metadata(&p).unwrap();
                (p, meta.len() as i64, mtime_of(&meta))
            })
            .collect()
    }

    fn write_zip(path: &str, files: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, text) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, text.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    fn hide(path: &str) {
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
        unsafe { SetFileAttributesW(&windows::core::HSTRING::from(path), FILE_ATTRIBUTE_HIDDEN).unwrap() };
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
    }

    fn wait_until(seconds: f64, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The thread ends within this time (a hang fails the test instead of stalling it).
    fn join_within<R>(handle: std::thread::JoinHandle<R>, seconds: f64) -> R {
        wait_until(seconds, || handle.is_finished());
        assert!(handle.is_finished(), "still running after {seconds} s");
        handle.join().unwrap()
    }

    // ---- reading and indexing ------------------------------------------------------------------

    #[test]
    fn reading_ahead_indexes_every_file_in_order() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 40);
        t.ix.index_files(fid, &folder, files.clone(), None).unwrap();
        assert_eq!(t.paths(fid), files.iter().map(|f| f.0.clone()).collect());
        assert_eq!(t.ix.progress()["done"], json!(40));
        assert_eq!(t.ix.store.file_ids_under(&folder).len(), 34); // the 6 empty notes are skipped
    }

    #[test]
    fn waiting_for_video_memory_hands_the_gpu_back() {
        let t = make(Fake::new(true));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 10);
        t.gov.set(|m, d| {
            d.duty = 0.0; // memory ran out: everything waits
            m.vram_free_gb = Some(0.2);
        });
        let ix = t.ix.clone();
        let worker = std::thread::spawn(move || ix.index_files(fid, &folder, files, None).unwrap());
        wait_until(5.0, || !t.e.unloaded().is_empty());
        assert_eq!(t.e.unloaded(), ["GPU memory almost full"], "the model kept its video memory although the game needed it");
        t.gov.set(|m, d| {
            d.duty = 1.0; // the memory is back
            m.vram_free_gb = Some(4.0);
        });
        join_within(worker, 10.0);
        assert_eq!(t.e.loads(), 1); // (started again for the files)
        assert_eq!(t.paths(fid).len(), 10);
    }

    #[test]
    fn files_inside_zips_are_indexed_like_a_folder() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        std::fs::write(format!("{folder}\\plain.txt"), "a note outside the archive").unwrap();
        let zpath = format!("{folder}\\Steuer 2024.zip");
        write_zip(&zpath, &[
            ("Belege/quittung.txt", "Quittung Laptop 899 Euro"),
            ("Belege/brief.txt", "Brief vom Finanzamt"),
            ("tool.exe", "binary, not indexed"),
        ]);
        t.ix.scan_folder(fid).unwrap();
        let (plain, quittung, brief) =
            (format!("{folder}\\plain.txt"), format!("{zpath}\\Belege\\quittung.txt"), format!("{zpath}\\Belege\\brief.txt"));
        assert_eq!(t.paths(fid), set([&plain, &quittung, &brief]));
        assert_eq!(t.ix.store.file_ids_under(&zpath).len(), 2); // the archive works as a folder scope

        // The archive changes: one file gone, one new. The watcher brings the index in line.
        write_zip(&zpath, &[("Belege/quittung.txt", "Quittung Laptop 899 Euro"), ("Belege/bescheid.txt", "Steuerbescheid 2024")]);
        t.ix.enqueue_paths(std::slice::from_ref(&zpath));
        t.work();
        let paths = t.paths(fid);
        assert!(paths.contains(&format!("{zpath}\\Belege\\bescheid.txt")) && !paths.contains(&brief));

        // Archives switched off in Settings -> File types: the next scan forgets what's inside.
        t.set("kinds_off", json!(["archives"]));
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.paths(fid), set([&plain]));
    }

    #[test]
    fn found_files_are_searchable_by_name_before_they_are_indexed() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 3);
        let (path, size, mtime) = files[1].clone();
        t.ix.store.add_pending(fid, &[(path.clone(), size, mtime, "text".into(), "docs note_01.txt".into())]).unwrap();
        let (pending_id, _, _, status) = t.ix.store.file_signatures(fid)[&path].clone();
        assert_eq!(status, "pending");
        let hits = t.ix.store.keyword_search("\"note_01\"*", 10, &[], None, None);
        assert_eq!(hits.iter().map(|h| h.file_id).collect::<Vec<_>>(), [pending_id]);
        assert_eq!(t.ix.store.folders()[0].pending, 1);

        t.ix.scan_folder(fid).unwrap(); // the scan picks up unfinished files
        assert!(t.found(fid).values().all(|s| s != "pending")); // all done
        assert_eq!(t.ix.store.folders()[0].pending, 0);
    }

    #[test]
    fn the_watcher_notes_changes_at_once_while_indexing_waits() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 3);
        t.ix.scan_folder(fid).unwrap();
        t.ix.pause(); // (or busy with a big folder: the watcher doesn't wait for it)
        let new = format!("{folder}\\einkaufsliste.txt");
        std::fs::write(&new, "Milch, Eier, Mehl").unwrap();
        std::fs::remove_file(&files[1].0).unwrap();

        t.ix.enqueue_paths(&[new.clone(), files[1].0.clone()]);
        let known = t.ix.store.file_signatures(fid);
        assert!(known[&new].3 == "pending" && !known.contains_key(&files[1].0)); // right away
        let hits = t.ix.store.keyword_search("\"einkaufsliste\"*", 10, &[], None, None);
        assert_eq!(hits.iter().map(|h| h.file_id).collect::<Vec<_>>(), [known[&new].0]);
        assert_eq!(t.ix.store.folders()[0].pending, 1);

        t.ix.resume();
        t.work();
        assert_eq!(t.found(fid)[&new], "indexed");
        assert_eq!(t.ix.store.folders()[0].pending, 0);
    }

    #[test]
    fn a_file_back_before_its_turn_stays() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 2);
        t.ix.scan_folder(fid).unwrap();
        let path = files[1].0.clone();
        std::fs::rename(&path, format!("{path}.bak")).unwrap();
        t.ix.enqueue_paths(std::slice::from_ref(&path)); // deleted...
        std::fs::rename(format!("{path}.bak"), &path).unwrap();
        t.ix.enqueue_paths(std::slice::from_ref(&path)); // ...and back, before the indexer got to either
        t.work();
        assert_eq!(t.found(fid)[&path], "indexed");
    }

    #[test]
    fn a_phone_copy_leaves_out_the_apps_own_data() {
        let t = make(Fake::new(false));
        let phone = t.path("Phone copy");
        let cache = format!("{phone}\\Android\\data\\com.example.music\\cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(format!("{cache}\\cover"), "not for searching").unwrap();
        let saved = format!("{phone}\\Android\\media\\com.example.chat");
        std::fs::create_dir_all(&saved).unwrap();
        std::fs::write(format!("{saved}\\note.txt"), "saved from a chat").unwrap();
        let (mut report, types) = (Report::default(), t.ix.types());
        let mut containers = Containers::new(types.clone(), HashMap::new(), HashMap::new());
        let mut found = Vec::new();
        t.ix.walk(&phone, 0, &mut report, &types, &mut containers, &mut |f| found.push(f.0));
        assert_eq!(found, [format!("{saved}\\note.txt")]);
        assert_eq!(report.data["program_folder"].n, 1);
    }

    #[test]
    fn files_that_failed_are_tried_again_on_the_next_scan() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let (path, size, mtime) = notes(&folder, 2)[1].clone();
        let failed = Entry {
            folder_id: fid, path: path.clone(), size, mtime, kind: "text".into(), status: "error".into(), chunks: 0,
            error: Some("!damaged_document".into()), search_name: String::new(),
        };
        t.ix.store.save_files(&[failed], &[]).unwrap();
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.found(fid)[&path], "indexed");
    }

    #[test]
    fn files_are_read_at_once_while_the_model_is_on_the_graphics_card() {
        let t = make(Fake::new(true));
        t.ix.cores.store(4, Ordering::SeqCst); // three readers, one core for the model
        t.ix.slow_read.store(30, Ordering::SeqCst);
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 24);
        *t.ix.fail_read.lock() = Some("note_05.txt".into());
        t.ix.index_files(fid, &folder, files.clone(), None).unwrap();
        assert_eq!(t.paths(fid), files.iter().map(|f| f.0.clone()).collect());
        assert_eq!(t.ix.progress()["done"], json!(24));
        assert_eq!(t.statuses(fid)["note_05.txt"], "error"); // (one reader failing stops none of the others)
        let most = t.ix.reading.1.load(Ordering::SeqCst);
        assert!((2..=3).contains(&most), "{most} files read at once");
    }

    #[test]
    fn the_model_on_the_processor_leaves_it_one_reader() {
        let t = make(Fake::new(false));
        t.ix.cores.store(8, Ordering::SeqCst);
        t.ix.slow_read.store(10, Ordering::SeqCst);
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 12), None).unwrap();
        assert_eq!(t.paths(fid).len(), 12);
        assert_eq!(t.ix.reading.1.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn files_in_archives_are_read_in_turn_beside_the_others() {
        let t = make(Fake::new(true));
        t.ix.cores.store(4, Ordering::SeqCst);
        let (fid, folder) = t.folder("docs");
        let mut expected: BTreeSet<String> = notes(&folder, 20).into_iter().map(|f| f.0).collect();
        for z in 0..3 {
            let zpath = format!("{folder}\\box {z}.zip");
            let members: Vec<(String, String)> = (0..6).map(|i| (format!("inner/{i}.txt"), format!("box {z} paper {i}"))).collect();
            write_zip(&zpath, &members.iter().map(|(n, x)| (n.as_str(), x.as_str())).collect::<Vec<_>>());
            expected.extend((0..6).map(|i| format!("{zpath}\\inner\\{i}.txt")));
        }
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.paths(fid), expected);
        assert_eq!(t.found(fid).values().filter(|s| *s == "indexed").count(), 17 + 18); // (the 3 empty notes are skipped)
    }

    #[test]
    fn a_failing_reader_cannot_hang_indexing() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 10);
        *t.ix.fail_read.lock() = Some("note_05.txt".into());
        let ix = t.ix.clone();
        let worker = std::thread::spawn(move || ix.index_files(fid, &folder, files, None).unwrap());
        join_within(worker, 10.0);
        assert_eq!(t.statuses(fid)["note_05.txt"], "error");
    }

    // ---- the model and the graphics card ---------------------------------------------------------

    #[test]
    fn the_model_stays_ready_while_theres_room() {
        let t = make(Fake::new(true));
        t.gov.set(|m, _| m.vram_free_gb = Some(4.0));
        for _ in 0..3 {
            t.ix.manage_device(); // (however long nothing happens)
        }
        assert!(t.e.ready() && t.e.unloaded().is_empty());
    }

    #[test]
    fn the_model_leaves_the_graphics_card_when_video_memory_runs_out() {
        let t = make(Fake::new(true));
        t.gov.set(|m, _| m.vram_free_gb = Some(0.2));
        t.ix.manage_device();
        assert_eq!(t.e.unloaded(), ["GPU memory almost full"]); // (all of it back; it starts again where it fits)
    }

    #[test]
    fn the_model_gives_the_card_back_when_windows_shrinks_its_budget() {
        let t = make(Fake::new(true));
        t.gov.set(|m, _| m.vram_free_gb = None); // (a card that can't tell its free memory)
        t.ix.manage_device();
        assert!(t.e.unloaded().is_empty());
        t.e.over_budget.store(true, Ordering::SeqCst); // another app needs the card's memory
        t.ix.manage_device();
        assert_eq!(t.e.unloaded(), ["GPU memory almost full"]);
        assert!(!t.ix.gpu_ok_now()); // (on the processor for a while)
    }

    #[test]
    fn pausing_holds_indexing_and_resuming_goes_on() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 10);
        t.ix.pause();
        let ix = t.ix.clone();
        let worker = std::thread::spawn(move || ix.index_files(fid, &folder, files, None).unwrap());
        std::thread::sleep(Duration::from_millis(400));
        assert!(!worker.is_finished() && t.paths(fid).is_empty()); // held, the model kept ready
        assert!(t.e.ready() && t.e.unloaded().is_empty());
        t.ix.resume();
        join_within(worker, 10.0);
        assert_eq!(t.paths(fid).len(), 10);
    }

    // ---- what's searched, and why the rest isn't ---------------------------------------------------

    #[test]
    fn every_file_left_out_comes_with_its_reason() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        std::fs::create_dir_all(format!("{folder}\\node_modules\\lib")).unwrap();
        std::fs::write(format!("{folder}\\node_modules\\lib\\index.js"), "module.exports = 1").unwrap();
        for name in ["brief.txt", ".env", "~$brief.docx", "urlaub.mp4", "setup.exe", "daten.xyz", "postfach.pst"] {
            std::fs::write(format!("{folder}\\{name}"), [b'x'; 10]).unwrap();
        }
        std::fs::write(format!("{folder}\\daten.xyz"), b"x\0".repeat(10)).unwrap(); // (not text: a type the app doesn't know stays unread)
        std::fs::write(format!("{folder}\\versteckt.txt"), "hidden").unwrap();
        hide(&format!("{folder}\\versteckt.txt"));
        let names: Vec<String> = (0..5).map(|i| format!("f{i}.txt")).collect();
        write_zip(&format!("{folder}\\backup.zip"), &names.iter().map(|n| (n.as_str(), "x")).collect::<Vec<_>>());

        crate::filetypes::TEST_MAX_ENTRIES.with(|m| m.set(Some(4)));
        t.ix.scan_folder(fid).unwrap();
        crate::filetypes::TEST_MAX_ENTRIES.with(|m| m.set(None));

        assert_eq!(t.paths(fid), set([&format!("{folder}\\brief.txt")]));
        let report = t.ix.store.details(fid, 5)["report"].clone();
        let counts: BTreeMap<String, i64> =
            report.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v["n"].as_i64().unwrap())).collect();
        let expected = [
            ("program_folder", 1), ("hidden", 2), ("office_temp", 1), ("media", 1), ("program", 1), ("type", 1),
            ("mailbox", 1), ("too_many", 1),
        ];
        assert_eq!(counts, expected.iter().map(|(k, n)| (k.to_string(), *n)).collect::<BTreeMap<_, _>>());
        assert_eq!(report["media"]["exts"], json!({".mp4": 1}));
        assert_eq!(report["type"]["examples"], json!([format!("{folder}\\daten.xyz")]));
        assert_eq!(t.ix.store.folders()[0].unsearched, 9);
    }

    #[test]
    fn what_a_file_holds_decides_when_its_name_doesnt() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        std::fs::write(format!("{folder}\\notiz.txt"), "eine Notiz").unwrap();
        for i in 0..4 {
            let text = format!("Spielstand: Level {i}, Zauberstab gefunden, Kröte besiegt");
            let cp1252: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect(); // (Latin-1 letters: the same bytes)
            std::fs::write(format!("{folder}\\spiel{i}.xyz"), cp1252).unwrap();
        }
        std::fs::write(format!("{folder}\\spiel4.xyz"), (0..8).flat_map(|_| 0..=255u8).collect::<Vec<u8>>()).unwrap(); // (same name, no text)
        std::fs::write(format!("{folder}\\Notizen"), "ohne Endung").unwrap();

        t.ix.scan_folder(fid).unwrap();
        let found = t.found(fid);
        let notiz = format!("{folder}\\notiz.txt");
        let mut expected: BTreeSet<String> = (0..4).map(|i| format!("{folder}\\spiel{i}.xyz")).collect();
        expected.extend([notiz.clone(), format!("{folder}\\Notizen")]);
        assert_eq!(found.keys().cloned().collect::<BTreeSet<_>>(), expected); // each file by what it holds
        assert_eq!(found[&format!("{folder}\\spiel3.xyz")], "indexed"); // read as plain text
        let report = t.ix.store.details(fid, 5)["report"].clone();
        assert_eq!(report["type"]["exts"], json!({".xyz": 1}));

        t.set("kinds_off", json!(["text"])); // text and code switched off
        t.ix.scan_folder(fid).unwrap();
        assert!(t.paths(fid).is_empty());
        assert_eq!(t.ix.store.details(fid, 5)["report"]["off"]["n"], 6);
    }

    #[test]
    fn folders_marked_as_caches_are_left_out() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        std::fs::create_dir_all(format!("{folder}\\thumbnails")).unwrap();
        std::fs::write(format!("{folder}\\thumbnails\\CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55").unwrap();
        std::fs::write(format!("{folder}\\thumbnails\\a.txt"), "a cached note").unwrap();
        std::fs::write(format!("{folder}\\brief.txt"), "a letter").unwrap();
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.paths(fid), set([&format!("{folder}\\brief.txt")]));
        assert_eq!(t.ix.store.details(fid, 5)["report"]["program_folder"]["n"], 1);
        t.set("program_folders", json!(true)); // wanted after all
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.paths(fid).len(), 3); // (the tag is text too)
    }

    #[test]
    fn hidden_files_and_program_folders_when_wanted() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        std::fs::create_dir_all(format!("{folder}\\build")).unwrap();
        std::fs::write(format!("{folder}\\build\\notes.md"), "build notes").unwrap();
        std::fs::write(format!("{folder}\\.notes.md"), "dot notes").unwrap();
        t.ix.scan_folder(fid).unwrap();
        assert!(t.paths(fid).is_empty());
        t.set("hidden_files", json!(true));
        t.set("program_folders", json!(true));
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(t.paths(fid), set([&format!("{folder}\\build\\notes.md"), &format!("{folder}\\.notes.md")]));
    }

    #[test]
    fn emails_and_their_attachments_are_both_searched() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("mail");
        let msg = format!("{folder}\\Angebot.msg");
        std::fs::copy(fixture("Angebot.msg"), &msg).unwrap(); // with preise.txt attached
        t.ix.scan_folder(fid).unwrap();
        let expected: HashMap<String, String> =
            [(msg.clone(), "indexed".to_string()), (format!("{msg}\\preise.txt"), "indexed".to_string())].into();
        assert_eq!(t.found(fid), expected);
        t.set("kinds_off", json!(["mail"])); // e-mail switched off: the emails and what came with them
        t.ix.scan_folder(fid).unwrap();
        assert!(t.paths(fid).is_empty());
    }

    #[test]
    fn unchanged_archives_are_not_opened_again() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let zpath = format!("{folder}\\fotos.zip");
        write_zip(&zpath, &[("a.txt", "Strand"), ("b.exe", "kein Dokument")]);
        let opened = || archive::WALKED.with(|w| w.borrow().iter().filter(|p| **p == zpath).count());

        t.ix.scan_folder(fid).unwrap();
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(opened(), 1); // the second scan knew what's inside
        assert_eq!(t.paths(fid), set([&format!("{zpath}\\a.txt")]));
        assert_eq!(t.ix.store.details(fid, 5)["report"]["program"]["n"], json!(1)); // (and why the rest isn't searched)

        let file = std::fs::File::options().write(true).open(&zpath).unwrap();
        file.set_modified(std::fs::metadata(&zpath).unwrap().modified().unwrap() + Duration::from_secs(10)).unwrap();
        drop(file);
        t.ix.scan_folder(fid).unwrap();
        assert_eq!(opened(), 2);
        t.set("kinds_off", json!(["text"])); // other kinds: looked into again
        t.ix.scan_folder(fid).unwrap();
        assert!(opened() == 3 && t.paths(fid).is_empty());
    }

    // ---- weak PCs: an engine in trouble, and memory --------------------------------------------

    #[test]
    fn an_engine_in_trouble_never_marks_files_unreadable() {
        let e = Fake::new(false);
        e.failures.lock().extend([
            EmbedError::Engine("DirectML failed".into()),
            EmbedError::GpuBusy("out of memory".into()),
            EmbedError::ModelAsleep,
        ]);
        let t = make(e);
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 10), None).unwrap();
        assert_eq!(t.statuses(fid).into_values().collect::<BTreeSet<_>>(), set(["indexed", "skipped"]));
        assert!(t.e.failures.lock().is_empty());
    }

    #[test]
    fn a_file_the_model_cant_take_is_the_only_one_marked() {
        let mut e = Fake::new(false);
        e.picky = Some("number 3 ");
        let t = make(e);
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 10), None).unwrap();
        let errors: Vec<String> = t.statuses(fid).into_iter().filter(|(_, s)| s == "error").map(|(name, _)| name).collect();
        assert_eq!(errors, ["note_03.txt"]);
    }

    #[test]
    fn memory_almost_full_for_a_while_lets_the_model_go() {
        let t = make(Fake::new(false));
        t.gov.set(|_, d| d.code = "ram_full".into());
        t.ix.manage_device();
        assert!(t.e.unloaded().is_empty()); // not at once: memory may free up again
        *t.ix.ram_low_since.lock() = Some(ago(RAM_RELEASE_AFTER_S as u64 + 1));
        t.ix.manage_device();
        assert_eq!(t.e.unloaded(), ["memory almost full"]);
    }

    #[test]
    fn the_model_waits_for_memory_before_it_loads_and_says_so() {
        let e = Fake::new(false).asleep("asleep");
        e.room.store(false, Ordering::SeqCst);
        let t = make(e);
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (ix, fake, notes_seen) = (Arc::downgrade(&t.ix), Arc::downgrade(&t.e), seen.clone());
        *t.e.on_load.lock() = Some(Box::new(move || {
            let (Some(ix), Some(fake)) = (ix.upgrade(), fake.upgrade()) else { return };
            let mut seen = notes_seen.lock();
            seen.push(ix.progress().get("note").cloned().unwrap_or(Value::Null));
            if seen.len() == 2 {
                fake.room.store(true, Ordering::SeqCst); // other apps closed meanwhile
            }
        }));
        let _ = t.ix.ensure_model();
        assert!(t.e.ready() && *t.e.spare.lock() == Some(INDEX_SPARE_GB)); // (more room than a search needs)
        assert_eq!(*seen.lock(), [Value::Null, json!({"key": "note.waiting_ram", "params": {"gb": 0.6}})]);
        assert_eq!(t.ix.progress().get("note"), Some(&Value::Null));
    }

    #[test]
    fn the_model_is_ready_from_the_start_when_theres_room() {
        let t = make(Fake::new(false).asleep("not loaded"));
        t.ix.load_at_start();
        assert_eq!((t.e.loads(), t.e.ready()), (1, true));
        let tight = make(Fake::new(false).asleep("not loaded"));
        tight.e.room.store(false, Ordering::SeqCst);
        tight.ix.load_at_start(); // too little memory: it waits for work or a search instead
        assert_eq!((tight.e.loads(), tight.e.status()), (0, "asleep".to_string()));
    }

    // ---- pausing and removing folders ----------------------------------------------------------

    #[test]
    fn removing_the_folder_being_indexed_stops_it_even_while_everything_is_paused() {
        let t = make(Fake::new(false));
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 10);
        t.ix.pause();
        let ix = t.ix.clone();
        let worker = std::thread::spawn(move || ix.index_files(fid, &folder, files, None).unwrap());
        std::thread::sleep(Duration::from_millis(300));
        assert!(!worker.is_finished()); // waiting, paused
        t.ix.remove_folder(fid).unwrap();
        join_within(worker, 5.0); // gone at once, not when everything goes on again
    }

    #[test]
    fn a_folder_paused_on_its_card_waits_and_the_others_go_on() {
        let t = make(Fake::new(false));
        let ((fa, a), (fb, b)) = (t.folder("a"), t.folder("b"));
        let (files_a, files_b) = (notes(&a, 10), notes(&b, 10));
        t.ix.pause_folder(fa);
        t.ix.scan_folder(fa).unwrap(); // (held: nothing happens)
        t.ix.index_files(fb, &b, files_b, None).unwrap();
        assert!(t.ix.store.file_signatures(fa).is_empty());
        assert_eq!(t.statuses(fb).into_values().collect::<BTreeSet<_>>(), set(["indexed", "skipped"]));
        assert_eq!(t.ix.paused_folders(), HashSet::from([fa]));
        t.ix.resume_folder(fa);
        assert!(t.ix.paused_folders().is_empty());
        assert_eq!(t.queued(), ["scan"]);
        t.ix.scan_folder(fa).unwrap();
        assert_eq!(t.statuses(fa).into_values().collect::<BTreeSet<_>>(), set(["indexed", "skipped"]));
        assert_eq!(t.statuses(fa).len(), files_a.len());
    }

    #[test]
    fn try_again_loads_the_model_even_when_nobody_waits_for_it() {
        let t = make(Fake::new(false).asleep("error")); // (it failed for a search)
        t.ix.retry_model();
        wait_until(5.0, || t.e.ready());
        assert!(t.e.ready() && t.e.loads() == 1);
    }

    #[test]
    fn a_search_while_the_card_is_full_starts_the_model_on_the_processor() {
        let t = make(Fake::new(true).asleep("asleep"));
        t.gov.set(|m, _| m.vram_free_gb = Some(0.5)); // another app has the card's memory
        t.ix.wake_model();
        wait_until(2.5, || t.e.ready());
        assert!(t.e.preferred() == [false] && !t.e.on_gpu());
    }

    #[test]
    fn the_work_moves_back_to_the_graphics_card_once_it_has_room() {
        let t = make(Fake::new(false));
        t.e.could_use_gpu.store(true, Ordering::SeqCst); // (started on the processor while the card was full)
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 3), None).unwrap(); // the card has room now, and there's work
        assert!(t.e.unloaded() == ["the graphics card is free again"] && t.e.preferred() == [true] && t.e.on_gpu());
        assert_eq!(t.paths(fid).len(), 3);
    }

    #[test]
    fn a_long_group_on_the_processor_moves_to_the_graphics_card_as_soon_as_it_has_room() {
        let t = make(Fake::new(false));
        t.e.could_use_gpu.store(true, Ordering::SeqCst); // (started on the processor while the card was full)
        t.gov.set(|m, _| m.vram_free_gb = Some(0.5)); // still full when the group starts
        let gov = t.gov.clone();
        *t.e.after_first.lock() = Some(Box::new(move || gov.set(|m, _| m.vram_free_gb = Some(4.0)))); // room again
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 6), None).unwrap();
        assert!(t.e.unloaded() == ["the graphics card is free again"] && t.e.preferred() == [true] && t.e.on_gpu());
        assert_eq!(t.paths(fid).len(), 6);
    }

    #[test]
    fn after_the_graphics_card_ran_full_the_processor_works_for_a_while() {
        let t = make(Fake::new(true));
        t.gov.set(|m, _| m.vram_free_gb = Some(0.2)); // (the model's own memory, or another app's)
        t.ix.manage_device();
        assert_eq!(t.e.unloaded(), ["GPU memory almost full"]);
        t.gov.set(|m, _| m.vram_free_gb = Some(3.0)); // free again once the model is gone
        let (fid, folder) = t.folder("docs");
        let files = notes(&folder, 7);
        t.ix.index_files(fid, &folder, files[..3].to_vec(), None).unwrap();
        assert!(t.e.preferred() == [false] && t.paths(fid).len() == 3); // (not straight back)
        *t.ix.vram_full_at.lock() = Some(ago(BACK_TO_GPU_S as u64 + 1));
        *t.ix.moved_to_gpu.lock() = Some(ago(BACK_TO_GPU_S as u64 + 1));
        t.ix.index_files(fid, &folder, files[3..].to_vec(), None).unwrap();
        assert!(t.e.preferred() == [false, true] && t.e.on_gpu()); // a while later, back to the card
        assert_eq!(t.paths(fid).len(), 7);
    }

    #[test]
    fn the_model_stays_on_the_processor_when_the_card_cant_take_it() {
        let t = make(Fake::new(false)); // no graphics card, or it failed this session
        let (fid, folder) = t.folder("docs");
        t.ix.index_files(fid, &folder, notes(&folder, 3), None).unwrap();
        assert!(t.e.unloaded().is_empty() && t.e.loads() == 0);
    }
}
