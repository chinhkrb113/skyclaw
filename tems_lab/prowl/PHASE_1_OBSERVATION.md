# Phase 1: Layered Observation Architecture

> **Depends on:** Phase 0 (accessibility tree, element screenshots)
> **Status:** C=100%, R=0%, K=100%

---

## 1.1 Tier Selection Function

### Design

Deterministic, O(1) function. No LLM calls. Examines tree metadata to decide observation tier.

**File:** `crates/temm1e-tools/src/browser_observation.rs` (new)

```rust
pub enum ObservationTier {
    /// Tier 1: Accessibility tree only. ~100-500 tokens.
    Tree,
    /// Tier 2: Tree + targeted DOM subtree as Markdown. ~500-2000 tokens.
    TreeWithDom { selector: String },
    /// Tier 3: Tree + element or viewport screenshot. ~2000-4000 tokens.
    TreeWithScreenshot { selector: Option<String> },
}

pub struct TreeMetadata {
    pub total_interactive: usize,
    pub unlabeled_interactive: usize,  // role present but name empty
    pub has_table: bool,
    pub has_form: bool,
    pub has_images_with_semantic_meaning: bool,
}

pub fn analyze_tree(tree_text: &str) -> TreeMetadata {
    // Count lines matching patterns — O(n) where n = tree lines, NOT an LLM call
    let mut meta = TreeMetadata::default();
    for line in tree_text.lines() {
        if line.contains("button") || line.contains("link") || line.contains("textbox") {
            meta.total_interactive += 1;
            if line.contains("\"\"") || !line.contains('"') {
                meta.unlabeled_interactive += 1;
            }
        }
        if line.contains("table") { meta.has_table = true; }
        if line.contains("form") { meta.has_form = true; }
        if line.contains("img") { meta.has_images_with_semantic_meaning = true; }
    }
    meta
}

pub fn select_tier(
    meta: &TreeMetadata,
    action_hint: Option<&str>,
    previous_action_failed: bool,
) -> ObservationTier {
    // Tier 3: Visual verification needed
    if previous_action_failed {
        return ObservationTier::TreeWithScreenshot { selector: None };
    }
    if let Some(hint) = action_hint {
        let h = hint.to_lowercase();
        if h.contains("captcha") || h.contains("image") || h.contains("visual") || h.contains("layout") {
            return ObservationTier::TreeWithScreenshot { selector: None };
        }
    }

    // Tier 2: DOM detail needed
    if meta.has_table {
        return ObservationTier::TreeWithDom { selector: "table".into() };
    }
    if meta.unlabeled_interactive > meta.total_interactive / 3 {
        // >33% of interactive elements have no name — DOM will help
        return ObservationTier::TreeWithDom { selector: "body".into() };
    }
    if meta.has_form {
        return ObservationTier::TreeWithDom { selector: "form".into() };
    }

    // Tier 1: Tree is sufficient (default)
    ObservationTier::Tree
}
```

### Estimated: ~80 lines
### Risk: 0% — new module, no existing code touched
### Test: Unit tests with mock tree metadata asserting correct tier selection

---

## 1.2 Unified `observe` Action

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

New action in `execute()` match:

