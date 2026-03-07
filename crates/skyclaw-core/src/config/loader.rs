use crate::types::config::SkyclawConfig;
use crate::types::error::SkyclawError;
use std::path::{Path, PathBuf};

/// Discover config file locations in priority order
fn config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // 1. System config
    paths.push(PathBuf::from("/etc/skyclaw/config.toml"));

    // 2. User config
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".skyclaw").join("config.toml"));
    }

    // 3. Workspace config
    paths.push(PathBuf::from("config.toml"));
    paths.push(PathBuf::from("skyclaw.toml"));

    paths
}

/// Load configuration from discovered config files, merging in order
pub fn load_config(explicit_path: Option<&Path>) -> Result<SkyclawConfig, SkyclawError> {
    let mut config_content = String::new();

    if let Some(path) = explicit_path {
        config_content = std::fs::read_to_string(path)
            .map_err(|e| SkyclawError::Config(format!("Failed to read {}: {}", path.display(), e)))?;
    } else {
        for path in config_paths() {
            if path.exists() {
                config_content = std::fs::read_to_string(&path)
                    .map_err(|e| SkyclawError::Config(format!("Failed to read {}: {}", path.display(), e)))?;
                break;
            }
        }
    }

    if config_content.is_empty() {
        return Ok(SkyclawConfig::default());
    }

    // Expand environment variables
    let expanded = super::env::expand_env_vars(&config_content);

    // Try TOML first (native format + ZeroClaw compat)
    if let Ok(config) = toml::from_str::<SkyclawConfig>(&expanded) {
        return Ok(config);
    }

    // Try YAML (OpenClaw compat)
    if let Ok(config) = serde_yaml::from_str::<SkyclawConfig>(&expanded) {
        return Ok(config);
    }

    Err(SkyclawError::Config(
        "Failed to parse config as TOML or YAML".to_string(),
    ))
}

impl Default for SkyclawConfig {
    fn default() -> Self {
        Self {
            skyclaw: Default::default(),
            gateway: Default::default(),
            provider: Default::default(),
            memory: Default::default(),
            vault: Default::default(),
            filestore: Default::default(),
            security: Default::default(),
            heartbeat: Default::default(),
            cron: Default::default(),
            channel: Default::default(),
            tools: Default::default(),
            tunnel: None,
            observability: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_default_config_no_file() {
        // When no config file exists, load_config should return defaults
        let config = load_config(None).unwrap();
        assert_eq!(config.gateway.host, "127.0.0.1");
        assert_eq!(config.gateway.port, 8080);
        assert_eq!(config.memory.backend, "sqlite");
        assert_eq!(config.vault.backend, "local-chacha20");
        assert_eq!(config.security.sandbox, "mandatory");
        assert!(config.channel.is_empty());
    }

    #[test]
    fn test_load_toml_config() {
        let toml_content = r#"
[gateway]
host = "0.0.0.0"
port = 9090
tls = true

[provider]
name = "anthropic"
api_key = "test-key-123"
model = "claude-sonnet-4-20250514"

[memory]
backend = "markdown"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.host, "0.0.0.0");
        assert_eq!(config.gateway.port, 9090);
        assert!(config.gateway.tls);
        assert_eq!(config.provider.name.as_deref(), Some("anthropic"));
        assert_eq!(config.provider.api_key.as_deref(), Some("test-key-123"));
        assert_eq!(config.memory.backend, "markdown");
    }

    #[test]
    fn test_load_yaml_config() {
        let yaml_content = r#"
gateway:
  host: "10.0.0.1"
  port: 3000
memory:
  backend: "sqlite"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.host, "10.0.0.1");
        assert_eq!(config.gateway.port, 3000);
        assert_eq!(config.memory.backend, "sqlite");
    }

