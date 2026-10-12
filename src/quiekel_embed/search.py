"""Hybrid search: meaning (vectors) + keywords (SQLite FTS5), merged per file.

Meaning finds "rental agreement" when you type "lease"; keywords find exact
names and numbers like "IMG_0042" or "2026-114" that embeddings are bad at.
Files containing all of your words come first; within that group and after it,
results are ordered by meaning, so the order always matches the confidence the
UI shows. Keyword finds get their meaning score looked up for that.
"""

import re
import unicodedata
from collections.abc import Callable

# What the filters in the UI mean, by the kind stored with every file.
KINDS = {"docs": ("doc",), "text": ("text",), "code": ("code",), "images": ("image",)}
PREFIX_MIN = 4  # shorter words must match whole words: "cat" shouldn't match "catalog"

RRF_K = 60
EXACT_BONUS = 1 / RRF_K  # an all-words match outranks a file found only one way

STOPWORDS = {
    # English
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "have", "i", "in",
    "is", "it", "its", "me", "my", "of", "on", "or", "that", "the", "this", "to", "was",
    "were", "what", "when", "where", "which", "who", "with", "about", "find", "show",
    # German
    "aber", "als", "am", "an", "auch", "auf", "aus", "bei", "das", "dass", "dem", "den",
    "der", "des", "die", "ein", "eine", "einem", "einen", "einer", "es", "für", "fur",
    "hat", "ich", "im", "in", "ist", "mein", "meine", "mit", "nach", "oder", "sich", "sie",
    "und", "von", "vom", "was", "wie", "wo", "zu", "zum", "zur",
}


# Combining marks (accents and the like), as unicodedata.combining() finds them in practice.
_MARKS = re.compile("[\u0300-\u036f\u1ab0-\u1aff\u1dc0-\u1dff\u20d0-\u20ff\ufe20-\ufe2f]")


def normalize(text: str) -> str:
    """Lowercase and strip accents, matching the FTS5 tokenizer (remove_diacritics)."""
    if text.isascii():
        return text.lower()
    return _MARKS.sub("", unicodedata.normalize("NFKD", text)).lower()


def query_terms(query: str) -> list[str]:
    # Letters and digits only, like the FTS5 tokenizer: "IMG_0042" -> img, 0042.
    words = [w for w in re.findall(r"[^\W_]+", normalize(query)) if len(w) > 1 or w.isdigit()]
    content = [w for w in words if w not in STOPWORDS] or words
    return list(dict.fromkeys(content))[:12]


def fts_query(terms: list[str], prefix_last: bool = True) -> str | None:
    """FTS5 query: any of the terms; the last one also as a prefix (search as you type)."""
    if not terms:
        return None
    parts = [f'"{t}"' for t in terms]
    if prefix_last and len(terms[-1]) >= 2:
        parts[-1] += "*"
    return " OR ".join(parts)


def looks_like_identifier(query: str) -> bool:
    """File names, codes and numbers, where exact keywords matter most."""
    return bool(re.search(r"\d|_|\w[.\-/]\w|\"", query))


def has_all_terms(terms: list[str], *texts: str) -> bool:
    haystack = normalize(" ".join(texts))
    # Words where "_" counts as a separator, as in the keyword index. Longer words may also
    # match the start of a word ("invoice" in "invoices"), short ones only whole words.
    def found(t: str) -> bool:
        end = "" if len(t) >= PREFIX_MIN else r"(?![^\W_])"
        return re.search(r"(?<![^\W_])" + re.escape(t) + end, haystack) is not None
    return all(found(t) for t in terms)


def _best_per_file(hits: list[dict]) -> list[dict]:
    seen, out = set(), []
    for h in hits:
        if h["file_id"] not in seen:
            seen.add(h["file_id"])
            out.append(h)
    return out


def fuse(
    vector_hits: list[dict], keyword_hits: list[dict], query: str, limit: int,
    similarity_of: Callable[[list[int]], dict[int, float]] | None = None,
) -> list[dict]:
    """Merge chunk-level hits (each list best-first) into ranked per-file results.

    similarity_of(file_ids) looks up the meaning score of files only the keywords found.
    """
    terms = query_terms(query)
    kw_weight = 1.0 if len(terms) <= 3 or looks_like_identifier(query) else 0.5
    files: dict[int, dict] = {}
    for rank, h in enumerate(_best_per_file(vector_hits)):
        files[h["file_id"]] = {
            "file_id": h["file_id"], "chunk": h["chunk"], "snippet": h["text"],
            "similarity": round(1 - float(h["_distance"]), 4),
            "score": 1 / (RRF_K + rank), "keyword": False, "exact": False,
        }
    for rank, h in enumerate(_best_per_file(keyword_hits)):
        exact = has_all_terms(terms, h["name"], h["text"])
        r = files.setdefault(h["file_id"], {
            "file_id": h["file_id"], "chunk": h["chunk"], "snippet": h["text"],
            "similarity": None, "score": 0.0, "keyword": False, "exact": False,
        })
        r["score"] += kw_weight / (RRF_K + rank) + (EXACT_BONUS if exact else 0)
        r["keyword"] = True
        if (exact and not r["exact"]) or r["similarity"] is None:
            # Show the passage that actually contains the words.
            r["chunk"], r["snippet"] = h["chunk"], h["text"]
        r["exact"] = r["exact"] or exact
    missing = [fid for fid, r in files.items() if r["similarity"] is None]
    if missing and similarity_of:
        for fid, sim in similarity_of(missing).items():
            files[fid]["similarity"] = round(float(sim), 4)
    return sorted(files.values(), key=_order)[:limit]


def _order(r: dict) -> tuple:
    """All your words first, then by meaning. Before the model is ready: by keyword rank."""
    sim = r["similarity"]
    return (not r["exact"], sim is None, -(sim or 0.0), -r["score"])
