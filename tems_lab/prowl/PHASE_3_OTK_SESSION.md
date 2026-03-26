# Phase 3: OTK Session Capture

> **Depends on:** Phase 2 (vault credential storage)
> **Status:** C=100%, R=0%, K=100%

---

## Architecture Decision: Screenshot-Based, Not Streaming

### Research Resolution (R9, R10)
CDP `Page.startScreencast` has fundamental issues:
- Frame rate limited by ack-per-frame protocol (50-200ms latency per frame)
- Known Chromium performance bug (issues.chromium.org/40934921)
- No existing Rust crate for browser streaming
- Building a WebSocket relay from scratch = high complexity, high risk

### Decision: Use periodic screenshot + click-map approach

Instead of streaming a live browser viewport, we:
1. Take a screenshot after each page state change
2. Overlay numbered markers on interactive elements (Set-of-Mark style)
3. Send screenshot to user via Telegram/Discord as a photo
4. User replies with a number or "done"
5. Agent executes the action, takes new screenshot, repeats

**This is dramatically simpler:**
- No WebSocket relay
- No frame management
- Works within existing channel capabilities (Telegram photo messages)
- No new infrastructure needed
- Latency is per-action (acceptable for login flow — user types password once)

---

## 3.1 OTK Session Capture Flow

### Revised protocol (screenshot-based)

```
1. Agent:   "I need to log into Amazon for you."
2. Agent:   Launches headless browser, navigates to amazon.com
3. Agent:   Takes screenshot, annotates with numbered interactive elements
4. Agent:   Sends annotated screenshot to user via Telegram:
            "Tap a number to interact, or type text to enter it.
             Type 'done' when you're logged in."
            [annotated screenshot showing: [1] Email field [2] Continue button ...]
5. User:    "1" (selects email field)
6. Agent:   Clicks element [1], takes new screenshot
7. User:    "user@email.com" (types email)
8. Agent:   Types text into focused field, takes new screenshot
9. User:    "2" (clicks Continue)
10. Agent:  Clicks element [2], takes new screenshot (password page)
11. User:   Types password
12. Agent:  Types into password field (using Zeroizing — zeroed immediately after)
13. User:   "done"
14. Agent:  Captures session state → encrypts → stores in vault
            Destroys browser session
15. Agent:  "Got it, I'm logged into Amazon now."
```

### Security properties preserved
- **Credential non-transit:** User types credentials in the chat (Telegram E2E encrypted to bot). Agent injects into browser via `type_str()`. LLM never sees the text — the interactive session bypasses the agent loop entirely.
- **Non-replayability:** OTK consumed on session capture completion
- **Encryption at rest:** Session stored via vault (ChaCha20-Poly1305)
- **User revocability:** `/revoke {service}` deletes from vault

### Key insight: During OTK session, the LLM is NOT involved
The interaction is direct: user → channel → browser control code → browser. No LLM call. The credential text flows through the channel handler, into `element.type_str()`, and is zeroed. The LLM only knows "OTK session in progress for Amazon" and receives the post-login accessibility tree when the user says "done."

---

## 3.2 Interactive Browser Session Handler

**File:** `crates/temm1e-tools/src/browser_session.rs` (new)

