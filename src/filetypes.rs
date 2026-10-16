//! What gets searched: the kinds of files Settings, File types switches on and off, the rules for
//! hidden files and program folders, and a folder's report of what wasn't searched, and why. A
//! file's name decides where it's clear (a PDF, a program, a film); otherwise what's inside does.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::archive;
use crate::config::{self, suffix};
use crate::extract::{kind_of, sniff, why_not};

/// What Settings, File types switches on and off.
pub const CATEGORIES: [&str; 5] = ["documents", "pictures", "text", "mail", "archives"];

/// The rules, and what they are unless changed.
pub const RULES: [(&str, bool); 2] = [
    ("hidden_files", false),    // hidden files and folders, and names that start with a dot
    ("program_folders", false), // node_modules, .git, build, AppData, ...
];
const SYSTEM_DIRS: [&str; 2] = ["$recycle.bin", "system volume information"]; // never, whatever the rules
const ANDROID_APP_DIRS: [&str; 2] = ["data", "obb"];
const MAIL_EXTS: [&str; 4] = [".eml", ".msg", ".mht", ".mhtml"];
pub const FORMAT: i64 = 3; // bump when what's found inside containers changes

/// The category a file of this kind counts in.
pub fn category(kind: &str, name: &str) -> &'static str {
    if MAIL_EXTS.contains(&suffix(name).as_str()) {
        return "mail";
    }
    match kind {
        "image" => "pictures",
        "text" | "code" => "text",
        _ => "documents",
    }
}

#[derive(Clone, Debug)]
pub struct Types {
    /// Categories switched off.
    pub off: BTreeSet<String>,
    pub rules: BTreeMap<String, bool>,
    /// Formats switched off on their own, by extension (".ipynb"), for the precise: a kind that's on
    /// can still leave some of its formats out.
    pub formats_off: BTreeSet<String>,
}

/// An extension as File types keeps it: ".ipynb" (a dot, then up to 15 letters or digits).
pub fn format_key(ext: &str) -> Option<String> {
    let e = ext.trim().trim_start_matches('.').to_lowercase();
    (!e.is_empty() && e.len() <= 15 && e.chars().all(|c| c.is_ascii_alphanumeric())).then(|| format!(".{e}"))
}

impl Types {
    pub fn new(off: &[String], rules: &HashMap<String, bool>) -> Self {
        let off = off.iter().filter(|c| CATEGORIES.contains(&c.as_str())).cloned().collect();
        let rules = RULES.iter().map(|(k, d)| (k.to_string(), *rules.get(*k).unwrap_or(d))).collect();
        Types { off, rules, formats_off: BTreeSet::new() }
    }

    pub fn of(settings: &crate::store::Settings) -> Self {
        let off: Vec<String> = serde_json::from_value(settings.get("kinds_off")).unwrap_or_default();
        let rules = RULES.iter().map(|(k, d)| (k.to_string(), settings.get(k).as_bool().unwrap_or(*d))).collect();
        let formats: Vec<String> = serde_json::from_value(settings.get("formats_off")).unwrap_or_default();
        let mut types = Types::new(&off, &rules);
        types.formats_off = formats.iter().filter_map(|f| format_key(f)).collect();
        types
    }

    /// Whether a file's format is switched off on its own.
    fn format_off(&self, name: &str) -> bool {
        !self.formats_off.is_empty() && self.formats_off.contains(&suffix(name))
    }

    pub fn rule(&self, name: &str) -> bool {
        self.rules.get(name).copied().unwrap_or(false)
    }

    /// How a file is read ("doc", "text", "code", "image"), or why it isn't ("off", "media",
    /// "program", "mailbox", "type"; "archive": it's looked into instead, see `inside`). Given its
    /// path, a file whose name doesn't say is looked at.
    pub fn decide(&self, name: &str, path: Option<&str>) -> Result<&'static str, &'static str> {
        let kind = match kind_of(name) {
            Some(k) => k,
            None if archive::container_type(name).is_some() => {
                return Err(if self.inside(name) { "archive" } else { "off" });
            }
            None => match why_not(name) {
                "type" => path.and_then(sniff).ok_or("type")?,
                why => return Err(why), // programs, media, mailboxes: never read
            },
        };
        if self.off.contains(category(kind, name)) || self.format_off(name) { Err("off") } else { Ok(kind) }
    }

