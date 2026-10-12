"""The model, as the app sees it.

EmbeddingGemma 2 runs through ONNX Runtime in a process of its own (model_process.py), with the
setup chosen for the PC (machine.py): on a graphics card through DirectML, or on the processor.
This is the app's side: it starts and stops that process, sends it the work in batches (longest
texts first, so each batch needs little padding), lets the governor pace between batches, and
halves a batch when the process runs out of memory.

The process only starts when there's memory to spare for the setup. When the graphics card fails
(it doesn't start, crashes, or keeps running out of memory), the processor takes over for the rest
of the session. Failures of the engine are never blamed on a file: the work waits and is tried
again (EngineError, GpuBusy, ModelAsleep). Only an item that can't be prepared is (BadInput).
"""

import logging
import threading
import time
from collections.abc import Callable
from dataclasses import replace

import numpy as np

from . import config, machine
from .model_process import Gone, ModelProcess, StartFailed

log = logging.getLogger(__name__)

RAM_SPARE_GB = 0.5  # left free for the rest of the PC when the model starts for a search


class GpuBusy(Exception):
    """The model ran out of memory even for a single item. Try again later."""


class ModelAsleep(Exception):
    """The model was let go while work was under way (a game started, memory ran low): the work
    waits until it's loaded again."""


class EngineError(Exception):
    """The engine failed by itself, whatever it was given: the work is tried again later."""


class BadInput(Exception):
    """An item that can't be prepared for the model (a broken picture, say): its file is unreadable."""


class NotEnoughMemory(Exception):
    """Too little free memory to start the model right now."""

    def __init__(self, free_gb: float, need_gb: float):
        super().__init__(f"{free_gb:.1f} GB free, {need_gb:.1f} GB needed")
        self.free_gb, self.need_gb = free_gb, need_gb


