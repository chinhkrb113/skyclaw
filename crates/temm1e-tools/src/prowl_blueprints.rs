//! Prowl Web Blueprints — pre-authored procedural blueprints for common web tasks.
//!
//! These are static blueprint documents (YAML frontmatter + Markdown body) that
//! get seeded into memory on first Prowl-enabled run. The agent's blueprint
//! system matches them via `semantic_tags` when the classifier detects a web task.

pub mod login_registry;

/// Pre-authored web blueprints for Tem Prowl.
///
/// Each entry is `(blueprint_id, blueprint_content)` where the content is a
/// YAML+Markdown document compatible with `parse_blueprint()` in `temm1e-agent`.
///
/// Seed these into memory during agent initialization when the browser tool is
/// enabled, using `MemoryEntryType::Blueprint`.
pub const WEB_BLUEPRINTS: &[(&str, &str)] = &[
    (
        "bp_prowl_search",
        include_str!("prowl_blueprints/web_search.md"),
    ),
    (
        "bp_prowl_login",
        include_str!("prowl_blueprints/web_login.md"),
    ),
    (
        "bp_prowl_extract",
        include_str!("prowl_blueprints/web_extract.md"),
    ),
    (
        "bp_prowl_compare",
        include_str!("prowl_blueprints/web_compare.md"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_blueprints_have_yaml_frontmatter() {
        for (id, content) in WEB_BLUEPRINTS {
            let trimmed = content.trim();
            assert!(
                trimmed.starts_with("---"),
                "Blueprint {id} missing YAML frontmatter opening"
            );
            let after_opening = trimmed[3..].trim_start_matches(['\r', '\n']);
            assert!(
                after_opening.contains("\n---"),
                "Blueprint {id} missing YAML frontmatter closing"
            );
        }
    }

    #[test]
    fn all_blueprints_contain_expected_id() {
        for (id, content) in WEB_BLUEPRINTS {
            assert!(
                content.contains(&format!("id: {id}")),
                "Blueprint {id} does not contain its expected id in frontmatter"
            );
        }
    }

    #[test]
    fn all_blueprints_have_phases() {
        for (id, content) in WEB_BLUEPRINTS {
            assert!(
                content.contains("## Phases"),
                "Blueprint {id} missing ## Phases section"
            );
        }
    }

    #[test]
    fn blueprint_count() {
        assert_eq!(WEB_BLUEPRINTS.len(), 4);
    }
}
