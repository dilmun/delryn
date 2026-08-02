//! Word lookup: a dictionary definition plus a Wikipedia summary for a selected
//! word or phrase (the reader's `K` command). Blocking HTTP via `ureq` — call
//! from a worker thread, never the render path (mirrors the metadata `search`).
//!
//! Dictionary source order: a local `sdcv` (StarDict CLI) first when it's on
//! `PATH`, so lookups work fully offline against the user's own dictionaries;
//! then the free, key-less [Free Dictionary API] when it's enabled.
//! Wikipedia is online-only. Every provider degrades to `None` on any error, so a
//! lookup with no network (and no sdcv) simply shows "nothing found" rather than
//! failing.
//!
//! [Free Dictionary API]: https://dictionaryapi.dev/

use std::process::Command;

use serde::Deserialize;

use crate::{agent, user_agent};

const DICT_URL: &str = "https://api.dictionaryapi.dev/api/v2/entries/en";
const WIKI_URL: &str = "https://en.wikipedia.org/api/rest_v1/page/summary";
/// Google's free, key-less web-translate endpoint (`client=gtx`) — the same one
/// translate-shell uses. Unofficial; degrades to `None` if it ever changes.
const TRANSLATE_URL: &str = "https://translate.googleapis.com/translate_a/single";

/// The combined result of a word lookup: the term as queried, a dictionary
/// definition (if any source had one), and a Wikipedia summary (if one exists).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LookupResult {
    pub word: String,
    pub definition: Option<Definition>,
    pub wiki: Option<WikiSummary>,
    pub translation: Option<Translation>,
}

impl LookupResult {
    /// Whether no provider returned anything (drives the "nothing found" UI).
    pub fn is_empty(&self) -> bool {
        self.definition.is_none() && self.wiki.is_none() && self.translation.is_none()
    }
}

/// A dictionary definition: the headword, an optional phonetic transcription, the
/// source it came from, and one [`Meaning`] block per part of speech (online) or
/// per dictionary (sdcv).
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub word: String,
    pub phonetic: Option<String>,
    /// A dictionary name (sdcv) or `"Free Dictionary API"` — shown as provenance.
    pub source: String,
    pub meanings: Vec<Meaning>,
}

/// One block of senses under a shared heading (a part of speech, or a dictionary
/// name for sdcv results).
#[derive(Debug, Clone, PartialEq)]
pub struct Meaning {
    pub label: String,
    pub items: Vec<DefItem>,
}

/// A single sense: the definition text and an optional usage example.
#[derive(Debug, Clone, PartialEq)]
pub struct DefItem {
    pub text: String,
    pub example: Option<String>,
}

/// A Wikipedia article summary (the plain-text lead extract), for the term.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiSummary {
    pub title: String,
    pub description: Option<String>,
    pub extract: String,
}

/// Which providers a lookup may consult, from the user's Lookup settings. A
/// definition prefers the first enabled source that answers (sdcv, then the
/// online dictionary); Wikipedia and translation are independent.
#[derive(Debug, Clone, Default)]
pub struct LookupSources {
    /// Local `sdcv` (StarDict) — offline, the user's own dictionaries.
    pub sdcv: bool,
    /// The online Free Dictionary API.
    pub dictionary: bool,
    /// The online Wikipedia summary.
    pub wikipedia: bool,
    /// Target language code to translate the term into (e.g. `"en"`), or `None`
    /// to skip translation.
    pub translate_to: Option<String>,
}

/// A translation of the looked-up term: the translated text and the language the
/// source was auto-detected as.
#[derive(Debug, Clone, PartialEq)]
pub struct Translation {
    pub text: String,
    pub source_lang: String,
}

