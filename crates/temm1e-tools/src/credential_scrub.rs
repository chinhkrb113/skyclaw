//! Credential scrubber — removes sensitive data before it reaches the LLM.
//!
//! Applied to all browser observations that follow credential injection so that
//! passwords, tokens, API keys, and auth headers never leak into the agent's
//! conversation context.

use std::sync::LazyLock;

use regex::Regex;

/// Matches sensitive URL query parameters (token, key, secret, password, etc.).
static SENSITIVE_URL_PARAMS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(token|key|secret|password|passwd|pwd|auth|access_token|api_key|session_id|csrf|nonce)=([^&\s]+)",
    )
    .expect("invalid SENSITIVE_URL_PARAMS regex")
});

/// Matches Authorization and similar auth headers.
static AUTH_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(authorization|x-api-key|x-auth-token):[^\n]+")
        .expect("invalid AUTH_HEADER regex")
});

/// Matches common API key patterns (OpenAI sk-, GitHub ghp_/gho_, generic key-).
static API_KEY_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(sk-[a-zA-Z0-9_-]{20,}|key-[a-zA-Z0-9_-]{20,}|ghp_[a-zA-Z0-9]{36}|gho_[a-zA-Z0-9]{36})",
    )
    .expect("invalid API_KEY_PATTERNS regex")
});

