//! Paths, limits and the file types the app reads.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::LazyLock;

pub const MODEL_ID: &str = "google/embeddinggemma-2";
/// Matryoshka truncation: about lossless down to 256 dimensions.
pub const EMBED_DIM: usize = crate::onnx::EMBED_DIM;

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        home.join("AppData").join("Local")
    })
}

fn default_data() -> PathBuf {
    local_app_data().join("QuiekelEmbed")
}

pub static DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::var_os("QUIEKEL_EMBED_DATA").map(PathBuf::from).unwrap_or_else(default_data);
    std::fs::create_dir_all(&dir).ok();
    std::fs::canonicalize(&dir).map(strip_verbatim).unwrap_or(dir)
});

/// "\\?\C:\x" (what canonicalize gives) to "C:\x".
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

pub fn log_path() -> PathBuf {
    DATA_DIR.join("quiekel-embed.log")
}
pub fn db_path() -> PathBuf {
    DATA_DIR.join("state.db")
}
pub fn thumbs_dir() -> PathBuf {
    DATA_DIR.join("thumbs")
}

/// A copy with a data folder of its own (QUIEKEL_EMBED_DATA), like a test copy that pretends to be
/// another PC, runs next to the app: its own lock, its folder's name in its titles.
pub static COPY_NAME: LazyLock<String> = LazyLock::new(|| {
    let default = std::fs::canonicalize(default_data()).map(strip_verbatim).unwrap_or_else(|_| default_data());
    if DATA_DIR.to_string_lossy().eq_ignore_ascii_case(&default.to_string_lossy()) {
        String::new()
    } else {
        DATA_DIR.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    }
});

pub const HOST: &str = "127.0.0.1";
pub static PORT: LazyLock<u16> =
    LazyLock::new(|| std::env::var("QUIEKEL_EMBED_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8765));

/// Windows taskbar identity, shared by the app's window and its shortcuts.
pub const APP_ID: &str = "QuiekelEmbed.Desktop";

// ---- file types: what gets searched, by extension -----------------------------------------

fn set(items: &[&'static str]) -> HashSet<&'static str> {
    items.iter().copied().collect()
}

pub static PROSE_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        ".txt", ".text", ".md", ".markdown", ".mdx", ".rst", ".org", ".adoc", ".asciidoc", ".textile", ".wiki",
        ".mediawiki", ".pod", ".rmd", ".qmd", ".csv", ".tsv", ".log", ".html", ".htm", ".xhtml", ".tex", ".bib",
        ".ris", ".srt", ".vtt", ".ass", ".ssa", ".lrc", ".nfo", ".diz", ".asc", ".ics", ".vcf", ".po",
    ])
});

