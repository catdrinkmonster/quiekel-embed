import threading
import time

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


def test_fullscreen_game_pauses_unless_full_mode():
    m = Metrics(fullscreen=True, idle_s=0)
    assert decide("balanced", m).duty == 0
    assert decide("gentle", m).duty == 0
    assert decide("full", m).duty == 1.0


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