/// Scrub credential-like content from text before it reaches the LLM.
///
/// `known_values` contains service names and known credential fragments to
/// redact. Values shorter than 4 characters are skipped to avoid false
/// positives (e.g., redacting "the" everywhere).
pub fn scrub(text: &str, known_values: &[&str]) -> String {
    let mut result = text.to_string();

    // 1. Redact known values (passwords, usernames, service names)
    for val in known_values {
        if !val.is_empty() && val.len() > 3 {
            result = result.replace(val, "[REDACTED]");
        }
    }

    // 2. Redact sensitive URL parameters
    result = SENSITIVE_URL_PARAMS
        .replace_all(&result, "$1=[REDACTED]")
        .to_string();

    // 3. Redact auth headers
    result = AUTH_HEADER
        .replace_all(&result, "$1: [REDACTED]")
        .to_string();

    // 4. Redact API key patterns
    result = API_KEY_PATTERNS
        .replace_all(&result, "[REDACTED_KEY]")
        .to_string();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_known_password() {
        let text = "Logged in with password MyS3cretP@ss! to the dashboard";
        let result = scrub(text, &["MyS3cretP@ss!"]);
        assert!(
            !result.contains("MyS3cretP@ss!"),
            "Password should be redacted"
        );
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_known_username() {
        let text = "Welcome, admin@example.com! Your session is active.";
        let result = scrub(text, &["admin@example.com"]);
        assert!(
            !result.contains("admin@example.com"),
            "Username should be redacted"
        );
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn scrub_url_token_param() {
        let text = "Redirected to https://example.com/callback?token=abc123def456&next=/home";
        let result = scrub(text, &[]);
        assert!(
            !result.contains("abc123def456"),
            "Token param should be redacted"
        );
        assert!(result.contains("token=[REDACTED]"));
        assert!(
            result.contains("next=/home"),
            "Non-sensitive params preserved"
        );
    }

    #[test]
    fn scrub_url_api_key_param() {
        let text = "GET /api?api_key=sk_live_abcdef123456&page=1";
        let result = scrub(text, &[]);
        assert!(result.contains("api_key=[REDACTED]"));
        assert!(result.contains("page=1"));
    }

    #[test]
    fn scrub_url_password_param() {
        let text = "https://host/login?password=hunter2&user=bob";
        let result = scrub(text, &[]);
        assert!(result.contains("password=[REDACTED]"));
    }

    #[test]
    fn scrub_url_access_token_param() {
        let text = "oauth?access_token=ya29.long_token_value_here";
        let result = scrub(text, &[]);
        assert!(result.contains("access_token=[REDACTED]"));
    }

    #[test]
    fn scrub_url_session_id_param() {
        let text = "https://app.com/page?session_id=sess_abc123&view=main";
        let result = scrub(text, &[]);
        assert!(result.contains("session_id=[REDACTED]"));
    }

    #[test]
    fn scrub_auth_header_bearer() {
        let text = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature";
        let result = scrub(text, &[]);
        assert!(
            !result.contains("eyJhbGciOiJIUzI1NiJ9"),
            "Bearer token should be redacted"
        );
        assert!(result.contains("Authorization: [REDACTED]"));
    }

    #[test]
    fn scrub_auth_header_basic() {
        let text = "authorization: Basic dXNlcjpwYXNz";
        let result = scrub(text, &[]);
        assert!(!result.contains("dXNlcjpwYXNz"));
        assert!(result.contains("authorization: [REDACTED]"));
    }

    #[test]
    fn scrub_x_api_key_header() {
        let text = "x-api-key: my-secret-api-key-12345";
        let result = scrub(text, &[]);
        assert!(result.contains("x-api-key: [REDACTED]"));
    }

    #[test]
    fn scrub_x_auth_token_header() {
        let text = "X-Auth-Token: tok_live_abcdef12345678";
        let result = scrub(text, &[]);
        assert!(result.contains("X-Auth-Token: [REDACTED]"));
    }

    #[test]
    fn scrub_openai_api_key() {
        let text = "Found key: sk-proj-abcdefghijklmnopqrstuvwx in the config";
        let result = scrub(text, &[]);
        assert!(
            !result.contains("sk-proj-abcdefghijklmnopqrstuvwx"),
            "OpenAI key should be redacted"
        );
        assert!(result.contains("[REDACTED_KEY]"));
    }

    #[test]
    fn scrub_github_pat() {
        let text = "Token: ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let result = scrub(text, &[]);
        assert!(result.contains("[REDACTED_KEY]"));
    }

    #[test]
    fn scrub_github_oauth() {
        let text = "OAuth: gho_abcdefghijklmnopqrstuvwxyz0123456789";
        let result = scrub(text, &[]);
        assert!(result.contains("[REDACTED_KEY]"));
    }

    #[test]
    fn scrub_preserves_non_sensitive_text() {
        let text = "Welcome to the dashboard. Your name is displayed above.";
        let result = scrub(text, &[]);
        assert_eq!(result, text, "Non-sensitive text should be unchanged");
    }

    #[test]
    fn scrub_short_known_values_skipped() {
        let text = "The cat sat on the mat";
        let result = scrub(text, &["cat"]);
        assert_eq!(
            result, text,
            "Known values of 3 chars or less should be skipped"
        );
    }

    #[test]
    fn scrub_empty_known_values_skipped() {
        let text = "Hello world";
        let result = scrub(text, &[""]);
        assert_eq!(result, text);
    }

    #[test]
    fn scrub_multiple_known_values() {
        let text = "user admin@test.com logged in with password hunter2hunter2";
        let result = scrub(text, &["admin@test.com", "hunter2hunter2"]);
        assert!(!result.contains("admin@test.com"));
        assert!(!result.contains("hunter2hunter2"));
    }

    #[test]
    fn scrub_combined_patterns() {
        let text = "URL: https://api.com?token=abc123&key=xyz789\n\
                     Authorization: Bearer eyJ_token_here\n\
                     API key found: sk-abcdefghijklmnopqrstuvwxyz";
        let result = scrub(text, &[]);
        assert!(result.contains("token=[REDACTED]"));
        assert!(result.contains("key=[REDACTED]"));
        assert!(result.contains("Authorization: [REDACTED]"));
        assert!(result.contains("[REDACTED_KEY]"));
    }

    #[test]
    fn scrub_case_insensitive_url_params() {
        let text = "URL: https://example.com?TOKEN=secret123&PASSWORD=pass456";
        let result = scrub(text, &[]);
        assert!(result.contains("TOKEN=[REDACTED]"));
        assert!(result.contains("PASSWORD=[REDACTED]"));
    }

    #[test]
    fn scrub_csrf_and_nonce_params() {
        let text = "form?csrf=abc123def&nonce=xyz789ghi";
        let result = scrub(text, &[]);
        assert!(result.contains("csrf=[REDACTED]"));
        assert!(result.contains("nonce=[REDACTED]"));
    }

    #[test]
    fn scrub_empty_text() {
        let result = scrub("", &["password"]);
        assert_eq!(result, "");
    }

    #[test]
    fn scrub_known_value_appears_multiple_times() {
        let text = "secret123 was used. Also secret123 appeared again.";
        let result = scrub(text, &["secret123"]);
        assert!(!result.contains("secret123"));
        // Should have two [REDACTED] occurrences
        assert_eq!(result.matches("[REDACTED]").count(), 2);
    }
}