```rust
use chromiumoxide::cdp::browser_protocol::accessibility::*;
use zeroize::Zeroizing;

pub struct InteractiveBrowseSession {
    page: Page,
    session_id: String,
    service: String,
    element_map: HashMap<usize, AxNodeId>,  // [1] → node_id
}

impl InteractiveBrowseSession {
    pub async fn new(browser: &Browser, service: &str, url: &str) -> Result<Self, Temm1eError> {
        let page = browser.new_page(url).await?;
        tokio::time::sleep(Duration::from_secs(2)).await;  // Wait for page load
        Ok(Self {
            page,
            session_id: uuid::Uuid::new_v4().to_string(),
            service: service.to_string(),
            element_map: HashMap::new(),
        })
    }

    /// Take annotated screenshot. Returns (png_bytes, text_description).
    pub async fn capture_annotated(&mut self) -> Result<(Vec<u8>, String), Temm1eError> {
        // 1. Get accessibility tree
        let ax = self.page.execute(GetFullAxTreeParams::default()).await?;

        // 2. Build element map and annotation overlay
        self.element_map.clear();
        let mut description = String::new();
        let mut index = 1;

        for node in &ax.result.nodes {
            if node.ignored { continue; }
            let role = get_role(node);
            let name = get_name(node);

            if is_interactive(role) {
                self.element_map.insert(index, node.node_id.clone());
                writeln!(&mut description, "[{}] {} \"{}\"", index, role, name)?;

                // Inject visual label via JS overlay
                if let Some(backend_id) = &node.backend_dom_node_id {
                    let js = format!(
                        r#"(() => {{
                            const node = document.querySelector('[data-prowl-id="{}"]');
                            // ... inject numbered overlay label at element position
                        }})()"#,
                        index
                    );
                    // Simpler approach: use CDP's DOM.getBoxModel for coordinates,
                    // then draw labels on the screenshot image in Rust
                }
                index += 1;
            }
        }

        // 3. Take screenshot
        let png = self.page.screenshot(ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .build()).await?;

        // 4. Annotate screenshot with numbered labels (draw on image)
        let annotated = annotate_screenshot(&png, &self.element_map, &ax.result.nodes)?;

        Ok((annotated, description))
    }

    /// Handle user input: number (click element) or text (type into focused element).
    pub async fn handle_input(&mut self, input: &str) -> Result<SessionAction, Temm1eError> {
        let trimmed = input.trim();

        if trimmed.eq_ignore_ascii_case("done") {
            return Ok(SessionAction::Done);
        }

        if let Ok(num) = trimmed.parse::<usize>() {
            // Click the numbered element
            if let Some(node_id) = self.element_map.get(&num) {
                let node = find_ax_node(&self.page, node_id).await?;
                let element = resolve_to_element(&self.page, &node).await?;
                element.click().await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                return Ok(SessionAction::Continue);
            }
            return Err(Temm1eError::Tool(format!("Element [{}] not found", num)));
        }

        // Text input: type into currently focused element
        // Wrap in Zeroizing in case it's a password
        let zeroizing_input = Zeroizing::new(trimmed.to_string());
        let js = format!(
            "document.activeElement?.tagName !== 'BODY' ? 'has_focus' : 'no_focus'"
        );
        let focus_check = self.page.evaluate(js).await?;

        if focus_check.into_value::<String>().unwrap_or_default() == "has_focus" {
            // Type into focused element via CDP keyboard events
            for ch in zeroizing_input.chars() {
                self.page.execute(
                    chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventParams::builder()
                        .r#type(DispatchKeyEventType::KeyDown)
                        .text(ch.to_string())
                        .build()
                ).await?;
                self.page.execute(
                    chromiumoxide::cdp::browser_protocol::input::DispatchKeyEventParams::builder()
                        .r#type(DispatchKeyEventType::KeyUp)
                        .build()
                ).await?;
            }
            // zeroizing_input drops here → zeros the typed text from memory
            return Ok(SessionAction::Continue);
        }

        Err(Temm1eError::Tool("No element is focused. Tap a number first.".into()))
    }

    /// Capture session state and encrypt to vault.
    pub async fn capture_session(&self, vault: &dyn Vault) -> Result<(), Temm1eError> {
        // Cookies via CDP
        use chromiumoxide::cdp::browser_protocol::network::*;
        let cookies = self.page.execute(GetCookiesParams::default()).await?;

        // localStorage via CDP DOMStorage
        use chromiumoxide::cdp::browser_protocol::dom_storage::*;
        let origin = self.page.url().await?.map(|u| {
            url::Url::parse(&u).ok().map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
        }).flatten().unwrap_or_default();

        let local_storage = if !origin.is_empty() {
            let storage_id = StorageId::builder()
                .security_origin(&origin)
                .is_local_storage(true)
                .build();
            self.page.execute(GetDomStorageItemsParams::new(storage_id)).await
                .map(|r| r.result.entries)
                .unwrap_or_default()
        } else {
            vec![]
        };

        let session_storage = if !origin.is_empty() {
            let storage_id = StorageId::builder()
                .security_origin(&origin)
                .is_local_storage(false)
                .build();
            self.page.execute(GetDomStorageItemsParams::new(storage_id)).await
                .map(|r| r.result.entries)
                .unwrap_or_default()
        } else {
            vec![]
        };

        // Build session state
        let state = SessionState {
            cookies: cookies.result.cookies,
            local_storage,
            session_storage,
            url: self.page.url().await?.unwrap_or_default(),
            captured_at: chrono::Utc::now().to_rfc3339(),
            service: self.service.clone(),
        };

        let json = serde_json::to_vec(&state)?;
        vault.store_secret(&format!("web_session:{}", self.service), &json).await?;

        Ok(())
    }
}

pub enum SessionAction {
    Continue,  // Take new screenshot, send to user
    Done,      // Capture session, close browser
}

#[derive(Serialize, Deserialize)]
pub struct SessionState {
    pub cookies: Vec<Cookie>,
    pub local_storage: Vec<Vec<String>>,   // key-value pairs
    pub session_storage: Vec<Vec<String>>,
    pub url: String,
    pub captured_at: String,
    pub service: String,
}
```

