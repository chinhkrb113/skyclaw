//! Layered observation architecture for browser automation.
//!
//! Provides deterministic, O(1) tier selection for the `observe` action.
//! Examines accessibility tree metadata to decide how much information the
//! agent needs: tree only (Tier 1), tree + DOM as Markdown (Tier 2), or
//! tree + screenshot for visual analysis (Tier 3).

/// Observation tier — determines how much page data the agent receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationTier {
    /// Tier 1: Accessibility tree only. ~100-500 tokens.
    Tree,
    /// Tier 2: Tree + targeted DOM subtree converted to Markdown. ~500-2000 tokens.
    TreeWithDom { selector: String },
    /// Tier 3: Tree + element or viewport screenshot. ~2000-4000 tokens (image).
    TreeWithScreenshot { selector: Option<String> },
}

/// Metadata extracted from a formatted accessibility tree.
/// Used by `select_tier()` to determine the observation tier.
#[derive(Debug, Clone, Default)]
pub struct TreeMetadata {
    /// Number of interactive elements (button, link, textbox, etc.).
    pub total_interactive: usize,
    /// Interactive elements whose name is empty (role present but unlabeled).
    pub unlabeled_interactive: usize,
    /// Whether the tree contains a table role.
    pub has_table: bool,
    /// Whether the tree contains a form role.
    pub has_form: bool,
    /// Whether the tree contains an img role (potentially semantic images).
    pub has_images_with_semantic_meaning: bool,
    /// Whether the page has a QR code detected via heuristic JS scan.
    /// When true, observation should escalate to Tier 3 (screenshot) so the
    /// user can see and scan the QR code.
    pub has_qr_code: bool,
}

/// Parse a formatted accessibility tree text and extract metadata counters.
///
/// The input is the output of `format_ax_tree()` — a numbered, indented text
/// representation. This function counts patterns in O(n) where n = tree lines.
pub fn analyze_tree(tree_text: &str) -> TreeMetadata {
    let mut meta = TreeMetadata::default();
    for line in tree_text.lines() {
        if line.contains("button") || line.contains("link") || line.contains("textbox") {
            meta.total_interactive += 1;
            // Unlabeled: has a role but name is empty (no quoted string after role)
            if line.contains("\"\"") || !line.contains('"') {
                meta.unlabeled_interactive += 1;
            }
        }
        if line.contains("table") {
            meta.has_table = true;
        }
        if line.contains("form") {
            meta.has_form = true;
        }
        if line.contains("img") {
            meta.has_images_with_semantic_meaning = true;
        }
    }
    meta
}

/// Deterministic tier selection based on tree metadata and context hints.
///
/// # Decision logic
///
/// - **Tier 3** (screenshot): previous action failed, QR code detected on
///   the page, or hint mentions visual/captcha/image/layout concerns.
/// - **Tier 2** (tree + DOM Markdown): page has tables, forms, or >33%
///   unlabeled interactive elements that benefit from DOM detail.
/// - **Tier 1** (tree only): default when the accessibility tree is
///   sufficient for the agent to act.
pub fn select_tier(
    meta: &TreeMetadata,
    action_hint: Option<&str>,
    previous_action_failed: bool,
) -> ObservationTier {
    // Tier 3: Visual verification needed
    if previous_action_failed {
        return ObservationTier::TreeWithScreenshot { selector: None };
    }
    // QR code detected — must escalate to screenshot so user can scan it
    if meta.has_qr_code {
        return ObservationTier::TreeWithScreenshot { selector: None };
    }
    if let Some(hint) = action_hint {
        let h = hint.to_lowercase();
        if h.contains("captcha")
            || h.contains("image")
            || h.contains("visual")
            || h.contains("layout")
        {
            return ObservationTier::TreeWithScreenshot { selector: None };
        }
    }

    // Tier 2: DOM detail needed
    if meta.has_table {
        return ObservationTier::TreeWithDom {
            selector: "table".into(),
        };
    }
    if meta.total_interactive > 0 && meta.unlabeled_interactive > meta.total_interactive / 3 {
        // >33% of interactive elements have no name — DOM will help identify them
        return ObservationTier::TreeWithDom {
            selector: "body".into(),
        };
    }
    if meta.has_form {
        return ObservationTier::TreeWithDom {
            selector: "form".into(),
        };
    }

    // Tier 1: Tree is sufficient (default)
    ObservationTier::Tree
}

