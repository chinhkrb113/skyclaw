# Phase 0: Foundation Fixes

> **Prerequisite phase.** All other phases depend on this.
> **Status:** C=100%, R=0%, K=100%

---

## 0.1 MCP Image Gap

### Problem
MCP bridge returns screenshot data as JSON text, not `ToolOutputImage`. Vision pipeline can't process it.

### Research Resolution (R4)
MCP spec (2025-11-25) defines image content as:
```json
{ "type": "image", "data": "<base64>", "mimeType": "image/png" }
```
Playwright MCP server returns this exact format for screenshots.

### Implementation

**File:** `crates/temm1e-mcp/src/client.rs`

Current `call_tool()` extracts only `content[].text` entries and joins them. Change to preserve content block types:

```rust
// Current (lossy):
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

// New (preserves images):
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
    pub image: Option<McpImage>,  // NEW
}

pub struct McpImage {
    pub data: String,       // base64
    pub mime_type: String,  // e.g. "image/png"
}
```

In `call_tool()`, after parsing the response `content` array:
```rust
for block in content_array {
    match block["type"].as_str() {
        Some("text") => text_parts.push(block["text"].as_str().unwrap_or("")),
        Some("image") => {
            image = Some(McpImage {
                data: block["data"].as_str().unwrap_or("").to_string(),
                mime_type: block["mimeType"].as_str().unwrap_or("image/png").to_string(),
            });
        }
        _ => {} // ignore unknown types
    }
}
```

**File:** `crates/temm1e-mcp/src/bridge.rs`

Add `last_image` field to `McpBridgeTool`:
```rust
pub struct McpBridgeTool {
    // ... existing fields ...
    last_image: Arc<Mutex<Option<ToolOutputImage>>>,  // NEW
}
```

In `execute()`, after receiving `McpToolResult`:
```rust
if let Some(img) = result.image {
    *self.last_image.lock().unwrap() = Some(ToolOutputImage {
        media_type: img.mime_type,
        data: img.data,
    });
}
```

Implement `take_last_image()`:
```rust
fn take_last_image(&self) -> Option<ToolOutputImage> {
    self.last_image.lock().unwrap().take()
}
```

### Estimated: ~50 lines across 2 files
### Risk: 0% — additive only, no behavior change for existing MCP tools without images
### Test: Mock MCP server returning image content, verify `take_last_image()` returns it

---

## 0.2 Accessibility Tree Extraction

### Research Resolution (R1, R5)
chromiumoxide 0.7.0 fully exposes `Accessibility.getFullAXTree`:
- `GetFullAxTreeParams` → `GetFullAxTreeReturns { nodes: Vec<AxNode> }`
- `AxNode` has: `node_id`, `role`, `name`, `value`, `properties`, `parent_id`, `child_ids`, `ignored`
- Password fields: `role: "textbox"` with `protected` state property
- Iframes: appear as boundary nodes, NOT flattened (must query per-frame)

Token cost reality check (from measured data):
- Full verbose tree: 14,000-19,000 tokens (too large)
- Filtered interactive-only: 3,000-8,000 tokens (achievable)
- Aggressively filtered (buttons/links/inputs only): ~200-500 tokens (our Tier 1 target)

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

New action handler in `execute()` match:

```rust
"accessibility_tree" | "observe_tree" => {
    let page = self.ensure_browser().await?;

    // CDP call
    use chromiumoxide::cdp::browser_protocol::accessibility::*;
    let result = page.execute(GetFullAxTreeParams::default()).await
        .map_err(|e| Temm1eError::Tool(format!("Accessibility tree: {e}")))?;

    let formatted = format_ax_tree(&result.result.nodes);
    Ok(ToolOutput { content: formatted, is_error: false })
}
```

New function `format_ax_tree()`:

