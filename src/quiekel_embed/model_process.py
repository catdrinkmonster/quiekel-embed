"""The model in a process of its own.

The app never loads the model itself: a model process does, with the setup chosen for the PC
(machine.py). Starting it loads the model; stopping it gives every byte back, which letting go of
a model inside a running process doesn't reliably do. Windows (a job object) holds it to its
setup's memory cap and processor threads, runs it below normal priority, and ends it together
with the app. When it crashes or runs out of memory, only it stops: the app starts it again.

They talk over a pipe, one request at a time: a batch of texts or pictures, or a search query.
Messages (pickled):
  app -> model: the setup (once), then ("embed", "text" | "image", items, prompt),
                ("query", text) or ("quit",)
  model -> app: ("status", "downloading" | "loading") and ("ready", {}) or ("failed", kind, text)
                while starting; then ("ok", vectors) or ("error", kind, text) per request;
                ("log", level, text) at any time.
  kind: "oom" (out of memory: smaller batches help), "gpu" (the graphics card failed to start),
        "input" (an item can't be prepared: that file's fault) or "engine" (anything else).
"""

import ctypes
import hashlib
import logging
import os
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from ctypes import wintypes
from multiprocessing import Pipe
from multiprocessing.connection import Connection

import numpy as np

from . import config

log = logging.getLogger(__name__)


def is_oom(e: BaseException) -> bool:
    """Running out of memory, as the engine or Windows says it (also on non-English Windows)."""
    if isinstance(e, MemoryError):
        return True
    text = str(e).lower()
    return (type(e).__name__ == "OutOfMemoryError" or "out of memory" in text
            or "e_outofmemory" in text or "8007000e" in text  # (DirectML's way of saying it)
            or "not enough memory" in text or "bad allocation" in text or "bad_alloc" in text)


def _text(e: BaseException) -> str:
    return f"{type(e).__name__}: {e}"[:500]


# ---- in the model process ----------------------------------------------------------------------


class _Forward(logging.Handler):
    """The model process's log goes to the app's log, through the pipe."""

    def __init__(self, conn: Connection):
        super().__init__(logging.INFO)
        self.conn = conn

    def emit(self, record):
        try:
            self.conn.send(("log", record.levelno, f"{record.name}: {record.getMessage()}"))
        except Exception:
            pass


class StandIn:
    """A tiny stand-in for the model (tests and previews): the same vector for the same text, and
    a few texts that act up on purpose. "!bad" can't be prepared, "!oom" runs out of memory,
    "!oom-over:N" too in a batch of more than N, "!crash" ends the process, "!hang" never answers,
    "!alloc:N" takes N MB first (out of memory under a cap). A graphics card named "!broken..."
    doesn't start."""

    prompts = {"SearchQuery": "task: search result | query: "}

    @staticmethod
    def _input_length(item) -> int:
        return len(item) if isinstance(item, str) else 0

    def preprocess(self, items: list, prompt: str | None = None) -> dict:
        if any(isinstance(x, str) and x.startswith("!bad") for x in items):
            raise ValueError("can't be read")
        return {"items": [x if isinstance(x, str) else np.asarray(x["image"]).mean() for x in items]}

    def embed(self, features: dict) -> np.ndarray:
        out = []
        for item in features["items"]:
            text = str(item)
            if text.startswith("!alloc:"):
                hold = bytearray(int(text[7:]) * 2**20)  # noqa: F841 - held while this batch runs
            if text.startswith("!oom-over:") and len(features["items"]) > int(text[10:]):
                raise MemoryError("out of memory")
            if text.startswith("!oom") and not text.startswith("!oom-over:"):
                raise MemoryError("out of memory")
            if text.startswith("!crash"):
                os._exit(3)
            if text.startswith("!hang"):
                threading.Event().wait()
            seed = int.from_bytes(hashlib.sha256(text.encode()).digest()[:8], "little")
            v = np.random.default_rng(seed).standard_normal(config.EMBED_DIM).astype(np.float32)
            out.append(v / np.linalg.norm(v))
        return np.stack(out) if out else np.zeros((0, config.EMBED_DIM), np.float32)


def _make_model(setup: dict, report: Callable[[str], None]):
    if setup["model"] == "stand-in":
        if setup["gpu"] and setup["card"].startswith("!broken"):
            raise RuntimeError("DirectML didn't start on this graphics card")
        return StandIn()
    from . import onnx_engine

    gpu = setup["gpu"]
    report("loading" if onnx_engine.cached(gpu) else "downloading")
    return onnx_engine.OnnxModel(gpu, setup["device"], threads=setup["threads"])