/// Truncate a string to at most `max_chars` characters at a safe UTF-8
/// boundary. Uses `char_indices()` to avoid slicing inside multi-byte chars.
pub fn truncate_safe(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    // Find the byte offset of the char boundary at or before max_chars bytes
    let boundary = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= max_chars)
        .last()
        .unwrap_or(0);
    &s[..boundary]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── analyze_tree tests ───────────────────────────────────────────

    #[test]
    fn analyze_empty_tree() {
        let meta = analyze_tree("");
        assert_eq!(meta.total_interactive, 0);
        assert_eq!(meta.unlabeled_interactive, 0);
        assert!(!meta.has_table);
        assert!(!meta.has_form);
        assert!(!meta.has_images_with_semantic_meaning);
    }

    #[test]
    fn analyze_tree_counts_interactive_elements() {
        let tree = r#"[1] button "Submit"
[2] link "Home"
[3] textbox "Email"
[4] heading "Welcome""#;
        let meta = analyze_tree(tree);
        assert_eq!(meta.total_interactive, 3);
        assert_eq!(meta.unlabeled_interactive, 0);
    }

    #[test]
    fn analyze_tree_counts_unlabeled_interactive() {
        let tree = r#"[1] button "Submit"
[2] link
[3] textbox
[4] button """#;
        let meta = analyze_tree(tree);
        assert_eq!(meta.total_interactive, 4);
        // [2] link has no quotes, [3] textbox has no quotes, [4] button has ""
        assert_eq!(meta.unlabeled_interactive, 3);
    }

    #[test]
    fn analyze_tree_detects_table() {
        let tree = "[1] table \"Data\"\n  [2] row\n    [3] cell \"A\"";
        let meta = analyze_tree(tree);
        assert!(meta.has_table);
    }

    #[test]
    fn analyze_tree_detects_form() {
        let tree = "[1] form \"Login\"\n  [2] textbox \"Username\"";
        let meta = analyze_tree(tree);
        assert!(meta.has_form);
        assert_eq!(meta.total_interactive, 1); // textbox
    }

    #[test]
    fn analyze_tree_detects_images() {
        let tree = "[1] img \"Logo\"\n[2] button \"Submit\"";
        let meta = analyze_tree(tree);
        assert!(meta.has_images_with_semantic_meaning);
    }

    #[test]
    fn analyze_tree_no_false_positives() {
        let tree = "[1] heading \"Welcome\"\n[2] navigation \"Main\"";
        let meta = analyze_tree(tree);
        assert_eq!(meta.total_interactive, 0);
        assert!(!meta.has_table);
        assert!(!meta.has_form);
        assert!(!meta.has_images_with_semantic_meaning);
    }

    // ── select_tier tests ────────────────────────────────────────────

    #[test]
    fn tier1_for_simple_tree() {
        let meta = TreeMetadata {
            total_interactive: 5,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::Tree);
    }

    #[test]
    fn tier1_when_all_labeled() {
        let meta = TreeMetadata {
            total_interactive: 10,
            unlabeled_interactive: 1, // 10% < 33%
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::Tree);
    }

    #[test]
    fn tier2_for_table() {
        let meta = TreeMetadata {
            total_interactive: 2,
            unlabeled_interactive: 0,
            has_table: true,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(
            tier,
            ObservationTier::TreeWithDom {
                selector: "table".into()
            }
        );
    }

    #[test]
    fn tier2_for_form() {
        let meta = TreeMetadata {
            total_interactive: 3,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: true,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(
            tier,
            ObservationTier::TreeWithDom {
                selector: "form".into()
            }
        );
    }

    #[test]
    fn tier2_for_many_unlabeled() {
        let meta = TreeMetadata {
            total_interactive: 6,
            unlabeled_interactive: 3, // 50% > 33%
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(
            tier,
            ObservationTier::TreeWithDom {
                selector: "body".into()
            }
        );
    }

    #[test]
    fn tier2_table_takes_priority_over_form() {
        let meta = TreeMetadata {
            total_interactive: 2,
            unlabeled_interactive: 0,
            has_table: true,
            has_form: true,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(
            tier,
            ObservationTier::TreeWithDom {
                selector: "table".into()
            }
        );
    }

    #[test]
    fn tier3_on_previous_action_failure() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, None, true);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_on_captcha_hint() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, Some("solve the captcha"), false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_on_image_hint() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, Some("check the image content"), false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_on_visual_hint() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, Some("verify the visual layout"), false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_on_layout_hint() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, Some("check layout"), false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_hint_case_insensitive() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, Some("CAPTCHA verification"), false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_failure_overrides_simple_tree() {
        // Even with a simple tree, failure triggers Tier 3
        let meta = TreeMetadata {
            total_interactive: 5,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, true);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier1_with_irrelevant_hint() {
        let meta = TreeMetadata {
            total_interactive: 3,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, Some("click the submit button"), false);
        assert_eq!(tier, ObservationTier::Tree);
    }

    #[test]
    fn tier1_zero_interactive_no_flags() {
        let meta = TreeMetadata::default();
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::Tree);
    }

    #[test]
    fn unlabeled_threshold_boundary() {
        // Exactly at 33% boundary: 3/9 = 33.3% > 9/3 = 3, so 3 > 3 is false
        let meta = TreeMetadata {
            total_interactive: 9,
            unlabeled_interactive: 3, // 3 > 9/3 = 3 -> false (not strictly greater)
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::Tree);

        // Just above: 4/9 > 3 -> true
        let meta2 = TreeMetadata {
            total_interactive: 9,
            unlabeled_interactive: 4,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier2 = select_tier(&meta2, None, false);
        assert_eq!(
            tier2,
            ObservationTier::TreeWithDom {
                selector: "body".into()
            }
        );
    }

    // ── truncate_safe tests ──────────────────────────────────────────

    #[test]
    fn truncate_safe_short_string() {
        let s = "hello";
        assert_eq!(truncate_safe(s, 100), "hello");
    }

    #[test]
    fn truncate_safe_exact_length() {
        let s = "hello";
        assert_eq!(truncate_safe(s, 5), "hello");
    }

    #[test]
    fn truncate_safe_truncates_ascii() {
        let s = "hello world";
        assert_eq!(truncate_safe(s, 5), "hello");
    }

    #[test]
    fn truncate_safe_respects_utf8_boundary() {
        // Vietnamese char with multi-byte encoding
        let s = "hello\u{1EBD}world"; // e with tilde = 3 bytes
        let result = truncate_safe(s, 6);
        // Should not panic, should stop before the multi-byte char if it doesn't fit
        assert!(result.len() <= 6);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn truncate_safe_empty_string() {
        assert_eq!(truncate_safe("", 10), "");
    }

    #[test]
    fn truncate_safe_zero_limit() {
        assert_eq!(truncate_safe("hello", 0), "");
    }

    // ── QR code detection tier escalation tests ─────────────────────

    #[test]
    fn tier3_on_qr_code_detected() {
        let meta = TreeMetadata {
            total_interactive: 3,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: true,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_qr_overrides_form() {
        // Even with a form, QR detection should escalate to Tier 3
        let meta = TreeMetadata {
            total_interactive: 5,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: true,
            has_images_with_semantic_meaning: false,
            has_qr_code: true,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn tier3_qr_overrides_table() {
        // Even with a table, QR detection should escalate to Tier 3
        let meta = TreeMetadata {
            total_interactive: 2,
            unlabeled_interactive: 0,
            has_table: true,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: true,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }

    #[test]
    fn no_qr_does_not_escalate() {
        // Explicitly false should not affect tier selection
        let meta = TreeMetadata {
            total_interactive: 3,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: false,
        };
        let tier = select_tier(&meta, None, false);
        assert_eq!(tier, ObservationTier::Tree);
    }

    #[test]
    fn qr_default_is_false() {
        let meta = TreeMetadata::default();
        assert!(!meta.has_qr_code);
    }

    #[test]
    fn tier3_failure_takes_priority_over_qr() {
        // Both failure and QR are true — both lead to Tier 3, failure checked first
        let meta = TreeMetadata {
            total_interactive: 3,
            unlabeled_interactive: 0,
            has_table: false,
            has_form: false,
            has_images_with_semantic_meaning: false,
            has_qr_code: true,
        };
        let tier = select_tier(&meta, None, true);
        assert_eq!(tier, ObservationTier::TreeWithScreenshot { selector: None });
    }
}
