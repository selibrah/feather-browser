use serde::{Deserialize, Serialize};

/// Supported search providers for FeatherBrowser.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SearchProvider {
    #[default]
    DuckDuckGo,
    Brave,
    Google,
    Custom { name: String, search_template: String },
}

impl SearchProvider {
    pub fn name(&self) -> &str {
        match self {
            SearchProvider::DuckDuckGo => "DuckDuckGo",
            SearchProvider::Brave => "Brave",
            SearchProvider::Google => "Google",
            SearchProvider::Custom { name, .. } => name.as_str(),
        }
    }

    pub fn format_query_url(&self, query: &str) -> String {
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        match self {
            SearchProvider::DuckDuckGo => format!("https://duckduckgo.com/?q={}", encoded),
            SearchProvider::Brave => format!("https://search.brave.com/search?q={}", encoded),
            SearchProvider::Google => format!("https://www.google.com/search?q={}", encoded),
            SearchProvider::Custom { search_template, .. } => {
                search_template.replace("{query}", &encoded)
            }
        }
    }
}

/// A structured suggestion item emitted by the Omnibar search engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OmnibarSuggestion {
    pub kind: String,
    pub title: String,
    pub url: String,
    pub icon: String,
}

/// Lightweight reference to a history item used during suggestion generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMatch {
    pub title: String,
    pub url: String,
    /// A starred page. Ranked ahead of plain history and badged differently.
    pub bookmarked: bool,
}

fn history_suggestion(h: &HistoryMatch) -> OmnibarSuggestion {
    OmnibarSuggestion {
        kind: if h.bookmarked { "bookmark" } else { "history" }.to_string(),
        title: if h.title.is_empty() { h.url.clone() } else { h.title.clone() },
        url: h.url.clone(),
        icon: if h.bookmarked { "⭐" } else { "🕒" }.to_string(),
    }
}

/// Core search query parser and suggestion builder.
#[derive(Debug, Clone)]
pub struct SearchEngine {
    pub default_provider: SearchProvider,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new(SearchProvider::default())
    }
}

impl SearchEngine {
    pub fn new(default_provider: SearchProvider) -> Self {
        Self { default_provider }
    }

    /// Resolves an arbitrary omnibar string into a valid destination URL.
    /// Handles schemes, bang shortcuts, domain detection, and search queries.
    pub fn resolve_query_or_url(&self, raw_input: &str) -> String {
        let input = raw_input.trim();
        if input.is_empty() {
            return self.default_provider.format_query_url("");
        }

        // 1. Check for quick-bang shortcuts (e.g. !g, !b, !gh, !w, !yt, !cr, !docs)
        if let Some(bang_url) = self.try_resolve_bang(input) {
            return bang_url;
        }

        // 2. Direct scheme check (http, https, file, about, data)
        if input.starts_with("http://")
            || input.starts_with("https://")
            || input.starts_with("file://")
            || input.starts_with("about:")
            || input.starts_with("data:")
        {
            return input.to_string();
        }

        // 3. Localhost and local IP address checks
        if input == "localhost" || input.starts_with("localhost:") || input.starts_with("localhost/") {
            return format!("http://{}", input);
        }
        if is_ip_address_or_host(input) {
            return format!("http://{}", input);
        }

        // 4. If input contains whitespace, it is unambiguously a search query
        if input.chars().any(char::is_whitespace) {
            return self.default_provider.format_query_url(input);
        }

        // 5. Check if it resembles a valid domain name or URL path
        if is_likely_domain(input) {
            return format!("https://{}", input);
        }

        // 6. Default fallback: treat single non-domain words or identifiers as search queries
        self.default_provider.format_query_url(input)
    }

    /// Evaluates if the input starts with a recognized bang shortcut.
    fn try_resolve_bang(&self, input: &str) -> Option<String> {
        let parts: Vec<&str> = input.splitn(2, char::is_whitespace).collect();
        let prefix = parts[0].to_lowercase();
        let query = if parts.len() > 1 { parts[1].trim() } else { "" };
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();

        match prefix.as_str() {
            "!g" | "!google" => Some(format!("https://www.google.com/search?q={}", encoded)),
            "!b" | "!brave" => Some(format!("https://search.brave.com/search?q={}", encoded)),
            "!ddg" | "!duckduckgo" => Some(format!("https://duckduckgo.com/?q={}", encoded)),
            "!gh" | "!github" => {
                if query.contains('/') && !query.contains(' ') {
                    Some(format!("https://github.com/{}", query))
                } else {
                    Some(format!("https://github.com/search?q={}", encoded))
                }
            }
            "!w" | "!wikipedia" => {
                Some(format!("https://en.wikipedia.org/wiki/Special:Search?search={}", encoded))
            }
            "!yt" | "!youtube" => {
                Some(format!("https://www.youtube.com/results?search_query={}", encoded))
            }
            "!r" | "!reddit" => {
                Some(format!("https://www.reddit.com/search/?q={}", encoded))
            }
            "!cr" | "!crates" => {
                Some(format!("https://crates.io/search?q={}", encoded))
            }
            "!docs" | "!docsrs" => {
                Some(format!("https://docs.rs/releases/search?query={}", encoded))
            }
            _ => None,
        }
    }

