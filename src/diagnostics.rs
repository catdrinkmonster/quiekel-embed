//! Diagnostics: what the app uses, for the tab Settings -> Diagnostics adds. While it's switched on,
//! once a second: the processor and memory of the app (with its window's processes, as Task Manager
//! counts them) and of its model process, the model's load on its graphics card and its video
//! memory, the whole PC's load, and where indexing spends its time; and how long searches take. The
//! last ten minutes are kept for the tab's charts; switched off, nothing is sampled.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::embedder::Embedder;
use crate::governor::Governor;
use crate::indexer::Indexer;
use crate::store::Store;

const KEEP: usize = 600; // samples: ten minutes, one a second
const SEARCHES: usize = 50; // the last searches' times
const WINDOW_EVERY: u32 = 10; // the window's processes are looked up again every this many samples
const GIB: f64 = 1_073_741_824.0;

pub struct Diagnostics {
    samples: Mutex<VecDeque<Value>>,
    searches: Mutex<VecDeque<Value>>,
    on: AtomicBool,
}

impl Diagnostics {
    /// The sampler, idle until switched on.
    pub fn start(embedder: Arc<Embedder>, governor: Arc<Governor>, indexer: Arc<Indexer>, on: bool) -> Arc<Diagnostics> {
        let d = Arc::new(Diagnostics {
            samples: Mutex::new(VecDeque::with_capacity(KEEP)),
            searches: Mutex::new(VecDeque::with_capacity(SEARCHES)),
            on: AtomicBool::new(on),
        });
        let me = d.clone();
        std::thread::Builder::new()
            .name("diagnostics".into())
            .spawn(move || {
                let mut last: HashMap<u32, (u64, Instant)> = HashMap::new();
                let mut gpu = Gpu::open();
                let (mut window, mut n) = (Vec::new(), 0u32);
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    if !me.on.load(Ordering::Relaxed) {
                        if !last.is_empty() {
                            last.clear();
                            me.samples.lock().clear();
                            me.searches.lock().clear();
                        }
                        continue;
                    }
                    if n % WINDOW_EVERY == 0 {
                        window = window_processes(std::process::id());
                    }
                    n = n.wrapping_add(1);
                    let sample = me.sample(&embedder, &governor, &indexer, &mut last, gpu.as_mut(), &window);
                    let mut s = me.samples.lock();
                    if s.len() == KEEP {
                        s.pop_front();
                    }
                    s.push_back(sample);
                }
            })
            .ok();
        d
    }

    pub fn set_on(&self, on: bool) {
        self.on.store(on, Ordering::Relaxed);
    }

    /// A search took `ms` in all, `model_ms` of it for the model to read the words.
    pub fn note_search(&self, ms: f64, model_ms: f64) {
        if !self.on.load(Ordering::Relaxed) {
            return;
        }
        let mut s = self.searches.lock();
        if s.len() == SEARCHES {
            s.pop_front();
        }
        s.push_back(json!({"t": now(), "ms": (ms * 10.0).round() / 10.0, "model_ms": (model_ms * 10.0).round() / 10.0}));
    }

    /// The samples so far (newest last), the last searches, and what doesn't change from one
    /// sample to the next: the setup the model runs with and the PC it was chosen for, the index.
    pub fn view(&self, embedder: &Embedder, store: &Store) -> Value {
        let state = embedder.state.read();
        let pc = crate::machine::profile();
        let card = pc.best_card();
        json!({
            "on": self.on.load(Ordering::Relaxed),
            "cores": pc.cores,
            "device": if embedder.ready() { if state.on_gpu { state.gpu_name.clone() } else { "processor".into() } } else { String::new() },
            "setup": state.setup,
            "cpu_because": state.cpu_because,
            // (in GB as Windows counts them: 1024^3 bytes)
            "pc": {
                "ram_gb": (pc.ram_gb * 1e9 / GIB * 10.0).round() / 10.0, "cores": pc.cores, "threads": crate::machine::logical_cpus(),
                "card": card.map(|c| c.name.clone()), "card_gb": card.map(|c| (c.own_vram as f64 / GIB * 10.0).round() / 10.0),
            },
            "index": store.facts(),
            "searches": self.searches.lock().iter().collect::<Vec<_>>(),
            "samples": self.samples.lock().iter().collect::<Vec<_>>(),
        })
    }

    fn sample(
        &self,
        embedder: &Embedder,
        governor: &Governor,
        indexer: &Indexer,
        last: &mut HashMap<u32, (u64, Instant)>,
        gpu: Option<&mut Gpu>,
        window: &[u32],
    ) -> Value {
        let app = std::process::id();
        let model = embedder.pid();
        let ncpu = crate::machine::logical_cpus().max(1) as f64;
        // % of all cores a process used since the last sample.
        let mut cpu = |pid: u32| -> f64 {
            let Some(now) = crate::governor::process_cpu_100ns(pid) else { return 0.0 };
            let at = Instant::now();
            let share = match last.get(&pid) {
                Some(&(before, then)) => {
                    let wall = at.duration_since(then).as_secs_f64();
                    if wall > 0.0 { now.saturating_sub(before) as f64 / 1e7 / wall * 100.0 / ncpu } else { 0.0 }
                }
                None => 0.0,
            };
            last.insert(pid, (now, at));
            share
        };
        // The app: itself and its window's processes.
        let app_cpu = cpu(app) + window.iter().map(|&p| cpu(p)).sum::<f64>();
        let model_cpu = model.map(&mut cpu).unwrap_or(0.0);
        let mem = crate::model_process::process_memory_mb;
        let app_mb = mem(app) + window.iter().map(|&p| mem(p)).sum::<f64>();
        let ours: Vec<u32> = std::iter::once(app).chain(window.iter().copied()).chain(model).collect();
        let g = gpu.and_then(|g| g.read(model, &ours));
        let m = governor.metrics();
        let p = indexer.progress();
        let r1 = |x: f64| (x * 10.0).round() / 10.0;
        json!({
            "t": now(),
            "app": {"cpu": r1(app_cpu), "mb": app_mb.round()},
            "model": {
                "cpu": r1(model_cpu), "mb": embedder.memory_mb().round(),
                "gpu": g.as_ref().map(|g| r1(g.model)),
                // (the counters see the model's video memory even while it's idle; its own answers
                // tell otherwise)
                "vram_mb": g.as_ref().and_then(|g| g.vram_mb).or_else(|| embedder.vram_mb()).map(f64::round),
                "shared_mb": g.as_ref().and_then(|g| g.shared_mb).map(f64::round),
            },
            "pc": {
                "cpu": r1(m.cpu_others + app_cpu + model_cpu).min(100.0), "ram_free_gb": r1(m.ram_free_gb),
                "ram_total_gb": r1(m.ram_total_gb), "gpu_others": g.as_ref().map(|g| r1(g.others)),
            },
            "state": p.get("state"),
            "queued": p.get("queued"),
            "pipeline": p.get("pipeline"),
        })
    }
}

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// The processes of the app's window (WebView2's, started for it): Task Manager counts them with
/// the app. Its model process isn't one of them.
fn window_processes(app: u32) -> Vec<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    let mut children: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
            children.entry(e.th32ParentProcessID).or_default().push((e.th32ProcessID, name));
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    let (mut out, mut seen, mut todo) = (Vec::new(), HashSet::from([app]), vec![app]);
    while let Some(parent) = todo.pop() {
        for (pid, name) in children.get(&parent).into_iter().flatten() {
            if name == "msedgewebview2.exe" && seen.insert(*pid) {
                out.push(*pid);
                todo.push(*pid);
            }
        }
    }
    out
}

