import os
import subprocess
import sys
import threading
import time

import pytest

from quiekel_embed import governor
from quiekel_embed.governor import AWAY_AFTER_S, Decision, Governor, Metrics, decide

AWAY = AWAY_AFTER_S + 1


def bare_governor(duty: float) -> Governor:
    g = Governor.__new__(Governor)  # no sampling thread: we set decisions by hand
    g.decision = Decision(duty, "normal", "headroom", {})
    return g


def test_pace_sleeps_in_proportion_to_the_duty_cycle():
    g = bare_governor(0.5)
    started = time.monotonic()
    g.pace(0.2)  # worked 0.2 s at 50% duty -> rest about 0.2 s
    assert 0.15 < time.monotonic() - started < 0.45
    g.decision = Decision(1.0, "full", "full", {})
    started = time.monotonic()
    g.pace(5.0)
    assert time.monotonic() - started < 0.05


def test_pace_blocks_while_paused_and_hands_back_resources():
    g = bare_governor(0.0)
    hook_calls = []

    def resume():
        g.decision = Decision(1.0, "full", "full", {})

    threading.Timer(0.8, resume).start()
    started = time.monotonic()
    g.pace(0.1, while_paused=lambda: hook_calls.append(1))
    assert time.monotonic() - started >= 0.7
    assert hook_calls  # e.g. the indexer releases the GPU while a game runs


def test_pace_stops_waiting_when_the_user_pauses():
    g = bare_governor(0.0)
    started = time.monotonic()
    g.pace(0.1, should_stop=lambda: time.monotonic() - started > 0.3)
    assert time.monotonic() - started < 1.5


def test_idle_machine_runs_full_speed_when_away():
    d = decide("balanced", Metrics(idle_s=AWAY))
    assert d.duty == 1.0 and d.level == "full"


def test_balanced_leaves_headroom_while_user_is_active():
    d = decide("balanced", Metrics(idle_s=5))
    assert 0.5 < d.duty < 1.0 and d.level == "normal"


def test_busy_apps_slow_indexing_down_and_only_memory_or_a_game_stops_it():
    busy = Metrics(cpu_others=95, gpu_others=95, idle_s=0)  # busy, but no game in front
    assert decide("balanced", busy).duty > 0 and decide("gentle", busy).duty > 0  # slower, never stopped
    game = Metrics(cpu_others=60, gpu_others=95, idle_s=0, game=True)
    assert decide("balanced", game).code == decide("gentle", game).code == "game"
    assert decide("balanced", game).duty == 0
    assert decide("full", game).duty == 1.0  # Maximum: anytime
    assert decide("off", Metrics(idle_s=AWAY)).duty == 0 and decide("off", Metrics()).code == "off"
    assert decide("balanced", Metrics(ram_free_gb=0.5, idle_s=AWAY)).duty == 0
    assert decide("full", Metrics(vram_free_gb=0.2, vram_total_gb=8.0)).duty == 0


def test_busy_cpu_and_gpu_throttle_balanced():
    assert decide("balanced", Metrics(cpu_others=90, idle_s=AWAY)).duty == 0.1
    assert decide("balanced", Metrics(cpu_others=65, idle_s=AWAY)).duty == 0.4
    assert decide("balanced", Metrics(cpu_others=30, idle_s=AWAY)).duty == 1.0
    d = decide("balanced", Metrics(gpu_others=80, idle_s=AWAY))
    assert d.duty == 0.1 and d.code == "gpu_busy" and d.params == {"pct": 80}


def test_gentle_is_more_sensitive_than_balanced():
    m = Metrics(cpu_others=40, idle_s=AWAY)
    assert decide("gentle", m).duty < decide("balanced", m).duty


def test_memory_limits_apply_in_every_mode():
    m = Metrics(ram_free_gb=0.6, idle_s=AWAY)
    for mode in ("gentle", "balanced", "full"):
        d = decide(mode, m)
        assert d.duty == 0 and d.code == "ram_full"
    assert decide("full", Metrics(vram_free_gb=0.1)).duty == 0


def test_battery():
    assert decide("gentle", Metrics(on_battery=True, idle_s=AWAY)).duty == 0
    assert decide("balanced", Metrics(on_battery=True, idle_s=AWAY)).duty == 0.25


def test_most_restrictive_reason_wins():
    d = decide("balanced", Metrics(cpu_others=65, gpu_others=80, idle_s=5))
    assert d.duty == 0.1 and d.code == "gpu_busy"


class FakeCounters:
    def __init__(self, load):
        self.load = load

    def read(self, ours):
        return self.load


def sampling_governor(counters) -> Governor:
    import psutil

    g = Governor.__new__(Governor)  # no sampling thread: sample() by hand
    g._get_mode, g._interval = (lambda: "balanced"), 2.0
    g._proc, g._ncpu = psutil.Process(), psutil.cpu_count() or 1
    g._nvml, g._counters, g._samples = None, counters, 0
    g.helpers, g._helper_procs = (lambda: []), {}
    g._game_until, g._logged, g._lock = 0.0, -60.0, threading.Lock()
    g.metrics, g.decision = Metrics(), Decision(1.0, "full", "starting", {})
    return g


@pytest.mark.parametrize("front, load, game", [
    ("window", 80.0, True),  # a game in a borderless window, on any graphics card
    ("window", 5.0, False),  # a video
    ("window", None, False),  # the load can't be read: only exclusive fullscreen counts
    ("exclusive", None, True),
    (None, 90.0, False),  # busy, but nothing fills the screen
])
def test_a_game_fills_the_screen_and_keeps_the_graphics_card_busy(monkeypatch, front, load, game):
    monkeypatch.setattr(governor, "foreground_fullscreen", lambda: front)
    g = sampling_governor(FakeCounters(load))
    g.sample()
    assert g.metrics.game == game


def test_windows_gpu_counters_read_as_a_share():
    counters = governor._GpuCounters.open()  # (none on a PC without a graphics driver)
    if counters is not None:
        time.sleep(0.2)
        load = counters.read({os.getpid()})
        assert load is None or 0 <= load <= 100


def test_the_model_processs_work_counts_as_the_apps_own():
    python = getattr(sys, "_base_executable", sys.executable)  # (a venv's python.exe only starts this one)
    busy = subprocess.Popen([python, "-c", "while True: pass"])  # stands in for the model
    try:
        g = sampling_governor(None)
        assert g._own_cpu([busy.pid]) < 5  # (a new process: its first reading counts 0)
        time.sleep(0.6)
        assert g._own_cpu([busy.pid]) > 100 / (os.cpu_count() or 1) / 2  # about one core: ours
    finally:
        busy.kill()
