"""Translations, shared by the web UI and the desktop shell (tray, splash screen).

All text lives in static/i18n.json. The backend sends codes (e.g. "gov.cpu_busy" with
{"pct": 72}) and the UI translates them; Python only translates what it shows itself.
"""

import ctypes
import json
import locale
import sqlite3
from functools import lru_cache
from pathlib import Path

from . import config

FILE = Path(__file__).resolve().parent / "static" / "i18n.json"
_WINDOWS_PRIMARY = {0x07: "de", 0x09: "en", 0x0C: "fr", 0x0A: "es"}
_DECIMAL_COMMA = {"de", "fr", "es"}


@lru_cache(maxsize=1)
def catalog() -> dict:
    return json.loads(FILE.read_text(encoding="utf-8"))


def languages() -> dict[str, str]:
    """Language code -> its own name, e.g. {"de": "Deutsch"}."""
    return catalog()["languages"]


def system_language() -> str:
    """The Windows display language, if we have it; English otherwise."""
    code = None
    try:
        code = _WINDOWS_PRIMARY.get(ctypes.windll.kernel32.GetUserDefaultUILanguage() & 0x3FF)
    except (AttributeError, OSError):
        pass
    if not code:
        code = (locale.getlocale()[0] or "").lower()[:2]
    return code if code in languages() else "en"


def resolve(setting: str | None) -> str:
    """A language setting ("auto" or a code) to the language to use."""
    return setting if setting in languages() else system_language()


def _format(value, lang: str) -> str:
    """Numbers the language's way: 0.6 GB is "0,6 GB" in German."""
    if isinstance(value, float):
        text = f"{value:g}"
        return text.replace(".", ",") if lang in _DECIMAL_COMMA else text
    return str(value)


def t(lang: str, key: str, /, **params) -> str:
    """Positional-only, so any placeholder name (even "lang" or "key") works as a parameter."""
    cat = catalog()
    text = cat.get(lang, {}).get(key) or cat["en"].get(key) or key
    for name, value in params.items():
        text = text.replace("{" + name + "}", _format(value, lang))
    return text


def stored_language() -> str:
    """The saved language, read straight from the database (the splash shows before the app loads)."""
    try:
        db = sqlite3.connect(f"{config.DB_PATH.as_uri()}?mode=ro", uri=True)
        try:
            row = db.execute("SELECT value FROM meta WHERE key = 'setting.language'").fetchone()
        finally:
            db.close()
        return resolve(json.loads(row[0]) if row else None)
    except (sqlite3.Error, ValueError, OSError):
        return system_language()