def _answer(model, request: tuple, gpu: bool) -> tuple:
    from PIL import Image

    try:
        if request[0] == "query":
            features = model.preprocess([request[1]], prompt=model.prompts.get("SearchQuery"))
        else:
            _, kind, items, prompt = request
            if kind == "image":
                items = [{"image": Image.fromarray(a)} for a in items]
            features = model.preprocess(items, prompt=prompt)
    except Exception as e:
        return ("error", "oom" if is_oom(e) else "input", _text(e))
    try:
        return ("ok", np.asarray(model.embed(features), np.float32))
    except Exception as e:
        # On the graphics card any failure counts as running out of memory: DirectML's errors
        # can't always even be read (on German Windows onnxruntime once failed to decode them).
        return ("error", "oom" if gpu or is_oom(e) else "engine", _text(e))


def serve(conn: Connection):
    """The model process: load the model with the setup the app sends, then answer requests."""
    setup = conn.recv()
    logging.getLogger().addHandler(_Forward(conn))
    logging.getLogger().setLevel(logging.INFO)
    try:
        model = _make_model(setup, lambda state: conn.send(("status", state)))
    except Exception as e:
        kind = "oom" if is_oom(e) else "gpu" if setup["gpu"] else "engine"
        conn.send(("failed", kind, _text(e)))
        return
    conn.send(("ready", {}))
    while True:
        try:
            request = conn.recv()
        except (EOFError, OSError):
            return  # the app is gone
        if request[0] == "quit":
            return
        conn.send(_answer(model, request, setup["gpu"]))


def main():
    from multiprocessing.connection import PipeConnection

    serve(PipeConnection(int(sys.argv[1])))


# ---- in the app --------------------------------------------------------------------------------


class Gone(Exception):
    """The model process isn't there anymore (it crashed, ran out of memory, or was stopped)."""


class StartFailed(Exception):
    def __init__(self, kind: str, text: str):
        super().__init__(text)
        self.kind = kind


# No answer to a request for this long: the process hangs, and is ended (a batch takes seconds;
# the slowest PC, busy with a game, a few minutes). Not while starting: the download may be slow.
REQUEST_TIMEOUT_S = 900

_CREATE_SUSPENDED, _CREATE_NO_WINDOW, _BELOW_NORMAL = 0x4, 0x08000000, 0x4000
_LIMIT_AFFINITY, _LIMIT_PRIORITY_CLASS = 0x10, 0x20
_LIMIT_PROCESS_MEMORY, _LIMIT_KILL_ON_JOB_CLOSE = 0x100, 0x2000


class _BasicLimits(ctypes.Structure):
    _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                ("SchedulingClass", wintypes.DWORD)]


class _ExtendedLimits(ctypes.Structure):
    _fields_ = [("BasicLimitInformation", _BasicLimits), ("IoInfo", ctypes.c_uint64 * 6),
                ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t)]


def _job(process_handle: int, memory_cap_gb: float | None, threads: int | None) -> int:
    """A job object holding the process: below normal priority, its memory cap, its processor
    threads, and ended when the app ends. Returns the job's handle (closing it ends the process)."""
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    job = kernel32.CreateJobObjectW(None, None)
    if not job:
        raise OSError(ctypes.get_last_error(), "CreateJobObject failed")
    limits = _ExtendedLimits()
    basic = limits.BasicLimitInformation
    basic.LimitFlags = _LIMIT_KILL_ON_JOB_CLOSE | _LIMIT_PRIORITY_CLASS
    basic.PriorityClass = _BELOW_NORMAL
    if memory_cap_gb:
        basic.LimitFlags |= _LIMIT_PROCESS_MEMORY
        limits.ProcessMemoryLimit = int(memory_cap_gb * 1e9)
    if threads:
        basic.LimitFlags |= _LIMIT_AFFINITY
        basic.Affinity = (1 << min(threads, os.cpu_count() or threads)) - 1
    if not kernel32.SetInformationJobObject(wintypes.HANDLE(job), 9, ctypes.byref(limits), ctypes.sizeof(limits)):
        raise OSError(ctypes.get_last_error(), "SetInformationJobObject failed")
    if not kernel32.AssignProcessToJobObject(wintypes.HANDLE(job), wintypes.HANDLE(process_handle)):
        raise OSError(ctypes.get_last_error(), "AssignProcessToJobObject failed")
    return job