    pub fn kind(&self, name: &str, path: Option<&str>) -> Option<&'static str> {
        self.decide(name, path).ok()
    }

    /// Whether to look inside a file: an archive, or an email for its attachments.
    pub fn inside(&self, name: &str) -> bool {
        if self.format_off(name) {
            return false;
        }
        match archive::container_type(name) {
            Some("mail") => !self.off.contains("mail"),
            Some(_) => !self.off.contains("archives"),
            None => false,
        }
    }

    /// Why a folder isn't looked into, or None. parent: the name of the folder it's in.
    pub fn skip_dir(&self, name: &str, hidden: bool, system: bool, parent: &str) -> Option<&'static str> {
        let lower = name.to_lowercase();
        if system || SYSTEM_DIRS.contains(&lower.as_str()) {
            return Some("hidden");
        }
        let program = config::IGNORED_DIRS.contains(lower.as_str())
            || (parent.eq_ignore_ascii_case("android") && ANDROID_APP_DIRS.contains(&lower.as_str()));
        if program && !self.rule("program_folders") {
            return Some("program_folder");
        }
        if (hidden || name.starts_with('.')) && !self.rule("hidden_files") {
            return Some("hidden");
        }
        None
    }

    /// Why a file isn't searched whatever its type, or None.
    pub fn skip_file(&self, name: &str, hidden: bool, system: bool) -> Option<&'static str> {
        if name.starts_with("~$") {
            return Some("office_temp"); // Office's lock files next to open documents
        }
        if system {
            return Some("hidden");
        }
        if (hidden || name.starts_with('.')) && !self.rule("hidden_files") {
            return Some("hidden");
        }
        if name.ends_with('~') || config::BACKUP_EXTS.contains(suffix(name).as_str()) {
            return Some("backup"); // an older version an app keeps next to the file
        }
        None
    }

    /// More entries than this: a backup rather than files of yours (left out, with that reason).
    pub fn max_entries(&self) -> usize {
        #[cfg(test)]
        if let Some(n) = TEST_MAX_ENTRIES.with(|m| m.get()) {
            return n;
        }
        archive::MAX_ENTRIES
    }

    /// Changes when what's searched inside containers would change.
    pub fn fingerprint(&self) -> String {
        serde_json::to_string(&json!([FORMAT, self.off, self.rules, self.formats_off])).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_format_can_be_switched_off_on_its_own() {
        let mut types = Types::new(&[], &HashMap::new());
        assert_eq!(types.decide("notes.ipynb", None), Ok("code"));
        types.formats_off = [".ipynb", ".zip"].iter().map(|s| s.to_string()).collect();
        assert_eq!(types.decide("notes.ipynb", None), Err("off"));
        assert_eq!(types.decide("readme.md", None), Ok("text")); // (the rest of its kind stays)
        assert!(!types.inside("photos.zip") && types.inside("photos.7z"));
        assert_ne!(Types::new(&[], &HashMap::new()).fingerprint(), types.fingerprint()); // (folders get scanned again)
        assert_eq!((format_key("IPYNB"), format_key(".a b"), format_key("")), (Some(".ipynb".into()), None, None));
        assert_eq!((format_category(".pdf"), format_category(".jpg"), format_category(".zip"), format_category(".msg")),
                   ("documents", "pictures", "archives", "mail"));
    }
}

#[cfg(test)]
thread_local! {
    /// Tests: archives with more entries than this count as too many.
    pub static TEST_MAX_ENTRIES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Why files weren't searched: counts by reason, a few examples each, and for types, which.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reason {
    pub n: i64,
    pub examples: Vec<String>,
    pub exts: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Report {
    pub data: BTreeMap<String, Reason>,
}

impl Report {
    const EXAMPLES: usize = 5;

    pub fn add(&mut self, reason: &str, path: &str, ext: Option<&str>, n: i64) {
        let r = self.data.entry(reason.to_string()).or_default();
        r.n += n;
        if r.examples.len() < Self::EXAMPLES {
            r.examples.push(path.to_string());
        }
        if let Some(e) = ext {
            *r.exts.entry(e.to_string()).or_insert(0) += n;
        }
    }

    pub fn note(&mut self, reason: &str, path: &str) {
        self.add(reason, path, None, 1);
    }

    pub fn add_type(&mut self, reason: &str, path: &str) {
        let ext = suffix(config::file_name(path));
        self.add(reason, path, Some(&ext), 1);
    }

    pub fn merge(&mut self, other: &Report) {
        for (reason, r) in &other.data {
            let mine = self.data.entry(reason.clone()).or_default();
            mine.n += r.n;
            let room = Self::EXAMPLES.saturating_sub(mine.examples.len());
            mine.examples.extend(r.examples.iter().take(room).cloned());
            for (ext, n) in &r.exts {
                *mine.exts.entry(ext.clone()).or_insert(0) += n;
            }
        }
    }

    pub fn total(&self) -> i64 {
        self.data.values().map(|r| r.n).sum()
    }
}

/// File types, as the page shows it. `found`: by category, the formats in your folders and how
/// many files each (a format switched off is listed too, with what's left of it), by name.
pub fn menu(types: &Types, found: &BTreeMap<String, BTreeMap<String, i64>>) -> Value {
    let defaults: BTreeMap<&str, bool> = RULES.iter().copied().collect();
    let formats = |c: &str| {
        let mut list: BTreeMap<String, i64> = found.get(c).cloned().unwrap_or_default();
        for f in &types.formats_off {
            if format_category(f) == c {
                list.entry(f.clone()).or_insert(0);
            }
        }
        list.into_iter().map(|(ext, n)| json!({"ext": ext, "n": n, "on": !types.formats_off.contains(&ext)})).collect::<Vec<_>>()
    };
    json!({
        "categories": CATEGORIES.iter().map(|c| json!({"key": c, "on": !types.off.contains(*c), "formats": formats(c)})).collect::<Vec<_>>(),
        "rules": types.rules,
        "defaults": defaults,
    })
}

/// The category a format (".pdf") belongs in, by its name alone.
pub fn format_category(ext: &str) -> &'static str {
    let name = format!("x{ext}");
    if archive::container_type(&name).is_some_and(|t| t != "mail") {
        return "archives";
    }
    match kind_of(&name) {
        Some(kind) => category(kind, &name),
        None => "text",
    }
}