    #[test]
    fn test_env_var_expansion_in_config() {
        std::env::set_var("SKYCLAW_TEST_API_KEY", "expanded-key-value");
        let toml_content = r#"
[provider]
name = "anthropic"
api_key = "${SKYCLAW_TEST_API_KEY}"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.provider.api_key.as_deref(), Some("expanded-key-value"));
        std::env::remove_var("SKYCLAW_TEST_API_KEY");
    }

    #[test]
    fn test_invalid_config_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "this is not valid TOML {{ or YAML").unwrap();

        let result = load_config(Some(tmp.path()));
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_config_file() {
        let result = load_config(Some(std::path::Path::new("/tmp/nonexistent_skyclaw_config_12345.toml")));
        assert!(result.is_err());
    }

    #[test]
    fn test_config_with_channels() {
        let toml_content = r#"
[channel.telegram]
enabled = true
token = "bot123"
allowlist = ["user1", "@user2"]
file_transfer = true
max_file_size = "50MB"

[channel.discord]
enabled = false
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.channel.len(), 2);
        let tg = &config.channel["telegram"];
        assert!(tg.enabled);
        assert_eq!(tg.token.as_deref(), Some("bot123"));
        assert_eq!(tg.allowlist, vec!["user1", "@user2"]);
        assert!(tg.file_transfer);

        let dc = &config.channel["discord"];
        assert!(!dc.enabled);
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[test]
    fn test_empty_config_file_returns_defaults() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        // Empty file should produce default config
        assert_eq!(config.gateway.host, "127.0.0.1");
        assert_eq!(config.gateway.port, 8080);
    }

    #[test]
    fn test_config_with_security_settings() {
        let toml_content = r#"
[security]
sandbox = "permissive"
file_scanning = false
skill_signing = "optional"
audit_log = false

[security.rate_limit]
requests_per_minute = 100
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.security.sandbox, "permissive");
        assert!(!config.security.file_scanning);
        assert_eq!(config.security.skill_signing, "optional");
        assert!(!config.security.audit_log);
        assert!(config.security.rate_limit.is_some());
        assert_eq!(config.security.rate_limit.unwrap().requests_per_minute, 100);
    }

    #[test]
    fn test_config_partial_overrides_keep_defaults() {
        let toml_content = r#"
[gateway]
port = 3000
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.gateway.port, 3000);
        // Host should still be default
        assert_eq!(config.gateway.host, "127.0.0.1");
        // Other sections should be defaults
        assert_eq!(config.memory.backend, "sqlite");
        assert_eq!(config.vault.backend, "local-chacha20");
    }

    #[test]
    fn test_env_var_expansion_missing_var() {
        let toml_content = r#"
[provider]
name = "anthropic"
api_key = "${NONEXISTENT_SKYCLAW_VAR_99999}"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        // Missing env var expands to empty string
        assert_eq!(config.provider.api_key.as_deref(), Some(""));
    }

    #[test]
    fn test_config_with_tunnel() {
        let toml_content = r#"
[tunnel]
provider = "cloudflare"
token = "cf-token-123"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert!(config.tunnel.is_some());
        let tunnel = config.tunnel.unwrap();
        assert_eq!(tunnel.provider, "cloudflare");
        assert_eq!(tunnel.token.as_deref(), Some("cf-token-123"));
    }

    #[test]
    fn test_config_observability() {
        let toml_content = r#"
[observability]
log_level = "debug"
otel_enabled = true
otel_endpoint = "http://localhost:4317"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let config = load_config(Some(tmp.path())).unwrap();
        assert_eq!(config.observability.log_level, "debug");
        assert!(config.observability.otel_enabled);
        assert_eq!(config.observability.otel_endpoint.as_deref(), Some("http://localhost:4317"));
    }

    #[test]
    #[ignore] // Performance test
    fn test_config_parsing_performance() {
        let toml_content = r#"
[gateway]
host = "0.0.0.0"
port = 8080

[provider]
name = "anthropic"
api_key = "sk-test"

[memory]
backend = "sqlite"

[security]
sandbox = "mandatory"
"#;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), toml_content).unwrap();

        let start = std::time::Instant::now();
        for _ in 0..100 {
            let _ = load_config(Some(tmp.path())).unwrap();
        }
        let elapsed = start.elapsed();
        let per_parse = elapsed / 100;
        assert!(per_parse.as_millis() < 10, "Config parse took {}ms, expected <10ms", per_parse.as_millis());
    }
}
