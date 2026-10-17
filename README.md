<p align="center">
  <img src="static/quiekel.svg" width="96" alt="Quiekel">
</p>

<h1 align="center">Quiekel Embed</h1>

<p align="center">Search your files by what's in them. Locally, on Windows.</p>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/screenshots/hero-dark.png">
  <img src="docs/screenshots/hero-light.png" alt="A search for 'letter from my landlord about the rent' finds the landlord's letter about a rent increase, written in German, and the English lease">
</picture>

Add the folders you want to search, then describe what you're looking for. "Letter from my landlord
about the rent" finds that letter even when it uses other words or is written in another language.
Names, numbers and codes work too, and photos are found by what they show.

Everything happens on your PC. Files are only read, never changed, and nothing about them leaves
the computer.

| Photos by what they show | Similar files on a map |
|---|---|
| <img src="docs/screenshots/photos.png" width="420" alt="A picture search for 'flower' finds a dahlia photo named IMG_0412.jpg first"> | <img src="docs/screenshots/map.png" width="420" alt="The 3D map with a document selected: its preview and its most similar files"> |

## Install

Windows 10 or 11, 64-bit. Open PowerShell (not as administrator) and run:

```powershell
irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex
```

From Command Prompt:

```bat
powershell -ExecutionPolicy Bypass -c "irm https://github.com/catdrinkmonster/quiekel-embed/raw/main/install.ps1 | iex"
```

This puts the latest release in `%LOCALAPPDATA%\Programs\Quiekel Embed` (under 40 MB to download),
adds Desktop and Start Menu shortcuts and opens the app. On its first start the app downloads the AI
model, 0.5 to 0.9 GB. It shows new versions and what changed in them, and updates when you say so
(or by itself, if you turn that on in Settings). Running the line over an older version replaces it;
your folders and settings stay.

## What it runs on

The model works in the background at low priority, so whatever else you're doing comes first. The
app picks how it runs from what the PC has:

| PC | The model runs on |
|---|---|
| A graphics card with 3 GB of its own or more | the graphics card |
| Built-in graphics or a smaller card, and 16 GB of memory or more | the graphics, using the PC's memory |
| Otherwise | the processor, with at most 1.5 GB of memory |
| Less than 12 GB of memory, or fewer than 4 cores | the processor, with at most 2 cores and 1 GB, loaded only while needed |

Any graphics card that runs DirectX 12 works; if it fails, the processor takes over. When the PC's
memory runs low, indexing waits. The pause button, in the app and in the tray menu, pauses it.

## What it reads

| | |
|---|---|
| Documents | PDF, Word, Excel, PowerPoint (also 97-2003, templates and macro files), OpenDocument, RTF, EPUB |
| Emails | Outlook (`.msg`) and standard (`.eml`) emails with their attachments, saved web pages (`.mht`) |
| Text and code | 200+ types |
| Pictures | JPEG, PNG, HEIC, WebP, AVIF, GIF, TIFF, BMP, SVG, PSD, camera raw |
| Archives | ZIP and TAR, also one inside another; 7z, RAR, CAB and ISO with Windows 11 (2023 update or later) |
| Anything else | read as text when there's text inside |

Files are recognised by their content, not their name. File types, on the Index page, switches kinds
of files off, or single formats of a kind. A folder's details list what wasn't searched, and why, and
leave folders inside it out.

## Privacy

The app goes online to download the model once, to look for new versions on GitHub, and to download
an update you install. The index is kept in `%LOCALAPPDATA%\QuiekelEmbed`.

## Uninstall

Quit from the tray icon, then in PowerShell:

```powershell
& "$env:LOCALAPPDATA\Programs\Quiekel Embed\quiekel-embed.exe" --shortcuts --remove
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Programs\Quiekel Embed", "$env:LOCALAPPDATA\QuiekelEmbed"
```

The model stays in the Hugging Face cache (`%USERPROFILE%\.cache\huggingface`).

## Building

Rust (stable, MSVC): `cargo build --release`. The app loads ONNX Runtime with DirectML, PDFium and
libheif at run time, from its own folder or from `vendor\`; `.github\libraries.ps1` fetches the
pinned versions there. Tests: `cargo test --release -- --test-threads=1`.

## License

[MIT](LICENSE). Model: [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) (Apache 2.0).
The release zip also holds ONNX Runtime with DirectML, PDFium, and libheif with libde265 (LGPL 3.0);
their licenses, and those of the crates built into the app, are in its `licenses` folder.

<sub>Screenshots show made-up files. Photos: "flower.jpg" by vultilion and "china.jpg" by
danielbuechele (CC BY 2.0), bundled with scikit-learn.</sub>
