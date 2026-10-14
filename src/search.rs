//! Hybrid search: meaning (vectors) + keywords (SQLite FTS5), merged per file.
//!
//! Meaning finds "rental agreement" when you type "lease"; keywords find exact names and numbers
//! like "IMG_0042" or "2026-114" that embeddings are bad at. Files containing all of your words
//! come first; within that group and after it, results are ordered by meaning, so the order always
//! matches the confidence the page shows. Keyword finds get their meaning score looked up for that.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

/// What the filters in the page mean, by the kind stored with every file.
pub fn kinds(filter: &str) -> &'static [&'static str] {
    match filter {
        "docs" => &["doc"],
        "text" => &["text"],
        "code" => &["code"],
        "images" => &["image"],
        _ => &[],
    }
}

const PREFIX_MIN: usize = 4; // shorter words must match whole words: "cat" shouldn't match "catalog"
const RRF_K: f64 = 60.0;
const EXACT_BONUS: f64 = 1.0 / RRF_K; // an all-words match outranks a file found only one way

static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // English
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "i", "in", "is", "it",
        "its", "me", "my", "of", "on", "or", "that", "the", "this", "to", "was", "were", "what", "when", "where",
        "which", "who", "with", "about", "find", "show",
        // German
        "aber", "als", "am", "an", "auch", "auf", "aus", "bei", "das", "dass", "dem", "den", "der", "des", "die",
        "ein", "eine", "einem", "einen", "einer", "es", "für", "fur", "hat", "ich", "im", "in", "ist", "mein",
        "meine", "mit", "nach", "oder", "sich", "sie", "und", "von", "vom", "was", "wie", "wo", "zu", "zum", "zur",
    ]
    .into_iter()
    .collect()
});

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// Lowercase and strip accents, matching the FTS5 tokenizer (remove_diacritics).
pub fn normalize(text: &str) -> String {
    if text.is_ascii() {
        return text.to_lowercase();
    }
    text.nfkd().filter(|c| !is_mark(*c)).collect::<String>().to_lowercase()
}

/// Letters and digits (Python's [^\W_]): what the keyword index treats as words.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

