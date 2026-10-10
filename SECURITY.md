# Security policy

## Reporting a vulnerability

Please **don't open a public issue** for security problems. Report them privately through
[GitHub's private vulnerability reporting](https://github.com/catdrinkmonster/quiekel-embed/security/advisories/new).
You'll get an answer as soon as possible, and credit in the fix if you like.

Only the latest release gets security fixes.

## What Quiekel Embed does over the network

- **Model download:** on first start, EmbeddingGemma 2 is downloaded from Hugging Face. After that
  the model loads offline.
- **Update check:** every 6 hours it asks `api.github.com` for the latest release of this repository.
  You can turn this off in the app (Updates → *Check GitHub for new versions*).
- **Nothing else.** Your files, their contents, your searches and the index never leave your PC.

## How it's built to be safe

- **Local only:** the web UI server listens on `127.0.0.1` only. It refuses requests whose `Host`
  isn't the app itself (DNS rebinding), requests from other websites (`Sec-Fetch-Site`), and changes
  without the app's own request header (cross-site form posts).
- **Locked-down page:** a strict Content-Security-Policy allows only the app's own scripts and styles.
  Release notes and file contents are always shown as plain text.
- **Indexed files only:** files are only opened or previewed if they're in the index. Scripts (`.bat`,
  `.ps1`, `.py`, `.js`, …) open in an editor instead of running.
- **Read-only:** the app never modifies, moves or deletes your files.
- **Safe updates.** One-click updates are only offered for a clean git checkout of this repository on
  `main`. They only install a release tag of the form `vX.Y.Z`, which must be part of the protected
  `main` branch, and only by fast-forward. Dependencies are installed with `uv sync --locked`
  (exact versions, hash-checked). If anything fails, the update is rolled back.
- **Pinned dependencies:** `uv.lock` pins every dependency with hashes. CI installs with `--locked`, and
  Dependabot watches for vulnerable versions.

## Repository protections

- Secret scanning with push protection.
- Dependabot alerts and security updates.
- CodeQL code scanning.
- `main` can't be force-pushed or deleted and changes go through pull requests.
- Release tags can only be created by maintainers and never moved.
- GitHub Actions run with read-only tokens and only SHA-pinned actions.