### Screenshot Annotation

For annotating screenshots with numbered labels, two approaches:

**Option A: JS overlay (simpler)**
Before taking the screenshot, inject JS that adds numbered div labels at each interactive element's position:
```javascript
// Get bounding rect for each element, create floating label div
const label = document.createElement('div');
label.style = 'position:fixed; background:red; color:white; font-size:14px; ...';
label.textContent = '1';
document.body.appendChild(label);
```
Then screenshot. Then remove overlays.

**Option B: Image processing in Rust**
Use the `image` crate to draw numbered rectangles on the PNG after capture. More reliable (doesn't affect page layout) but requires image manipulation dependency.

**Recommendation: Option A** — simpler, no new dependencies, and the overlay is removed immediately after screenshot.

---

## 3.3 Integration in main.rs

**File:** `src/main.rs`

When user triggers OTK session (via `authenticate` action returning "needs OTK"):

```rust
// In message handler, when agent returns OTK session request:
let mut session = InteractiveBrowseSession::new(&browser, service, url).await?;
let (screenshot, description) = session.capture_annotated().await?;

// Send screenshot to user
channel.send_photo(chat_id, screenshot, &format!(
    "Log into {}. Tap a number to interact, type text to enter it.\nType 'done' when logged in.\n\n{}",
    service, description
)).await?;

// Enter interactive loop (bypasses agent — direct user↔browser)
loop {
    let user_msg = wait_for_user_message(chat_id).await?;

    match session.handle_input(&user_msg.text).await? {
        SessionAction::Continue => {
            let (screenshot, description) = session.capture_annotated().await?;
            channel.send_photo(chat_id, screenshot, &description).await?;
        }
        SessionAction::Done => {
            session.capture_session(&vault).await?;
            channel.send_message(chat_id, &format!(
                "Got it, I'm now logged into {}. What would you like me to do?", service
            )).await?;
            break;
        }
    }
}
```

---

## 3.4 Session Restore

**File:** `crates/temm1e-tools/src/browser.rs`

New action `restore_session`:
```rust
"restore_session" => {
    let service = input.arguments["service"].as_str()?;
    let vault = self.vault.as_ref()?;

    let bytes = vault.get_secret(&format!("web_session:{}", service)).await?
        .ok_or(Temm1eError::Tool(format!("No session for '{}'", service)))?;
    let state: SessionState = serde_json::from_slice(&bytes)?;

    let page = self.ensure_browser().await?;

    // Restore cookies
    use chromiumoxide::cdp::browser_protocol::network::*;
    page.execute(SetCookiesParams::new(state.cookies)).await?;

    // Restore localStorage
    if !state.local_storage.is_empty() {
        let origin = url::Url::parse(&state.url).ok()
            .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")))
            .unwrap_or_default();
        let storage_id = StorageId::builder()
            .security_origin(&origin)
            .is_local_storage(true)
            .build();
        for entry in &state.local_storage {
            if entry.len() >= 2 {
                page.execute(SetDomStorageItemParams::new(
                    storage_id.clone(), &entry[0], &entry[1]
                )).await?;
            }
        }
    }

    // Navigate to the saved URL
    page.goto(&state.url).await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify session is alive
    let tree = get_accessibility_tree(&page).await?;
    let tree_text = format_ax_tree(&tree);
    let has_login = tree_text.to_lowercase().contains("sign in")
        || tree_text.to_lowercase().contains("log in");

    if has_login {
        Ok(ToolOutput {
            content: format!("Session for {} has expired. Need to re-authenticate.", service),
            is_error: true,
        })
    } else {
        Ok(ToolOutput {
            content: format!("Session restored for {}. Current page:\n{}", service, tree_text),
            is_error: false,
        })
    }
}
```

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 3.1 OTK Flow (screenshot-based) | 100% | 0% | 100% | Ready — simplified from streaming |
| 3.2 Interactive Session Handler | 100% | 0% | 100% | Ready |
| 3.3 main.rs Integration | 100% | 0% | 95% | Ready — minor: exact channel photo API |
| 3.4 Session Restore | 100% | 0% | 100% | Ready |
