# Tem Prowl: Implementation Documentation

> **Branch:** `tem-browse`
> **Date:** 2026-03-19
> **Status:** Implementation plan — pre-development
> **Paper:** `tems_lab/TEM_PROWL_PAPER.md`

---

## 0. What Already Exists

Before building anything, inventory what TEMM1E already has:

| Component | Status | Location | Notes |
|-----------|--------|----------|-------|
| **BrowserTool** | Fully implemented (1855 LOC) | `crates/temm1e-tools/src/browser.rs` | CDP via chromiumoxide, stealth mode, session persistence, vision pipeline |
| **Vision pipeline** | Working | `crates/temm1e-agent/src/runtime.rs` | `take_last_image()` → `ContentPart::Image` injection into conversation |
| **MCP bridge** | Fully implemented | `crates/temm1e-mcp/` | Playwright & Puppeteer in self-extend registry, hot-reload, resilient |
| **Vault** | Fully implemented | `crates/temm1e-vault/` | ChaCha20-Poly1305, `vault://` URI scheme, zeroizing key material |
| **Hive (Many Tems)** | Fully implemented (2490 LOC, 71 tests) | `crates/temm1e-hive/` | Blackboard, pheromones, DAG, worker pool, Queen decomposition |
| **Blueprint system** | Fully implemented | `crates/temm1e-agent/src/blueprint.rs` | Phase DAG, category matching, procedural memory |
| **Tool executor** | Working | `crates/temm1e-agent/src/executor.rs` | Parallel execution, sandbox enforcement |
| **Stealth anti-detection** | Implemented in BrowserTool | `browser.rs` | navigator.webdriver, WebGL, plugins, permissions spoofing |
| **Session save/restore** | Implemented in BrowserTool | `browser.rs` | Cookie save/load to `~/.temm1e/sessions/{name}.json` via CDP |

**Key insight: Tem Prowl is NOT building a browser from scratch.** It's composing, extending, and formalizing existing pieces into the architecture described in the paper.

---

## 1. Implementation Phases

### Phase 0: Foundation Fixes (prerequisite)
### Phase 1: Layered Observation Architecture
### Phase 2: Credential Isolation Protocol
### Phase 3: OTK Session Capture
### Phase 4: Web Blueprints
### Phase 5: Swarm Browsing (Hive integration)
### Phase 6: Benchmark Suite (TBench)

Each phase is independently shippable and testable. No phase depends on a later phase.

---

## Phase 0: Foundation Fixes

### 0.1 MCP Image Gap

**Problem:** MCP tool results containing screenshots are returned as JSON-serialized data strings, not as `ToolOutputImage` structs. The vision pipeline (`take_last_image()`) cannot process them. This means Playwright MCP screenshot results don't get injected as images for the LLM.

**Fix location:** `crates/temm1e-mcp/src/bridge.rs`

**What to do:**
1. In `McpBridgeTool::execute()`, after receiving `McpToolResult`, scan the raw MCP response `content` array
2. Detect `content[].type == "image"` blocks (per MCP spec: `{ type: "image", data: "<base64>", mimeType: "image/png" }`)
3. Store the first image as `ToolOutputImage { media_type, data }` in an internal field
4. Implement `take_last_image()` on `McpBridgeTool` to return it

**Estimated change:** ~40 lines in `bridge.rs`, ~10 lines in `client.rs` to preserve raw content types.

**Test:** Add MCP server that returns image content, verify vision pipeline receives it.

### 0.2 Accessibility Tree Tool

**Problem:** The existing `BrowserTool` only offers: navigate, click, type, screenshot, get_text, get_html, evaluate, save/restore session. There is no accessibility tree extraction — the key observation tier from the paper.

**What to do:** Add an `accessibility_tree` action to the existing `BrowserTool`.

**Implementation via CDP:**
```rust
// In browser.rs, new action handler:
"accessibility_tree" => {
    let page = self.ensure_browser().await?;
    // CDP call: Accessibility.getFullAXTree
    let tree = page.execute(
        cdp::browser_protocol::accessibility::GetFullAXTreeParams::default()
    ).await?;
    // Format as numbered flat list (Tier 1 representation)
    let formatted = format_accessibility_tree(&tree.nodes);
    Ok(ToolOutput { content: formatted, is_error: false })
}
```