```rust
"observe" => {
    let page = self.ensure_browser().await?;
    let hint = input.arguments.get("hint").and_then(|v| v.as_str());
    let retry = input.arguments.get("retry").and_then(|v| v.as_bool()).unwrap_or(false);

    // Always get accessibility tree first
    use chromiumoxide::cdp::browser_protocol::accessibility::*;
    let ax_result = page.execute(GetFullAxTreeParams::default()).await
        .map_err(|e| Temm1eError::Tool(format!("Observe: {e}")))?;
    let tree_text = format_ax_tree(&ax_result.result.nodes);

    // Analyze and select tier
    let meta = browser_observation::analyze_tree(&tree_text);
    let tier = browser_observation::select_tier(&meta, hint, retry);

    match tier {
        ObservationTier::Tree => {
            Ok(ToolOutput { content: tree_text, is_error: false })
        }
        ObservationTier::TreeWithDom { selector } => {
            let js = format!(
                "(() => {{ const el = document.querySelector('{}'); return el ? el.outerHTML : 'not found'; }})()",
                selector.replace('\'', "\\'")
            );
            let dom_html = page.evaluate(js).await
                .map(|r| r.into_value::<String>().unwrap_or_default())
                .unwrap_or_default();
            let markdown = htmd::convert(&dom_html).unwrap_or(dom_html);

            // Truncate markdown to 4000 chars (safe boundary via char_indices)
            let md_truncated = truncate_safe(&markdown, 4000);

            Ok(ToolOutput {
                content: format!("{}\n\n--- DOM Detail ---\n{}", tree_text, md_truncated),
                is_error: false,
            })
        }
        ObservationTier::TreeWithScreenshot { selector } => {
            let png_data = if let Some(sel) = selector {
                let el = page.find_element(&sel).await?;
                el.screenshot(CaptureScreenshotFormat::Png).await?
            } else {
                page.screenshot(ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .build()).await?
            };
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
            *self.last_image.lock().unwrap() = Some(ToolOutputImage {
                media_type: "image/png".to_string(),
                data: b64,
            });
            Ok(ToolOutput { content: tree_text, is_error: false })
            // Vision pipeline picks up image via take_last_image()
        }
    }
}
```

### Tool declaration update

Add `observe` to the tool's `parameters_schema()`:

```json
{
    "type": "object",
    "properties": {
        "action": { "enum": [..., "observe"] },
        "hint": { "type": "string", "description": "Optional hint about what to look for (e.g., 'table', 'form', 'visual')" },
        "retry": { "type": "boolean", "description": "Set true if previous action failed — triggers visual verification" }
    }
}
```

### Estimated: ~60 lines
### Risk: 0% — new action, all existing actions unchanged

---

## 1.3 HTML-to-Markdown

### Research Resolution (R7)
`html2text` is inadequate for tables. Use `htmd` instead — Turndown.js-inspired, explicit table-to-Markdown pipe format.

### Implementation

**Cargo.toml** (temm1e-tools):
```toml
htmd = "0.1"  # HTML to Markdown
```

**Usage** (already shown in 1.2 above):
```rust
let markdown = htmd::convert(&html_string).unwrap_or(html_string);
```

`htmd` API: `htmd::convert(html: &str) -> Result<String>`. Single function call. Handles tables, headings, links, lists, emphasis, code blocks.

### Estimated: 1 line of code + 1 Cargo.toml dependency
### Risk: 0% — new dependency, no existing code changed
### Test: Feed known HTML (with table, headings, links), verify Markdown output

---

## 1.4 Incremental Observation

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

Add to `BrowserTool` struct:
```rust
last_tree_hash: Arc<Mutex<Option<u64>>>,
```

In `observe` action, after generating `tree_text`:
```rust
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

let mut hasher = DefaultHasher::new();
tree_text.hash(&mut hasher);
let current_hash = hasher.finish();

let mut last_hash = self.last_tree_hash.lock().unwrap();
if *last_hash == Some(current_hash) {
    return Ok(ToolOutput {
        content: "[Page unchanged since last observation]".to_string(),
        is_error: false,
    });
}
*last_hash = Some(current_hash);
// ... continue with tier selection ...
```

In `navigate` action, reset the hash:
```rust
*self.last_tree_hash.lock().unwrap() = None;
```

### Estimated: ~15 lines
### Risk: 0% — additive, only affects new `observe` action
### Test: Call observe twice without navigation, verify second returns "[unchanged]"

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 1.1 Tier Selection | 100% | 0% | 100% | Ready |
| 1.2 observe Action | 100% | 0% | 100% | Ready |
| 1.3 HTML-to-Markdown | 100% | 0% | 100% | Ready — use `htmd` crate |
| 1.4 Incremental Observation | 100% | 0% | 100% | Ready |