/// Graphics-card use from Windows' performance counters. A card's load is its busiest engine
/// type's (3D, compute, copy, ...), 100 at most; each card is counted on its own.
struct Gpu {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    engines: windows::Win32::System::Performance::PDH_HCOUNTER,
    // (processes' video memory: not on every PC; without it, only the load)
    dedicated: Option<windows::Win32::System::Performance::PDH_HCOUNTER>,
    shared: Option<windows::Win32::System::Performance::PDH_HCOUNTER>,
}

unsafe impl Send for Gpu {}

/// One reading: the model's load on the card it uses and everyone else's there (not the app's own;
/// on the busiest card while the model uses none), and the model's video memory: of the card's
/// own, and of the PC's memory the card uses.
struct GpuNow {
    model: f64,
    others: f64,
    vram_mb: Option<f64>,
    shared_mb: Option<f64>,
}

/// "pid_1234_luid_0x0_0xC2E6_phys_0_eng_0_engtype_3D": its process.
fn instance_pid(name: &str) -> Option<u32> {
    name.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

/// Its graphics card.
fn instance_card(name: &str) -> Option<&str> {
    name.split("luid_").nth(1)?.split("_phys").next()
}

impl Gpu {
    fn open() -> Option<Gpu> {
        use windows::Win32::System::Performance::{
            PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhOpenQueryW,
        };
        use windows::core::{PCWSTR, w};
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut engines = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut engines) != 0 {
                let _ = PdhCloseQuery(query);
                return None;
            }
            let optional = |path| {
                let mut c = PDH_HCOUNTER::default();
                (PdhAddEnglishCounterW(query, path, 0, &mut c) == 0).then_some(c)
            };
            let dedicated = optional(w!("\\GPU Process Memory(*)\\Dedicated Usage"));
            let shared = optional(w!("\\GPU Process Memory(*)\\Shared Usage"));
            PdhCollectQueryData(query); // (a load needs two readings)
            Some(Gpu { query, engines, dedicated, shared })
        }
    }

    /// A counter's value for each of its instances.
    fn values(&self, counter: windows::Win32::System::Performance::PDH_HCOUNTER) -> Option<Vec<(String, f64)>> {
        use windows::Win32::System::Performance::{PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PdhGetFormattedCounterArrayW};
        const MORE_DATA: u32 = 0x800007D2;
        unsafe {
            let (mut size, mut count) = (0u32, 0u32);
            let status = PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if status == 0 {
                return Some(Vec::new());
            }
            if status != MORE_DATA {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count as usize {
                let item = &*items.add(i);
                if item.FmtValue.CStatus > 1 || item.szName.is_null() {
                    continue;
                }
                out.push((item.szName.to_string().unwrap_or_default(), item.FmtValue.Anonymous.doubleValue));
            }
            Some(out)
        }
    }

    fn read(&mut self, model: Option<u32>, ours: &[u32]) -> Option<GpuNow> {
        use windows::Win32::System::Performance::PdhCollectQueryData;
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
        }
        // The model's video memory, by card: where it holds the most is the card it uses.
        let memory = |counter: Option<windows::Win32::System::Performance::PDH_HCOUNTER>| -> Option<HashMap<String, f64>> {
            let mut by_card = HashMap::new();
            for (name, v) in self.values(counter?).unwrap_or_default() {
                if model.is_some() && instance_pid(&name) == model {
                    *by_card.entry(instance_card(&name).unwrap_or("").to_string()).or_insert(0.0) += v;
                }
            }
            Some(by_card)
        };
        let dedicated = memory(self.dedicated);
        let shared = memory(self.shared);
        // Load by card and engine type: the model's, and everyone else's but the app's own.
        let (mut mine, mut theirs): (HashMap<(String, String), f64>, HashMap<(String, String), f64>) = (HashMap::new(), HashMap::new());
        for (name, v) in self.values(self.engines)? {
            let (Some(pid), Some(card), Some(kind)) = (instance_pid(&name), instance_card(&name), name.split("engtype_").nth(1)) else {
                continue;
            };
            let key = (card.to_string(), kind.to_string());
            if model == Some(pid) {
                *mine.entry(key).or_insert(0.0) += v;
            } else if !ours.contains(&pid) {
                *theirs.entry(key).or_insert(0.0) += v;
            }
        }
        let held = dedicated.iter().chain(&shared).flatten().max_by(|a, b| a.1.total_cmp(b.1)).filter(|(_, v)| **v > 0.0).map(|(c, _)| c.clone());
        let busy = mine.iter().max_by(|a, b| a.1.total_cmp(b.1)).filter(|(_, v)| **v > 0.0).map(|((c, _), _)| c.clone());
        let card = held.or(busy);
        let busiest = |m: &HashMap<(String, String), f64>| {
            m.iter().filter(|((c, _), _)| card.as_ref().is_none_or(|x| x == c)).map(|(_, v)| *v).fold(0.0f64, f64::max).min(100.0)
        };
        let total = |m: &Option<HashMap<String, f64>>| m.as_ref().and_then(|m| model.map(|_| m.values().sum::<f64>() / 1e6));
        Some(GpuNow { model: busiest(&mine), others: busiest(&theirs), vram_mb: total(&dedicated), shared_mb: total(&shared) })
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_counters_are_told_apart_by_process_and_card() {
        let engine = "pid_4242_luid_0x00000000_0x0000C2E6_phys_0_eng_3_engtype_Compute 0";
        assert_eq!((instance_pid(engine), instance_card(engine)), (Some(4242), Some("0x00000000_0x0000C2E6")));
        let memory = "pid_7_luid_0x00000000_0x00011A2B_phys_0";
        assert_eq!((instance_pid(memory), instance_card(memory)), (Some(7), Some("0x00000000_0x00011A2B")));
        assert_eq!(instance_pid("_Total"), None);
        // The app's own window: none in a test, and never the test itself.
        assert!(!window_processes(std::process::id()).contains(&std::process::id()));
    }
}