**Formatting:** Convert the CDP AXTree nodes into the numbered flat representation from the paper:
```
[1] heading "Search Results" level=1
[2] textbox "Search" value="wireless headphones"
[3] button "Search"
[4] list "Results"
  [5] link "Sony WH-1000XM5 — $348.00"
```

Each node: `[index] role "name" state=value`. Indentation reflects hierarchy. Only interactive and semantic nodes (skip generic containers, decorative elements).

**Estimated change:** ~150 lines in `browser.rs` (action handler + tree formatter).

**Test:** Navigate to a known page, extract accessibility tree, verify elements are identified correctly.

### 0.3 Element-Scoped Screenshots

**Problem:** Current `screenshot` action captures the full viewport. The paper's Tier 3 calls for element-scoped screenshots to reduce token cost.

**What to do:** Add optional `selector` parameter to the `screenshot` action.

```rust
"screenshot" => {
    let selector = input.arguments.get("selector").and_then(|v| v.as_str());
    let screenshot = if let Some(sel) = selector {
        // Element-scoped screenshot
        let element = page.find_element(sel).await?;
        element.screenshot(/* params */).await?
    } else {
        // Full viewport (existing behavior)
        page.screenshot(/* params */).await?
    };
    // ... existing base64 encoding and last_image storage
}
```

**Estimated change:** ~20 lines in `browser.rs`.

---

## Phase 1: Layered Observation Architecture

### 1.1 Tier Selection Logic

**What:** A deterministic function that decides which observation tier to use, WITHOUT calling the LLM.

**Where:** New module `crates/temm1e-tools/src/browser_observation.rs`

**Design:**
```rust
pub enum ObservationTier {
    AccessibilityTree,                    // Tier 1: ~100-500 tokens
    AccessibilityTreeWithDom(String),     // Tier 2: tree + targeted DOM subtree
    AccessibilityTreeWithScreenshot,      // Tier 3: tree + element/viewport screenshot
}

/// Deterministic tier selection — no LLM calls.
pub fn select_tier(
    tree: &AccessibilityTree,
    task_hint: Option<&str>,      // from the LLM's last action decision
    previous_action_failed: bool,
) -> ObservationTier {
    // Tier 1: Default. Always try accessibility tree first.
    // Tier 2: If tree has unlabeled interactive elements (role but no name),
    //         or task_hint mentions "table", "form", "details"
    // Tier 3: If previous action failed (vision for verification),
    //         or task_hint mentions "image", "visual", "captcha", "layout"
}
```

**Key constraint:** This function is `O(1)` — it examines tree metadata (element count, unlabeled count), not the full tree. The LLM never decides how to observe.

**Integration point:** The `BrowserTool::execute()` method for `observe` action calls `select_tier()` and returns the appropriate observation.

### 1.2 New `observe` Action

**What:** A unified observation action that returns the appropriate tier automatically.

**Where:** `crates/temm1e-tools/src/browser.rs`

```rust
"observe" => {
    let page = self.ensure_browser().await?;
    let tree = get_accessibility_tree(&page).await?;
    let hint = input.arguments.get("hint").and_then(|v| v.as_str());
    let failed = input.arguments.get("retry").and_then(|v| v.as_bool()).unwrap_or(false);

    let tier = select_tier(&tree, hint, failed);

    match tier {
        ObservationTier::AccessibilityTree => {
            Ok(ToolOutput { content: format_tree(&tree), is_error: false })
        }
        ObservationTier::AccessibilityTreeWithDom(selector) => {
            let dom = page.execute_js(&format!(
                "document.querySelector('{}')?.outerHTML || 'not found'", selector
            )).await?;
            let markdown = html_to_markdown(&dom);
            Ok(ToolOutput {
                content: format!("{}\n\n---\nDOM Detail:\n{}", format_tree(&tree), markdown),
                is_error: false,
            })
        }
        ObservationTier::AccessibilityTreeWithScreenshot => {
            let screenshot = page.screenshot(/* viewport, 512px width */).await?;
            self.store_last_image(screenshot);
            Ok(ToolOutput {
                content: format_tree(&tree),
                is_error: false,
            })
            // Vision pipeline picks up the image via take_last_image()
        }
    }
}
```