def _executable() -> tuple[str, dict]:
    """The Python to start, and its environment. In a virtual environment, its base Python told to
    act as the environment's (what multiprocessing does on Windows): one process, not a launcher
    that starts another one."""
    env = dict(os.environ)
    base = getattr(sys, "_base_executable", sys.executable)
    if os.name == "nt" and base and os.path.normcase(base) != os.path.normcase(sys.executable):
        env["__PYVENV_LAUNCHER__"] = sys.executable
        return base, env
    return sys.executable, env


class ModelProcess:
    def __init__(self, setup):
        self.setup = setup
        self._lock = threading.Lock()  # one request at a time
        self._popen: subprocess.Popen | None = None
        self._conn: Connection | None = None
        self._job: int | None = None
        self._dead = False

    @property
    def alive(self) -> bool:
        return self._popen is not None and not self._dead and self._popen.poll() is None

    @property
    def pid(self) -> int | None:
        return self._popen.pid if self._popen else None

    def start(self, on_status: Callable[[str], None] = lambda state: None):
        """Start the process and wait until its model is loaded (or StartFailed)."""
        ours, theirs = Pipe(duplex=True)
        handle = theirs.fileno()
        os.set_handle_inheritable(handle, True)
        exe, env = _executable()
        info = subprocess.STARTUPINFO(lpAttributeList={"handle_list": [handle]})
        self._popen = subprocess.Popen(
            [exe, "-m", "quiekel_embed.model_process", str(handle)], env=env, startupinfo=info,
            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            creationflags=_CREATE_SUSPENDED | _CREATE_NO_WINDOW | _BELOW_NORMAL,
        )
        theirs.close()
        self._conn = ours
        try:
            self._job = _job(int(self._popen._handle), self.setup.memory_cap_gb, self.setup.threads_cap)
        except OSError as e:
            self._popen.kill()  # (still suspended: it never ran)
            self._dead = True
            raise StartFailed("engine", _text(e)) from e
        ctypes.windll.ntdll.NtResumeProcess(wintypes.HANDLE(int(self._popen._handle)))
        try:
            ours.send(self.setup.as_dict())
            while True:
                message = self._receive()
                if message[0] == "status":
                    on_status(message[1])
                elif message[0] == "ready":
                    log.info("Model process %d ready (%s setup)", self._popen.pid, self.setup.name)
                    return
                elif message[0] == "failed":
                    raise StartFailed(message[1], message[2])
        except Gone as e:
            self.stop()
            raise StartFailed("engine", str(e)) from e
        except StartFailed:
            self.stop()
            raise

    def ask(self, request: tuple) -> tuple:
        """A request and its answer. Gone if the process went away meanwhile."""
        with self._lock:
            if not self.alive:
                raise Gone("the model process isn't running")
            try:
                self._conn.send(request)
            except (OSError, ValueError) as e:
                self._dead = True
                raise Gone(_text(e)) from e
            return self._receive(timeout=REQUEST_TIMEOUT_S)

    def _receive(self, timeout: float | None = None) -> tuple:
        """The next message that isn't a log line. Gone when the process ends, or when it doesn't
        answer in time (it's ended then)."""
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            try:
                while not self._conn.poll(0.25):
                    if self._popen.poll() is not None:
                        raise EOFError
                    if deadline is not None and time.monotonic() > deadline:
                        self._popen.kill()
                        raise EOFError("no answer")
                message = self._conn.recv()
            except (EOFError, OSError, ValueError) as e:
                self._dead = True
                try:
                    code = self._popen.wait(2)
                except subprocess.TimeoutExpired:
                    code = None
                raise Gone(f"the model process ended (exit code {code})") from e
            if message[0] == "log":
                log.log(message[1], "Model process: %s", message[2])
                continue
            return message

    def stop(self):
        """Ask it to end, end it if it doesn't, and give everything back."""
        popen = self._popen
        if popen is None:
            return
        waited = self._lock.acquire(timeout=15)  # (a batch under way finishes first)
        try:
            self._end(popen)
        finally:
            if waited:
                self._lock.release()

    def _end(self, popen: subprocess.Popen):
        if popen.poll() is None and not self._dead:
            try:
                self._conn.send(("quit",))
            except (OSError, ValueError):
                pass
            try:
                popen.wait(5)
            except subprocess.TimeoutExpired:
                popen.kill()
        if self._job:
            ctypes.windll.kernel32.CloseHandle(wintypes.HANDLE(self._job))  # (ends whatever is left)
            self._job = None
        if self._conn:
            self._conn.close()
        self._dead = True

    def memory_mb(self) -> float:
        try:
            import psutil

            return psutil.Process(self._popen.pid).memory_info().rss / 1e6 if self.alive else 0.0
        except Exception:
            return 0.0


if __name__ == "__main__":
    main()
