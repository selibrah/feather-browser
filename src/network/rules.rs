//! Converts EasyList-format filter lists into WebKit's content-blocking JSON.
//!
//! `adblock::Engine` can only tell us about a navigation we hand it. WebKit's own
//! content-rule engine sits in the networking layer and sees every subresource a page
//! requests, which is where ads and trackers actually live. The `adblock` crate ships the
//! converter between the two formats — the same path Brave uses to ship these lists on
//! iOS and Safari.

use adblock::lists::{FilterSet, ParseOptions, RuleTypes};
use log::{info, warn};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// How many rules go into one compiled list.
///
/// WebKit caps the size of a single rule list and fails the whole compile when it is
/// exceeded, so the lists are split into chunks well under any plausible limit.
/// `WKUserContentController` accepts as many lists as we hand it, so nothing is dropped.
// ponytail: fixed chunk size, measured at ~1.5s to compile on this machine and cached by
// WebKit afterwards. Tune it if first-launch compile time starts to matter.
pub const RULES_PER_CHUNK: usize = 25_000;

/// Filter lists the browser will load if it finds them, in precedence order.
pub const LIST_FILENAMES: [&str; 2] = ["easylist.txt", "easyprivacy.txt"];

/// One list's worth of rules, ready to hand to WebKit.
pub struct RuleChunk {
    /// WebKit content-blocking JSON.
    pub json: String,
    /// Stable identifier derived from the rules themselves. WebKit keeps compiled lists in
    /// an on-disk store keyed by this, so an unchanged chunk is never recompiled.
    pub identifier: String,
    pub rule_count: usize,
}

/// Every chunk the filter lists converted into.
pub struct ContentRules {
    pub chunks: Vec<RuleChunk>,
    /// Total across all chunks.
    pub rule_count: usize,
}

/// What a previous launch compiled, so this one does not have to.
///
/// WebKit keeps compiled rule lists in its own on-disk store keyed by identifier. Given the
/// identifiers, a launch can attach the rules without parsing a filter list, building a
/// `FilterSet` or serializing a line of JSON — which together cost more resident memory than
/// the rest of the browser put together.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// Hash of the filter list text these identifiers were built from. A changed list means
    /// the identifiers are stale and everything has to be rebuilt.
    pub source_hash: String,
    /// Each compiled list's identifier and how many rules it holds. The counts are stored
    /// rather than divided out later, so the number the topbar shows is the real one.
    pub chunks: Vec<(String, usize)>,
}

impl Manifest {
    pub fn rule_count(&self) -> usize {
        self.chunks.iter().map(|(_, n)| n).sum()
    }
}

/// Where the manifest is kept, beside the lists it describes.
pub fn manifest_path(dir: &Path) -> PathBuf {
    dir.join("content-rules.json")
}