### 1.3 HTML-to-Markdown Conversion

**What:** Convert HTML subtrees to Markdown for Tier 2 DOM extraction (5-10x compression vs raw HTML).

**Where:** `crates/temm1e-tools/src/html_markdown.rs`

**Options:**
- Pure Rust: `html2text` crate (lightweight, no JS runtime)
- Or: simple custom converter for tables, headings, links, lists (our main needs)

**Estimated:** ~100 lines for a focused converter covering: headings, links, lists, tables, paragraphs, strong/em.

### 1.4 Incremental Observation (Context Compression)

**What:** After the first full observation, subsequent observations send only the delta.

**Where:** `crates/temm1e-tools/src/browser.rs` — internal state in `BrowserTool`

**Design:**
- `BrowserTool` holds `last_tree_hash: Option<u64>` and `last_tree: Option<String>`
- On `observe`, hash the new tree. If identical to `last_tree_hash`, return `"[Page unchanged]"`
- If different, compute a text diff (or just return the full new tree — simpler, and accessibility trees are small)
- Reset on `navigate` (full page change)

**Estimated:** ~30 lines.

---

## Phase 2: Credential Isolation Protocol

### 2.1 Architecture

**The key rule:** Credential bytes never enter any string that reaches the LLM. The implementation enforces this through two execution domains separated at the Rust type level.

**Where:**
- Credential storage: existing `crates/temm1e-vault/` (no changes needed)
- Credential injection: new code in `crates/temm1e-tools/src/browser.rs`
- Credential scrubber: new module `crates/temm1e-tools/src/credential_scrub.rs`

### 2.2 Vault-Backed Credential Storage for Web Services

**What:** Extend the vault to store web credentials with service metadata.

**Key format:**
```
vault key: "web_cred:{service_name}"
vault value: JSON { "username": "...", "password": "...", "service_url": "..." }
```

The vault already encrypts with ChaCha20-Poly1305. No new crypto needed.

**User flow via Telegram:**
```
User: /addcred amazon
Tem:  Send me your Amazon credentials. I'll encrypt them immediately
      and never show them to anyone (including myself).
User: user@email.com / MyPassword123
Tem:  [immediately deletes user's message via Telegram API]
      Credentials for Amazon encrypted and stored. I'll use them
      when you ask me to browse Amazon.
```

**Implementation:**
- New tool: `WebCredentialTool` (or extend `KeyManageTool`) with actions: `store_web_cred`, `delete_web_cred`, `list_web_creds`
- Message deletion: Telegram API `deleteMessage` to remove the credential message from chat history
- Storage: `vault.store_secret("web_cred:amazon", encrypted_json_bytes)`

**Estimated:** ~100 lines for the tool, ~20 lines for message deletion integration.

### 2.3 Credential Injection in BrowserTool

**What:** When the LLM detects a login page and requests authentication, the tool layer handles it without the LLM seeing credentials.

**New action in BrowserTool:** `authenticate`

```rust
"authenticate" => {
    let service = input.arguments["service"].as_str()
        .ok_or(Temm1eError::Tool("service name required".into()))?;

    // 1. Retrieve from vault (credential execution domain)
    let cred_bytes = vault.get_secret(&format!("web_cred:{}", service)).await?
        .ok_or(Temm1eError::Tool(format!("No credentials stored for {}", service)))?;
    let cred: WebCredential = serde_json::from_slice(&cred_bytes)?;

    // 2. Detect login form fields via accessibility tree
    let page = self.ensure_browser().await?;
    let username_selector = find_field(&page, "username|email|login").await?;
    let password_selector = find_field(&page, "password").await?;
    let submit_selector = find_button(&page, "sign in|log in|submit").await?;

    // 3. Inject credentials directly into DOM (credential bytes never leave this fn)
    page.fill(&username_selector, &cred.username).await?;
    page.fill(&password_selector, &cred.password).await?;

    // 4. Zeroize credential from memory
    // (Rust ownership: cred is dropped here, zeroize on drop)

    // 5. Click submit
    page.click(&submit_selector).await?;
    page.wait_for_navigation().await?;

    // 6. Return post-login observation (credentials are gone from the page)
    let tree = get_accessibility_tree(&page).await?;
    Ok(ToolOutput {
        content: format!("Authenticated to {}. Current page:\n{}", service, format_tree(&tree)),
        is_error: false,
    })
}
```

