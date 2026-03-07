//! Credential detector — scans text for leaked API keys, tokens, and secrets.

use regex::Regex;
use std::sync::LazyLock;

/// A credential detected in plain text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCredential {
    /// A human-readable key name (e.g. "anthropic_api_key").
    pub key: String,
    /// The raw secret value that was matched.
    pub value: String,
    /// The provider or category (e.g. "anthropic", "openai", "generic").
    pub provider: String,
}

/// Known provider-specific patterns.
struct ProviderPattern {
    /// Regex that captures the secret value in group 1 (or the full match).
    regex: Regex,
    /// Key name to assign when this pattern matches.
    key: &'static str,
    /// Provider label.
    provider: &'static str,
}

/// Generic assignment patterns (e.g. `api_key=VALUE`).
struct GenericPattern {
    regex: Regex,
    provider: &'static str,
}

// ── Static pattern tables ───────────────────────────────────────────────

static PROVIDER_PATTERNS: LazyLock<Vec<ProviderPattern>> = LazyLock::new(|| {
    vec![
        ProviderPattern {
            regex: Regex::new(r"(sk-ant-[A-Za-z0-9_\-]{20,})").unwrap(),
            key: "anthropic_api_key",
            provider: "anthropic",
        },
        ProviderPattern {
            // OpenAI keys: sk- followed by at least 20 alnum/dash chars.
            // We match all sk-* and later filter out sk-ant-* (handled by
            // the Anthropic pattern which runs first and populates
            // `seen_values`).
            regex: Regex::new(r"(sk-[A-Za-z0-9_\-]{20,})").unwrap(),
            key: "openai_api_key",
            provider: "openai",
        },
        ProviderPattern {
            regex: Regex::new(r"(gsk_[A-Za-z0-9_\-]{20,})").unwrap(),
            key: "groq_api_key",
            provider: "groq",
        },
        ProviderPattern {
            regex: Regex::new(r"(AIza[A-Za-z0-9_\-]{20,})").unwrap(),
            key: "google_api_key",
            provider: "google",
        },
        ProviderPattern {
            regex: Regex::new(r"(xoxb-[A-Za-z0-9\-]{20,})").unwrap(),
            key: "slack_bot_token",
            provider: "slack",
        },
        ProviderPattern {
            regex: Regex::new(r"(xoxp-[A-Za-z0-9\-]{20,})").unwrap(),
            key: "slack_user_token",
            provider: "slack",
        },
    ]
});

static GENERIC_PATTERNS: LazyLock<Vec<GenericPattern>> = LazyLock::new(|| {
    vec![
        GenericPattern {
            // matches: api_key=VALUE, API_KEY="VALUE", api_key = 'VALUE'
            regex: Regex::new(
                r#"(?i)(api_key)\s*=\s*['"]?([A-Za-z0-9_\-./+]{8,})['"]?"#,
            )
            .unwrap(),
            provider: "generic",
        },
        GenericPattern {
            regex: Regex::new(
                r#"(?i)(token)\s*=\s*['"]?([A-Za-z0-9_\-./+]{8,})['"]?"#,
            )
            .unwrap(),
            provider: "generic",
        },
        GenericPattern {
            regex: Regex::new(
                r#"(?i)(secret)\s*=\s*['"]?([A-Za-z0-9_\-./+]{8,})['"]?"#,
            )
            .unwrap(),
            provider: "generic",
        },
        GenericPattern {
            // env-var style: export FOO_KEY=...  or  FOO_SECRET=...
            regex: Regex::new(
                r#"(?:export\s+)?([A-Z_]{2,}(?:KEY|SECRET|TOKEN))\s*=\s*['"]?([A-Za-z0-9_\-./+]{8,})['"]?"#,
            )
            .unwrap(),
            provider: "env",
        },
    ]
});