pub static CODE_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        // data and configuration
        ".json", ".jsonl", ".ndjson", ".geojson", ".yaml", ".yml", ".toml", ".ini", ".cfg", ".conf", ".properties",
        ".xml", ".xsd", ".xsl", ".xslt", ".plist", ".gpx", ".kml", ".opml", ".rss", ".atom", ".xaml", ".resx",
        ".config", ".props", ".targets", ".csproj", ".vbproj", ".fsproj", ".vcxproj", ".sln", ".nuspec", ".reg",
        ".inf", ".graphql", ".gql", ".proto", ".prisma",
        // programming and scripting languages
        ".py", ".pyw", ".pyi", ".pyx", ".ipynb", ".js", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts", ".jsx",
        ".coffee", ".java", ".kt", ".kts", ".groovy", ".gradle", ".scala", ".sbt", ".clj", ".cljs", ".edn", ".c",
        ".h", ".cpp", ".hpp", ".cc", ".hh", ".cxx", ".hxx", ".ino", ".m", ".mm", ".cs", ".fs", ".fsi", ".fsx",
        ".vb", ".vbs", ".bas", ".frm", ".go", ".rs", ".zig", ".nim", ".odin", ".d", ".rb", ".erb", ".php", ".pl",
        ".pm", ".swift", ".dart", ".lua", ".luau", ".r", ".jl", ".hs", ".lhs", ".elm", ".ml", ".mli", ".ex",
        ".exs", ".erl", ".hrl", ".gleam", ".rkt", ".scm", ".lisp", ".el", ".pas", ".dpr", ".asm", ".s", ".f",
        ".f90", ".f95", ".for", ".cob", ".cbl", ".ada", ".adb", ".ads", ".v", ".sv", ".vhd", ".vhdl", ".tcl",
        ".awk", ".hx", ".sol", ".cu", ".cuh", ".glsl", ".vert", ".frag", ".hlsl", ".wgsl", ".shader", ".gd",
        ".pde", ".sql", ".sh", ".bash", ".zsh", ".fish", ".ps1", ".psm1", ".psd1", ".bat", ".cmd", ".ahk", ".au3",
        ".nsi", ".iss", ".tf", ".tfvars", ".hcl", ".nix", ".cmake", ".mk", ".mak", ".vim",
        // the web and its templates
        ".css", ".scss", ".sass", ".less", ".styl", ".vue", ".svelte", ".astro", ".jsp", ".asp", ".aspx",
        ".cshtml", ".razor", ".haml", ".slim", ".pug", ".hbs", ".mustache", ".ejs", ".njk", ".jinja", ".j2",
        ".twig", ".liquid", ".tpl",
    ])
});

/// Some files are known by their name alone.
pub static KNOWN_NAMES: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    [
        ("readme", "text"), ("license", "text"), ("licence", "text"), ("copying", "text"), ("notice", "text"),
        ("changelog", "text"), ("changes", "text"), ("authors", "text"), ("contributors", "text"),
        ("todo", "text"), ("makefile", "code"), ("dockerfile", "code"), ("containerfile", "code"),
        ("jenkinsfile", "code"), ("vagrantfile", "code"), ("gemfile", "code"), ("rakefile", "code"),
        ("procfile", "code"), ("justfile", "code"),
    ]
    .into_iter()
    .collect()
});

pub static DOC_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        ".pdf", ".rtf",
        // Microsoft Office, also its macro, template and slide-show variants
        ".docx", ".docm", ".dotx", ".dotm", ".xlsx", ".xlsm", ".xltx", ".xltm", ".pptx", ".pptm", ".ppsx", ".ppsm",
        ".potx", ".potm",
        // Office 97-2003
        ".doc", ".dot", ".xls", ".xlt", ".ppt", ".pps", ".pot",
        // OpenDocument (also as templates, and as flat XML)
        ".odt", ".ott", ".fodt", ".ods", ".ots", ".fods", ".odp", ".otp", ".fodp", ".odg", ".otg", ".fodg",
        // e-books
        ".epub",
        // emails and saved web pages
        ".eml", ".msg", ".mht", ".mhtml",
    ])
});

/// Camera raw photos: searched by the preview picture every camera stores inside.
pub static RAW_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        ".cr2", ".cr3", ".crw", ".nef", ".nrw", ".arw", ".srf", ".sr2", ".dng", ".orf", ".rw2", ".raf", ".pef",
        ".srw", ".x3f", ".3fr", ".iiq", ".mrw", ".kdc", ".dcr", ".erf", ".mef", ".mos", ".rwl",
    ])
});

pub static IMAGE_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    let mut s = set(&[
        ".jpg", ".jpeg", ".jfif", ".jpe", ".png", ".apng", ".webp", ".avif", ".bmp", ".dib", ".gif", ".tif",
        ".tiff", ".heic", ".heif", ".psd", ".svg",
    ]);
    s.extend(RAW_EXTS.iter());
    s
});