**The LLM sees:** `"Authenticated to Amazon. Current page: [accessibility tree of account page]"`
**The LLM never sees:** username, password, email, or any credential bytes.

### 2.4 Login Form Detection

**What:** Detect username/password fields and submit buttons without hardcoded selectors.

**Where:** Helper functions in `browser.rs`

**Approach:** Use the accessibility tree — login forms have predictable roles:
- `textbox` with name containing "email", "username", "user", "login"
- `textbox` with type "password" (role attribute from accessibility tree)
- `button` with name containing "sign in", "log in", "submit", "continue"

This is timeproof — ARIA roles and accessible names are W3C-mandated, not site-specific.

**Fallback:** If accessibility tree detection fails, use CSS selectors for common patterns:
```css
input[type="email"], input[type="text"][name*="user"], input[type="text"][name*="email"]
input[type="password"]
button[type="submit"], input[type="submit"]
```

**Estimated:** ~80 lines for detection helpers.

### 2.5 Credential Scrubber

**What:** Post-login observation filter that redacts credential-like content before it reaches the LLM.

**Where:** `crates/temm1e-tools/src/credential_scrub.rs`

**Design:**
```rust
pub fn scrub_credentials(text: &str, known_credentials: &[&str]) -> String {
    let mut result = text.to_string();

    // 1. Redact known credential values
    for cred in known_credentials {
        result = result.replace(cred, "[REDACTED]");
    }

    // 2. Redact common patterns
    // - URL query params: token=, key=, secret=, password=, auth=, access_token=
    // - Authorization header values
    // - API key formats (sk-..., key-..., etc.)
    result = SENSITIVE_PARAM_REGEX.replace_all(&result, "$1=[REDACTED]").to_string();

    result
}
```

**Integration:** Called in `BrowserTool::execute()` on every `ToolOutput.content` before returning to the agent runtime.

**Estimated:** ~60 lines.

---

## Phase 3: OTK Session Capture

### 3.1 Overview

This extends the existing OTK infrastructure (used for API key onboarding — `crates/temm1e-gateway/src/setup_tokens.rs`) to web authentication.

### 3.2 Ephemeral Browser Session

**What:** When the user needs to authenticate to a site, Tem creates an ephemeral browser session accessible via a one-time link.

**Where:** New module `crates/temm1e-gateway/src/browse_session.rs`

**Flow:**
```
1. Agent decides: "I need Amazon auth. User doesn't have stored credentials."
2. Agent calls tool: authenticate(service="amazon", method="otk")
3. BrowserTool:
   a. Launches a browser, navigates to amazon.com
   b. Generates OTK (32 random bytes)
   c. Registers the browser session with the OTK in a session map
   d. Returns link: "https://temm1e-labs.github.io/temm1e/browse#{otk_hex}"
4. Agent sends link to user via Telegram
5. User taps link → sees the live browser (via noVNC or similar)
6. User logs into Amazon normally
7. User clicks "Done — hand back to Tem"
8. Gateway:
   a. Captures session state (cookies, localStorage, sessionStorage)
   b. Encrypts with vault key
   c. Stores as web_cred:amazon:session
   d. Destroys OTK (non-replayable)
   e. Tears down the ephemeral browser link
9. Agent receives callback → continues browsing with authenticated session
```

### 3.3 Session Capture Page

**What:** A static page (hosted on GitHub Pages, like the existing setup page) that connects to the ephemeral browser via WebSocket.

**Where:** `docs/browse/index.html`

**Tech options for browser streaming:**
1. **noVNC** — VNC in the browser via WebSocket. Mature, well-understood. Requires a VNC server wrapping the headless Chrome.
2. **Chrome DevTools Protocol screencast** — CDP has `Page.startScreencast` which streams JPEG frames. Lighter than VNC. Can relay via WebSocket.
3. **Playwright's browser context as a service** — Browserbase-style, but self-hosted.

