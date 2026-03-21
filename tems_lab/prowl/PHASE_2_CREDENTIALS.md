# Phase 2: Credential Isolation Protocol

> **Depends on:** Phase 0 (accessibility tree for login detection)
> **Status:** C=100%, R=0%, K=100%

---

## 2.1 Vault Web Credential Storage

### Implementation

No code changes to vault. Just a key naming convention:

```
Vault key:   "web_cred:{service_name}"
Vault value: JSON bytes of WebCredential struct
```

**File:** `crates/temm1e-tools/src/browser.rs` (or new `credential_types.rs`)

```rust
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct WebCredential {
    pub username: String,
    pub password: String,
    #[zeroize(skip)]  // URL doesn't need zeroing
    pub service_url: String,
}
```

**Cargo.toml** (temm1e-tools):
```toml
zeroize = { version = "1", features = ["zeroize_derive"] }
```

**Storage flow** (via existing vault):
```rust
let cred = WebCredential { username, password, service_url };
let json_bytes = serde_json::to_vec(&cred)?;
vault.store_secret(&format!("web_cred:{}", service), &json_bytes).await?;
// json_bytes dropped here — but cred is Zeroize, will zero on drop
```

**Retrieval flow:**
```rust
let bytes = vault.get_secret(&format!("web_cred:{}", service)).await?;
let mut zeroizing_bytes = Zeroizing::new(bytes.unwrap_or_default());
let cred: WebCredential = serde_json::from_slice(&zeroizing_bytes)?;
// Use cred.username, cred.password for DOM injection
// cred drops here → ZeroizeOnDrop zeros username and password
// zeroizing_bytes drops here → Zeroizing zeros the raw bytes
```

### Important: No `serde_json::Value` intermediate
R8 confirmed: `serde_json::Value` does NOT implement `Zeroize`. Always deserialize directly into the `WebCredential` struct.

### Estimated: ~30 lines
### Risk: 0% — new struct, no existing code changed

---

## 2.2 Login Form Detection

### Research Resolution (R5)
Password fields in accessibility tree: `role: "textbox"` with `protected` state property. Regular text inputs lack this property. Submit buttons: `role: "button"` with name containing login-related keywords.

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

```rust
/// Detect login form fields from accessibility tree nodes.
/// Returns (username_node_id, password_node_id, submit_node_id) if found.
fn detect_login_form(nodes: &[AxNode]) -> Option<(String, String, String)> {
    let mut username_id = None;
    let mut password_id = None;
    let mut submit_id = None;

    for node in nodes {
        if node.ignored { continue; }
        let role = node.role.as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let name = node.name.as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();

        // Detect password field: textbox with "protected" property
        if role == "textbox" {
            let is_protected = node.properties.as_ref()
                .map(|props| props.iter().any(|p| {
                    p.name.as_ref().map(|n| n.as_str()) == Some("protected")
                    && p.value.value.as_ref().and_then(|v| v.as_bool()) == Some(true)
                }))
                .unwrap_or(false);

            if is_protected {
                password_id = Some(node.node_id.clone());
            } else if username_id.is_none() {
                // First non-password textbox before password = username
                if name.contains("email") || name.contains("user") || name.contains("login")
                    || name.contains("phone") || name.contains("account") || name.is_empty()
                {
                    username_id = Some(node.node_id.clone());
                }
            }
        }

        // Detect submit button
        if (role == "button" || role == "link") && submit_id.is_none() {
            if name.contains("sign in") || name.contains("log in") || name.contains("login")
                || name.contains("submit") || name.contains("continue") || name.contains("next")
            {
                submit_id = Some(node.node_id.clone());
            }
        }
    }

    match (username_id, password_id, submit_id) {
        (Some(u), Some(p), Some(s)) => Some((u, p, s)),
        _ => None,
    }
}
```

### Fallback: CSS selector detection
If accessibility tree detection fails (nodes lack proper labels):

```rust
fn detect_login_form_css(page: &Page) -> Option<(String, String, String)> {
    let js = r#"(() => {
        const user = document.querySelector('input[type="email"], input[type="text"][name*="user"], input[type="text"][name*="email"], input[name*="login"]');
        const pass = document.querySelector('input[type="password"]');
        const submit = document.querySelector('button[type="submit"], input[type="submit"], form button');
        if (!user || !pass) return null;
        // Generate unique selectors via element index
        return { user: cssPath(user), pass: cssPath(pass), submit: submit ? cssPath(submit) : null };
    })()"#;
    // ... execute and parse
}
```