    /// Builds a ranked list of suggestions for the omnibar based on the user's typed input.
    pub fn build_suggestions(
        &self,
        raw_input: &str,
        history: &[HistoryMatch],
    ) -> Vec<OmnibarSuggestion> {
        let input = raw_input.trim();
        let mut suggestions = Vec::new();

        if input.is_empty() {
            // When empty, show top recent history entries
            for h in history.iter().take(5) {
                suggestions.push(history_suggestion(h));
            }
            return suggestions;
        }

        // 1. Primary Action: Bang shortcut
        if let Some(bang_url) = self.try_resolve_bang(input) {
            let bang_label = self.describe_bang_label(input);
            suggestions.push(OmnibarSuggestion {
                kind: "bang".to_string(),
                title: bang_label,
                url: bang_url,
                icon: "⚡".to_string(),
            });
        }
        // 2. Primary Action: Direct Domain
        else if is_likely_domain(input) {
            let full_url = if input.starts_with("http://") || input.starts_with("https://") {
                input.to_string()
            } else {
                format!("https://{}", input)
            };
            suggestions.push(OmnibarSuggestion {
                kind: "navigate".to_string(),
                title: format!("Open {}", full_url),
                url: full_url,
                icon: "🌐".to_string(),
            });

            // Also offer searching for this domain string
            suggestions.push(OmnibarSuggestion {
                kind: "search".to_string(),
                title: format!("Search {} for \"{}\"", self.default_provider.name(), input),
                url: self.default_provider.format_query_url(input),
                icon: "🔍".to_string(),
            });
        }
        // 3. Primary Action: Search query
        else {
            suggestions.push(OmnibarSuggestion {
                kind: "search".to_string(),
                title: format!("Search {} for \"{}\"", self.default_provider.name(), input),
                url: self.default_provider.format_query_url(input),
                icon: "🔍".to_string(),
            });
        }

        // 4. Append matching history suggestions
        for h in history.iter().take(4) {
            // Avoid duplicate suggestion if history URL matches primary suggestion
            if suggestions.iter().any(|s| s.url == h.url) {
                continue;
            }
            suggestions.push(history_suggestion(h));
        }

        suggestions
    }

    fn describe_bang_label(&self, input: &str) -> String {
        let parts: Vec<&str> = input.splitn(2, char::is_whitespace).collect();
        let prefix = parts[0].to_lowercase();
        let query = if parts.len() > 1 { parts[1].trim() } else { "" };

        let engine = match prefix.as_str() {
            "!g" | "!google" => "Google Search",
            "!b" | "!brave" => "Brave Search",
            "!ddg" | "!duckduckgo" => "DuckDuckGo Search",
            "!gh" | "!github" => "GitHub Search",
            "!w" | "!wikipedia" => "Wikipedia Search",
            "!yt" | "!youtube" => "YouTube Search",
            "!r" | "!reddit" => "Reddit Search",
            "!cr" | "!crates" => "Crates.io Package Search",
            "!docs" | "!docsrs" => "Docs.rs Documentation Search",
            _ => "Quick Bang",
        };

        if query.is_empty() {
            format!("{}: Open search", engine)
        } else {
            format!("{}: \"{}\"", engine, query)
        }
    }
}

/// Determines whether a string is likely an IPv4 address (e.g. 127.0.0.1:8080).
fn is_ip_address_or_host(s: &str) -> bool {
    let host_part = s.split(':').next().unwrap_or(s);
    let segments: Vec<&str> = host_part.split('.').collect();
    if segments.len() == 4 && segments.iter().all(|seg| seg.parse::<u8>().is_ok()) {
        return true;
    }
    false
}

