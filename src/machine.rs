//! What this PC has, and the setup the model runs with on it.
//!
//! The PC is looked at once: memory, processor cores, graphics cards. That picks one of three
//! setups, each complete and tested as a whole:
//!
//!   graphics   DirectML on a graphics card with 3 GB of its own or more; on graphics built into
//!              the processor, or a card with less, when the PC has 16 GB or more. Half precision,
//!              batches of 8. Built-in graphics let the model go when it's not used (it takes the
//!              PC's memory); a card of its own keeps it ready.
//!   processor  the processor: the 8-bit model, half the cores, batches of 8, at most 1.5 GB, kept
//!              ready.
//!   light      a smaller PC (less than 12 GB, or fewer than 4 cores): the processor with at most
//!              two threads, batches of 4, at most 1 GB, and the model only loaded while it's used.
//!
//! For testing, QUIEKEL_EMBED_PC pretends to be another PC, e.g. "ram=8 cores=4 gpu=none free=2";
//! QUIEKEL_EMBED_SETUP forces a setup by name.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const OWN_VRAM_GB: f64 = 3.0; // less video memory of its own: it needs the PC's memory to make up for it
const PICTURE_BATCHES_VRAM_GB: f64 = 6.0; // pictures several at a time from this much video memory
const SHARED_GPU_MIN_RAM_GB: f64 = 16.0; // built-in graphics only do the work with this much memory
const LIGHT_BELOW_RAM_GB: f64 = 12.0; // less memory than this, or fewer cores: the light setup
const LIGHT_BELOW_CORES: usize = 4;
pub const SETUPS: [&str; 3] = ["graphics", "processor", "light"];

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub index: u32, // DXGI's adapter index, which DirectML takes as its device
    pub name: String,
    pub own_vram: u64, // bytes of video memory of its own
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub ram_gb: f64,
    pub cores: usize, // physical
    pub cards: Vec<Card>,
    pub free_cap_gb: Option<f64>,   // pretending: free memory is seen as at most this
    pub threads_cap: Option<usize>, // pretending: the model process gets only this many threads
    pub simulated: bool,
}

impl Profile {
    /// The card with the most memory of its own: on a laptop the dedicated one.
    pub fn best_card(&self) -> Option<&Card> {
        self.cards.iter().fold(None, |best: Option<&Card>, c| match best {
            Some(b) if b.own_vram >= c.own_vram => Some(b),
            _ => Some(c),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Setup {
    pub name: String, // graphics | processor | light
    pub gpu: bool,
    pub device: i32, // DirectML's adapter index
    pub card: String,
    pub shared: bool, // the graphics card uses the PC's memory
    pub threads: usize,
    pub text_batch: usize,
    pub image_batch: usize,
    pub memory_cap_gb: Option<f64>, // Windows holds the model process to this
    pub ram_needed_gb: f64,         // what the model takes, so free memory needed to start it
    pub keep_loaded: bool,          // ready for searches while there's nothing to do
    pub threads_cap: Option<usize>, // pretending a smaller PC: processor threads the process gets
    pub model: String,              // "stand-in": a tiny model for tests
}

// ---- measuring -------------------------------------------------------------------------------

/// This PC, measured on first use (or the one QUIEKEL_EMBED_PC pretends to be).
pub fn profile() -> &'static Profile {
    static PROFILE: OnceLock<Profile> = OnceLock::new();
    PROFILE.get_or_init(|| {
        let real = Profile {
            ram_gb: memory().0 as f64 / 1e9,
            cores: physical_cores(),
            cards: graphics_cards(),
            free_cap_gb: None,
            threads_cap: None,
            simulated: false,
        };
        match std::env::var("QUIEKEL_EMBED_PC") {
            Ok(text) if !text.trim().is_empty() => simulate(&real, &text),
            _ => real,
        }
    })
}

/// Another PC, from "ram=8 cores=4 gpu=igpu free=2" (anything left out stays as it is).
pub fn simulate(real: &Profile, text: &str) -> Profile {
    let values: std::collections::HashMap<&str, &str> =
        text.split([',', ' ', '\t']).filter_map(|part| part.split_once('=')).collect();
    let mut p = real.clone();
    p.simulated = true;
    if let Some(v) = values.get("ram").and_then(|v| v.parse::<f64>().ok()) {
        p.ram_gb = v;
    }
    if let Some(v) = values.get("cores").and_then(|v| v.parse::<i64>().ok()) {
        let cores = v.max(1) as usize;
        p.cores = cores;
        p.threads_cap = Some(cores);
    }
    if let Some(v) = values.get("free").and_then(|v| v.parse::<f64>().ok()) {
        p.free_cap_gb = Some(v);
    }
    let gpu = values.get("gpu").copied().unwrap_or("card");
    let real_card = real.best_card().cloned();
    if gpu == "broken" {
        p.cards = vec![Card { index: 99, name: "Broken graphics card (pretended)".into(), own_vram: 8_000_000_000 }];
    } else if gpu == "none" || real_card.is_none() {
        p.cards = vec![];
    } else if let Some(card) = real_card {
        if gpu == "igpu" {
            p.cards = vec![Card { index: card.index, name: "Built-in graphics (pretended)".into(), own_vram: 128 << 20 }];
        } else if let Some(gb) = gpu.strip_prefix("card:").and_then(|g| g.parse::<f64>().ok()) {
            p.cards = vec![Card { index: card.index, name: card.name.clone(), own_vram: (gb * 1e9) as u64 }];
        }
    }
    p
}

/// (total, available) physical memory in bytes.
pub fn memory() -> (u64, u64) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    unsafe {
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            return (m.ullTotalPhys, m.ullAvailPhys);
        }
    }
    (8_000_000_000, 4_000_000_000)
}

/// Memory free for the app (Windows' "available"), or what a pretended PC would have.
pub fn free_ram_gb() -> f64 {
    free_ram_gb_on(profile())
}

/// Memory free for the app on this PC, or on the one a profile pretends to be.
pub fn free_ram_gb_on(p: &Profile) -> f64 {
    let free = memory().1 as f64 / 1e9;
    match p.free_cap_gb {
        Some(cap) => free.min(cap),
        None => free,
    }
}

pub fn total_ram_gb() -> f64 {
    profile().ram_gb
}

pub fn logical_cpus() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2)
}

