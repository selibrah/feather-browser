use regex::Regex;
use url::Url;

/// URL cleaner that strips telemetry, tracking parameters, and query junk
/// prior to socket connection or WebView navigation.
#[derive(Debug, Clone)]
pub struct UrlCleaner {
    tracking_regex: Regex,
}

impl Default for UrlCleaner {
    fn default() -> Self {
        Self::new()
    }
}

impl UrlCleaner {
    /// Constructs a new UrlCleaner with the standard tracking token patterns.
    pub fn new() -> Self {
        // Pattern: utm_*, fbclid, gclid, msclkid, mc_eid, _ga
        let pattern = r"(?i)(&|\?)(utm_[^&]+|fbclid=[^&]+|gclid=[^&]+|msclkid=[^&]+|mc_eid=[^&]+|_ga=[^&]+)";
        let tracking_regex = Regex::new(pattern).expect("Valid tracking token regex");
        Self { tracking_regex }
    }

    /// Cleans a URL string by stripping tracking query parameters.
    ///
    /// Idempotent: a URL with nothing to strip is returned byte-for-byte unchanged.
    /// Callers rely on that (the navigation handler compares input to output), and it
    /// keeps signed URLs — presigned S3 links, OAuth callbacks — out of the re-encoder.
    pub fn clean_url(&self, input: &str) -> String {
        let mut parsed = match Url::parse(input) {
            Ok(p) => p,
            // Relative or non-standard input: fall back to regex stripping.
            Err(_) => return self.clean_url_fallback(input),
        };

        if parsed.query().is_none() {
            return input.to_string();
        }

        let mut kept: Vec<(String, String)> = Vec::new();
        let mut stripped = false;
        for (key, value) in parsed.query_pairs() {
            if self.is_tracking_param(&key) {
                stripped = true;
            } else {
                kept.push((key.into_owned(), value.into_owned()));
            }
        }

        if !stripped {
            return input.to_string();
        }

        if kept.is_empty() {
            parsed.set_query(None);
        } else {
            // `extend_pairs` percent-encodes as it writes. The hand-rolled builder this
            // replaced did not, so an encoded '&' or '=' inside a value came back out as
            // a live delimiter and split one parameter into several.
            parsed.query_pairs_mut().clear().extend_pairs(&kept);
        }

        parsed.to_string()
    }

    /// Check if a query parameter key is a known tracking token.
    fn is_tracking_param(&self, key: &str) -> bool {
        let lower = key.to_lowercase();
        lower.starts_with("utm_")
            || lower == "fbclid"
            || lower == "gclid"
            || lower == "msclkid"
            || lower == "mc_eid"
            || lower == "_ga"
    }

    /// Fallback regex sanitizer for arbitrary URL strings
    fn clean_url_fallback(&self, input: &str) -> String {
        let mut cleaned = self.tracking_regex.replace_all(input, "").to_string();
        // Fix trailing or malformed query prefixes
        if cleaned.ends_with('?') {
            cleaned.pop();
        } else if let Some(idx) = cleaned.find('?') {
            let after_q = &cleaned[idx + 1..];
            if after_q.starts_with('&') {
                cleaned.replace_range(idx + 1..idx + 2, "");
            }
        }
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_url_tracking_tokens() {
        let cleaner = UrlCleaner::new();
        let target = "https://example.com/?utm_source=news&fbclid=xyz&id=1";
        let cleaned = cleaner.clean_url(target);
        assert_eq!(cleaned, "https://example.com/?id=1");
    }

    #[test]
    fn test_clean_url_all_tracking_tokens() {
        let cleaner = UrlCleaner::new();
        let target = "https://example.com/?utm_source=newsletter&utm_medium=email&fbclid=12345";
        let cleaned = cleaner.clean_url(target);
        assert_eq!(cleaned, "https://example.com/");
    }

    #[test]
    fn test_clean_url_without_query() {
        let cleaner = UrlCleaner::new();
        let target = "https://example.com/blog/article";
        let cleaned = cleaner.clean_url(target);
        assert_eq!(cleaned, "https://example.com/blog/article");
    }

    #[test]
    fn test_clean_url_with_fragment() {
        let cleaner = UrlCleaner::new();
        let target = "https://example.com/page?gclid=abc&section=hero#anchor";
        let cleaned = cleaner.clean_url(target);
        assert_eq!(cleaned, "https://example.com/page?section=hero#anchor");
    }

    #[test]
    fn test_url_without_tracking_is_returned_verbatim() {
        let cleaner = UrlCleaner::new();
        // Re-encoding a query we have no reason to touch breaks signed URLs.
        for target in [
            "https://auth.example.com/authorize?redirect_uri=https%3A%2F%2Fapp.com%2Fcb%3Fx%3D1&state=abc",
            "https://s3.amazonaws.com/b/k?X-Amz-Signature=d4e5&X-Amz-Expires=900",
            "https://duckduckgo.com/?q=rust+wry",
            "https://example.com/?flag",
        ] {
            assert_eq!(cleaner.clean_url(target), target);
        }
    }

    #[test]
    fn test_encoded_delimiters_survive_stripping() {
        let cleaner = UrlCleaner::new();
        // The old builder emitted `?q=a&b=c&id=1` here, splitting one parameter into three.
        let cleaned = cleaner.clean_url("https://example.com/?utm_source=n&q=a%26b%3Dc&id=1");
        let parsed = Url::parse(&cleaned).unwrap();
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("q".to_string(), "a&b=c".to_string()),
                ("id".to_string(), "1".to_string()),
            ]
        );
    }

    #[test]
    fn test_oauth_redirect_survives_stripping() {
        let cleaner = UrlCleaner::new();
        let cleaned = cleaner
            .clean_url("https://auth.example.com/authorize?fbclid=x&redirect_uri=https%3A%2F%2Fapp.com%2Fcb%3Fx%3D1");
        let parsed = Url::parse(&cleaned).unwrap();
        let redirect = parsed
            .query_pairs()
            .find(|(k, _)| k == "redirect_uri")
            .map(|(_, v)| v.into_owned());
        assert_eq!(redirect, Some("https://app.com/cb?x=1".to_string()));
    }

    #[test]
    fn test_clean_url_is_idempotent() {
        let cleaner = UrlCleaner::new();
        // The navigation handler loops forever if a second pass changes anything.
        let once = cleaner.clean_url("https://example.com/?utm_source=n&q=a%26b&id=1");
        assert_eq!(cleaner.clean_url(&once), once);
    }
}
