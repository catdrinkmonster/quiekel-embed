import re

from quiekel_embed import i18n

PLACEHOLDER = re.compile(r"\{(\w+)\}")
STATIC = i18n.FILE.parent
NAMESPACES = ("activity|search|folders|settings|update|result|match|keys|level|mode|device|model|common|nav|"
              "lang|time|eta|gov|note|err|upd|tray|splash|map|scope|theme|types|why|folder|problem|perf|help")


def test_every_language_has_exactly_the_english_keys_and_placeholders():
    cat = i18n.catalog()
    english = cat["en"]
    for lang in i18n.languages():
        strings = cat[lang]
        assert set(strings) == set(english), (
            f"{lang}: missing {sorted(set(english) - set(strings))}, extra {sorted(set(strings) - set(english))}")
        for key, text in english.items():
            assert set(PLACEHOLDER.findall(strings[key])) == set(PLACEHOLDER.findall(text)), f"{lang}: {key}"
            assert strings[key].strip(), f"{lang}: {key} is empty"


def test_every_key_the_ui_uses_exists():
    english = i18n.catalog()["en"]
    js = (STATIC / "app.js").read_text(encoding="utf-8")
    html = (STATIC / "index.html").read_text(encoding="utf-8")
    used = set(re.findall(rf'"((?:{NAMESPACES})\.[a-z_.]+)"', js))
    used |= set(re.findall(r'data-i18n(?:-[a-z]+)?="([a-z_.]+)"', html))
    # tp("key", n) uses "key.one" / "key.other".
    missing = [k for k in sorted(used) if k not in english and f"{k}.other" not in english]
    assert not missing, missing


def test_examples_have_the_same_count_everywhere():
    cat = i18n.catalog()
    for lang in i18n.languages():
        assert len(cat[lang]["search.examples"].split("|")) == 4


def test_translate_with_params_and_fallbacks():
    assert i18n.t("de", "gov.cpu_busy", pct=72) == "CPU ausgelastet (72 % durch andere Apps)"
    assert i18n.t("xx", "nav.search") == "Search"  # unknown language: English
    assert i18n.t("de", "no.such.key") == "no.such.key"


def test_numbers_are_written_the_language_way():
    assert i18n.t("de", "gov.ram_low", gb=2.4) == "Arbeitsspeicher wird knapp (2,4 GB frei)"
    assert i18n.t("en", "gov.ram_low", gb=2.4) == "Memory getting low (2.4 GB free)"
    assert i18n.t("fr", "note.waiting_vram", gb=0.6).endswith("(0,6 Go libres)")


def test_resolve():
    assert i18n.resolve("de") == "de"
    assert i18n.resolve("auto") in i18n.languages()
    assert i18n.resolve("klingon") in i18n.languages()


def test_every_reason_for_not_searching_something_is_in_words():
    from quiekel_embed.filetypes import GROUPS, PROBLEMS, REASONS, RULES

    english = i18n.catalog()["en"]
    needed = [f"why.{r}" for r in REASONS] + [f"problem.{p}" for p in PROBLEMS]
    needed += [f"types.group.{g}" for g in GROUPS] + [f"types.rule.{r}" for r in RULES]
    assert [k for k in needed if k not in english] == []
