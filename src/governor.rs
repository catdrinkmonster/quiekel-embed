//! What the PC has to spare, read every two seconds: the processor, memory and the graphics card.
//! Indexing runs below everything else, so it takes what's free; the only thing that makes it
//! wait is memory running out (and the pause button, which the indexer handles). The readings
//! also feed the Settings page's meters.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::machine;

#[derive(Clone, Debug, Serialize)]
pub struct Metrics {
    pub cpu_others: f64, // % of all cores used by other processes
    pub ram_free_gb: f64,
    pub ram_total_gb: f64,
    pub gpu_others: Option<f64>, // % GPU time used by other processes
    pub vram_free_gb: Option<f64>,
    pub vram_total_gb: Option<f64>,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            cpu_others: 0.0,
            ram_free_gb: 99.0,
            ram_total_gb: 0.0,
            gpu_others: None,
            vram_free_gb: None,
            vram_total_gb: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Decision {
    pub duty: f64,     // share of time indexing may work: 1 = full speed, 0 = paused
    pub level: String, // full | normal | throttled | paused
    pub code: String,  // why, as an i18n key under "gov."
    pub params: Map<String, Value>,
}

impl Decision {
    fn new(duty: f64, level: &str, code: &str, params: Value) -> Decision {
        let params = match params {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        Decision { duty, level: level.into(), code: code.into(), params }
    }

    /// English text, for logs.
    pub fn reason(&self) -> String {
        crate::i18n::t_obj("en", &format!("gov.{}", self.code), &self.params)
    }
}

fn gb(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Whether indexing may work now: only memory running out makes it wait (busy apps come first
/// anyway; a full graphics card sends the model to the processor instead).
pub fn decide(m: &Metrics) -> Decision {
    if m.ram_free_gb < 1.0 {
        return Decision::new(0.0, "paused", "ram_full", json!({"gb": gb(m.ram_free_gb)}));
    }
    Decision::new(1.0, "full", "full", json!({}))
}

// ---- Windows signals ---------------------------------------------------------------------------

/// Windows background mode for the calling thread: low CPU, disk and memory priority.
pub fn set_background_priority(enabled: bool) {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
    };
    unsafe {
        let _ = SetThreadPriority(
            GetCurrentThread(),
            if enabled { THREAD_MODE_BACKGROUND_BEGIN } else { THREAD_MODE_BACKGROUND_END },
        );
    }
}

fn filetime(ft: windows::Win32::Foundation::FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

/// The whole PC's busy share since the last reading, as psutil.cpu_percent(None).
struct SystemCpu {
    last: Option<(u64, u64)>, // (idle, total)
}

impl SystemCpu {
    fn read(&mut self) -> f64 {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::GetSystemTimes;
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        unsafe {
            if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_err() {
                return 0.0;
            }
        }
        let (idle, total) = (filetime(idle), filetime(kernel) + filetime(user)); // kernel includes idle
        let pct = match self.last {
            Some((i0, t0)) if total > t0 => {
                let busy = (total - t0).saturating_sub(idle.saturating_sub(i0));
                busy as f64 / (total - t0) as f64 * 100.0
            }
            _ => 0.0,
        };
        self.last = Some((idle, total));
        pct
    }
}

/// CPU time of one process (this one, or the model's).
pub fn process_cpu_100ns(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        let r = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(filetime(k) + filetime(u))
    }
}

/// GPU load from other processes and free video memory, via NVIDIA's NVML.
struct Nvml {
    nvml: nvml_wrapper::Nvml,
}

impl Nvml {
    fn open() -> Option<Nvml> {
        let nvml = nvml_wrapper::Nvml::init().ok()?;
        nvml.device_by_index(0).ok()?;
        Some(Nvml { nvml })
    }

    /// (GPU load of other processes, free and total video memory). ours: the app's processes.
    fn read(&self, ours: &HashSet<u32>) -> Option<(Option<f64>, f64, f64)> {
        let device = self.nvml.device_by_index(0).ok()?;
        let mem = device.memory_info().ok()?;
        let since = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_micros() as u64)
            .saturating_sub(3_000_000);
        let others = match device.process_utilization_stats(since) {
            Ok(samples) => {
                let mut per_pid: HashMap<u32, u32> = HashMap::new();
                for s in samples {
                    if !ours.contains(&s.pid) {
                        let e = per_pid.entry(s.pid).or_insert(0);
                        *e = (*e).max(s.sm_util);
                    }
                }
                Some((per_pid.values().sum::<u32>() as f64).min(100.0))
            }
            // NotFound: no samples in the window, i.e. nobody else used the GPU.
            Err(nvml_wrapper::error::NvmlError::NotFound) => Some(0.0),
            Err(_) => None,
        };
        Some((others, mem.free as f64 / 1e9, mem.total as f64 / 1e9))
    }
}

/// GPU load from other processes through Windows' own counters, the ones Task Manager shows: any
/// graphics card. Only the 3D engines count (a video plays on its own).
struct GpuCounters {
    query: windows::Win32::System::Performance::PDH_HQUERY,
    counter: windows::Win32::System::Performance::PDH_HCOUNTER,
}

// The counter handles are only used under the governor's lock.
unsafe impl Send for GpuCounters {}

impl GpuCounters {
    fn open() -> Option<GpuCounters> {
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
            Some(GpuCounters { query, counter })
        }
    }

