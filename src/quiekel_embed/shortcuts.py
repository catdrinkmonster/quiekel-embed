"""Windows shortcuts: Desktop, Start Menu, and (optionally) Start with Windows."""

import argparse
import ctypes
import os
import subprocess
import sys
from ctypes import wintypes
from pathlib import Path

from . import config

ROOT = Path(__file__).resolve().parents[2]
ICON = Path(__file__).resolve().parent / "assets" / "icon.ico"
NAME = "Quiekel Embed.lnk"
LEGACY_NAMES = ("Local Embed.lnk",)  # what earlier versions called themselves
STARTUP_DIR = Path(os.environ.get("APPDATA", "")) / "Microsoft" / "Windows" / "Start Menu" / "Programs" / "Startup"

# Values arrive through environment variables, so paths with spaces need no quoting.
_PS = r"""
$dir = [Environment]::GetFolderPath($env:QE_FOLDER)
$path = Join-Path $dir $env:QE_NAME
if ($env:QE_REMOVE -eq '1') { Remove-Item -LiteralPath $path -ErrorAction SilentlyContinue; exit 0 }
$s = (New-Object -ComObject WScript.Shell).CreateShortcut($path)
$s.TargetPath = $env:QE_TARGET
$s.Arguments = $env:QE_ARGS
$s.WorkingDirectory = $env:QE_WORKDIR
$s.IconLocation = "$($env:QE_ICON),0"
$s.Description = 'Search your files by meaning'
$s.Save()
Write-Output $path
"""


def launcher() -> Path:
    """The no-console launcher uv generates for the `quiekel-embed-app` gui-script."""
    return Path(sys.prefix) / "Scripts" / "quiekel-embed-app.exe"


def _shortcut(folder: str, args: str = "", remove: bool = False, name: str = NAME) -> str:
    env = {
        **os.environ,
        "QE_FOLDER": folder, "QE_NAME": name, "QE_REMOVE": "1" if remove else "0",
        "QE_TARGET": str(launcher()), "QE_ARGS": args, "QE_WORKDIR": str(ROOT),
        "QE_ICON": str(ICON),
    }
    out = subprocess.run(
        ["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", _PS],
        env=env, capture_output=True, text=True, check=True,
        creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
    )
    path = out.stdout.strip()
    if path:
        set_app_id(path)
    return path


# ---- taskbar identity -------------------------------------------------------------------
# A shortcut that carries the app's ID (System.AppUserModel.ID) tells Windows what the running
# window is: the taskbar then shows this shortcut's name and pig icon instead of Python's, and
# "Pin to taskbar" pins the shortcut rather than python.exe. WScript.Shell can't set it, so this
# goes through the shell's property store.

class _GUID(ctypes.Structure):
    _fields_ = [("Data1", wintypes.DWORD), ("Data2", wintypes.WORD), ("Data3", wintypes.WORD),
                ("Data4", ctypes.c_ubyte * 8)]

    @classmethod
    def parse(cls, text: str) -> "_GUID":
        guid = cls()
        ctypes.oledll.ole32.CLSIDFromString(text, ctypes.byref(guid))
        return guid


class _PROPERTYKEY(ctypes.Structure):
    _fields_ = [("fmtid", _GUID), ("pid", wintypes.DWORD)]


class _PROPVARIANT(ctypes.Structure):
    """Just enough of PROPVARIANT for a string (VT_LPWSTR)."""
    _fields_ = [("vt", wintypes.USHORT), ("reserved", wintypes.USHORT * 3),
                ("value", wintypes.LPCWSTR), ("padding", ctypes.c_void_p)]


_IID_IPROPERTYSTORE = "{886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99}"
_PKEY_APP_ID = ("{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}", 5)
_VT_LPWSTR, _GPS_READWRITE = 31, 2


def set_app_id(lnk: str, app_id: str = config.APP_ID):
    ole32 = ctypes.windll.ole32
    initialized = ole32.CoInitialize(None) in (0, 1)  # S_OK / S_FALSE (already initialized)
    store = ctypes.c_void_p()
    try:
        open_store = ctypes.windll.shell32.SHGetPropertyStoreFromParsingName
        open_store.argtypes = [wintypes.LPCWSTR, ctypes.c_void_p, ctypes.c_int,
                               ctypes.POINTER(_GUID), ctypes.POINTER(ctypes.c_void_p)]
        open_store.restype = ctypes.HRESULT
        open_store(lnk, None, _GPS_READWRITE, ctypes.byref(_GUID.parse(_IID_IPROPERTYSTORE)), ctypes.byref(store))
        vtable = ctypes.cast(store, ctypes.POINTER(ctypes.POINTER(ctypes.c_void_p)))[0]
        # IPropertyStore methods by vtable slot: 2 Release, 6 SetValue, 7 Commit
        set_value = ctypes.WINFUNCTYPE(ctypes.HRESULT, ctypes.c_void_p, ctypes.POINTER(_PROPERTYKEY),
                                       ctypes.POINTER(_PROPVARIANT))(vtable[6])
        commit = ctypes.WINFUNCTYPE(ctypes.HRESULT, ctypes.c_void_p)(vtable[7])
        key = _PROPERTYKEY(_GUID.parse(_PKEY_APP_ID[0]), _PKEY_APP_ID[1])
        set_value(store, ctypes.byref(key), ctypes.byref(_PROPVARIANT(vt=_VT_LPWSTR, value=app_id)))
        commit(store)
    finally:
        if store:
            ctypes.WINFUNCTYPE(ctypes.c_ulong, ctypes.c_void_p)(vtable[2])(store)
        if initialized:
            ole32.CoUninitialize()


def refresh_icons(paths: list[str]):
    """Ask Explorer to redraw these shortcuts, so a changed icon shows up right away."""
    shell32 = ctypes.windll.shell32
    for path in paths:
        shell32.SHChangeNotify(0x00002000, 0x0005, ctypes.c_wchar_p(path), None)  # UPDATEITEM, PATHW
    shell32.SHChangeNotify(0x08000000, 0x0000, None, None)  # ASSOCCHANGED: drop cached icons


def autostart_enabled() -> bool:
    return (STARTUP_DIR / NAME).exists()


def set_autostart(enabled: bool):
    # --hidden: start quietly in the tray and keep folders indexed.
    _shortcut("Startup", args="--hidden", remove=not enabled)


def main():
    parser = argparse.ArgumentParser(description="Create or remove Quiekel Embed shortcuts.")
    parser.add_argument("--remove", action="store_true", help="remove all shortcuts")
    args = parser.parse_args()

    if not launcher().exists():
        sys.exit(f"Launcher not found at {launcher()}. Run `uv sync` first.")
    legacy_autostart = any((STARTUP_DIR / n).exists() for n in LEGACY_NAMES)
    for folder in ("Desktop", "Programs", "Startup"):
        for legacy in LEGACY_NAMES:
            _shortcut(folder, remove=True, name=legacy)
    created = []
    for folder in ("Desktop", "Programs"):
        path = _shortcut(folder, remove=args.remove)
        if path:
            created.append(path)
            print("created", path)
    refresh_icons(created)
    if args.remove:
        set_autostart(False)
        print("removed Desktop, Start Menu and Startup shortcuts")
    elif legacy_autostart:
        set_autostart(True)  # keep starting with Windows under the new name