pub fn query_terms(query: &str) -> Vec<String> {
    let norm = normalize(query);
    let mut words = Vec::new();
    let mut cur = String::new();
    for c in norm.chars() {
        if is_word_char(c) {
            cur.push(c);
        } else if !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let words: Vec<String> =
        words.into_iter().filter(|w| w.chars().count() > 1 || w.chars().all(|c| c.is_numeric())).collect();
    let content: Vec<String> = words.iter().filter(|w| !STOPWORDS.contains(w.as_str())).cloned().collect();
    let chosen = if content.is_empty() { words } else { content };
    let mut seen = HashSet::new();
    chosen.into_iter().filter(|w| seen.insert(w.clone())).take(12).collect()
}

/// FTS5 query: any of the terms; the last one also as a prefix (search as you type).
pub fn fts_query(terms: &[String]) -> Option<String> {
    if terms.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = terms.iter().map(|t| format!("\"{t}\"")).collect();
    if terms.last().map(|t| t.chars().count() >= 2).unwrap_or(false) {
        parts.last_mut().unwrap().push('*');
    }
    Some(parts.join(" OR "))
}

static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\d|_|\w[.\-/]\w|""#).unwrap());

/// File names, codes and numbers, where exact keywords matter most.
pub fn looks_like_identifier(query: &str) -> bool {
    IDENT.is_match(query)
}

pub fn has_all_terms(terms: &[String], texts: &[&str]) -> bool {
    let haystack: Vec<char> = normalize(&texts.join(" ")).chars().collect();
    // Words where "_" counts as a separator, as in the keyword index. Longer words may also match
    // the start of a word ("invoice" in "invoices"), short ones only whole words.
    terms.iter().all(|t| {
        let needle: Vec<char> = t.chars().collect();
        let n = needle.len();
        if n == 0 || n > haystack.len() {
            return n == 0;
        }
        (0..=haystack.len() - n).any(|i| {
            haystack[i..i + n] == needle[..]
                && (i == 0 || !is_word_char(haystack[i - 1]))
                && (n >= PREFIX_MIN || i + n == haystack.len() || !is_word_char(haystack[i + n]))
        })
    })
}

/// A passage found by meaning.
#[derive(Clone, Debug)]
pub struct VectorHit {
    pub file_id: i64,
    pub chunk: i64,
    pub text: String,
    pub similarity: f32,
}

/// A passage found by keywords.
#[derive(Clone, Debug)]
pub struct KeywordHit {
    pub file_id: i64,
    pub chunk: i64,
    pub name: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Result {
    pub file_id: i64,
    pub chunk: i64,
    pub snippet: String,
    pub similarity: Option<f64>,
    pub score: f64,
    pub keyword: bool,
    pub exact: bool,
}

fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

/// Merge passage hits (each list best first) into ranked per-file results. similarity_of(file
/// ids) looks up the meaning score of files only the keywords found.
pub fn fuse(
    vector_hits: &[VectorHit],
    keyword_hits: &[KeywordHit],
    query: &str,
    limit: usize,
    similarity_of: Option<&dyn Fn(&[i64]) -> HashMap<i64, f32>>,
) -> Vec<Result> {
    let terms = query_terms(query);
    let kw_weight = if terms.len() <= 3 || looks_like_identifier(query) { 1.0 } else { 0.5 };
    let mut order: Vec<i64> = Vec::new();
    let mut files: HashMap<i64, Result> = HashMap::new();
    let mut seen = HashSet::new();
    let mut rank = 0usize;
    for h in vector_hits {
        if !seen.insert(h.file_id) {
            continue;
        }
        order.push(h.file_id);
        files.insert(h.file_id, Result {
            file_id: h.file_id,
            chunk: h.chunk,
            snippet: h.text.clone(),
            similarity: Some(round4(h.similarity as f64)),
            score: 1.0 / (RRF_K + rank as f64),
            keyword: false,
            exact: false,
        });
        rank += 1;
    }
    let mut seen = HashSet::new();
    let mut rank = 0usize;
    for h in keyword_hits {
        if !seen.insert(h.file_id) {
            continue;
        }
        let exact = has_all_terms(&terms, &[&h.name, &h.text]);
        let r = files.entry(h.file_id).or_insert_with(|| {
            order.push(h.file_id);
            Result {
                file_id: h.file_id,
                chunk: h.chunk,
                snippet: h.text.clone(),
                similarity: None,
                score: 0.0,
                keyword: false,
                exact: false,
            }
        });
        r.score += kw_weight / (RRF_K + rank as f64) + if exact { EXACT_BONUS } else { 0.0 };
        r.keyword = true;
        if (exact && !r.exact) || r.similarity.is_none() {
            // Show the passage that actually contains the words.
            r.chunk = h.chunk;
            r.snippet = h.text.clone();
        }
        r.exact = r.exact || exact;
        rank += 1;
    }
    let missing: Vec<i64> = order.iter().copied().filter(|id| files[id].similarity.is_none()).collect();
    if let (false, Some(f)) = (missing.is_empty(), similarity_of) {
        for (fid, sim) in f(&missing) {
            if let Some(r) = files.get_mut(&fid) {
                r.similarity = Some(round4(sim as f64));
            }
        }
    }
    let mut out: Vec<Result> = order.into_iter().filter_map(|id| files.remove(&id)).collect();
    // All your words first, then by meaning. Before the model is ready: by keyword rank.
    out.sort_by(|a, b| {
        let ka = (!a.exact, a.similarity.is_none());
        let kb = (!b.exact, b.similarity.is_none());
        ka.cmp(&kb)
            .then_with(|| b.similarity.unwrap_or(0.0).total_cmp(&a.similarity.unwrap_or(0.0)))
            .then_with(|| b.score.total_cmp(&a.score))
    });
    out.truncate(limit);
    out
}

// Likely what was searched for: all its words, or a meaning score over the bar for its kind and
// close to the best of that kind. Set on searches with known answers (documents in four languages,
// photos with people's captions): the right file lands above the divider about nine times in ten,
// with one or two others. When nothing clears the bar, the best few still come first.
pub const LIKELY_TEXT: f64 = 0.73;
pub const LIKELY_PICTURE: f64 = 0.71;
pub const LIKELY_MARGIN: f64 = 0.04;
pub const LIKELY_FALLBACK: usize = 3;

/// Which results (each: meaning score, all words found, a picture) are likely what was searched
/// for. When none is, the first LIKELY_FALLBACK.
pub fn likely(results: &[(Option<f64>, bool, bool)]) -> Vec<bool> {
    let best = |picture: bool| results.iter().filter(|r| r.2 == picture).filter_map(|r| r.0).fold(f64::MIN, f64::max);
    let (best_text, best_picture) = (best(false), best(true));
    let mut out: Vec<bool> = results
        .iter()
        .map(|&(similarity, exact, picture)| {
            let (bar, best) = if picture { (LIKELY_PICTURE, best_picture) } else { (LIKELY_TEXT, best_text) };
            exact || similarity.is_some_and(|s| s >= bar && s >= best - LIKELY_MARGIN)
        })
        .collect();
    if !out.contains(&true) {
        out.iter_mut().take(LIKELY_FALLBACK).for_each(|x| *x = true);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn likely_results_clear_the_bar_and_stay_close_to_the_best() {
        let text = |s: f64| (Some(s), false, false);
        // One clear winner: the rest is further than the margin behind it.
        assert_eq!(likely(&[text(0.80), text(0.70), text(0.69)]), [true, false, false]);
        // Several good ones close together.
        assert_eq!(likely(&[text(0.78), text(0.76), text(0.75), text(0.60)]), [true, true, true, false]);
        // All your words count whatever the score; pictures have their own bar and best.
        assert_eq!(likely(&[text(0.80), (Some(0.50), true, false), (Some(0.74), false, true), (Some(0.62), false, true)]), [true, true, true, false]);
        // Nothing clears the bar: the best few still come first. None at all: none.
        assert_eq!(likely(&[text(0.60), text(0.59), text(0.58), text(0.57)]), [true, true, true, false]);
        assert_eq!(likely(&[(None, false, false), (None, false, false)]), [true, true]);
        assert!(likely(&[]).is_empty());
    }

    #[test]
    fn terms_are_words_without_stopwords() {
        assert_eq!(query_terms("letter from my landlord about the rent"), vec!["letter", "landlord", "rent"]);
        assert_eq!(query_terms("IMG_0042"), vec!["img", "0042"]);
        assert_eq!(query_terms("Grüße aus Köln"), vec!["gruße", "koln"]);
        assert_eq!(query_terms("the"), vec!["the"]);
    }

    #[test]
    fn fts_query_ends_in_a_prefix() {
        assert_eq!(fts_query(&["rent".into(), "due".into()]).unwrap(), "\"rent\" OR \"due\"*");
    }

    #[test]
    fn all_terms_match_whole_or_start_of_words() {
        let t = |q: &str| query_terms(q);
        assert!(has_all_terms(&t("invoice 2026"), &["Invoices_2026.pdf", ""]));
        assert!(!has_all_terms(&t("cat"), &["catalog", ""]));
        assert!(has_all_terms(&t("cat"), &["my cat", ""]));
    }

    #[test]
    fn exact_matches_come_first_then_meaning() {
        let v = vec![
            VectorHit { file_id: 1, chunk: 0, text: "about housing".into(), similarity: 0.8 },
            VectorHit { file_id: 2, chunk: 0, text: "rent due on the first".into(), similarity: 0.6 },
        ];
        let k = vec![KeywordHit { file_id: 2, chunk: 0, name: "contract.txt".into(), text: "rent due on the first".into() }];
        let r = fuse(&v, &k, "rent due", 10, None);
        assert_eq!(r.iter().map(|x| x.file_id).collect::<Vec<_>>(), vec![2, 1]);
        assert!(r[0].exact && r[0].keyword);
    }

    fn terms(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn by_meaning(file_id: i64, chunk: i64, distance: f32, text: &str) -> VectorHit {
        VectorHit { file_id, chunk, text: text.into(), similarity: 1.0 - distance }
    }

    fn by_words(file_id: i64, chunk: i64, name: &str, text: &str) -> KeywordHit {
        KeywordHit { file_id, chunk, name: name.into(), text: text.into() }
    }

    fn ids(results: &[Result]) -> Vec<i64> {
        results.iter().map(|r| r.file_id).collect()
    }

    #[test]
    fn query_terms_drop_stopwords_and_accents() {
        assert_eq!(query_terms("Photo of a Whiteboard"), ["photo", "whiteboard"]);
        assert_eq!(query_terms("Rechnung für Strom"), ["rechnung", "strom"]);
        assert_eq!(query_terms("Électricité"), ["electricite"]);
    }

    #[test]
    fn fts_query_prefixes_the_last_term() {
        assert_eq!(fts_query(&terms(&["invoice", "2026", "114"])).unwrap(), r#""invoice" OR "2026" OR "114"*"#);
        assert_eq!(fts_query(&[]), None);
    }

    #[test]
    fn all_terms_match_word_prefixes() {
        assert!(has_all_terms(&terms(&["img", "0042"]), &["IMG_0042.jpg photos", ""]));
        assert!(!has_all_terms(&terms(&["img", "0043"]), &["IMG_0042.jpg", ""]));
        assert!(has_all_terms(&terms(&["invoice"]), &["invoices 2026", ""])); // longer words: the start of a word counts
        assert!(!has_all_terms(&terms(&["cat"]), &["catalog.py", ""])); // short words: whole words only
        assert!(has_all_terms(&terms(&["cat"]), &["cat_food.md", ""]));
    }

    #[test]
    fn an_exact_keyword_match_beats_semantic_neighbours() {
        let near = [by_meaning(1, 0, 0.30, "t"), by_meaning(2, 0, 0.35, "t"), by_meaning(3, 0, 0.40, "t")];
        let out = fuse(&near, &[by_words(3, 0, "IMG_0042.jpg photos", "")], "IMG_0042", 10, None);
        assert!(out[0].file_id == 3 && out[0].exact && out[0].keyword);
    }

    #[test]
    fn keyword_only_results_work_before_the_model_loads() {
        let out = fuse(&[], &[by_words(7, 2, "", "Invoice 2026-114")], "invoice 2026-114", 10, None);
        assert_eq!(ids(&out), [7]);
        assert!(out[0].similarity.is_none() && out[0].chunk == 2);
    }

    #[test]
    fn all_your_words_first_then_by_meaning() {
        let near = [by_meaning(1, 0, 0.20, "t"), by_meaning(2, 0, 0.30, "t"), by_meaning(3, 0, 0.25, "t")];
        let out = fuse(&near, &[by_words(2, 0, "", "red apple")], "red apple", 10, None);
        assert_eq!(ids(&out), [2, 1, 3]); // 2 has all the words; then 0.80 > 0.75
    }

    #[test]
    fn keyword_finds_get_their_meaning_score() {
        let meaning: &dyn Fn(&[i64]) -> HashMap<i64, f32> = &|_| HashMap::from([(9, 0.9)]);
        let out = fuse(&[by_meaning(1, 0, 0.30, "t")], &[by_words(9, 0, "", "something about pears")], "pears apples", 10, Some(meaning));
        assert_eq!(ids(&out), [9, 1]); // neither has all the words: 0.9 beats 0.7
        assert_eq!(out[0].similarity, Some(0.9));
    }

    #[test]
    fn the_snippet_switches_to_the_passage_with_the_words() {
        let out = fuse(&[by_meaning(5, 0, 0.2, "intro")], &[by_words(5, 4, "", "the landlord letter")], "landlord letter", 5, None);
        assert!(out[0].snippet == "the landlord letter" && out[0].chunk == 4);
    }
}