/// Checks whether an input without scheme looks like a domain name with a valid TLD or path.
fn is_likely_domain(s: &str) -> bool {
    if s.chars().any(char::is_whitespace) {
        return false;
    }

    let host_part = s.split('/').next().unwrap_or(s);
    let host_no_port = host_part.split(':').next().unwrap_or(host_part);

    if !host_no_port.contains('.') {
        return false;
    }

    let dot_segments: Vec<&str> = host_no_port.split('.').collect();
    if dot_segments.len() < 2 {
        return false;
    }

    let last_seg = dot_segments.last().copied().unwrap_or("");
    // Valid TLD: 2 to 18 alphabetical characters (e.g., com, org, net, io, dev, app, rs, ai)
    if last_seg.len() >= 2
        && last_seg.len() <= 18
        && last_seg.chars().all(|c| c.is_ascii_alphabetic())
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_explicit_url() {
        let engine = SearchEngine::default();
        assert_eq!(
            engine.resolve_query_or_url("https://example.com/path"),
            "https://example.com/path"
        );
        assert_eq!(
            engine.resolve_query_or_url("http://insecure.org"),
            "http://insecure.org"
        );
    }

    #[test]
    fn test_resolve_domains() {
        let engine = SearchEngine::default();
        assert_eq!(engine.resolve_query_or_url("github.com"), "https://github.com");
        assert_eq!(
            engine.resolve_query_or_url("news.ycombinator.com/newest"),
            "https://news.ycombinator.com/newest"
        );
        assert_eq!(engine.resolve_query_or_url("crates.io"), "https://crates.io");
        assert_eq!(engine.resolve_query_or_url("tauri.app"), "https://tauri.app");
    }

    #[test]
    fn test_resolve_localhost_and_ip() {
        let engine = SearchEngine::default();
        assert_eq!(engine.resolve_query_or_url("localhost:3000"), "http://localhost:3000");
        assert_eq!(engine.resolve_query_or_url("127.0.0.1:8080"), "http://127.0.0.1:8080");
        assert_eq!(engine.resolve_query_or_url("192.168.1.1"), "http://192.168.1.1");
    }

    #[test]
    fn test_resolve_search_queries() {
        let engine = SearchEngine::default();
        assert_eq!(
            engine.resolve_query_or_url("rust wry tutorial"),
            "https://duckduckgo.com/?q=rust+wry+tutorial"
        );
        assert_eq!(
            engine.resolve_query_or_url("concurrency patterns"),
            "https://duckduckgo.com/?q=concurrency+patterns"
        );
        // Single word without dot is treated as search
        assert_eq!(
            engine.resolve_query_or_url("weather"),
            "https://duckduckgo.com/?q=weather"
        );
    }

    #[test]
    fn test_resolve_bang_shortcuts() {
        let engine = SearchEngine::default();
        assert_eq!(
            engine.resolve_query_or_url("!g apple silicon"),
            "https://www.google.com/search?q=apple+silicon"
        );
        assert_eq!(
            engine.resolve_query_or_url("!gh tauri-apps/wry"),
            "https://github.com/tauri-apps/wry"
        );
        assert_eq!(
            engine.resolve_query_or_url("!w rust programming"),
            "https://en.wikipedia.org/wiki/Special:Search?search=rust+programming"
        );
        assert_eq!(
            engine.resolve_query_or_url("!cr serde"),
            "https://crates.io/search?q=serde"
        );
    }

    #[test]
    fn test_build_suggestions() {
        let engine = SearchEngine::default();
        let history = vec![
            HistoryMatch {
                title: "Rust Programming Language".into(),
                url: "https://www.rust-lang.org/".into(),
                bookmarked: false,
            },
            HistoryMatch {
                title: "GitHub - tauri-apps/wry".into(),
                url: "https://github.com/tauri-apps/wry".into(),
                bookmarked: true,
            },
        ];

        // Search query suggestions
        let suggestions = engine.build_suggestions("rust", &history);
        assert!(!suggestions.is_empty());
        assert_eq!(suggestions[0].kind, "search");
        assert!(suggestions[0].url.contains("duckduckgo.com/?q=rust"));
        assert_eq!(suggestions[1].kind, "history");
        assert_eq!(suggestions[1].url, "https://www.rust-lang.org/");

        // Bang suggestions
        let bang_sugg = engine.build_suggestions("!gh wry", &[]);
        assert_eq!(bang_sugg[0].kind, "bang");
        assert!(bang_sugg[0].url.contains("github.com"));
    }

    #[test]
    fn test_bookmarks_are_badged_and_ranked_above_history() {
        let engine = SearchEngine::default();
        // The host hands them back bookmarks-first; the builder must not re-order them
        // behind plain history or relabel them.
        let history = vec![
            HistoryMatch {
                title: "Starred".into(),
                url: "https://starred.example/".into(),
                bookmarked: true,
            },
            HistoryMatch {
                title: "Just visited".into(),
                url: "https://visited.example/".into(),
                bookmarked: false,
            },
        ];
        let s = engine.build_suggestions("", &history);
        assert_eq!(s[0].kind, "bookmark");
        assert_eq!(s[0].icon, "⭐");
        assert_eq!(s[1].kind, "history");
    }
}
