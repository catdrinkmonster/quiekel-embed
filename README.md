<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="src/quiekel_embed/static/pig-dark.svg">
    <img src="src/quiekel_embed/static/pig.svg" width="120" alt="Quiekel, the pig">
  </picture>
</p>

<h1 align="center">Quiekel Embed</h1>

<p align="center">Search your own files by what's <em>in</em> them. Locally, on your PC.</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/search-dark.png">
    <img src="docs/screenshots/search-light.png" alt="Searching for 'letter from my landlord about the rent' finds a German letter about a rent increase first">
  </picture>
</p>

- **Meaning, not just keywords.** Ask in your own words, in any of 100+ languages: the question above is English, the letter it found is German.
- **Almost any file.** Documents, emails with their attachments, text and code, photos found by what they show, even inside ZIP, 7z and RAR archives.
- **Private.** The AI model runs on your own graphics card (or CPU). Your files and searches never leave your PC.
- **Stays out of your way.** It keeps your folders up to date in the background and slows down while your other apps need the PC.

| Photos, found by what they show | A map of your files |
|---|---|
| <img src="docs/screenshots/photos.png" width="400" alt="An image search for 'flower' finds a dahlia photo named IMG_0412.jpg first"> | <img src="docs/screenshots/map.png" width="400" alt="The 3D map with a selected letter: its preview and its most similar files, with how alike they are"> |

## Install

Windows 10 or 11. Open **PowerShell** and paste:

```powershell
irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex
```

It sets up what's missing (uv, and Git if needed), puts **Quiekel Embed** on your Desktop and starts
it. It downloads about 4.5 GB (the AI model included) and needs about 6.5 GB of disk. An NVIDIA
graphics card makes it much faster but isn't required. To update, run the line again.

<details>
<summary>Install by hand, or uninstall</summary>

By hand, with [uv](https://docs.astral.sh/uv/) and [Git](https://git-scm.com/) installed:

```powershell
git clone https://github.com/catdrinkmonster/quiekel-embed "$env:LOCALAPPDATA\Programs\Quiekel Embed"
cd "$env:LOCALAPPDATA\Programs\Quiekel Embed"
uv sync --locked
uv run quiekel-embed-shortcuts
```

To uninstall, quit Quiekel from its tray icon, then:

```powershell
cd "$env:LOCALAPPDATA\Programs\Quiekel Embed"; uv run quiekel-embed-shortcuts --remove; cd ~
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Programs\Quiekel Embed", "$env:LOCALAPPDATA\QuiekelEmbed"
```

The model stays in the Hugging Face cache (`~\.cache\huggingface`) in case other apps use it.
</details>

## What it searches

| | |
|---|---|
| Documents | PDF, Word, Excel and PowerPoint (also 97-2003, templates and macro files), LibreOffice and OpenOffice, RTF, EPUB and other e-books, XPS |
| Emails | Outlook (`.msg`) and standard (`.eml`) emails with their attachments, saved web pages (`.mht`) |
| Text and code | More than 200 types, from `.txt` and `.md` to `.py`, `.json` and `.sql` |
| Pictures | JPEG, PNG, HEIC, WebP, AVIF, GIF, TIFF, SVG, PSD and more, and camera raw photos |
| Inside archives | ZIP, 7z, RAR, TAR, CAB and ISO, also an archive inside an archive (7z, RAR, CAB and ISO need Windows 11, 2023 or later) |

**Settings → File types** switches types off, adds your own (read as plain text), and decides about
hidden files and very big archives. A folder's ⓘ shows what wasn't searched, and why.

## Privacy

The app goes online for two things only: downloading the AI model on first start and, if you switch
it on, checking for updates. Your files, your searches and the index stay on your PC, in
`%LOCALAPPDATA%\QuiekelEmbed`, and **your files are never changed.** [SECURITY.md](SECURITY.md) says
how the app is locked down and how to report a vulnerability.

## License

[MIT](LICENSE). Uses Google's [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) (Apache 2.0).
To work on the app itself, see [AGENTS.md](AGENTS.md).

<sub>The files in the screenshots are made up. The two photos are "flower.jpg" by vultilion and
"china.jpg" by danielbuechele (CC BY 2.0), as bundled with scikit-learn.</sub>