/// Scan `text` and return all detected credentials.
///
/// Provider-specific patterns are checked first; generic patterns are then
/// applied, but duplicate values already found by a provider pattern are
/// skipped.
pub fn detect_credentials(text: &str) -> Vec<DetectedCredential> {
    let mut results: Vec<DetectedCredential> = Vec::new();
    let mut seen_values: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Provider-specific patterns.
    for pat in PROVIDER_PATTERNS.iter() {
        for caps in pat.regex.captures_iter(text) {
            let value = caps.get(1).unwrap().as_str().to_string();
            if seen_values.insert(value.clone()) {
                results.push(DetectedCredential {
                    key: pat.key.to_string(),
                    value,
                    provider: pat.provider.to_string(),
                });
            }
        }
    }

    // 2. Generic assignment patterns.
    for pat in GENERIC_PATTERNS.iter() {
        for caps in pat.regex.captures_iter(text) {
            let key_name = caps.get(1).unwrap().as_str().to_lowercase();
            let value = caps.get(2).unwrap().as_str().to_string();
            if seen_values.insert(value.clone()) {
                results.push(DetectedCredential {
                    key: key_name,
                    value,
                    provider: pat.provider.to_string(),
                });
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_anthropic() {
        let input = "key is sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA";
        let creds = detect_credentials(input);
        assert_eq!(creds.len(), 1);
        assert_eq!(creds[0].provider, "anthropic");
        assert_eq!(creds[0].key, "anthropic_api_key");
    }

    #[test]
    fn detect_openai() {
        let input = "export OPENAI=sk-abcdefghijklmnopqrstuvwx";
        let creds = detect_credentials(input);
        assert!(creds.iter().any(|c| c.provider == "openai"));
    }

    #[test]
    fn detect_groq() {
        let input = "gsk_abcdefghijklmnopqrstuvwx";
        let creds = detect_credentials(input);
        assert_eq!(creds[0].provider, "groq");
    }

    #[test]
    fn detect_google() {
        let input = "AIzaSyA-abcdefghijklmnopqrstu";
        let creds = detect_credentials(input);
        assert_eq!(creds[0].provider, "google");
    }

    #[test]
    fn detect_slack() {
        let input = "xoxb-12345678901234567890-abc and xoxp-12345678901234567890-xyz";
        let creds = detect_credentials(input);
        assert_eq!(creds.len(), 2);
        assert!(creds.iter().all(|c| c.provider == "slack"));
    }

    #[test]
    fn detect_generic_api_key() {
        let input = r#"api_key="my_super_secret_value_1234""#;
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
        assert_eq!(creds[0].value, "my_super_secret_value_1234");
    }

    #[test]
    fn detect_env_var() {
        let input = "export MY_SECRET_TOKEN=abcdefghijklmnop";
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
    }

    #[test]
    fn no_false_positives() {
        let input = "This is a normal sentence with no secrets.";
        let creds = detect_credentials(input);
        assert!(creds.is_empty());
    }

    // ── T5b: New edge case / security tests ────────────────────────────

    #[test]
    fn empty_input_returns_empty() {
        let creds = detect_credentials("");
        assert!(creds.is_empty());
    }

    #[test]
    fn short_tokens_not_detected() {
        // Tokens shorter than 20 chars after prefix should not be detected
        let input = "sk-short";
        let creds = detect_credentials(input);
        assert!(creds.is_empty());
    }

    #[test]
    fn no_false_positive_on_url() {
        let input = "Visit https://example.com/path?q=value for more info";
        let creds = detect_credentials(input);
        assert!(creds.is_empty());
    }

    #[test]
    fn no_false_positive_on_code_snippet() {
        let input = r#"let x = "hello_world"; println!("{}", x);"#;
        let creds = detect_credentials(input);
        assert!(creds.is_empty());
    }

    #[test]
    fn multiple_credentials_in_same_text() {
        let input = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA and gsk_BBBBBBBBBBBBBBBBBBBBBB";
        let creds = detect_credentials(input);
        assert_eq!(creds.len(), 2);
        let providers: Vec<&str> = creds.iter().map(|c| c.provider.as_str()).collect();
        assert!(providers.contains(&"anthropic"));
        assert!(providers.contains(&"groq"));
    }

    #[test]
    fn duplicate_value_deduplication() {
        // Same token appearing twice should only be detected once
        let input = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA and sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA";
        let creds = detect_credentials(input);
        assert_eq!(creds.len(), 1);
    }

    #[test]
    fn generic_api_key_with_quotes() {
        let input = r#"api_key = 'my_super_long_secret_123'"#;
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
    }

    #[test]
    fn generic_token_detected() {
        let input = r#"token = "abcdefghijklmnopqrstuv""#;
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
    }

    #[test]
    fn generic_secret_detected() {
        let input = r#"secret="my_super_duper_secret_value""#;
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
    }

    #[test]
    fn anthropic_key_not_matched_as_openai() {
        // The Anthropic pattern runs first; the OpenAI `sk-*` pattern should
        // not emit a duplicate because seen_values deduplication kicks in.
        let input = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA";
        let creds = detect_credentials(input);
        assert_eq!(creds.len(), 1);
        assert_eq!(creds[0].provider, "anthropic");
    }

    #[test]
    fn unicode_text_no_crash() {
        let input = "This is Unicode: \u{1F600}\u{1F601} and no secrets \u{2764}";
        let creds = detect_credentials(input);
        assert!(creds.is_empty());
    }

    #[test]
    fn multiline_input() {
        let input = "line 1\napi_key=abcdefghijklmnop\nline 3";
        let creds = detect_credentials(input);
        assert!(!creds.is_empty());
    }
}