    /// % of the 3D engines' time used by other processes (100 at most), or None if unknown.
    fn read(&self, ours: &HashSet<u32>) -> Option<f64> {
        use windows::Win32::System::Performance::{
            PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        };
        const MORE_DATA: u32 = 0x800007D2;
        let mine: Vec<String> = ours.iter().map(|p| format!("pid_{p}_")).collect();
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let status = PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if status == 0 {
                return Some(0.0); // (no GPU engines in use at all)
            }
            if status != MORE_DATA {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let mut busy = 0.0;
            for i in 0..count as usize {
                let item = &*items.add(i);
                if item.FmtValue.CStatus > 1 || item.szName.is_null() {
                    continue;
                }
                let name = item.szName.to_string().unwrap_or_default();
                if name.contains("engtype_3D") && !mine.iter().any(|m| name.starts_with(m)) {
                    busy += item.FmtValue.Anonymous.doubleValue;
                }
            }
            Some(busy.min(100.0))
        }
    }
}

fn smooth(prev: Option<f64>, new: f64) -> f64 {
    match prev {
        Some(p) => p + 0.5 * (new - p),
        None => new,
    }
}

struct Sampler {
    system: SystemCpu,
    own: HashMap<u32, u64>, // CPU time at the last reading, by process
    last_at: Option<Instant>,
    nvml: Option<Nvml>,
    counters: Option<GpuCounters>,
    samples: u64,
    logged: Option<Instant>,
}

pub struct Governor {
    sampler: Mutex<Sampler>,
    state: RwLock<(Metrics, Decision)>,
    ncpu: usize,
    /// The app's other processes (the model's): their work is ours, not "another app being busy".
    pub helpers: RwLock<Box<dyn Fn() -> Vec<u32> + Send + Sync>>,
}

impl Governor {
    pub fn new() -> Arc<Governor> {
        let nvml = Nvml::open();
        let counters = if nvml.is_some() { None } else { GpuCounters::open() };
        let g = Arc::new(Governor {
            sampler: Mutex::new(Sampler {
                system: SystemCpu { last: None },
                own: HashMap::new(),
                last_at: None,
                nvml,
                counters,
                samples: 0,
                logged: None,
            }),
            state: RwLock::new((Metrics::default(), Decision::new(1.0, "full", "starting", json!({})))),
            ncpu: machine::logical_cpus(),
            helpers: RwLock::new(Box::new(Vec::new)),
        });
        {
            let mut s = g.sampler.lock();
            s.system.read(); // prime the counters
        }
        g.sample();
        let me = g.clone();
        std::thread::Builder::new()
            .name("governor".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    me.sample();
                }
            })
            .ok();
        g
    }

    /// A governor that reads nothing: it sees what a test sets.
    #[cfg(test)]
    pub fn fixed() -> Arc<Governor> {
        Arc::new(Governor {
            sampler: Mutex::new(Sampler {
                system: SystemCpu { last: None },
                own: HashMap::new(),
                last_at: None,
                nvml: None,
                counters: None,
                samples: 0,
                logged: None,
            }),
            state: RwLock::new((Metrics { ram_free_gb: 16.0, ..Default::default() }, Decision::new(1.0, "full", "full", json!({})))),
            ncpu: 8,
            helpers: RwLock::new(Box::new(Vec::new)),
        })
    }

    /// What a test's governor sees from now on.
    #[cfg(test)]
    pub fn set(&self, change: impl FnOnce(&mut Metrics, &mut Decision)) {
        let mut state = self.state.write();
        let (metrics, decision) = &mut *state;
        change(metrics, decision);
    }

    pub fn metrics(&self) -> Metrics {
        self.state.read().0.clone()
    }

    pub fn decision(&self) -> Decision {
        self.state.read().1.clone()
    }

    pub fn duty(&self) -> f64 {
        self.state.read().1.duty
    }


    /// % of all cores the app's own processes use (the first reading of a new one counts 0).
    fn own_cpu(&self, s: &mut Sampler, pids: &[u32], elapsed_s: f64) -> f64 {
        let mut used = 0u64;
        let mut seen = HashMap::new();
        for &pid in pids {
            let Some(now) = process_cpu_100ns(pid) else { continue };
            if let Some(&before) = s.own.get(&pid) {
                used += now.saturating_sub(before);
            }
            seen.insert(pid, now);
        }
        s.own = seen;
        if elapsed_s <= 0.0 {
            return 0.0;
        }
        used as f64 / 1e7 / elapsed_s * 100.0 / self.ncpu as f64
    }

    pub fn sample(&self) {
        let mut s = self.sampler.lock();
        let now = Instant::now();
        let elapsed = s.last_at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        s.last_at = Some(now);
        let total = s.system.read();
        let helpers: Vec<u32> = (self.helpers.read())().into_iter().filter(|&p| p != 0).collect();
        let mut pids = vec![std::process::id()];
        pids.extend(&helpers);
        let own = self.own_cpu(&mut s, &pids, elapsed);
        let ours: HashSet<u32> = pids.iter().copied().collect();
        let (mut gpu_others, mut vram_free, mut vram_total) = (None, None, None);
        if let Some(n) = &s.nvml {
            if let Some((o, f, t)) = n.read(&ours) {
                gpu_others = o;
                vram_free = Some(f);
                vram_total = Some(t);
            }
        }
        s.samples += 1;
        let prev = self.state.read().0.clone();
        if s.counters.is_some() && s.samples % 5 == 1 {
            gpu_others = s.counters.as_ref().and_then(|c| c.read(&ours)); // (Windows' counters are slow to read)
        } else if s.counters.is_some() {
            gpu_others = prev.gpu_others; // (between readings, the last one)
        }
        let m = Metrics {
            cpu_others: smooth(Some(prev.cpu_others), (total - own).max(0.0)),
            ram_free_gb: machine::free_ram_gb(),
            ram_total_gb: machine::total_ram_gb(),
            gpu_others: gpu_others.map(|g| smooth(prev.gpu_others, g)),
            vram_free_gb: vram_free,
            vram_total_gb: vram_total,
        };
        let d = decide(&m);
        let mut state = self.state.write();
        // A new level is logged right away; a reason that keeps flipping at most once a minute.
        let reason = d.reason();
        if d.level != state.1.level
            || (reason != state.1.reason() && s.logged.is_none_or(|t| now.duration_since(t).as_secs() >= 60))
        {
            log::info!("Indexing {}: {reason}", if d.duty > 0.0 { "goes on" } else { "waits" });
            s.logged = Some(now);
        }
        *state = (m, d);
    }


    /// Called between pieces of indexing work: while memory has run out, waits (calling
    /// `while_waiting` every couple of seconds, e.g. to let the model go) until there's room again
    /// or `should_stop` says so.
    pub fn pace(&self, should_stop: &dyn Fn() -> bool, while_waiting: Option<&dyn Fn()>) {
        let mut last_hook: Option<Instant> = None;
        while self.duty() <= 0.0 && !should_stop() {
            if let Some(f) = while_waiting {
                if last_hook.is_none_or(|t| t.elapsed().as_secs_f64() >= 2.0) {
                    f();
                    last_hook = Some(Instant::now());
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_memory_makes_indexing_wait() {
        assert_eq!(decide(&Metrics::default()).duty, 1.0);
        let busy = Metrics { cpu_others: 95.0, gpu_others: Some(95.0), ..Default::default() };
        assert_eq!(decide(&busy).duty, 1.0); // (it runs below everything else: busy apps come first anyway)
        let d = decide(&Metrics { ram_free_gb: 0.5, ..Default::default() });
        assert_eq!((d.duty, d.code.as_str(), d.level.as_str()), (0.0, "ram_full", "paused"));
        let card_full = Metrics { vram_free_gb: Some(0.1), vram_total_gb: Some(8.0), ..Default::default() };
        assert_eq!(decide(&card_full).duty, 1.0); // (the model moves to the processor instead)
    }

    #[test]
    fn pace_waits_while_memory_has_run_out() {
        let g = Governor::fixed();
        g.set(|m, d| {
            m.ram_free_gb = 0.5;
            *d = decide(m);
        });
        let waited = std::sync::atomic::AtomicUsize::new(0);
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(700));
                g.set(|m, d| {
                    m.ram_free_gb = 8.0; // other apps closed
                    *d = decide(m);
                });
            });
            g.pace(&|| false, Some(&|| {
                waited.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
        });
        assert!(started.elapsed() >= Duration::from_millis(600) && waited.load(std::sync::atomic::Ordering::SeqCst) >= 1);
        let quick = Instant::now();
        g.pace(&|| false, None);
        assert!(quick.elapsed() < Duration::from_millis(50)); // room: no waiting at all
    }
}