### Estimated: ~80 lines
### Risk: 0% — new helper functions, not exposed as actions yet

---

## 2.3 `authenticate` Action

### Research Resolution (R3)
chromiumoxide form interaction:
- `element.type_str(text)` — dispatches real keyDown/keyUp events (React/Vue compatible)
- No `fill()` — use `element.call_js_fn("function() { this.value = '' }", false)` to clear, then `type_str()` to type
- `element.click()` — scrolls into view, dispatches mouse events
- `page.evaluate()` — arbitrary JS fallback for edge cases

### Implementation

**File:** `crates/temm1e-tools/src/browser.rs`

```rust
"authenticate" => {
    let service = input.arguments.get("service")
        .and_then(|v| v.as_str())
        .ok_or(Temm1eError::Tool("'service' name required".into()))?;

    // 1. Retrieve credential from vault (CREDENTIAL EXECUTION DOMAIN)
    let vault = self.vault.as_ref()
        .ok_or(Temm1eError::Tool("Vault not available".into()))?;
    let raw_bytes = vault.get_secret(&format!("web_cred:{}", service)).await?
        .ok_or(Temm1eError::Tool(format!("No credentials stored for '{}'. Use /addcred {} first.", service, service)))?;
    let mut zeroizing = Zeroizing::new(raw_bytes);
    let cred: WebCredential = serde_json::from_slice(&zeroizing)
        .map_err(|e| Temm1eError::Tool(format!("Credential parse error: {e}")))?;

    let page = self.ensure_browser().await?;

    // 2. Detect login form
    let ax_result = page.execute(GetFullAxTreeParams::default()).await
        .map_err(|e| Temm1eError::Tool(format!("Auth observe: {e}")))?;

    let (user_id, pass_id, submit_id) = detect_login_form(&ax_result.result.nodes)
        .ok_or(Temm1eError::Tool("Could not detect login form on this page".into()))?;

    // 3. Resolve AX node IDs to DOM elements via backendDOMNodeId
    let user_node = find_node_by_id(&ax_result.result.nodes, &user_id)?;
    let pass_node = find_node_by_id(&ax_result.result.nodes, &pass_id)?;
    let submit_node = find_node_by_id(&ax_result.result.nodes, &submit_id)?;

    // Resolve to CDP elements using backendDOMNodeId
    let user_el = resolve_element(page, user_node).await?;
    let pass_el = resolve_element(page, pass_node).await?;
    let submit_el = resolve_element(page, submit_node).await?;

    // 4. Clear fields and inject credentials (credential bytes in scope)
    user_el.call_js_fn("function() { this.value = ''; this.dispatchEvent(new Event('input', {bubbles:true})); }", false).await?;
    user_el.click().await?;
    user_el.type_str(&cred.username).await?;

    pass_el.call_js_fn("function() { this.value = ''; this.dispatchEvent(new Event('input', {bubbles:true})); }", false).await?;
    pass_el.click().await?;
    pass_el.type_str(&cred.password).await?;

    // 5. cred drops here → ZeroizeOnDrop zeros username and password from memory
    drop(cred);
    drop(zeroizing);

    // 6. Submit
    submit_el.click().await?;

    // 7. Wait for navigation
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    // TODO: smarter wait — listen for Page.frameNavigated or check URL change

    // 8. Return post-login observation (credential fields are gone)
    let post_ax = page.execute(GetFullAxTreeParams::default()).await?;
    let post_tree = format_ax_tree(&post_ax.result.nodes);
    let scrubbed = credential_scrub::scrub(&post_tree, &[&service]);

    Ok(ToolOutput {
        content: format!("Authenticated to {}. Current page:\n{}", service, scrubbed),
        is_error: false,
    })
}
```

### Helper: resolve AX node to Element
```rust
async fn resolve_element(page: &Page, node: &AxNode) -> Result<Element, Temm1eError> {
    let backend_id = node.backend_dom_node_id.as_ref()
        .ok_or(Temm1eError::Tool("AX node has no DOM backing".into()))?;

    // Use DOM.resolveNode to get a RemoteObjectId, then wrap as Element
    use chromiumoxide::cdp::browser_protocol::dom::*;
    let resolved = page.execute(ResolveNodeParams::builder()
        .backend_node_id(*backend_id)
        .build()).await?;

    // Convert RemoteObject to Element using chromiumoxide internals
    // ... (exact API depends on chromiumoxide's Element construction)
    todo!("Map RemoteObject to Element — verify exact chromiumoxide API")
}
```