fn physical_cores() -> usize {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    unsafe {
        let mut len = 0u32;
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
        if len == 0 {
            return logical_cpus();
        }
        let mut buf = vec![0u8; len as usize];
        let info = buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
        if GetLogicalProcessorInformationEx(RelationProcessorCore, Some(info), &mut len).is_err() {
            return logical_cpus();
        }
        let (mut at, mut cores) = (0usize, 0usize);
        while at < len as usize {
            let item = &*(buf.as_ptr().add(at) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX);
            if item.Relationship == RelationProcessorCore {
                cores += 1;
            }
            if item.Size == 0 {
                break;
            }
            at += item.Size as usize;
        }
        if cores == 0 { logical_cpus() } else { cores }
    }
}

// ---- choosing ----------------------------------------------------------------------------------

/// Why this PC runs the model on the processor by itself: "none" (no graphics card), "shared" (too
/// little video memory, and too little of the PC's to make up for it), or "" (it doesn't).
pub fn without_gpu(p: &Profile) -> &'static str {
    let Some(card) = p.best_card() else { return "none" };
    let shared = (card.own_vram as f64) < OWN_VRAM_GB * 1e9;
    if shared && p.ram_gb < SHARED_GPU_MIN_RAM_GB { "shared" } else { "" }
}

/// The setup for this PC. use_gpu=false: not the graphics card now (it failed this session, or a
/// game has it).
pub fn choose(p: &Profile, use_gpu: bool, forced: Option<&str>) -> Setup {
    let forced = forced.map(str::to_string).unwrap_or_else(|| std::env::var("QUIEKEL_EMBED_SETUP").unwrap_or_default());
    let forced = forced.trim().to_lowercase();
    let card = p.best_card();
    let shared = card.is_some_and(|c| (c.own_vram as f64) < OWN_VRAM_GB * 1e9);
    let card_fits = without_gpu(p).is_empty();
    let small = p.ram_gb < LIGHT_BELOW_RAM_GB || p.cores < LIGHT_BELOW_CORES;
    let name = if SETUPS.contains(&forced.as_str()) {
        if forced != "graphics" || card.is_some() { forced.clone() } else { "processor".into() }
    } else if use_gpu && card_fits {
        "graphics".into()
    } else if small {
        "light".into()
    } else {
        "processor".into()
    };
    let base = |name: &str, threads: usize, text_batch: usize, cap: Option<f64>, keep: bool| Setup {
        name: name.into(),
        gpu: false,
        device: 0,
        card: String::new(),
        shared: false,
        threads,
        text_batch,
        image_batch: 1,
        memory_cap_gb: cap,
        ram_needed_gb: 0.6,
        keep_loaded: keep,
        threads_cap: p.threads_cap,
        model: "onnx".into(),
    };
    match name.as_str() {
        "graphics" => {
            let card = card.expect("a graphics card");
            Setup {
                gpu: true,
                device: card.index as i32,
                card: card.name.clone(),
                shared,
                // Pictures' text part 8 at a time: a session of its own, worth its memory on a card
                // with plenty of it.
                image_batch: if card.own_vram as f64 >= PICTURE_BATCHES_VRAM_GB * 1e9 { 8 } else { 1 },
                ram_needed_gb: if shared { 3.0 } else { 0.6 },
                ..base("graphics", (p.cores / 2).clamp(1, 4), 8, None, !shared)
            }
        }
        "processor" => base("processor", (p.cores / 2).clamp(1, 8), 8, Some(1.5), true),
        _ => base("light", (p.cores / 2).clamp(1, 2), 4, Some(1.0), false),
    }
}

