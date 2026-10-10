"""What gets searched: every type of file the app can read, as Settings → File types shows them,
with what was changed there (types switched off, types added to be read as plain text, and the
rules for archives, emails and hidden files). And a folder's report of what wasn't searched, and
why."""

import json
import re
from pathlib import Path

from . import archive, config
from .extract import kind_of, why_not

# The menu's groups, in its order.
GROUPS = {
    "doc": [
        ".pdf", ".docx", ".docm", ".dotx", ".dotm", ".doc", ".dot", ".rtf",
        ".xlsx", ".xlsm", ".xltx", ".xltm", ".xls", ".xlt",
        ".pptx", ".pptm", ".ppsx", ".ppsm", ".potx", ".potm", ".ppt", ".pps", ".pot",
        ".odt", ".ott", ".fodt", ".ods", ".ots", ".fods", ".odp", ".otp", ".fodp", ".odg", ".otg", ".fodg",
        ".epub", ".mobi", ".fb2", ".xps", ".oxps", ".cbz",
    ],
    "mail": [".eml", ".msg", ".mht", ".mhtml"],
    "text": sorted(config.PROSE_EXTS),
    "code": sorted(config.CODE_EXTS),
    "image": sorted(config.IMAGE_EXTS - config.RAW_EXTS),
    "raw": sorted(config.RAW_EXTS),
    "archive": sorted(archive.ARCHIVE_EXTS),
}
# The rules, and what they are unless changed.
RULES = {
    "attachments": True,  # look into emails' attachments
    "nested_archives": True,  # look into archives inside archives (one level)
    "big_archives": False,  # look into archives with more than 20,000 files (mostly backups)
    "hidden_files": False,  # hidden files and folders, and names that start with a dot
    "program_folders": False,  # node_modules, .git, build, AppData, …
}
SYSTEM_DIRS = {"$recycle.bin", "system volume information"}  # never, whatever the rules
# Every reason the app gives for not searching something (its texts: why.<reason>, why.<reason>_d).
REASONS = (
    # what a scan leaves out
    "type", "off", "media", "program", "mailbox", "hidden", "office_temp", "program_folder", "link",
    "no_access_dir", "unreadable_dir", "too_many", "encrypted", "damaged_archive", "split_archive",
    "needs_windows", "nested_deep", "nested_off", "nested_large", "odd_name", "inline_image",
    # files that were read, and skipped
    "empty", "too_large", "tiny_image", "binary", "no_text", "password", "not_image",
)
# Why a file couldn't be read (its texts: problem.<key>).
PROBLEMS = ("no_access", "in_use", "gone", "no_memory", "damaged_image", "damaged_pdf", "damaged_document",
            "damaged_archive", "damaged_email", "embed_failed", "unreadable_dir")
FORMAT = 1  # bump when what's found inside containers changes: they're looked into again
_EXT = re.compile(r"^\.[a-z0-9][a-z0-9_+\-]{0,15}$")


def clean_ext(value: str) -> str | None:
    """".JSON", "json" -> ".json"; None for anything that isn't an extension."""
    ext = "." + str(value).strip().lower().lstrip(".")
    return ext if _EXT.match(ext) else None


def _suffix(name: str) -> str:
    return Path(name).suffix.lower()


class Types:
    def __init__(self, off=(), added=(), **rules):
        self.off = {e for e in map(clean_ext, off) if e}
        known = set(config.TEXT_EXTS | config.DOC_EXTS | config.IMAGE_EXTS | archive.ARCHIVE_EXTS)
        self.added = {e for e in map(clean_ext, added) if e} - known
        self.rules = {**RULES, **{k: bool(v) for k, v in rules.items() if k in RULES}}

    @classmethod
    def of(cls, settings) -> "Types":
        return cls(settings.get("types_off"), settings.get("types_added"),
                   **{k: settings.get(k) for k in RULES})

    def kind(self, name: str) -> str | None:
        """How a file is read ("doc", "text", "code", "image"), or None: it isn't searched."""
        ext = _suffix(name)
        if ext in self.off:
            return None
        kind = kind_of(Path(name))
        if kind is None and ext in self.added:
            return "text"
        return kind

    def inside(self, name: str) -> bool:
        """Whether to look inside a file: an archive, or an email for its attachments."""
        kind = archive.container_type(name)
        if kind is None or _suffix(name) in self.off:
            return False
        return self.rules["attachments"] if kind == "mail" else True

    def why_not(self, name: str) -> str:
        return "off" if _suffix(name) in self.off else why_not(name)

    def skip_dir(self, name: str, hidden: bool, system: bool) -> str | None:
        """Why a folder isn't looked into, or None."""
        lower = name.lower()
        if system or lower in SYSTEM_DIRS:
            return "hidden"
        if lower in config.IGNORED_DIRS and not self.rules["program_folders"]:
            return "program_folder"
        if (hidden or name.startswith(".")) and not self.rules["hidden_files"]:
            return "hidden"
        return None

    def skip_file(self, name: str, hidden: bool, system: bool) -> str | None:
        """Why a file isn't searched whatever its type, or None."""
        if name.startswith("~$"):
            return "office_temp"  # Office's lock files next to open documents
        if system:
            return "hidden"
        if (hidden or name.startswith(".")) and not self.rules["hidden_files"]:
            return "hidden"
        return None

    @property
    def max_entries(self) -> int:
        return archive.MAX_ENTRIES_BIG if self.rules["big_archives"] else archive.MAX_ENTRIES

    def fingerprint(self) -> str:
        """Changes when what's searched inside containers would change."""
        return json.dumps([FORMAT, sorted(self.off), sorted(self.added), self.rules], sort_keys=True)


class Report:
    """Why files weren't searched: counts by reason, a few examples each, and for types, which."""

    EXAMPLES = 5

    def __init__(self, data: dict | None = None):
        self.data: dict[str, dict] = data if data is not None else {}

    def add(self, reason: str, path: str, ext: str | None = None, n: int = 1):
        r = self.data.setdefault(reason, {"n": 0, "examples": [], "exts": {}})
        r["n"] += n
        if len(r["examples"]) < self.EXAMPLES:
            r["examples"].append(path)
        if ext is not None:
            r["exts"][ext] = r["exts"].get(ext, 0) + n

    def add_type(self, reason: str, path: str):
        self.add(reason, path, ext=_suffix(path.rsplit("\\", 1)[-1]) or "")

    def merge(self, other: dict):
        for reason, r in other.items():
            mine = self.data.setdefault(reason, {"n": 0, "examples": [], "exts": {}})
            mine["n"] += r["n"]
            mine["examples"].extend(r["examples"][:self.EXAMPLES - len(mine["examples"])])
            for ext, n in r["exts"].items():
                mine["exts"][ext] = mine["exts"].get(ext, 0) + n


def menu(types: Types) -> dict:
    """Settings → File types, as the page shows it."""
    return {
        "groups": [{"key": key, "exts": exts} for key, exts in GROUPS.items()],
        "off": sorted(types.off),
        "added": sorted(types.added),
        "rules": types.rules,
        "defaults": RULES,
    }
