"""Resource governor: decides how hard background indexing may run right now.

Every two seconds it samples CPU, memory, GPU load from *other* processes, free
video memory, battery, whether you are away, and whether a game or fullscreen app
is in front. It turns that into a duty cycle (1.0 = full speed, 0 = paused) plus a
plain-language reason for the UI. The indexer calls `pace()` after each unit of
work and sleeps as much as the duty cycle asks.
"""

import ctypes
import logging
import os
import threading
import time
from collections.abc import Callable
from ctypes import wintypes
from dataclasses import dataclass

import psutil

from . import i18n

log = logging.getLogger(__name__)

MODES = ("gentle", "balanced", "full")  # names and descriptions live in i18n.json
DEFAULT_MODE = "balanced"
AWAY_AFTER_S = 180  # no keyboard/mouse input for this long counts as "away"
FULLSCREEN_HOLD_S = 15  # keep treating a game as running through short alt-tabs


@dataclass
class Metrics:
    cpu_others: float = 0.0  # % of all cores used by other processes
    ram_free_gb: float = 99.0
    ram_total_gb: float = 0.0
    gpu_others: float | None = None  # % GPU time used by other processes
    vram_free_gb: float | None = None
    vram_total_gb: float | None = None
    on_battery: bool = False
    fullscreen: bool = False
    idle_s: float = 0.0


@dataclass
class Decision:
    duty: float  # share of time indexing may work: 1 = full speed, 0 = paused
    level: str  # full | normal | throttled | paused
    code: str  # why, as an i18n key under "gov." (the UI translates it)
    params: dict

    @property
    def reason(self) -> str:
        """English text, for logs."""
        return i18n.t("en", f"gov.{self.code}", **self.params)


def decide(mode: str, m: Metrics) -> Decision:
    """Pure policy: metrics in, duty cycle out. The most restrictive rule wins."""
    caps: list[tuple[float, str, dict]] = []
    gb = lambda v: round(v, 1)  # noqa: E731 - a number: each language writes it its own way

    # Hard limits in every mode: running out of memory hurts everything on the PC.
    if m.ram_free_gb < 1.0:
        caps.append((0.0, "ram_full", {"gb": gb(m.ram_free_gb)}))
    if m.vram_free_gb is not None and m.vram_free_gb < 0.3:
        caps.append((0.0, "vram_full", {}))

    if mode != "full":
        gentle = mode == "gentle"
        away = m.idle_s >= AWAY_AFTER_S
        if m.fullscreen:
            caps.append((0.0, "fullscreen", {}))
        if m.on_battery:
            caps.append((0.0 if gentle else 0.25, "battery", {}))

        hi, mid = (50, 25) if gentle else (85, 60)
        if m.cpu_others >= mid:
            caps.append((0.1 if m.cpu_others >= hi else 0.4, "cpu_busy", {"pct": round(m.cpu_others)}))

        if m.gpu_others is not None:
            hi, mid = (40, 15) if gentle else (70, 35)
            if m.gpu_others >= mid:
                caps.append((0.1 if m.gpu_others >= hi else 0.4, "gpu_busy", {"pct": round(m.gpu_others)}))

        if m.ram_free_gb < (4.0 if gentle else 2.5):
            caps.append((0.3, "ram_low", {"gb": gb(m.ram_free_gb)}))
        if m.vram_free_gb is not None and m.vram_free_gb < 1.0:
            caps.append((0.4, "vram_low", {"gb": gb(m.vram_free_gb)}))

        if gentle:
            caps.append((0.5 if away else 0.25, "gentle", {}))
        elif not away:
            caps.append((0.75, "headroom", {}))

    if not caps:
        return Decision(1.0, "full", "full" if mode == "full" else "full_away", {})
    duty, code, params = min(caps, key=lambda c: c[0])
    level = "paused" if duty == 0 else "normal" if duty >= 0.75 else "throttled"
    return Decision(duty, level, code, params)


# ---- Windows signals -------------------------------------------------------

_SHELL_CLASSES = {"Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"}


class _MONITORINFO(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("rcMonitor", wintypes.RECT),
                ("rcWork", wintypes.RECT), ("dwFlags", wintypes.DWORD)]


class _LASTINPUTINFO(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.UINT), ("dwTime", wintypes.DWORD)]


def foreground_fullscreen() -> bool:
    """True when a game, video or presentation fills the screen (not just a maximized window)."""
    if os.name != "nt":
        return False
    try:
        user32 = ctypes.windll.user32
        state = ctypes.c_int()
        # 3 = exclusive fullscreen Direct3D, 4 = presentation mode
        if ctypes.windll.shell32.SHQueryUserNotificationState(ctypes.byref(state)) == 0 \
                and state.value in (3, 4):
            return True
        hwnd = user32.GetForegroundWindow()
        if not hwnd or user32.IsZoomed(hwnd):
            return False
        pid = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if pid.value == os.getpid():
            return False
        cls = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, cls, 256)
        if cls.value in _SHELL_CLASSES:
            return False
        rect = wintypes.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        info = _MONITORINFO(cbSize=ctypes.sizeof(_MONITORINFO))
        user32.GetMonitorInfoW(user32.MonitorFromWindow(hwnd, 2), ctypes.byref(info))
        mon = info.rcMonitor
        return (rect.left <= mon.left and rect.top <= mon.top
                and rect.right >= mon.right and rect.bottom >= mon.bottom)
    except (AttributeError, OSError):
        return False


def idle_seconds() -> float:
    if os.name != "nt":
        return 0.0
    try:
        info = _LASTINPUTINFO(cbSize=ctypes.sizeof(_LASTINPUTINFO))
        ctypes.windll.user32.GetLastInputInfo(ctypes.byref(info))
        return ((ctypes.windll.kernel32.GetTickCount() - info.dwTime) & 0xFFFFFFFF) / 1000.0
    except (AttributeError, OSError):
        return 0.0


