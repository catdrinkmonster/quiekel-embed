"""What this PC has, and the setup the model runs with on it.

The PC is looked at once: memory, processor cores, graphics cards. That picks one of three setups,
each complete and tested as a whole (rather than rules that react while the app runs):

  graphics   DirectML on a graphics card with 3 GB of its own or more; on graphics built into the
             processor, or a card with less, when the PC has 16 GB or more (the PC's memory makes
             up for it). Half precision, batches of 8: DirectML keeps the memory of its largest
             batch while the model is loaded (measured: 1.5 to 2.7 GB with pictures, against 4 to
             5.4 GB for batches of 32, and nearly as fast). Built-in graphics let the model go when
             it's not used (it takes the PC's memory); a card of its own keeps it ready.
  processor  the processor: the 8-bit model, half the cores, batches of 8, at most 1.5 GB, kept
             ready.
  light      a smaller PC (less than 12 GB, or fewer than 4 cores): the processor with at most two
             threads, batches of 4, at most 1 GB, and the model only loaded while it's used.

The model runs in a process of its own (model_process.py); Windows holds it to the setup's memory.

For testing, QUIEKEL_EMBED_PC pretends to be another PC, e.g. "ram=8 cores=4 gpu=none free=2":
the setup follows, the model process really gets only that many processor threads, and the free
memory the app sees is at most `free` GB. gpu is none, igpu (built in, sharing the PC's memory),
card (this PC's own), card:6 (one with 6 GB) or broken (one DirectML can't start on).
QUIEKEL_EMBED_SETUP forces a setup by name.
"""

import ctypes
import os
import uuid
from ctypes import wintypes
from dataclasses import dataclass, replace

import psutil

OWN_VRAM_GB = 3  # less video memory of its own than this: it needs the PC's memory to make up for it
SHARED_GPU_MIN_RAM_GB = 16  # built-in graphics only do the work with this much memory
LIGHT_BELOW_RAM_GB = 12  # less memory than this, or fewer cores: the light setup
LIGHT_BELOW_CORES = 4
SETUPS = ("graphics", "processor", "light")


@dataclass(frozen=True)
class Card:
    index: int  # DXGI's adapter index, which DirectML takes as its device
    name: str
    own_vram: int  # bytes of video memory of its own


@dataclass(frozen=True)
class Profile:
    ram_gb: float
    cores: int  # physical
    cards: tuple[Card, ...]
    free_cap_gb: float | None = None  # pretending: free memory is seen as at most this
    threads_cap: int | None = None  # pretending: the model process gets only this many threads
    simulated: bool = False

    @property
    def best_card(self) -> Card | None:
        """The card with the most memory of its own: on a laptop the dedicated one."""
        return max(self.cards, key=lambda c: c.own_vram, default=None)


@dataclass(frozen=True)
class Setup:
    name: str  # graphics | processor | light
    gpu: bool
    device: int  # DirectML's adapter index
    card: str  # the graphics card's name ("" on the processor)
    shared: bool  # the graphics card uses the PC's memory
    threads: int
    text_batch: int
    image_batch: int
    memory_cap_gb: float | None  # Windows holds the model process to this (None: no cap)
    ram_needed_gb: float  # what the model takes, so free memory needed to start it
    keep_loaded: bool = True  # ready for searches while there's nothing to do (Balanced, Maximum)
    threads_cap: int | None = None  # pretending a smaller PC: processor threads the process gets
    model: str = "onnx"  # "stand-in": a tiny model for tests and previews (model_process.py)

    def as_dict(self) -> dict:
        return dict(self.__dict__)


# ---- measuring ---------------------------------------------------------------------------------

_profile: Profile | None = None


def profile() -> Profile:
    """This PC, measured on first use (or the one QUIEKEL_EMBED_PC pretends to be)."""
    global _profile
    if _profile is None:
        real = Profile(ram_gb=psutil.virtual_memory().total / 1e9,
                       cores=psutil.cpu_count(logical=False) or psutil.cpu_count() or 2,
                       cards=tuple(graphics_cards()))
        pretend = os.environ.get("QUIEKEL_EMBED_PC", "").strip()
        _profile = simulate(real, pretend) if pretend else real
    return _profile


def simulate(real: Profile, text: str) -> Profile:
    """Another PC, from "ram=8 cores=4 gpu=igpu free=2" (anything left out stays as it is)."""
    values = dict(part.split("=", 1) for part in text.replace(",", " ").split() if "=" in part)
    p = replace(real, simulated=True)
    if "ram" in values:
        p = replace(p, ram_gb=float(values["ram"]))
    if "cores" in values:
        cores = max(1, int(values["cores"]))
        p = replace(p, cores=cores, threads_cap=cores)
    if "free" in values:
        p = replace(p, free_cap_gb=float(values["free"]))
    gpu = values.get("gpu", "card")
    real_card = real.best_card
    if gpu == "broken":
        p = replace(p, cards=(Card(99, "Broken graphics card (pretended)", 8 * 10**9),))  # (no such adapter)
    elif gpu == "none" or real_card is None:
        p = replace(p, cards=())
    elif gpu == "igpu":
        p = replace(p, cards=(Card(real_card.index, "Built-in graphics (pretended)", 128 * 2**20),))
    elif gpu.startswith("card:"):
        p = replace(p, cards=(Card(real_card.index, real_card.name, int(float(gpu[5:]) * 1e9)),))
    return p