/// Look up `term` across the enabled `sources`: a dictionary definition (local
/// `sdcv` first, else the online API) plus a Wikipedia summary. Blocking — run on
/// a worker thread. Whitespace-trims the term; providers that fail yield `None`.
pub fn look_up(term: &str, sources: LookupSources) -> LookupResult {
    let term = term.trim();
    if term.is_empty() {
        return LookupResult::default();
    }
    let definition = sources
        .sdcv
        .then(|| sdcv(term))
        .flatten()
        .or_else(|| sources.dictionary.then(|| online_define(term)).flatten());
    let wiki = sources.wikipedia.then(|| wikipedia(term)).flatten();
    let translation = sources
        .translate_to
        .as_deref()
        .and_then(|target| translate(term, target));
    LookupResult {
        word: term.to_string(),
        definition,
        wiki,
        translation,
    }
}

/// Percent-encode a term for a URL *path* segment (space → `%20`, unlike the
/// query encoder which uses `+`). Leaves the unreserved set unescaped.
fn enc_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Local StarDict lookup via the `sdcv` CLI (`-n` non-interactive, `-j` JSON).
/// `None` if sdcv isn't installed or has no entry — callers fall back online.
fn sdcv(word: &str) -> Option<Definition> {
    let out = Command::new("sdcv")
        .args(["-n", "-j", word])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_sdcv(&out.stdout, word)
}

/// Parse `sdcv -j` output (an array of `{dict, word, definition}`) into a
/// [`Definition`] — one [`Meaning`] per dictionary, its lines split into items.
fn parse_sdcv(json: &[u8], word: &str) -> Option<Definition> {
    let hits: Vec<SdcvHit> = serde_json::from_slice(json).ok()?;
    let meanings: Vec<Meaning> = hits
        .into_iter()
        .map(|h| Meaning {
            label: h.dict,
            items: h
                .definition
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| DefItem {
                    text: l.to_string(),
                    example: None,
                })
                .collect(),
        })
        .filter(|m| !m.items.is_empty())
        .collect();
    (!meanings.is_empty()).then(|| Definition {
        word: word.to_string(),
        phonetic: None,
        source: "sdcv".to_string(),
        meanings,
    })
}

/// Free, key-less Dictionary API lookup. A 404 ("no definitions") surfaces as a
/// `ureq` status error, so `.ok()?` yields `None` and the caller degrades.
fn online_define(word: &str) -> Option<Definition> {
    let url = format!("{DICT_URL}/{}", enc_path(word));
    let mut resp = agent()
        .get(&url)
        .header("User-Agent", user_agent())
        .call()
        .ok()?;
    let entries: Vec<ApiEntry> = resp.body_mut().read_json().ok()?;
    parse_dict_api(entries, word)
}

/// Map the Dictionary API's entries onto a [`Definition`]: first non-empty
/// phonetic wins; every meaning's part of speech becomes a [`Meaning`] block.
fn parse_dict_api(entries: Vec<ApiEntry>, word: &str) -> Option<Definition> {
    let phonetic = entries.iter().find_map(|e| {
        e.phonetic
            .clone()
            .filter(|p| !p.is_empty())
            .or_else(|| e.phonetics.iter().find_map(|p| p.non_empty_text()))
    });
    let meanings: Vec<Meaning> = entries
        .into_iter()
        .flat_map(|e| e.meanings)
        .map(|m| Meaning {
            label: m.part_of_speech,
            items: m
                .definitions
                .into_iter()
                .filter(|d| !d.definition.trim().is_empty())
                .map(|d| DefItem {
                    text: d.definition,
                    example: d.example.filter(|e| !e.trim().is_empty()),
                })
                .collect(),
        })
        .filter(|m| !m.items.is_empty())
        .collect();
    (!meanings.is_empty()).then(|| Definition {
        word: word.to_string(),
        phonetic,
        source: "Free Dictionary API".to_string(),
        meanings,
    })
}

/// Wikipedia lead-summary lookup. Skips disambiguation pages (noise); a missing
/// article 404s into `None`.
fn wikipedia(term: &str) -> Option<WikiSummary> {
    let url = format!("{WIKI_URL}/{}", enc_path(term));
    let mut resp = agent()
        .get(&url)
        .header("User-Agent", user_agent())
        .call()
        .ok()?;
    let s: WikiResp = resp.body_mut().read_json().ok()?;
    parse_wiki(s)
}