/// Content hash of the filter lists, for deciding whether a manifest still applies.
pub fn hash_sources(list_texts: &[String]) -> String {
    let mut hasher = DefaultHasher::new();
    for text in list_texts {
        text.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Reads the manifest, if one is there and parses.
pub fn load_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(manifest_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save_manifest(dir: &Path, manifest: &Manifest) {
    match serde_json::to_string(manifest).map(|json| std::fs::write(manifest_path(dir), json)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => warn!("Could not write the rule manifest: {}", e),
        Err(e) => warn!("Could not serialize the rule manifest: {}", e),
    }
}

/// Drops the manifest so the next launch rebuilds from source.
pub fn forget_manifest(dir: &Path) {
    let _ = std::fs::remove_file(manifest_path(dir));
}

/// Reads whatever filter lists are present in `dir`. Missing files are skipped, not fatal.
pub fn load_lists(dir: &Path) -> Vec<String> {
    let mut texts = Vec::new();
    for name in LIST_FILENAMES {
        let path = dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                info!("Loaded filter list {} ({} KB)", name, text.len() / 1024);
                texts.push(text);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("Could not read filter list {}: {}", path.display(), e),
        }
    }
    texts
}

/// Filter lists live in the data directory, next to the session database.
pub fn default_list_dir() -> PathBuf {
    crate::paths::data_dir()
}

/// Converts filter list text into WebKit content-blocking JSON.
///
/// Returns `Err` when the lists contain nothing convertible — an empty rule array is
/// rejected by WebKit, so there is nothing useful to hand it.
pub fn build(list_texts: &[String]) -> Result<ContentRules, String> {
    if list_texts.is_empty() {
        return Err("no filter lists supplied".to_string());
    }

    // `into_content_blocking` returns Err(()) unless the set was built in debug mode: the
    // converter needs the raw filter text that non-debug parsing discards.
    let mut set = FilterSet::new(true);

    // Cosmetic filters become `css-display-none` rules, which inflate the count sharply
    // against a hard cap. Network rules are what stop the request from being made at all.
    let opts = ParseOptions {
        rule_types: RuleTypes::NetworkOnly,
        ..ParseOptions::default()
    };

    for text in list_texts {
        set.add_filter_list(text, opts);
    }

    let (rules, _used) = set
        .into_content_blocking()
        .map_err(|_| "adblock refused to convert the filter set".to_string())?;

    if rules.is_empty() {
        return Err("filter lists produced no content-blocking rules".to_string());
    }

    let rule_count = rules.len();
    let mut chunks = Vec::new();

    for part in rules.chunks(RULES_PER_CHUNK) {
        let json = serde_json::to_string(part)
            .map_err(|e| format!("could not serialize content rules: {}", e))?;

        let mut hasher = DefaultHasher::new();
        json.hash(&mut hasher);

        chunks.push(RuleChunk {
            identifier: format!("feather-{:016x}", hasher.finish()),
            json,
            rule_count: part.len(),
        });
    }

    info!(
        "Content rules ready: {} rules in {} list(s).",
        rule_count,
        chunks.len()
    );

    Ok(ContentRules { chunks, rule_count })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "||doubleclick.net^\n||googlesyndication.com^\n! a comment\n||scorecardresearch.com^\n";

    #[test]
    fn test_build_produces_rules() {
        let rules = build(&[SAMPLE.to_string()]).expect("sample list converts");
        assert!(rules.rule_count >= 3, "expected one rule per filter, got {}", rules.rule_count);
        assert_eq!(rules.chunks.len(), 1, "a tiny list needs exactly one chunk");
    }

    #[test]
    fn test_every_rule_survives_chunking() {
        // A real EasyList converts to more rules than one WebKit list should hold. Nothing
        // may be dropped on the way: the trackers live in the tail of the list.
        let many: Vec<String> = (0..RULES_PER_CHUNK + 10)
            .map(|i| format!("||tracker{}.example^\n", i))
            .collect();
        let rules = build(&[many.concat()]).unwrap();

        assert!(rules.chunks.len() > 1, "should have split into several lists");
        let summed: usize = rules.chunks.iter().map(|c| c.rule_count).sum();
        assert_eq!(summed, rules.rule_count, "chunks must account for every rule");
        assert!(rules.rule_count >= RULES_PER_CHUNK + 10, "no rule may be dropped");
    }

    #[test]
    fn test_json_is_a_webkit_rule_array() {
        let rules = build(&[SAMPLE.to_string()]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rules.chunks[0].json).unwrap();
        let array = parsed.as_array().expect("WebKit requires a top-level array");
        let first = &array[0];
        // WebKit rejects the whole list if either key is missing from any entry.
        assert!(first.get("trigger").is_some(), "rule needs a trigger");
        assert!(first.get("action").is_some(), "rule needs an action");
        assert!(first["trigger"].get("url-filter").is_some(), "trigger needs a url-filter");
    }

    #[test]
    fn test_identifier_is_stable_and_content_derived() {
        let a = build(&[SAMPLE.to_string()]).unwrap();
        let b = build(&[SAMPLE.to_string()]).unwrap();
        assert_eq!(
            a.chunks[0].identifier, b.chunks[0].identifier,
            "same input must reuse WebKit's cached list"
        );

        let c = build(&[format!("{}||extra-tracker.example^\n", SAMPLE)]).unwrap();
        assert_ne!(
            a.chunks[0].identifier, c.chunks[0].identifier,
            "changed input must force a recompile"
        );
    }

    #[test]
    fn test_empty_input_is_an_error_not_an_empty_list() {
        assert!(build(&[]).is_err());
        assert!(build(&["! nothing but a comment\n".to_string()]).is_err());
    }

    #[test]
    fn test_missing_list_files_are_skipped() {
        let dir = std::env::temp_dir().join("feather-rules-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_lists(&dir).is_empty());
    }
}
