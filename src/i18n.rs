//! Translations, shared by the web page and the window and tray: all text lives in
//! static/i18n.json. The backend sends codes ("gov.cpu_busy" with {"pct": 72}) and the page
//! translates them; only what the app shows itself is translated here.

use std::sync::LazyLock;

use serde_json::{Map, Value};

pub static CATALOG: LazyLock<Value> =
    LazyLock::new(|| serde_json::from_str(include_str!("../static/i18n.json")).expect("i18n.json"));

/// Language code -> its own name, e.g. {"de": "Deutsch"}.
pub fn languages() -> Map<String, Value> {
    CATALOG["languages"].as_object().cloned().unwrap_or_default()
}

/// The Windows display language, if we have it; English otherwise.
pub fn system_language() -> String {
    // SAFETY: a plain query.
    let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3FF;
    let code = match lang {
        0x07 => "de",
        0x09 => "en",
        0x0C => "fr",
        0x0A => "es",
        _ => "en",
    };
    if languages().contains_key(code) { code.to_string() } else { "en".into() }
}

/// A language setting ("auto" or a code) to the language to use.
pub fn resolve(setting: &str) -> String {
    if languages().contains_key(setting) { setting.to_string() } else { system_language() }
}

/// Numbers the language's way: 0.6 GB is "0,6 GB" in German.
pub fn format_value(value: &Value, lang: &str) -> String {
    match value {
        Value::Number(n) if n.is_f64() => {
            let f = n.as_f64().unwrap_or(0.0);
            let text = format_g(f);
            if matches!(lang, "de" | "fr" | "es") { text.replace('.', ",") } else { text }
        }
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python's "{:g}" for the small numbers the app shows (memory in GB, percentages).
pub fn format_g(f: f64) -> String {
    if f == f.trunc() && f.abs() < 1e15 {
        return format!("{}", f as i64);
    }
    let s = format!("{:.6}", f);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

pub fn t(lang: &str, key: &str, params: &[(&str, Value)]) -> String {
    let text = CATALOG
        .get(lang)
        .and_then(|l| l.get(key))
        .or_else(|| CATALOG["en"].get(key))
        .and_then(Value::as_str)
        .unwrap_or(key);
    let mut out = text.to_string();
    for (name, value) in params {
        out = out.replace(&format!("{{{name}}}"), &format_value(value, lang));
    }
    out
}

/// With parameters from a JSON object (as the backend keeps them).
pub fn t_obj(lang: &str, key: &str, params: &Map<String, Value>) -> String {
    let list: Vec<(&str, Value)> = params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    t(lang, key, &list)
}