def set_background_priority(enabled: bool):
    """Windows background mode for the calling thread: low CPU, disk and memory priority."""
    if os.name != "nt":
        return
    kernel32 = ctypes.windll.kernel32
    mode = 0x00010000 if enabled else 0x00020000  # THREAD_MODE_BACKGROUND_BEGIN / _END
    kernel32.SetThreadPriority(kernel32.GetCurrentThread(), mode)


class _Nvml:
    """GPU load from other processes and free video memory, via NVIDIA's NVML."""

    def __init__(self):
        import pynvml

        pynvml.nvmlInit()
        self.nv = pynvml
        self.handle = pynvml.nvmlDeviceGetHandleByIndex(0)
        self.own_pid = os.getpid()

    @classmethod
    def open(cls):
        try:
            return cls()
        except Exception:
            return None  # no NVIDIA GPU or driver: GPU signals are simply unavailable

    def read(self) -> tuple[float | None, float, float]:
        nv = self.nv
        mem = nv.nvmlDeviceGetMemoryInfo(self.handle)
        try:
            since = int((time.time() - 3) * 1_000_000)
            per_pid: dict[int, int] = {}
            for s in nv.nvmlDeviceGetProcessUtilization(self.handle, since):
                if s.pid != self.own_pid:
                    per_pid[s.pid] = max(per_pid.get(s.pid, 0), s.smUtil)
            others = float(min(100, sum(per_pid.values())))
        except nv.NVMLError as e:
            # NOT_FOUND = no samples in the window, i.e. nobody else used the GPU.
            others = 0.0 if e.value == nv.NVML_ERROR_NOT_FOUND else None
        return others, mem.free / 1e9, mem.total / 1e9


class Governor:
    def __init__(self, get_mode: Callable[[], str], interval: float = 2.0):
        self._get_mode = get_mode
        self._interval = interval
        self._proc = psutil.Process()
        self._ncpu = psutil.cpu_count() or 1
        self._nvml = _Nvml.open()
        self._fullscreen_until = 0.0
        self._logged = -60.0
        self._lock = threading.Lock()
        self.metrics = Metrics()
        self.decision = Decision(1.0, "full", "starting", {})
        psutil.cpu_percent(None)  # prime the counters
        self._proc.cpu_percent(None)
        self.sample()
        threading.Thread(target=self._loop, name="governor", daemon=True).start()

    @property
    def gpu_available(self) -> bool:
        return self._nvml is not None

    def _loop(self):
        while True:
            time.sleep(self._interval)
            try:
                self.sample()
            except Exception:
                log.exception("Resource sampling failed")

    def sample(self):
        now = time.monotonic()
        total = psutil.cpu_percent(None)
        own = self._proc.cpu_percent(None) / self._ncpu
        battery = psutil.sensors_battery()
        if foreground_fullscreen():
            self._fullscreen_until = now + FULLSCREEN_HOLD_S
        gpu_others = vram_free = vram_total = None
        if self._nvml:
            try:
                gpu_others, vram_free, vram_total = self._nvml.read()
            except Exception:
                log.exception("NVML read failed")
        prev = self.metrics
        vm = psutil.virtual_memory()
        m = Metrics(
            cpu_others=_smooth(prev.cpu_others, max(0.0, total - own)),
            ram_free_gb=vm.available / 1e9,
            ram_total_gb=vm.total / 1e9,
            gpu_others=None if gpu_others is None else _smooth(prev.gpu_others, gpu_others),
            vram_free_gb=vram_free,
            vram_total_gb=vram_total,
            on_battery=bool(battery and not battery.power_plugged),
            fullscreen=now < self._fullscreen_until,
            idle_s=idle_seconds(),
        )
        d = decide(self._get_mode(), m)
        with self._lock:
            # A new level is logged right away. A reason that keeps flipping (another app's GPU load
            # hovering around a threshold) at most once a minute, or it would fill the log.
            if d.level != self.decision.level or (d.reason != self.decision.reason and now - self._logged >= 60):
                log.info("Indexing speed: %s (%.0f%%)", d.reason, d.duty * 100)
                self._logged = now
            self.metrics, self.decision = m, d

    def refresh(self):
        """Re-decide with the latest metrics, e.g. right after the mode changed."""
        with self._lock:
            self.decision = decide(self._get_mode(), self.metrics)

    def pace(
        self,
        work_s: float,
        should_stop: Callable[[], bool] = lambda: False,
        while_paused: Callable[[], None] | None = None,
    ):
        """Called after each unit of indexing work: sleeps to honour the duty cycle,
        and blocks while indexing is paused for resources (calling `while_paused`
        every couple of seconds, e.g. to hand the GPU back)."""
        last_hook = 0.0
        while self.decision.duty <= 0 and not should_stop():
            if while_paused and time.monotonic() - last_hook >= 2:
                while_paused()
                last_hook = time.monotonic()
            time.sleep(0.5)
        duty = self.decision.duty
        if duty <= 0 or duty >= 1 or work_s <= 0:
            return  # stopped while paused (the caller handles that), or nothing to pace
        end = time.monotonic() + min(work_s * (1 - duty) / duty, 20.0)
        # Sleep in small steps so that e.g. walking away switches to full speed at once.
        while not should_stop() and self.decision.duty < 1:
            left = end - time.monotonic()
            if left <= 0:
                break
            time.sleep(min(0.25, left))


def _smooth(prev: float | None, new: float, alpha: float = 0.5) -> float:
    return new if prev is None else prev + alpha * (new - prev)