class Embedder:
    def __init__(self, profile: machine.Profile | None = None, model: str = "onnx"):
        self._profile = profile
        self._model = model  # "stand-in": a tiny model, for tests and previews
        self._proc: ModelProcess | None = None
        self._load_lock = threading.Lock()  # a search and indexing may both want to start it
        self.setup: machine.Setup | None = None
        self.status = "not loaded"  # not loaded | downloading | loading | ready | asleep | error
        self.error: str | None = None
        self.on_gpu = False
        # Why it runs on the processor: none (no graphics card), shared (built-in graphics, too
        # little memory), failed (the card failed this session), busy (a game, its memory full).
        self.cpu_because = ""
        self.gpu_name = ""
        self.engine = ""  # directml | cpu: where the model runs
        self.download_gb = 0.9  # the model's size to download for where it runs (the page shows it)
        # Set by the indexer: called with the seconds each batch took, to pace itself.
        self.pace: Callable[[float], None] | None = None
        # Set by the indexer: < 1 shrinks batches when video memory is tight.
        self.batch_scale = 1.0
        self.last_used = time.monotonic()
        # The graphics card failed this session: the processor does the work until the app starts again.
        self.gpu_trouble = False

    @property
    def profile(self) -> machine.Profile:
        return self._profile or machine.profile()

    @property
    def ready(self) -> bool:
        return self._proc is not None and self._proc.alive

    @property
    def keep_loaded(self) -> bool:
        """Whether its setup stays ready for searches while there's nothing to do (Balanced): not on
        a smaller PC or built-in graphics, where it takes the PC's own memory."""
        setup = self.setup or machine.choose(self.profile, use_gpu=not self.gpu_trouble)
        return setup.keep_loaded

    @property
    def could_use_gpu(self) -> bool:
        """The graphics card would take the model, when it's free."""
        return not self.gpu_trouble and machine.choose(self.profile).gpu

    @property
    def device(self) -> str:
        if not self.ready:
            return ""
        return f"{self.gpu_name} (DirectML)" if self.on_gpu else "CPU"

    @property
    def pid(self) -> int | None:
        """The model process, while it runs (the governor counts its work as the app's own)."""
        proc = self._proc
        return proc.pid if proc is not None and proc.alive else None

    def memory_mb(self) -> float:
        proc = self._proc
        return proc.memory_mb() if proc else 0.0

    # ---- starting and stopping ----------------------------------------------------------------

    def load(self, prefer_gpu: bool = True, spare_gb: float = RAM_SPARE_GB):
        """Start the model process; on the processor when prefer_gpu is False (e.g. a game is
        running). Raises NotEnoughMemory unless spare_gb stays free besides what it takes."""
        with self._load_lock:
            if self.ready:
                return
            self.error = None
            setup = machine.choose(self.profile, use_gpu=prefer_gpu and not self.gpu_trouble)
            try:
                try:
                    self._start(setup, spare_gb)
                except StartFailed as e:
                    if not setup.gpu:
                        raise
                    log.warning("The graphics card didn't take the model (%s): using the processor", e)
                    self.gpu_trouble = True
                    self._start(machine.choose(self.profile, use_gpu=False), spare_gb)
            except NotEnoughMemory as e:
                self.status = "asleep"  # (starts once there's room)
                log.info("Model waits for memory (%s)", e)
                raise
            except StartFailed as e:
                self.status, self.error = "error", str(e)
                log.error("Model failed to load: %s", e)
                raise
            self.cpu_because = "" if self.on_gpu else "failed" if self.gpu_trouble else \
                machine.without_gpu(self.profile) or "busy"
        self.last_used = time.monotonic()

    def _start(self, setup: machine.Setup, spare_gb: float):
        need, free = setup.ram_needed_gb + spare_gb, machine.free_ram_gb()
        if free < need:
            raise NotEnoughMemory(free, need)
        if self._model != "onnx":
            setup = replace(setup, model=self._model)
        self.download_gb = 0.9 if setup.gpu else 0.5  # (half precision for a graphics card, 8-bit otherwise)
        self.status = "loading"
        proc = ModelProcess(setup)
        proc.start(on_status=lambda state: setattr(self, "status", state))
        self._proc, self.setup = proc, setup
        self.on_gpu, self.gpu_name = setup.gpu, setup.card
        self.engine = "directml" if setup.gpu else "cpu"
        self.status = "ready"
        log.info("Model ready on %s (%s setup, %d threads)", self.device, setup.name, setup.threads)

    def unload(self, why: str):
        """Stop the model process: all its memory (and video memory) is given back. The next
        search or file to index starts it again."""
        proc = self._proc
        if proc is None:
            return
        with self._load_lock:
            if self._proc is not proc:
                return
            self._proc = None
            self.on_gpu, self.engine, self.status = False, "", "asleep"
        proc.stop()
        log.info("Model unloaded (%s)", why)

    def avoid_gpu(self, why: str):
        """The graphics card keeps failing: stop, and start on the processor from now on."""
        if self.gpu_trouble:
            return
        self.gpu_trouble = True
        log.info("Using the processor from now on (%s)", why)
        if self.on_gpu:
            self.unload(why)

    # ---- the work -----------------------------------------------------------------------------

    def _ask(self, request: tuple) -> tuple:
        proc = self._proc
        if proc is None:
            raise ModelAsleep("the model was let go")
        try:
            return proc.ask(request)
        except Gone as e:
            if self._proc is not proc:
                raise ModelAsleep("the model was let go") from e
            # It crashed, or Windows held it to its memory cap: started again for the next try.
            with self._load_lock:
                if self._proc is proc:
                    self._proc = None
                    self.on_gpu, self.engine, self.status = False, "", "asleep"
            if proc.setup.gpu:
                self.gpu_trouble = True  # (on the graphics card: the processor takes over)
            proc.stop()
            log.warning("The model process stopped: %s", e)
            raise EngineError(str(e)) from e

    def _embed(self, kind: str, items: list, base: int, prompt: str | None = None) -> np.ndarray:
        """Embed in batches, longest first so each batch needs little padding. A batch that runs
        out of memory is halved; a single item that does raises GpuBusy."""
        out = np.zeros((len(items), config.EMBED_DIM), np.float32)
        if not items:
            return out
        order = sorted(range(len(items)), key=lambda i: -len(items[i]) if isinstance(items[i], str) else 0)
        size = max(1, int(base * self.batch_scale))
        i = 0
        while i < len(order):
            end = min(i + size, len(order))
            started = time.perf_counter()
            reply = self._ask(("embed", kind, [items[j] for j in order[i:end]], prompt))
            if reply[0] == "ok":
                out[order[i:end]] = reply[1]
                i = end
                if self.pace:
                    self.pace(time.perf_counter() - started)
                continue
            _, problem, text = reply
            if problem == "input":
                raise BadInput(text)
            if problem != "oom":
                raise EngineError(text)
            if size == 1:
                raise GpuBusy(text)
            size = max(1, size // 2)
            log.info("Memory tight: batch size now %d (%s)", size, text[:120])
        return out

    def _batch(self, kind: str) -> int:
        setup = self.setup
        if setup is None:
            return 8 if kind == "text" else 1
        return setup.text_batch if kind == "text" else setup.image_batch

    def documents(self, texts: list[str]) -> np.ndarray:
        """Embed passages already formatted as 'title: ... | text: ...'."""
        self.last_used = time.monotonic()
        return self._embed("text", texts, self._batch("text"), prompt="")

    def images(self, images: list) -> np.ndarray:
        self.last_used = time.monotonic()
        arrays = [np.asarray(im.convert("RGB")) for im in images]
        return self._embed("image", arrays, self._batch("image"))

    def query(self, text: str) -> np.ndarray:
        self.last_used = time.monotonic()
        reply = self._ask(("query", text))
        if reply[0] != "ok":
            raise GpuBusy(reply[2]) if reply[1] == "oom" else EngineError(reply[2])
        return reply[1][0]