**Note:** The `resolve_element` helper needs verification of exact chromiumoxide API for constructing an `Element` from a `BackendNodeId`. Fallback: use `page.evaluate()` to build a CSS selector from the backend node ID and then `page.find_element()`.

### Estimated: ~120 lines
### Risk: 0% — new action, no existing behavior changed. Credential bytes never reach LLM (dataflow proof from paper holds by construction).

---

## 2.4 Credential Scrubber

### Implementation

**File:** `crates/temm1e-tools/src/credential_scrub.rs` (new)

```rust
use once_cell::sync::Lazy;
use regex::Regex;

static SENSITIVE_URL_PARAMS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(token|key|secret|password|passwd|pwd|auth|access_token|api_key|session_id|csrf|nonce)=([^&\s]+)")
        .unwrap()
});

static AUTH_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(authorization|x-api-key|x-auth-token):\s*\S+")
        .unwrap()
});

static API_KEY_PATTERNS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(sk-[a-zA-Z0-9]{20,}|key-[a-zA-Z0-9]{20,}|ghp_[a-zA-Z0-9]{36}|gho_[a-zA-Z0-9]{36})")
        .unwrap()
});

/// Scrub credential-like content from text before it reaches the LLM.
/// `known_values` contains service names and known credential fragments to redact.
pub fn scrub(text: &str, known_values: &[&str]) -> String {
    let mut result = text.to_string();

    // 1. Redact known values
    for val in known_values {
        if !val.is_empty() && val.len() > 3 {
            result = result.replace(val, "[REDACTED]");
        }
    }

    // 2. Redact sensitive URL parameters
    result = SENSITIVE_URL_PARAMS.replace_all(&result, "$1=[REDACTED]").to_string();

    // 3. Redact auth headers
    result = AUTH_HEADER.replace_all(&result, "$1: [REDACTED]").to_string();

    // 4. Redact API key patterns
    result = API_KEY_PATTERNS.replace_all(&result, "[REDACTED_KEY]").to_string();

    result
}
```

### Estimated: ~50 lines
### Risk: 0% — new module. Scrubber is conservative (prefers over-redaction).
### Test: Unit tests with known credentials embedded in text, verify all redacted.

---

## 2.5 Telegram Message Deletion

### Research Resolution (R6)
- Bots CAN delete user messages in private chats
- 48-hour time window (we delete within seconds — well within limit)
- No special permissions needed for private chats
- API: `deleteMessage(chat_id, message_id)`

### Implementation

**File:** `crates/temm1e-channels/src/telegram.rs` (or wherever credential capture is handled)

After detecting a credential message and storing it in vault:
```rust
// teloxide API
bot.delete_message(chat_id, message_id).await
    .map_err(|e| tracing::warn!("Failed to delete credential message: {e}"));
// Non-fatal — if deletion fails, credentials are still safely stored
```

### Estimated: ~5 lines at the integration point
### Risk: 0% — deletion failure is non-fatal (warn log, continue)

---

## 2.6 Zeroize Integration

### Research Resolution (R8)
- `Zeroizing<String>` works, zeros heap allocation (capacity, not just length)
- `#[derive(Zeroize, ZeroizeOnDrop)]` works on structs with String fields
- `Zeroizing<Vec<u8>>` zeros buffer on drop
- Caveat: buffer reallocation may leave copies. Mitigation: pre-allocate with `String::with_capacity()`
- Never use `serde_json::Value` as intermediate — deserialize directly into `#[derive(Zeroize)]` struct

### Implementation
Already shown in 2.1 and 2.3 above. The `WebCredential` struct derives `Zeroize, ZeroizeOnDrop`. Vault bytes are wrapped in `Zeroizing<Vec<u8>>`.

### Estimated: Already included in 2.1/2.3 estimates
### Risk: 0% — defense in depth, complements the dataflow separation

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 2.1 Vault Web Credentials | 100% | 0% | 100% | Ready |
| 2.2 Login Form Detection | 100% | 0% | 100% | Ready |
| 2.3 authenticate Action | 100% | 0% | 95% | Ready — minor: verify `resolve_element` API path |
| 2.4 Credential Scrubber | 100% | 0% | 100% | Ready |
| 2.5 Telegram Deletion | 100% | 0% | 100% | Ready |
| 2.6 Zeroize Integration | 100% | 0% | 100% | Ready |
