import os
from pathlib import Path

MODEL_ID = os.environ.get("QUIEKEL_EMBED_MODEL", "google/embeddinggemma-2")
# Matryoshka truncation: Google reports ~lossless quality down to 256 dims.
EMBED_DIM = 256

_DEFAULT_DATA = Path(os.environ.get("LOCALAPPDATA", Path.home() / "AppData" / "Local")) / "QuiekelEmbed"
DATA_DIR = Path(os.environ.get("QUIEKEL_EMBED_DATA", _DEFAULT_DATA)).resolve()
LOG_PATH = DATA_DIR / "quiekel-embed.log"
DB_PATH = DATA_DIR / "state.db"
VECTORS_DIR = DATA_DIR / "vectors"
THUMBS_DIR = DATA_DIR / "thumbs"

HOST = "127.0.0.1"
PORT = int(os.environ.get("QUIEKEL_EMBED_PORT", "8765"))

# Windows taskbar identity, shared by the app's window and its shortcuts (see shortcuts.py).
APP_ID = "QuiekelEmbed.Desktop"

# ---- file types: what gets searched, by extension -----------------------------------------

PROSE_EXTS = {
    ".txt", ".text", ".md", ".markdown", ".mdx", ".rst", ".org", ".adoc", ".asciidoc", ".textile",
    ".wiki", ".mediawiki", ".pod", ".rmd", ".qmd", ".csv", ".tsv", ".log", ".html", ".htm", ".xhtml",
    ".tex", ".bib", ".ris", ".srt", ".vtt", ".ass", ".ssa", ".lrc", ".nfo", ".diz", ".asc", ".ics",
    ".vcf", ".po",
}
CODE_EXTS = {
    # data and configuration
    ".json", ".jsonl", ".ndjson", ".geojson", ".yaml", ".yml", ".toml", ".ini", ".cfg", ".conf",
    ".properties", ".xml", ".xsd", ".xsl", ".xslt", ".plist", ".gpx", ".kml", ".opml", ".rss",
    ".atom", ".xaml", ".resx", ".config", ".props", ".targets", ".csproj", ".vbproj", ".fsproj",
    ".vcxproj", ".sln", ".nuspec", ".reg", ".inf", ".graphql", ".gql", ".proto", ".prisma",
    # programming and scripting languages
    ".py", ".pyw", ".pyi", ".pyx", ".ipynb", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts",
    ".jsx", ".coffee", ".java", ".kt", ".kts", ".groovy", ".gradle", ".scala", ".sbt", ".clj",
    ".cljs", ".edn", ".c", ".h", ".cpp", ".hpp", ".cc", ".hh", ".cxx", ".hxx", ".ino", ".m", ".mm",
    ".cs", ".fs", ".fsi", ".fsx", ".vb", ".vbs", ".bas", ".frm", ".go", ".rs", ".zig", ".nim",
    ".odin", ".d", ".rb", ".erb", ".php", ".pl", ".pm", ".swift", ".dart", ".lua", ".luau", ".r",
    ".jl", ".hs", ".lhs", ".elm", ".ml", ".mli", ".ex", ".exs", ".erl", ".hrl", ".gleam", ".rkt",
    ".scm", ".lisp", ".el", ".pas", ".dpr", ".asm", ".s", ".f", ".f90", ".f95", ".for", ".cob",
    ".cbl", ".ada", ".adb", ".ads", ".v", ".sv", ".vhd", ".vhdl", ".tcl", ".awk", ".hx", ".sol",
    ".cu", ".cuh", ".glsl", ".vert", ".frag", ".hlsl", ".wgsl", ".shader", ".gd", ".pde",
    ".sql", ".sh", ".bash", ".zsh", ".fish", ".ps1", ".psm1", ".psd1", ".bat", ".cmd", ".ahk",
    ".au3", ".nsi", ".iss", ".tf", ".tfvars", ".hcl", ".nix", ".cmake", ".mk", ".mak", ".vim",
    # the web and its templates
    ".css", ".scss", ".sass", ".less", ".styl", ".vue", ".svelte", ".astro", ".jsp", ".asp",
    ".aspx", ".cshtml", ".razor", ".haml", ".slim", ".pug", ".hbs", ".mustache", ".ejs", ".njk",
    ".jinja", ".j2", ".twig", ".liquid", ".tpl",
}
TEXT_EXTS = PROSE_EXTS | CODE_EXTS
# Some files are known by their name alone.
KNOWN_NAMES = {
    "readme": "text", "license": "text", "licence": "text", "copying": "text", "notice": "text",
    "changelog": "text", "changes": "text", "authors": "text", "contributors": "text", "todo": "text",
    "makefile": "code", "dockerfile": "code", "containerfile": "code", "jenkinsfile": "code",
    "vagrantfile": "code", "gemfile": "code", "rakefile": "code", "procfile": "code", "justfile": "code",
}
DOC_EXTS = {
    ".pdf", ".rtf",
    # Microsoft Office, also its macro, template and slide-show variants
    ".docx", ".docm", ".dotx", ".dotm", ".xlsx", ".xlsm", ".xltx", ".xltm",
    ".pptx", ".pptm", ".ppsx", ".ppsm", ".potx", ".potm",
    # Office 97-2003
    ".doc", ".dot", ".xls", ".xlt", ".ppt", ".pps", ".pot",
    # OpenDocument: LibreOffice, OpenOffice (also as templates, and as flat XML)
    ".odt", ".ott", ".fodt", ".ods", ".ots", ".fods", ".odp", ".otp", ".fodp", ".odg", ".otg", ".fodg",
    # e-books and other page formats
    ".epub", ".mobi", ".fb2", ".xps", ".oxps", ".cbz",
    # emails and saved web pages
    ".eml", ".msg", ".mht", ".mhtml",
}
# Camera raw photos: searched by the preview picture every camera stores inside.
RAW_EXTS = {
    ".cr2", ".cr3", ".crw", ".nef", ".nrw", ".arw", ".srf", ".sr2", ".dng", ".orf", ".rw2", ".raf",
    ".pef", ".srw", ".x3f", ".3fr", ".iiq", ".mrw", ".kdc", ".dcr", ".erf", ".mef", ".mos", ".rwl",
}
IMAGE_EXTS = {
    ".jpg", ".jpeg", ".jfif", ".jpe", ".png", ".apng", ".webp", ".avif", ".bmp", ".dib", ".gif",
    ".tif", ".tiff", ".heic", ".heif", ".jp2", ".j2k", ".jpf", ".jpx", ".psd", ".tga", ".pcx",
    ".qoi", ".svg",
} | RAW_EXTS