/// Keep only real article summaries with a non-empty extract.
fn parse_wiki(s: WikiResp) -> Option<WikiSummary> {
    if s.kind.as_deref() == Some("disambiguation") {
        return None;
    }
    let extract = s.extract.trim().to_string();
    (!extract.is_empty()).then(|| WikiSummary {
        title: s.title,
        description: s.description.filter(|d| !d.trim().is_empty()),
        extract,
    })
}

/// Translate `text` into `target` via Google's free web endpoint (source
/// auto-detected). `None` on any error.
fn translate(text: &str, target: &str) -> Option<Translation> {
    if text.is_empty() || target.is_empty() {
        return None;
    }
    let url = format!(
        "{TRANSLATE_URL}?client=gtx&sl=auto&tl={}&dt=t&q={}",
        enc_path(target),
        enc_path(text)
    );
    let mut resp = agent()
        .get(&url)
        .header("User-Agent", user_agent())
        .call()
        .ok()?;
    let v: serde_json::Value = resp.body_mut().read_json().ok()?;
    parse_translate(&v)
}

/// Parse the endpoint's nested-array response: `[[["translated","orig",…],…],
/// null,"<detected-lang>",…]`. Concatenates every sentence chunk.
fn parse_translate(v: &serde_json::Value) -> Option<Translation> {
    let mut text = String::new();
    for seg in v.get(0)?.as_array()? {
        if let Some(chunk) = seg.get(0).and_then(serde_json::Value::as_str) {
            text.push_str(chunk);
        }
    }
    let text = text.trim().to_string();
    let source_lang = v
        .get(2)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    (!text.is_empty()).then_some(Translation { text, source_lang })
}

#[derive(Deserialize)]
struct SdcvHit {
    dict: String,
    definition: String,
}

#[derive(Deserialize)]
struct ApiEntry {
    #[serde(default)]
    phonetic: Option<String>,
    #[serde(default)]
    phonetics: Vec<ApiPhonetic>,
    #[serde(default)]
    meanings: Vec<ApiMeaning>,
}

#[derive(Deserialize)]
struct ApiPhonetic {
    #[serde(default)]
    text: Option<String>,
}

impl ApiPhonetic {
    fn non_empty_text(&self) -> Option<String> {
        self.text.clone().filter(|t| !t.is_empty())
    }
}

#[derive(Deserialize)]
struct ApiMeaning {
    #[serde(rename = "partOfSpeech", default)]
    part_of_speech: String,
    #[serde(default)]
    definitions: Vec<ApiDef>,
}

#[derive(Deserialize)]
struct ApiDef {
    #[serde(default)]
    definition: String,
    #[serde(default)]
    example: Option<String>,
}