**Recommended: CDP screencast** — it's the lightest option and we already use CDP (chromiumoxide). The flow:
- Gateway starts `Page.startScreencast` on the ephemeral browser
- Frames are relayed to the client via WebSocket
- User input (clicks, keystrokes) are relayed back as CDP `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent`
- When user clicks "Done", client sends a signal, gateway captures session

### 3.4 Session State Persistence

**What:** After OTK capture, store the authenticated session encrypted in the vault for future use.

**Storage format:**
```json
{
    "cookies": [...],
    "local_storage": {...},
    "session_storage": {...},
    "url": "https://www.amazon.com/gp/css/homepage.html",
    "captured_at": "2026-03-19T22:14:00Z",
    "service": "amazon"
}
```

**Vault key:** `web_session:{service_name}`

**Restore:** When the agent needs to browse Amazon later:
```rust
let session = vault.get_secret("web_session:amazon").await?;
let state: SessionState = serde_json::from_slice(&session)?;
browser.set_cookies(&state.cookies).await?;
// localStorage/sessionStorage via CDP: Page.evaluate
```

This maps to the existing `save_session`/`restore_session` actions in `BrowserTool`, but vault-backed instead of file-backed.

### 3.5 Session Health Check

**What:** Before using a stored session, verify it's still valid.

**Where:** Helper in `browser.rs`

```rust
async fn check_session_health(page: &Page, service: &str) -> bool {
    // Navigate to a known authenticated page for the service
    let check_url = get_health_url(service); // e.g., amazon.com/gp/css/homepage.html
    page.navigate(check_url).await;
    let tree = get_accessibility_tree(&page).await;
    // If tree contains "Sign In" / "Log In" button → session expired
    !tree_contains_login_prompt(&tree)
}
```

**On failure:** Notify user and offer OTK re-auth link.

---

## Phase 4: Web Blueprints

### 4.1 Blueprint Design

Web Blueprints are standard TEMM1E blueprints (stored in memory as `MemoryEntryType::Blueprint`) that encode common web interaction patterns. They are NOT separate agents — they are procedural memory that Tem references when browsing.

### 4.2 Core Web Blueprints

**`web_search` blueprint:**
```yaml
---
id: bp_web_search
name: Web Search Pattern
semantic_tags: ["web", "search", "browse", "find"]
task_signature: "search for {query} on {site}"
---
## Objective
Search for information on a website.

## Phases
### Phase 1: Navigate (independent)
- Call `browser(action="navigate", url="{site}")
- Call `browser(action="observe")` to get page structure

### Phase 2: Search (depends: Phase 1)
- Identify search textbox from accessibility tree (role=textbox, name contains "search")
- Call `browser(action="type", selector="{search_selector}", text="{query}")`
- Call `browser(action="click", selector="{submit_selector}")` or press Enter
- Call `browser(action="observe")` to see results

### Phase 3: Extract (depends: Phase 2)
- Parse results from accessibility tree
- Extract structured data (titles, links, prices, etc.)
- Return formatted results to user
```

**`web_login` blueprint:**
```yaml
---
id: bp_web_login
name: Web Login Flow
semantic_tags: ["web", "login", "authenticate", "sign in"]
task_signature: "log into {service}"
---
## Objective
Authenticate to a web service.

## Phases
### Phase 1: Check existing session
- Call `browser(action="restore_session", name="{service}")`
- Call `browser(action="observe")` — if authenticated, DONE

### Phase 2: Vault credentials (depends: Phase 1, only if session invalid)
- Call `browser(action="authenticate", service="{service}")`
- If vault has credentials → injected automatically, DONE

### Phase 3: OTK capture (depends: Phase 2, only if no vault credentials)
- Call `browser(action="authenticate", service="{service}", method="otk")`
- Send OTK link to user
- Wait for callback
- Call `browser(action="observe")` to confirm authenticated state

## Failure Recovery
- If login fails: check for CAPTCHA (escalate to user), 2FA (escalate to user), wrong credentials (notify user)
```

**`web_extract_table` blueprint:**
```yaml
---
id: bp_web_extract_table
name: Web Table Extraction
semantic_tags: ["web", "extract", "table", "data", "scrape"]
task_signature: "extract {data} from {site}"
---
## Objective
Extract structured data from a web page.

