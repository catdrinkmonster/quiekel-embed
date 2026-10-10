import os
import subprocess
import sys

import pytest

from quiekel_embed import config, shortcuts

pytestmark = pytest.mark.skipif(sys.platform != "win32", reason="Windows shortcuts")

PS = ["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"]


def test_shortcut_carries_the_taskbar_id(tmp_path):
    lnk = tmp_path / "Quiekel Embed.lnk"
    env = {**os.environ, "QE_LNK": str(lnk), "QE_TARGET": sys.executable}
    subprocess.run(PS + ["$s = (New-Object -ComObject WScript.Shell).CreateShortcut($env:QE_LNK); "
                         "$s.TargetPath = $env:QE_TARGET; $s.Save()"], env=env, check=True)

    shortcuts.set_app_id(str(lnk))

    read = ("$i = (New-Object -ComObject Shell.Application).NameSpace((Split-Path $env:QE_LNK))"
            ".ParseName((Split-Path $env:QE_LNK -Leaf)); $i.ExtendedProperty('System.AppUserModel.ID')")
    out = subprocess.run(PS + [read], env=env, check=True, capture_output=True, text=True)
    assert out.stdout.strip() == config.APP_ID