# Why other files aren't searched, by kind (see the folder's details in the app).
MEDIA_EXTS = {
    ".mp3", ".wav", ".flac", ".m4a", ".aac", ".ogg", ".oga", ".opus", ".wma", ".aiff", ".aif",
    ".mid", ".midi", ".amr", ".ape", ".mka", ".mp4", ".m4v", ".mkv", ".avi", ".mov", ".wmv", ".webm",
    ".flv", ".mpg", ".mpeg", ".m2ts", ".mts", ".3gp", ".vob", ".ogv", ".asf", ".rm", ".rmvb",
}
PROGRAM_EXTS = {
    ".exe", ".dll", ".msi", ".msix", ".msixbundle", ".appx", ".appxbundle", ".sys", ".drv", ".ocx",
    ".cpl", ".scr", ".com", ".efi", ".mui", ".msp", ".lib", ".a", ".o", ".obj", ".pdb", ".pyd",
    ".pyc", ".so", ".dylib", ".class", ".jar", ".war", ".apk", ".aab", ".ipa", ".deb", ".rpm",
    ".nupkg", ".whl", ".crx", ".xpi", ".lnk", ".url", ".ttf", ".otf", ".ttc", ".woff", ".woff2",
    ".fon", ".eot", ".vhd", ".vhdx", ".vmdk", ".img", ".dmg", ".bin", ".dat", ".tmp", ".cache",
    ".db", ".sqlite", ".sqlite3", ".mdb", ".accdb", ".ldf", ".mdf", ".dmp", ".etl", ".evtx",
}
MAILBOX_EXTS = {".pst", ".ost", ".nst", ".olm", ".mbox"}

# Folder names that are never worth indexing.
IGNORED_DIRS = {
    "node_modules", "__pycache__", ".git", ".svn", ".hg", ".venv", "venv", "env",
    "site-packages", ".cache", ".idea", ".vscode", "$recycle.bin",
    "system volume information", "appdata", "target", ".next", ".nuxt", "dist", "build",
}

MAX_TEXT_BYTES = 5 * 1024 * 1024
MAX_DOC_BYTES = 200 * 1024 * 1024
MAX_IMAGE_BYTES = 80 * 1024 * 1024
MAX_RAW_BYTES = 300 * 1024 * 1024  # camera raw files: only their preview picture is read
MIN_IMAGE_SIDE = 96  # skip icons and tiny UI assets
MAX_IMAGE_SIDE = 1024  # images are downscaled before embedding

CHUNK_CHARS = 1500
CHUNK_OVERLAP = 200
MAX_CHUNKS_PER_FILE = 300
# Scanned PDFs (no text layer) are embedded as page images instead.
SCANNED_PDF_PAGES = 3

TEXT_BATCH = 32
IMAGE_BATCH = 8
# Files are embedded together until a group holds this many chunks.
GROUP_CHUNKS = 128