## Phases
### Phase 1: Navigate and observe
- Navigate to target URL
- Get accessibility tree
- If table role found → extract directly from tree
- If not → escalate to Tier 2 (DOM extraction with html_to_markdown)

### Phase 2: Extract and structure
- Parse table/list data from observation
- Format as structured output (JSON or Markdown table)
- Return to user
```

**`web_compare` blueprint:**
```yaml
---
id: bp_web_compare
name: Multi-Site Comparison
semantic_tags: ["web", "compare", "price", "shop", "aggregate"]
task_signature: "compare {item} across {sites}"
---
## Objective
Compare information across multiple websites.

## Phases
### Phase 1-N: Search each site (independent — parallelizable by Hive)
- For each site: navigate → search → extract structured data
- Store results in scratchpad

### Phase N+1: Aggregate (depends: all previous)
- Compare extracted data
- Rank by user's criteria
- Format comparison table
- Return to user

## Notes
- This blueprint is a natural candidate for Hive decomposition
- Queen decomposes into N independent browse tasks + 1 aggregation task
- Each Tem gets its own browser context
```

### 4.3 Blueprint Registration

Blueprints are stored via the existing memory system. On first run with Prowl enabled, seed the default web blueprints into memory:

```rust
// In agent initialization, if prowl feature enabled:
if config.tools.browser {
    seed_web_blueprints(&memory).await?;
}
```

The classifier's `blueprint_hint` field will naturally match web tasks to these blueprints via the `semantic_tags`.

---

## Phase 5: Swarm Browsing (Hive Integration)

### 5.1 Browser Pool

**What:** A managed pool of browser contexts that Hive workers claim alongside tasks.

**Where:** New module `crates/temm1e-tools/src/browser_pool.rs`

**Design:**
```rust
pub struct BrowserPool {
    browser: Browser,                          // Single Chromium process
    contexts: Vec<Option<BrowserContext>>,      // Pre-allocated slots
    available: Arc<AtomicU64>,                 // Bitset: 1 = available
    max_contexts: usize,
    config: BrowserPoolConfig,
}

impl BrowserPool {
    pub async fn new(max_contexts: usize, config: BrowserPoolConfig) -> Result<Self, Temm1eError>;

    /// Atomically claim a browser context. Returns slot index + context.
    pub async fn acquire(&self) -> Result<(usize, BrowserContext), Temm1eError>;

    /// Release a context back to the pool. Clears cookies/storage.
    pub async fn release(&self, slot: usize) -> Result<(), Temm1eError>;

    /// Acquire a context with a pre-loaded session (for authenticated browsing).
    pub async fn acquire_with_session(
        &self,
        session: &SessionState,
    ) -> Result<(usize, BrowserContext), Temm1eError>;
}
```

**Config:**
```toml
[tools.browser_pool]
max_contexts = 4          # Max parallel browsers
idle_timeout_secs = 120   # Per-context idle timeout
stealth = true            # Apply anti-detection to all contexts
```

### 5.2 Hive Worker Browser Assignment

**What:** When Hive decomposes a web task, each worker Tem gets its own browser context.

**Where:** Extension in `crates/temm1e-hive/src/worker.rs`

**Current worker flow:**
```
worker.run_loop() → select_task() → claim_task() → execute_task() → complete/fail
```

**Extended flow for browse tasks:**
```
worker.run_loop() → select_task() → claim_task()
  → if task.tags contains "browse":
      context = browser_pool.acquire()
      tool_context.browser_context = Some(context)
  → execute_task(tool_context)
  → browser_pool.release(context)
  → complete/fail
