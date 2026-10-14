//! Diagnostics: what the app uses, for the tab Settings -> Diagnostics adds. While it's switched on,
//! once a second: the processor and memory of the app and of its model process, their share of the
//! graphics card and the model's video memory, the whole PC's load, and where indexing spends its
//! time. The last ten minutes are kept for the tab's charts; switched off, nothing is sampled.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::embedder::Embedder;
use crate::governor::Governor;
use crate::indexer::Indexer;

const KEEP: usize = 600; // samples: ten minutes, one a second

pub struct Diagnostics {
    samples: Mutex<VecDeque<Value>>,
    on: AtomicBool,
}

impl Diagnostics {
    /// The sampler, idle until switched on.
    pub fn start(embedder: Arc<Embedder>, governor: Arc<Governor>, indexer: Arc<Indexer>, on: bool) -> Arc<Diagnostics> {
        let d = Arc::new(Diagnostics { samples: Mutex::new(VecDeque::with_capacity(KEEP)), on: AtomicBool::new(on) });
        let me = d.clone();
        std::thread::Builder::new()
            .name("diagnostics".into())
            .spawn(move || {
                let mut last: HashMap<u32, (u64, Instant)> = HashMap::new();
                let mut gpu = Gpu::open();
                loop {
                    std::thread::sleep(Duration::from_secs(1));
                    if !me.on.load(Ordering::Relaxed) {
                        if !last.is_empty() {
                            last.clear();
                            me.samples.lock().clear();
                        }
                        continue;
                    }
                    let sample = me.sample(&embedder, &governor, &indexer, &mut last, gpu.as_mut());
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

    /// The samples so far (newest last), and what doesn't change from one to the next.
    pub fn view(&self, embedder: &Embedder) -> Value {
        let state = embedder.state.read();
        json!({
            "on": self.on.load(Ordering::Relaxed),
            "cores": crate::machine::profile().cores,
            "device": if embedder.ready() { if state.on_gpu { state.gpu_name.clone() } else { "processor".into() } } else { String::new() },
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
        let app_cpu = cpu(app);
        let model_cpu = model.map(&mut cpu).unwrap_or(0.0);
        let (gpu_ours, gpu_others) = gpu.and_then(|g| g.read(&[Some(app), model])).map_or((None, None), |(o, t)| (Some(o), Some(t)));
        let m = governor.metrics();
        let p = indexer.progress();
        let r1 = |x: f64| (x * 10.0).round() / 10.0;
        json!({
            "t": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            "app": {"cpu": r1(app_cpu), "mb": crate::model_process::process_memory_mb(app).round()},
            "model": {
                "cpu": r1(model_cpu), "mb": embedder.memory_mb().round(), "gpu": gpu_ours.map(r1),
                "vram_mb": embedder.vram_mb().map(f64::round),
            },
            "pc": {
                "cpu": r1(m.cpu_others + app_cpu + model_cpu).min(100.0), "ram_free_gb": r1(m.ram_free_gb),
                "gpu_others": gpu_others.map(r1),
            },
            "state": p.get("state"),
            "pipeline": p.get("pipeline"),
        })
    }
}

/// Graphics-card use from Windows' performance counters: (the app's processes, everyone else),
/// each the busiest engine type's share, 100 at most.
struct Gpu {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
}

unsafe impl Send for Gpu {}

impl Gpu {
    fn open() -> Option<Gpu> {
        use windows::Win32::System::Performance::{
            PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW, PdhCollectQueryData, PdhOpenQueryW,
        };
        use windows::core::{PCWSTR, w};
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(query, w!("\\GPU Engine(*)\\Utilization Percentage"), 0, &mut counter) != 0 {
                return None;
            }
            PdhCollectQueryData(query); // (a load needs two readings)
            Some(Gpu { query, counter })
        }
    }

    fn read(&mut self, ours: &[Option<u32>]) -> Option<(f64, f64)> {
        use windows::Win32::System::Performance::{
            PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        };
        const MORE_DATA: u32 = 0x800007D2;
        let mine: Vec<String> = ours.iter().flatten().map(|p| format!("pid_{p}_")).collect();
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let status = PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if status == 0 {
                return Some((0.0, 0.0));
            }
            if status != MORE_DATA {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            // By engine type, for us and for everyone else: the busiest type is the card's load.
            let (mut us, mut them): (HashMap<String, f64>, HashMap<String, f64>) = (HashMap::new(), HashMap::new());
            for i in 0..count as usize {
                let item = &*items.add(i);
                if item.FmtValue.CStatus > 1 || item.szName.is_null() {
                    continue;
                }
                let name = item.szName.to_string().unwrap_or_default();
                let Some(kind) = name.split("engtype_").nth(1) else { continue };
                let side = if mine.iter().any(|m| name.starts_with(m)) { &mut us } else { &mut them };
                *side.entry(kind.to_string()).or_insert(0.0) += item.FmtValue.Anonymous.doubleValue;
            }
            let busiest = |m: &HashMap<String, f64>| m.values().copied().fold(0.0f64, f64::max).min(100.0);
            Some((busiest(&us), busiest(&them)))
        }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Performance::PdhCloseQuery(self.query);
        }
    }
}