```rust
fn format_ax_tree(nodes: &[AxNode]) -> String {
    // Build parent→children map
    let children_map: HashMap<&str, Vec<&AxNode>> = /* ... */;

    // Track interactive/semantic roles we care about
    const INTERACTIVE_ROLES: &[&str] = &[
        "button", "link", "textbox", "combobox", "checkbox", "radio",
        "slider", "spinbutton", "switch", "tab", "menuitem", "option",
        "searchbox", "textarea",
    ];
    const SEMANTIC_ROLES: &[&str] = &[
        "heading", "navigation", "main", "form", "list", "listitem",
        "table", "row", "cell", "img", "alert", "dialog",
    ];

    let mut output = String::new();
    let mut index = 1;

    fn walk(node, depth, index, output) {
        if node.ignored { return; }

        let role = node.role.as_ref().map(|v| v.value.as_str()).unwrap_or("");
        let name = node.name.as_ref().map(|v| v.value.as_str()).unwrap_or("");

        // Skip generic containers (div, span equivalents)
        if !INTERACTIVE_ROLES.contains(&role) && !SEMANTIC_ROLES.contains(&role) {
            // Still recurse into children
            for child in children_of(node) { walk(child, depth, index, output); }
            return;
        }

        // Format: [index] role "name" state=value
        let indent = "  ".repeat(depth);
        write!(output, "{indent}[{index}] {role}");
        if !name.is_empty() { write!(output, " \"{name}\""); }

        // Add value for inputs
        if let Some(val) = &node.value {
            write!(output, " value=\"{}\"", val.value);
        }

        // Add key properties (focused, disabled, expanded, checked, level)
        if let Some(props) = &node.properties {
            for prop in props {
                match prop.name.as_str() {
                    "focused" | "disabled" | "expanded" | "checked" | "required" | "level" => {
                        write!(output, " {}={}", prop.name, prop.value.value);
                    }
                    _ => {}
                }
            }
        }

        writeln!(output);
        *index += 1;

        for child in children_of(node) { walk(child, depth + 1, index, output); }
    }

    walk(root_node, 0, &mut index, &mut output);
    output
}
```

### Key design decisions:
1. **Filter aggressively** — only interactive + semantic roles. Skip `generic`, `group`, `paragraph`, `StaticText`.
2. **Numbered indices** — stable references across turns (`[5]` = same element)
3. **Include value for inputs** — shows current form state
4. **Include key properties** — focused, disabled, expanded, checked, level (for headings)
5. **Recursive with depth indent** — preserves hierarchy

### Estimated: ~150 lines
### Risk: 0% — new action, existing actions unchanged
### Test: Navigate to a test page, extract tree, verify: (a) interactive elements found, (b) indices are stable, (c) output < 1000 tokens for a typical page

---

## 0.3 Element-Scoped Screenshots

### Research Resolution (R3)
chromiumoxide `Element` has `screenshot(format: CaptureScreenshotFormat) -> Result<Vec<u8>>`. This captures just the element's bounding box. Exactly what we need.

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

Extend the existing `screenshot` action:

```rust
"screenshot" => {
    let page = self.ensure_browser().await?;

    let png_data = if let Some(selector) = input.arguments.get("selector").and_then(|v| v.as_str()) {
        // Element-scoped screenshot
        let element = page.find_element(selector).await
            .map_err(|e| Temm1eError::Tool(format!("Element not found '{}': {e}", selector)))?;
        element.screenshot(CaptureScreenshotFormat::Png).await
            .map_err(|e| Temm1eError::Tool(format!("Element screenshot: {e}")))?
    } else {
        // Full viewport (existing behavior)
        page.screenshot(ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .build())
            .await
            .map_err(|e| Temm1eError::Tool(format!("Screenshot: {e}")))?
    };

    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_data);
    // ... existing last_image storage logic ...
}
```

### Estimated: ~20 lines (modify existing action)
### Risk: 0% — existing behavior preserved when `selector` is absent
### Test: Screenshot a specific element by selector, verify image contains only that element

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 0.1 MCP Image Gap | 100% | 0% | 100% | Ready |
| 0.2 Accessibility Tree | 100% | 0% | 100% | Ready |
| 0.3 Element Screenshots | 100% | 0% | 100% | Ready |
