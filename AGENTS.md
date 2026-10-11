# AGENTS.md

Notes for anyone changing this repository, people and coding agents alike.

## Commits, comments and docs

- Commit messages say what changed and why, in plain, neutral words. Keep them generic: nothing
  that introduces risk or raises eyebrows.
- The same goes for code comments, docs, issues and pull requests:
  - No other products, companies or people named as a reference, model or inspiration.
  - No personal details: hardware, habits, schedules, names, paths, accounts.
- No tool or assistant attribution: no `Co-Authored-By` trailers, no "generated with" lines.
- The commit author is the GitHub no-reply address, never a personal email.
- Never commit secrets, local tool settings, logs, indexes, benchmark output or anything from a
  personal folder. Screenshots show made-up files only.

## Working on the code

- Python 3.13 via [uv](https://docs.astral.sh/uv/): `uv sync --group directml`, then `uv run pytest -q`.
- Two AI engines: PyTorch (group `nvidia`, the default) and ONNX Runtime with DirectML (group
  `directml`). The installer installs one, by the graphics card; `--group directml` on top of the
  default gives a working copy both, and `QUIEKEL_EMBED_ENGINE=torch|onnx` picks one.
- Run the app with `uv run quiekel-embed-app`, or `uv run quiekel-embed` for the browser version.
- Every UI text lives in `src/quiekel_embed/static/i18n.json`: every key in English, German,
  French and Spanish.
- Tests must pass. CI runs all of them on Windows for every pull request: with both engines
  (PyTorch's CPU build, the machines have no NVIDIA card), and as installed without PyTorch.
- The app stays gentle on the PC: no windows or focus changes over fullscreen apps. Indexing slows
  down for busy apps as the Performance setting says, and only waits when memory runs out.

## Shipping

- `main` is protected. Changes go through pull requests and are rebased or squashed; CI must pass.
- The version lives in `pyproject.toml` (and `uv.lock`, via `uv lock`).
- A release is a `vX.Y.Z` tag on `main` plus a GitHub release. The app's one-click update only
  installs such tags, so tags are never moved or deleted.
