"""Finishes a Quiekel Embed update after the app has exited.

Run by the app (standard library only, isolated interpreter):
  1. wait for the app process to exit,
  2. fast-forward the checkout to the release tag (never a merge),
  3. install exactly the locked, hash-checked dependencies (`uv sync --locked`), with the AI
     engine the installer chose (engine.txt),
  4. on any failure, roll back to the previous commit,
  5. write the outcome for the app to report, and start the app again.
"""

import argparse
import ctypes
import json
import os
import re
import subprocess
import time

NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def wait_for_exit(pid: int, timeout_s: float = 60):
    if os.name == "nt":
        SYNCHRONIZE = 0x00100000
        handle = ctypes.windll.kernel32.OpenProcess(SYNCHRONIZE, False, pid)
        if handle:
            ctypes.windll.kernel32.WaitForSingleObject(handle, int(timeout_s * 1000))
            ctypes.windll.kernel32.CloseHandle(handle)
    time.sleep(1.0)  # let Windows release file handles


def run(cmd: list[str], cwd: str) -> tuple[bool, str]:
    env = {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GCM_INTERACTIVE": "never"}
    try:
        out = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=1800,
                             env=env, creationflags=NO_WINDOW)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, str(e)
    return out.returncode == 0, (out.stderr or out.stdout).strip()[-2000:]


def engine_groups(root: str) -> list[str]:
    """PyTorch is the default engine; a copy installed for DirectML keeps DirectML."""
    try:
        with open(os.path.join(root, "engine.txt"), encoding="utf-8") as f:
            engine = f.read().strip().lower()
    except OSError:
        engine = ""
    return ["--no-group", "nvidia", "--group", "directml"] if engine == "directml" else []


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--wait-pid", type=int, required=True)
    p.add_argument("--root", required=True)
    p.add_argument("--tag", required=True)
    p.add_argument("--uv", required=True)
    p.add_argument("--result", required=True)
    p.add_argument("--relaunch", default="")
    a = p.parse_args()
    if not re.fullmatch(r"v\d+\.\d+\.\d+", a.tag):
        raise SystemExit("bad tag")

    wait_for_exit(a.wait_pid)
    result = {"tag": a.tag, "ok": False, "error": None, "finished": time.time()}
    ok, before = run(["git", "-C", a.root, "rev-parse", "HEAD"], a.root)
    if not ok:
        result["error"] = f"git rev-parse failed: {before}"
    else:
        ok, out = run(["git", "-C", a.root, "merge", "--ff-only", f"refs/tags/{a.tag}"], a.root)
        if not ok:
            result["error"] = f"Fast-forward failed: {out}"
        else:
            ok, out = run([a.uv, "sync", "--locked", "--project", a.root, *engine_groups(a.root)], a.root)
            if ok:
                result["ok"] = True
            else:
                result["error"] = f"Installing dependencies failed, so the update was rolled back: {out}"
                # Safe: the app checked there were no uncommitted changes before updating.
                run(["git", "-C", a.root, "reset", "--hard", before], a.root)
                run([a.uv, "sync", "--locked", "--project", a.root, *engine_groups(a.root)], a.root)

    try:
        os.makedirs(os.path.dirname(a.result), exist_ok=True)
        with open(a.result, "w", encoding="utf-8") as f:
            json.dump(result, f)
    except OSError:
        pass
    if a.relaunch and os.path.exists(a.relaunch):
        flags = getattr(subprocess, "DETACHED_PROCESS", 0) | getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
        subprocess.Popen([a.relaunch], cwd=a.root, creationflags=flags, close_fds=True)


if __name__ == "__main__":
    main()