def free_ram_gb() -> float:
    """Memory free for the app (Windows' "available"), or what a pretended PC would have."""
    free = psutil.virtual_memory().available / 1e9
    cap = profile().free_cap_gb
    return free if cap is None else min(free, cap)


def total_ram_gb() -> float:
    return profile().ram_gb


# ---- choosing ----------------------------------------------------------------------------------


def without_gpu(p: Profile) -> str:
    """Why this PC runs the model on the processor by itself: "none" (no graphics card), "shared"
    (too little video memory, and too little of the PC's to make up for it), or "" (it doesn't)."""
    card = p.best_card
    if card is None:
        return "none"
    shared = card.own_vram < OWN_VRAM_GB * 1e9
    return "shared" if shared and p.ram_gb < SHARED_GPU_MIN_RAM_GB else ""


def choose(p: Profile, use_gpu: bool = True, forced: str | None = None) -> Setup:
    """The setup for this PC. use_gpu=False: not the graphics card now (it failed this session, or
    a game has it)."""
    forced = (forced if forced is not None else os.environ.get("QUIEKEL_EMBED_SETUP", "")).strip().lower()
    card = p.best_card
    shared = card is not None and card.own_vram < OWN_VRAM_GB * 1e9
    card_fits = not without_gpu(p)
    small = p.ram_gb < LIGHT_BELOW_RAM_GB or p.cores < LIGHT_BELOW_CORES
    if forced in SETUPS:
        name = forced if forced != "graphics" or card is not None else "processor"
    else:
        name = "graphics" if use_gpu and card_fits else "light" if small else "processor"
    common = {"threads_cap": p.threads_cap}
    if name == "graphics":
        return Setup("graphics", True, card.index, card.name, shared, threads=max(1, min(4, p.cores // 2)),
                     text_batch=8, image_batch=1, memory_cap_gb=None,
                     ram_needed_gb=3.0 if shared else 0.6, keep_loaded=not shared, **common)
    if name == "processor":
        return Setup("processor", False, 0, "", False, threads=max(1, min(8, p.cores // 2)),
                     text_batch=8, image_batch=1, memory_cap_gb=1.5, ram_needed_gb=0.6, **common)
    return Setup("light", False, 0, "", False, threads=max(1, min(2, p.cores // 2)),
                 text_batch=4, image_batch=1, memory_cap_gb=1.0, ram_needed_gb=0.6, keep_loaded=False,
                 **common)


# ---- graphics cards ----------------------------------------------------------------------------


def graphics_cards() -> list[Card]:
    """Windows' graphics adapters through DXGI. Software adapters (like the Basic Render Driver)
    are left out."""
    if os.name != "nt":
        return []

    class Desc(ctypes.Structure):
        _fields_ = [("Description", ctypes.c_wchar * 128), ("VendorId", wintypes.UINT),
                    ("DeviceId", wintypes.UINT), ("SubSysId", wintypes.UINT), ("Revision", wintypes.UINT),
                    ("DedicatedVideoMemory", ctypes.c_size_t), ("DedicatedSystemMemory", ctypes.c_size_t),
                    ("SharedSystemMemory", ctypes.c_size_t), ("AdapterLuid", ctypes.c_int64),
                    ("Flags", wintypes.UINT)]

    def method(obj, index, *argtypes):
        table = ctypes.cast(obj, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p))).contents
        return ctypes.WINFUNCTYPE(ctypes.c_long, ctypes.c_void_p, *argtypes)(table[index])

    cards = []
    try:
        iid = (ctypes.c_ubyte * 16).from_buffer_copy(uuid.UUID("770aae78-f26f-4dba-a829-253c83d1b387").bytes_le)
        factory = ctypes.c_void_p()
        if ctypes.windll.dxgi.CreateDXGIFactory1(ctypes.byref(iid), ctypes.byref(factory)) != 0:
            return []
        try:
            enum = method(factory, 12, wintypes.UINT, ctypes.POINTER(ctypes.c_void_p))  # EnumAdapters1
            for index in range(16):
                adapter = ctypes.c_void_p()
                if enum(factory, index, ctypes.byref(adapter)) != 0:
                    break
                desc = Desc()
                method(adapter, 10, ctypes.POINTER(Desc))(adapter, ctypes.byref(desc))  # GetDesc1
                method(adapter, 2)(adapter)  # Release
                if not desc.Flags & 0x2:  # DXGI_ADAPTER_FLAG_SOFTWARE
                    cards.append(Card(index, desc.Description, desc.DedicatedVideoMemory))
        finally:
            method(factory, 2)(factory)  # Release
    except (OSError, AttributeError, ValueError):
        return []
    return cards