```

The worker passes the claimed `BrowserContext` into the `ToolContext` so the `BrowserTool` uses that specific context instead of creating its own.

### 5.3 Browse-Specific Pheromone Signals

**What:** Four new signal types for the pheromone field.

**Where:** `crates/temm1e-hive/src/types.rs` — extend `SignalType` enum

```rust
pub enum SignalType {
    // Existing
    Completion,
    Failure,
    Difficulty,
    Urgency,
    Progress,
    HelpWanted,
    // New (Prowl)
    BotDetected,      // Anti-bot system triggered on a domain
    SessionExpired,   // Auth session invalidated for a service
    DataFound,        // Useful data extracted (enables progressive delivery)
    RateLimit,        // HTTP 429 or equivalent on a domain
}
```

**Decay rates:**
- `BotDetected`: ~3 min half-life (ρ = 0.004)
- `SessionExpired`: ~1 min half-life (ρ = 0.012)
- `DataFound`: ~10 min half-life (ρ = 0.001)
- `RateLimit`: ~5 min half-life (ρ = 0.002)

**Emission points in BrowserTool:**
- After receiving HTTP 403/429 → emit `BotDetected` or `RateLimit` with `target = domain`
- After detecting login page when session was expected → emit `SessionExpired` with `target = service`
- After successful data extraction → emit `DataFound` with `target = task_id`

### 5.4 Progressive Delivery

**What:** As individual Tems complete their browse subtasks, send partial results to the user immediately.

**Where:** Integration in the Hive aggregation logic

**Design:**
The Hive already has a completion callback mechanism. Extend it:

```rust
// In hive orchestrator, after each task completion:
if task.tags.contains("browse") && task.status == Complete {
    let partial = format_partial_result(&task.result);
    // Send to user via channel
    channel.send_message(OutboundMessage {
        chat_id: order.chat_id.clone(),
        text: partial,
        edit_message_id: progress_message_id,  // Edit the existing progress message
    }).await?;
}
```

**User sees:**
```
Tem: Searching 4 sites for Tokyo flights...
     ✓ Google Flights: $450 JAL direct
     ⏳ Kayak: checking...
     ⏳ Skyscanner: checking...
     ⏳ United: checking...
```

Updated in-place as each Tem completes.

### 5.5 Queen Decomposition for Web Tasks

**What:** The Queen needs to understand how to decompose web tasks into parallelizable browse subtasks.

**Where:** System prompt extension in `crates/temm1e-hive/src/queen.rs`

Add web-specific decomposition guidance to the Queen's prompt:

```
When decomposing web browsing tasks:
- Each website/service is an INDEPENDENT subtask (different domains = no dependencies)
- Tag browse subtasks with "browse" so workers claim browser contexts
- Always add a final "aggregate" task that depends on all browse subtasks
- Include the target URL in each subtask's description
- If a subtask requires authentication, include "auth:{service}" in the context_tags
```

The Queen already produces DAGs — this just teaches it web-specific patterns.

---

## Phase 6: Benchmark Suite (TBench)

### 6.1 Benchmark Infrastructure

**Where:** `tems_lab/prowl/bench/`

**Design:** Python-based benchmarks (like the existing Lambda benchmarks) that:
1. Start TEMM1E with browser tools enabled
2. Send test messages via CLI chat
3. Capture tool call logs, timing, token counts
4. Verify results against expected outcomes

### 6.2 Test Suites

**TBench-Simple (50 tasks):**
- Navigate to URL + extract title
- Search Wikipedia for a topic
- Get current weather from a weather site
- Extract a specific data point from a known page
- Fill and submit a simple form on a test site

**TBench-Auth (30 tasks):**
- Vault credential injection on a test login page
- OTK session capture flow (with simulated user login)
- Session health check (valid and expired sessions)
- Credential scrubber verification (inject known credentials, verify they don't appear in LLM context)

**TBench-Swarm (30 tasks):**
- Compare data from 3-5 test sites simultaneously
- Verify wall-clock speedup > 2x vs sequential
- Verify token cost ratio (swarm/single) < 1.0
- Inject browser crash in one worker, verify others continue
- Progressive delivery: verify partial results arrive before full completion

**TBench-Adversarial (20 tasks):**
- Navigate sites with Cloudflare challenge pages
- Handle popup/interstitial ads
- SPA navigation (React/Vue test apps)
- Cookie consent banners
- Rate-limited endpoints

**TBench-Resilience (20 tasks):**
- Browser crash recovery
- Network timeout handling
- Expired session detection and re-auth
- DOM mutation during action execution
- Out-of-memory browser context handling

---

## Dependency Map

```
Phase 0 (Foundation Fixes)
  ├── 0.1 MCP Image Gap
  ├── 0.2 Accessibility Tree Tool
  └── 0.3 Element-Scoped Screenshots
        │