#[derive(Deserialize)]
struct WikiResp {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    extract: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dictionary_api_entry() {
        let json = r#"[
            {
                "word": "hello",
                "phonetic": "/həˈloʊ/",
                "phonetics": [{"text": ""}, {"text": "/həˈloʊ/"}],
                "meanings": [
                    {
                        "partOfSpeech": "noun",
                        "definitions": [
                            {"definition": "A greeting.", "example": "she gave a warm hello"},
                            {"definition": "An expression of surprise."}
                        ]
                    },
                    {
                        "partOfSpeech": "verb",
                        "definitions": [{"definition": "To greet with \"hello\"."}]
                    }
                ]
            }
        ]"#;
        let entries: Vec<ApiEntry> = serde_json::from_str(json).unwrap();
        let def = parse_dict_api(entries, "hello").expect("a definition");
        assert_eq!(def.phonetic.as_deref(), Some("/həˈloʊ/"));
        assert_eq!(def.source, "Free Dictionary API");
        assert_eq!(def.meanings.len(), 2);
        assert_eq!(def.meanings[0].label, "noun");
        assert_eq!(def.meanings[0].items.len(), 2);
        assert_eq!(def.meanings[0].items[0].text, "A greeting.");
        assert_eq!(
            def.meanings[0].items[0].example.as_deref(),
            Some("she gave a warm hello")
        );
        assert!(def.meanings[0].items[1].example.is_none());
        assert_eq!(def.meanings[1].label, "verb");
    }

    #[test]
    fn empty_meanings_yield_no_definition() {
        let entries: Vec<ApiEntry> = serde_json::from_str(
            r#"[{"word":"x","meanings":[{"partOfSpeech":"noun","definitions":[]}]}]"#,
        )
        .unwrap();
        assert!(parse_dict_api(entries, "x").is_none());
    }

    #[test]
    fn parses_sdcv_output() {
        let json = r#"[
            {"dict":"WordNet","word":"delryn","definition":"a book of the largest size\nmade by folding a sheet once"},
            {"dict":"Empty","word":"delryn","definition":"   \n  "}
        ]"#;
        let def = parse_sdcv(json.as_bytes(), "delryn").expect("a definition");
        assert_eq!(def.source, "sdcv");
        // The all-whitespace dictionary is dropped; WordNet keeps two lines.
        assert_eq!(def.meanings.len(), 1);
        assert_eq!(def.meanings[0].label, "WordNet");
        assert_eq!(def.meanings[0].items.len(), 2);
        assert_eq!(def.meanings[0].items[0].text, "a book of the largest size");
    }

    #[test]
    fn parses_wikipedia_summary_and_skips_disambiguation() {
        let ok = r#"{"type":"standard","title":"Folio","description":"leaf of a book","extract":"A folio is a leaf of paper."}"#;
        let s: WikiResp = serde_json::from_str(ok).unwrap();
        let w = parse_wiki(s).expect("a summary");
        assert_eq!(w.title, "Folio");
        assert_eq!(w.description.as_deref(), Some("leaf of a book"));
        assert_eq!(w.extract, "A folio is a leaf of paper.");

        let disamb = r#"{"type":"disambiguation","title":"Folio","extract":"Folio may refer to:"}"#;
        let s: WikiResp = serde_json::from_str(disamb).unwrap();
        assert!(parse_wiki(s).is_none());
    }

    #[test]
    fn parses_google_translate_response() {
        // Two sentence chunks + a detected source language.
        let json = r#"[[["Hello ","Hola ",null,null,10],["world","mundo",null,null,3]],null,"es",null,null,null,null,[]]"#;
        let v: serde_json::Value = serde_json::from_str(json).unwrap();
        let t = parse_translate(&v).expect("a translation");
        assert_eq!(t.text, "Hello world");
        assert_eq!(t.source_lang, "es");

        // An empty sentence list yields nothing.
        let empty: serde_json::Value = serde_json::from_str(r#"[[],null,"und"]"#).unwrap();
        assert!(parse_translate(&empty).is_none());
    }

    #[test]
    fn path_encoding_uses_percent_twenty_for_space() {
        assert_eq!(enc_path("machine learning"), "machine%20learning");
        assert_eq!(enc_path("naïve"), "na%C3%AFve");
        assert_eq!(enc_path("well-being_2.0~"), "well-being_2.0~");
    }

    #[test]
    fn all_sources_disabled_makes_no_network_call() {
        // Every source off ⇒ both degrade to None without any network call.
        let none = LookupSources::default();
        let r = look_up("serendipity", none);
        assert_eq!(r.word, "serendipity");
        assert!(r.is_empty());
    }

    /// Live smoke test (network) — run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_lookup_hello() {
        let all = LookupSources {
            sdcv: true,
            dictionary: true,
            wikipedia: true,
            translate_to: Some("es".to_string()),
        };
        let r = look_up("hello", all);
        assert!(!r.is_empty(), "expected a live definition or summary");
    }
}
