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

TEXT_EXTS = {
    ".txt", ".md", ".markdown", ".rst", ".org", ".csv", ".tsv", ".json", ".jsonl",
    ".yaml", ".yml", ".toml", ".ini", ".cfg", ".conf", ".log", ".xml", ".html", ".htm",
    ".tex", ".bib", ".srt", ".vtt",
    # code
    ".py", ".ipynb", ".js", ".mjs", ".ts", ".tsx", ".jsx", ".java", ".kt", ".c", ".h",
    ".cpp", ".hpp", ".cc", ".cs", ".go", ".rs", ".rb", ".php", ".swift", ".lua", ".r",
    ".sql", ".sh", ".ps1", ".bat", ".cmd", ".css", ".scss", ".vue", ".svelte", ".dart",
    ".scala", ".gd", ".luau",
}
CODE_EXTS = TEXT_EXTS - {
    ".txt", ".md", ".markdown", ".rst", ".org", ".csv", ".tsv", ".log", ".html", ".htm",
    ".tex", ".bib", ".srt", ".vtt",
}
DOC_EXTS = {".pdf", ".docx", ".pptx", ".xlsx"}
IMAGE_EXTS = {".jpg", ".jpeg", ".png", ".webp", ".bmp", ".gif", ".tif", ".tiff", ".heic", ".heif"}

# Folder names that are never worth indexing.
IGNORED_DIRS = {
    "node_modules", "__pycache__", ".git", ".svn", ".hg", ".venv", "venv", "env",
    "site-packages", ".cache", ".idea", ".vscode", "$recycle.bin",
    "system volume information", "appdata", "target", ".next", ".nuxt", "dist", "build",
}

MAX_TEXT_BYTES = 5 * 1024 * 1024
MAX_DOC_BYTES = 200 * 1024 * 1024
MAX_IMAGE_BYTES = 80 * 1024 * 1024
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
