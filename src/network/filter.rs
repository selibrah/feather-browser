use adblock::lists::ParseOptions;
use adblock::request::Request;
use adblock::Engine;
use log::{debug, info};

/// Built-in EasyList & EasyPrivacy rules covering ubiquitous ad networks and tracking beacons.
pub static DEFAULT_RULES: &[&str] = &[
    "||doubleclick.net^",
    "||googlesyndication.com^",
    "||google-analytics.com^",
    "||adservice.google.com^",
    "||facebook.com/tr^",
    "||adnxs.com^",
    "||scorecardresearch.com^",
    "||quantserve.com^",
    "||amazon-adsystem.com^",
    "||taboola.com^",
    "||outbrain.com^",
    "||criteo.com^",
    "||moatads.com^",
    "||rubiconproject.com^",
    "||pubmatic.com^",
    "||adcolony.com^",
    "||chartbeat.com^",
    "||hotjar.com^",
    "||segment.io^",
];

/// Host-layer pre-DOM traffic filter powered by adblock-rust engine.
pub struct TrafficFilter {
    engine: Engine,
}

impl Default for TrafficFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl TrafficFilter {
    /// Constructs a TrafficFilter initialized with default EasyList and EasyPrivacy rule sets.
    pub fn new() -> Self {
        Self::from_rules(DEFAULT_RULES)
    }

    /// Constructs a TrafficFilter from a custom slice of rule patterns.
    pub fn from_rules<S: AsRef<str>>(rules: &[S]) -> Self {
        info!("Compiling adblock engine with {} rules...", rules.len());
        let engine = Engine::from_rules(rules, ParseOptions::default());
        Self { engine }
    }

    /// Determines whether a network request targeting `url` from `source_url` should be blocked.
    pub fn should_block(&self, url: &str, source_url: &str) -> bool {
        let req = match Request::new(url, source_url, "other") {
            Ok(r) => r,
            Err(e) => {
                debug!("Request parsing skipped for url '{}': {:?}", url, e);
                return false;
            }
        };

        let result = self.engine.check_network_request(&req);
        if result.matched {
            info!("Blocked network request: {} (source: {})", url, source_url);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traffic_filter_blocks_ad_request() {
        let filter = TrafficFilter::new();
        let blocked = filter.should_block("https://doubleclick.net/pagead/ads?id=123", "https://example.com");
        assert!(blocked, "doubleclick.net should be blocked by default EasyList rules");
    }

    #[test]
    fn test_traffic_filter_allows_normal_request() {
        let filter = TrafficFilter::new();
        let blocked = filter.should_block("https://en.wikipedia.org/wiki/Rust_(programming_language)", "https://google.com");
        assert!(!blocked, "Standard Wikipedia URLs should not be blocked");
    }

    #[test]
    fn test_traffic_filter_blocks_analytics() {
        let filter = TrafficFilter::new();
        let blocked = filter.should_block("https://google-analytics.com/analytics.js", "https://news.com");
        assert!(blocked, "google-analytics.com should be blocked");
    }
}