/// Why other files aren't searched, by kind (see the folder's details in the app).
pub static MEDIA_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        ".mp3", ".wav", ".flac", ".m4a", ".aac", ".ogg", ".oga", ".opus", ".wma", ".aiff", ".aif", ".mid",
        ".midi", ".amr", ".ape", ".mka", ".mp4", ".m4v", ".mkv", ".avi", ".mov", ".wmv", ".webm", ".flv", ".mpg",
        ".mpeg", ".m2ts", ".mts", ".3gp", ".vob", ".ogv", ".asf", ".rm", ".rmvb",
    ])
});

pub static PROGRAM_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        ".exe", ".dll", ".msi", ".msix", ".msixbundle", ".appx", ".appxbundle", ".sys", ".drv", ".ocx", ".cpl",
        ".scr", ".com", ".efi", ".mui", ".msp", ".lib", ".a", ".o", ".obj", ".pdb", ".pyd", ".pyc", ".so",
        ".dylib", ".class", ".jar", ".war", ".apk", ".aab", ".ipa", ".deb", ".rpm", ".nupkg", ".whl", ".crx",
        ".xpi", ".lnk", ".url", ".ttf", ".otf", ".ttc", ".woff", ".woff2", ".fon", ".eot", ".vhd", ".vhdx",
        ".vmdk", ".img", ".dmg", ".bin", ".dat", ".tmp", ".cache", ".db", ".sqlite", ".sqlite3", ".mdb", ".accdb",
        ".ldf", ".mdf", ".dmp", ".etl", ".evtx",
        // apps' own data, keys and certificates
        ".ldb", ".db-journal", ".db-wal", ".db-shm", ".sqlite3-journal", ".pfx", ".p12", ".cer", ".crt", ".der",
        ".pem", ".key",
    ])
});

pub static MAILBOX_EXTS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| set(&[".pst", ".ost", ".nst", ".olm", ".mbox"]));

/// Older versions apps keep (so are names that end in ~): the file itself is searched instead.
pub static BACKUP_EXTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| set(&[".bak", ".old", ".orig"]));

/// Folder names that are never worth indexing.
pub static IGNORED_DIRS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    set(&[
        "node_modules", "__pycache__", ".git", ".svn", ".hg", ".venv", "venv", "env", "site-packages", ".cache",
        ".idea", ".vscode", "$recycle.bin", "system volume information", "appdata", "target", ".next", ".nuxt",
        "dist", "build",
    ])
});

pub const MAX_TEXT_BYTES: u64 = 5 * 1024 * 1024;
pub const MAX_DOC_BYTES: u64 = 200 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 80 * 1024 * 1024;
pub const MAX_RAW_BYTES: u64 = 300 * 1024 * 1024; // camera raw files: only their preview picture is read
pub const MIN_IMAGE_SIDE: u32 = 96; // skip icons and tiny UI assets
pub const MAX_IMAGE_SIDE: u32 = 1024; // images are downscaled before embedding

pub const CHUNK_CHARS: usize = 1500;
pub const CHUNK_OVERLAP: usize = 200;
pub const MAX_CHUNKS_PER_FILE: usize = 300;
/// Scanned PDFs (no text layer) are embedded as page images instead.
pub const SCANNED_PDF_PAGES: usize = 3;
/// Files are embedded together until a group holds this many chunks.
pub const GROUP_CHUNKS: usize = 128;

/// A file name's extension, lowercase with its dot ("" for none), as Python's Path.suffix.
pub fn suffix(name: &str) -> String {
    let base = name.rsplit(['\\', '/']).next().unwrap_or(name);
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) if i == base.len() - 1 => String::new(),
        Some(i) => base[i..].to_lowercase(),
    }
}

/// A path's last part.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// A library the app ships: next to the app, or in vendor\<sub> while developing.
pub fn vendor_file(sub: &str, name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let mut dir = exe.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    for _ in 0..4 {
        if dir.join(name).is_file() {
            return dir.join(name);
        }
        let vendored = dir.join("vendor").join(sub).join(name);
        if vendored.is_file() {
            return vendored;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    PathBuf::from("vendor").join(sub).join(name)
}
