"""Which setup the model gets, for PCs from a gaming PC to an old laptop, and pretending to be one."""

import pytest

from quiekel_embed import machine

GB = 10**9


def pc(ram, cores, own_vram_gb=None, name="Graphics card"):
    cards = () if own_vram_gb is None else (machine.Card(0, name, int(own_vram_gb * GB)),)
    return machine.Profile(ram_gb=ram, cores=cores, cards=cards)


@pytest.mark.parametrize("what, profile, name, threads, batch, cap", [
    ("gaming PC", pc(32, 8, 8), "graphics", 4, 8, None),
    ("laptop with a small graphics card", pc(16, 4, 4), "graphics", 2, 8, None),
    ("graphics card with 3 GB", pc(8, 4, 3), "graphics", 2, 8, None),
    ("graphics card with 2 GB, 16 GB", pc(16, 4, 2), "graphics", 2, 8, None),
    ("graphics card with 2 GB, 8 GB", pc(8, 4, 2), "light", 2, 4, 1.0),
    ("built-in graphics, 16 GB", pc(16, 4, 0.125), "graphics", 2, 8, None),
    ("built-in graphics, 8 GB", pc(8, 4, 0.125), "light", 2, 4, 1.0),
    ("old card with 1 GB of its own, 8 GB", pc(8, 2, 1), "light", 1, 4, 1.0),
    ("desktop without a graphics card", pc(16, 6), "processor", 3, 8, 1.5),
    ("workstation without a graphics card", pc(64, 16), "processor", 8, 8, 1.5),
    ("old dual core, 4 GB", pc(4, 2), "light", 1, 4, 1.0),
])
def test_every_pc_gets_a_setup_that_fits_it(what, profile, name, threads, batch, cap, monkeypatch):
    monkeypatch.delenv("QUIEKEL_EMBED_SETUP", raising=False)
    setup = machine.choose(profile)
    assert (setup.name, setup.threads, setup.text_batch, setup.memory_cap_gb) == (name, threads, batch, cap), what
    assert setup.gpu == (name == "graphics")


def test_only_setups_that_spare_the_pcs_memory_stay_ready_when_idle(monkeypatch):
    monkeypatch.delenv("QUIEKEL_EMBED_SETUP", raising=False)
    assert machine.choose(pc(32, 8, 8)).keep_loaded  # a card of its own
    assert machine.choose(pc(16, 6)).keep_loaded  # the processor, on a PC with room
    assert not machine.choose(pc(16, 4, 0.125)).keep_loaded  # built-in graphics: the PC's memory
    assert not machine.choose(pc(8, 4)).keep_loaded  # a smaller PC


def test_built_in_graphics_need_their_share_of_the_pcs_memory(monkeypatch):
    monkeypatch.delenv("QUIEKEL_EMBED_SETUP", raising=False)
    shared, own = machine.choose(pc(16, 4, 0.125)), machine.choose(pc(16, 4, 8))
    assert shared.shared and shared.ram_needed_gb > own.ram_needed_gb


def test_without_the_graphics_card_the_processor_takes_over(monkeypatch):
    monkeypatch.delenv("QUIEKEL_EMBED_SETUP", raising=False)
    assert machine.choose(pc(32, 8, 8), use_gpu=False).name == "processor"  # (a game has it, or it failed)
    assert machine.choose(pc(8, 4, 8), use_gpu=False).name == "light"


def test_a_setup_can_be_forced_for_testing(monkeypatch):
    monkeypatch.setenv("QUIEKEL_EMBED_SETUP", "light")
    assert machine.choose(pc(32, 8, 8)).name == "light"
    monkeypatch.setenv("QUIEKEL_EMBED_SETUP", "graphics")
    assert machine.choose(pc(16, 8)).name == "processor"  # (no card to force it onto)


def test_pretending_to_be_another_pc():
    real = pc(32, 8, 8, name="Real card")
    weak = machine.simulate(real, "ram=8 cores=4 gpu=none free=2")
    assert (weak.ram_gb, weak.cores, weak.threads_cap, weak.cards, weak.free_cap_gb, weak.simulated) == \
        (8, 4, 4, (), 2, True)
    built_in = machine.simulate(real, "gpu=igpu").best_card
    assert built_in.index == 0 and built_in.own_vram < machine.OWN_VRAM_GB * GB
    assert machine.simulate(real, "gpu=card:6").best_card.own_vram == 6 * GB
    assert machine.simulate(real, "ram=12").cards == real.cards  # (what's left out stays as it is)
    assert machine.simulate(pc(16, 8), "gpu=broken").best_card.index == 99  # (DirectML can't start there)


def test_a_pretended_pc_has_at_most_its_free_memory(monkeypatch):
    monkeypatch.setattr(machine, "_profile", machine.simulate(pc(32, 8), "free=1.5"))
    assert machine.free_ram_gb() <= 1.5
    monkeypatch.setattr(machine, "_profile", pc(32, 8))
    assert machine.free_ram_gb() > 0


def test_this_pcs_graphics_cards_can_be_read():
    for card in machine.graphics_cards():  # (none on a PC without a graphics driver)
        assert card.name and card.own_vram >= 0


@pytest.mark.parametrize("profile, why", [
    (pc(32, 8, 8), ""),
    (pc(16, 4, 0.125), ""),  # built-in graphics with enough memory to share
    (pc(8, 4, 0.125), "shared"),
    (pc(8, 4, 2), "shared"),  # (a small card: the PC's memory would have to make up for it)
    (pc(32, 8), "none"),
])
def test_why_a_pc_runs_the_model_on_the_processor(profile, why):
    assert machine.without_gpu(profile) == why