// ---- graphics cards ----------------------------------------------------------------------------

/// This process's graphics memory on a card, in MB: (in use, the budget Windows gives it). The
/// budget shrinks when other apps need the card's memory: going over it means giving some back.
pub fn video_memory(device: u32) -> Option<(f64, f64)> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3, IDXGIFactory1,
    };
    use windows::core::Interface;
    unsafe {
        let adapter: IDXGIAdapter3 = CreateDXGIFactory1::<IDXGIFactory1>().ok()?.EnumAdapters1(device).ok()?.cast().ok()?;
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut info).ok()?;
        Some((info.CurrentUsage as f64 / 1e6, info.Budget as f64 / 1e6))
    }
}

/// Windows' graphics adapters through DXGI. Software adapters (like the Basic Render Driver) are
/// left out.
pub fn graphics_cards() -> Vec<Card> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1};
    let mut cards = Vec::new();
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return cards };
        for index in 0..16u32 {
            let Ok(adapter) = factory.EnumAdapters1(index) else { break };
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                continue;
            }
            let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            cards.push(Card {
                index,
                name: String::from_utf16_lossy(&desc.Description[..end]),
                own_vram: desc.DedicatedVideoMemory as u64,
            });
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc(ram: f64, cores: usize, vram_gb: Option<f64>) -> Profile {
        Profile {
            ram_gb: ram,
            cores,
            cards: vram_gb.map(|g| vec![Card { index: 0, name: "Card".into(), own_vram: (g * 1e9) as u64 }]).unwrap_or_default(),
            free_cap_gb: None,
            threads_cap: None,
            simulated: false,
        }
    }

    #[test]
    fn setups() {
        let g = choose(&pc(32.0, 8, Some(8.0)), true, Some(""));
        assert_eq!((g.name.as_str(), g.threads, g.text_batch, g.keep_loaded), ("graphics", 4, 8, true));
        let shared = choose(&pc(16.0, 8, Some(0.128)), true, Some(""));
        assert_eq!((shared.name.as_str(), shared.keep_loaded, shared.ram_needed_gb), ("graphics", false, 3.0));
        assert_eq!(choose(&pc(8.0, 8, Some(0.128)), true, Some("")).name, "light");
        let cpu = choose(&pc(32.0, 16, None), true, Some(""));
        assert_eq!((cpu.name.as_str(), cpu.threads, cpu.memory_cap_gb), ("processor", 8, Some(1.5)));
        let light = choose(&pc(8.0, 4, None), true, Some(""));
        assert_eq!((light.name.as_str(), light.threads, light.text_batch), ("light", 2, 4));
        assert_eq!(choose(&pc(32.0, 8, None), true, Some("graphics")).name, "processor");
        assert_eq!(without_gpu(&pc(8.0, 4, Some(0.5))), "shared");
    }

    #[test]
    fn pretending() {
        let real = pc(32.0, 8, Some(8.0));
        let p = simulate(&real, "ram=8 cores=4 gpu=igpu free=2");
        assert_eq!((p.ram_gb, p.cores, p.threads_cap, p.free_cap_gb), (8.0, 4, Some(4), Some(2.0)));
        assert_eq!(p.cards[0].own_vram, 128 << 20);
        assert!(simulate(&real, "gpu=none").cards.is_empty());
        assert_eq!(simulate(&real, "gpu=card:6").cards[0].own_vram, 6_000_000_000);
        assert_eq!(simulate(&real, "gpu=broken").cards[0].index, 99);
    }
}