Phase 1 (Layered Observation) ─── depends on Phase 0
  ├── 1.1 Tier Selection Logic
  ├── 1.2 observe Action
  ├── 1.3 HTML-to-Markdown
  └── 1.4 Incremental Observation
        │
Phase 2 (Credential Isolation) ─── independent of Phase 1
  ├── 2.1 (architecture — no code)
  ├── 2.2 Vault Web Credentials
  ├── 2.3 authenticate Action
  ├── 2.4 Login Form Detection
  └── 2.5 Credential Scrubber
        │
Phase 3 (OTK Session Capture) ─── depends on Phase 2
  ├── 3.1 Overview
  ├── 3.2 Ephemeral Browser Session
  ├── 3.3 Session Capture Page
  ├── 3.4 Session State Persistence
  └── 3.5 Session Health Check
        │
Phase 4 (Web Blueprints) ─── depends on Phase 1
  ├── 4.1 Blueprint Design
  ├── 4.2 Core Web Blueprints (4 blueprints)
  └── 4.3 Blueprint Registration
        │
Phase 5 (Swarm Browsing) ─── depends on Phase 1, 2
  ├── 5.1 Browser Pool
  ├── 5.2 Worker Browser Assignment
  ├── 5.3 Browse Pheromone Signals
  ├── 5.4 Progressive Delivery
  └── 5.5 Queen Web Decomposition
        │
Phase 6 (Benchmarks) ─── depends on all previous
  └── TBench suite
```

**Parallelizable:** Phase 1 and Phase 2 can be built simultaneously. Phase 4 can start as soon as Phase 1 is done.

---

## Estimated Scope

| Phase | New LOC (est.) | Files touched | Files created | Risk |
|-------|---------------|---------------|---------------|------|
| Phase 0 | ~240 | 2 (browser.rs, bridge.rs) | 1 (browser_observation.rs) | Low — extending existing code |
| Phase 1 | ~300 | 2 (browser.rs, lib.rs) | 2 (browser_observation.rs, html_markdown.rs) | Low — new features, no behavior changes |
| Phase 2 | ~360 | 3 (browser.rs, key_manage.rs, main.rs) | 1 (credential_scrub.rs) | Medium — credential handling is security-critical |
| Phase 3 | ~500 | 3 (main.rs, setup_tokens.rs, browser.rs) | 2 (browse_session.rs, docs/browse/index.html) | High — ephemeral browser streaming is complex |
| Phase 4 | ~200 | 1 (main.rs or agent init) | 4 (blueprint YAML files) | Low — uses existing blueprint system |
| Phase 5 | ~400 | 4 (worker.rs, types.rs, lib.rs, queen.rs) | 1 (browser_pool.rs) | Medium — Hive integration requires careful concurrency |
| Phase 6 | ~600 | 0 | 10+ (test scripts, benchmark framework) | Low — testing, no production code |
| **Total** | **~2,600** | **~15** | **~21** | |

---

## Feature Flags

```toml
[features]
prowl = ["browser"]   # Tem Prowl: web-native browsing
                       # Depends on browser feature (chromiumoxide)
                       # Enables: accessibility tree, observe action, credential isolation,
                       #          browser pool, web blueprints
```

All Prowl code gated behind `#[cfg(feature = "prowl")]`. When disabled, TEMM1E is byte-identical to pre-Prowl.

---

## What We Are NOT Building (Yet)

- **Custom browser engine.** We use Chromium via CDP (chromiumoxide) or Playwright via MCP. Not building a browser.
- **CAPTCHA solving.** We escalate to the user. Always.
- **Credential scraping.** We never extract credentials from web pages. We inject them and observe the result.
- **Cross-device session sync.** Future work. Requires Chrome profile sync infrastructure.
- **Web action recording/replay.** Future work (Tem Prowl v2). Requires interaction recording infrastructure.
- **Distributed swarm browsing.** Many Tems v1 is single-process. Multi-machine browsing is future work.

---

*Implementation plan for Tem Prowl. Grounded in the existing TEMM1E codebase (v3.0.0, 15 crates). March 2026.*
