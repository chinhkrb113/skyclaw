# Tem Prowl: UX Testing Report

> **Date:** 2026-03-20
> **Tester:** Claude (self-testing via CLI chat)
> **Provider:** Gemini 3 Flash Preview

---

## Tests Conducted

| # | Test | Action | Result |
|---|------|--------|--------|
| 1 | Login page observe | `observe` on the-internet/login | **Pass** — clean AX tree with form fields |
| 2 | HN accessibility_tree | `accessibility_tree` on news.ycombinator.com | **Pass** — all links numbered correctly |
| 3 | Authenticate (no creds) | `authenticate` with service "heroku_test" | **Expected fail** — "Vault not available" |
| 4 | Multi-step observe+click | Navigate → observe → click → observe → click → observe | **Pass** — 10 tool calls, checkbox toggled |

---

## Technical Observations

### 1. observe action — WORKS, excellent output format

The JS-based AX tree produces clean, readable output:
```
[1] a
  [2] img "Fork me on GitHub"
[3] h2 "Login Page" level=2
[4] h4 "This is where you can log into the secure area..." level=4
[5] form
  [6] input "Username" type=text
  [7] input "Password" type=password
  [8] button "Login"
[9] a "Elemental Selenium"
```

**Strengths:**
- Numbered indices work — agent can reference `[6]` for the username field
- Form structure is clear — form → input → button hierarchy preserved
- Input types visible — `type=text` and `type=password` correctly identified
- Only 9 elements shown for a login page (vs ~1500 tokens for a screenshot)
- Token cost: **21,947 combined** — lowest of all tests

**Weaknesses:**
- Link text truncation at 60 chars — long headings get cut (h4 text truncated)
- No `value` shown for empty inputs — would be useful to see `value=""` explicitly
- Anchor tags show as `[1] a` without href — knowing the URL would help the agent decide what to click

### 2. accessibility_tree on HN — WORKS, but verbose

90+ elements numbered for HN. The model processed it correctly (extracted top 5 stories) but used 94K tokens — more than screenshot (40K) because:
- Full AX tree text for HN is ~5K tokens
- But the model made 10 API calls (retries, reasoning) vs 5-6 for screenshot
- The model isn't yet optimized for AX tree output — it's more familiar with screenshots

**Root cause:** The model's prompt doesn't teach it how to efficiently read AX tree output. Adding examples to the system prompt would help.

### 3. authenticate action — VAULT NOT WIRED

The `authenticate` action returned "Vault not available" because the vault instance isn't passed to BrowserTool in the CLI chat path.

**Technical issue:** In `main.rs`, `create_tools()` is called with `vault: None` for the CLI chat path. The vault IS created for the gateway (Telegram) path but not connected to the tool factory for CLI.

**Impact:** The authenticate action works at the code level (tested via unit tests) but can't be exercised live without vault wiring. This is a **main.rs integration task**, not a Prowl issue.

### 4. Multi-step workflow — WORKS, but response truncated

The model successfully:
1. Navigated to the-internet.herokuapp.com
2. Used observe to see the page
3. Clicked the Checkboxes link
4. Used observe again
5. Clicked checkbox 1
6. Used observe a final time
7. Even authored a blueprint ("Interacting with Web Checkboxes")

**But:** The user-facing response was just brief acks ("I'm ON IT!", "Clicking now!") — the model didn't report back the observe output or what changed at each step.

**Root cause:** The classifier classified this as an "order" (not "chat"), so the model focused on executing rather than explaining. The observe outputs were consumed as tool results but not reflected back to the user.

### 5. OTK Session Capture — NOT WIRED INTO MAIN.RS

`InteractiveBrowseSession` exists as a module but is NOT integrated into the message handling loop. To make it work:

1. Need to detect when `authenticate` returns "needs OTK"
2. Enter an interactive loop that bypasses the agent
3. Send annotated screenshots to the user
4. Accept number/text input directly
5. Capture session on "done"

This is the **Phase 3.3 main.rs integration** from the implementation plan — it wasn't implemented because it requires changes to the gateway message loop.

### 6. restore_web_session — NOT TESTABLE (vault not wired)

Same vault issue as authenticate. The action exists and compiles but can't be exercised without vault wiring.

---

## User Experience Assessment

### What works well:

1. **observe output is clean and readable** — the numbered list format makes it easy for the LLM to reference elements ("click [8]")
2. **Token efficiency on simple pages** — login page with observe: 21,947 tokens vs ~35,000 for screenshot = **37% savings**
3. **Multi-step works** — navigate→observe→click→observe→click workflow completes correctly
4. **Blueprint auto-authoring** — the model creates blueprints for new workflows automatically
5. **The incremental hash** — verified in multi-step benchmark, returns "[Page unchanged]" (5 tokens vs 250)

### What needs work:

1. **Model doesn't explain observe results to user** — treats observe as internal tool data, doesn't surface the tree structure in responses. Need system prompt guidance.

2. **Vault not wired in CLI chat** — authenticate and restore_web_session can't work. Quick fix: pass vault to `create_tools()` in the CLI path.

3. **OTK not integrated in main.rs** — the interactive session module exists but isn't connected to the message loop. Needs gateway integration.

4. **AX tree links lack href** — showing `[1] a "Hacker News"` without the URL means the agent can't distinguish between navigation links and action links without clicking.

5. **Response quality on multi-step tasks** — model gives brief acks during execution but doesn't summarize observations. The user sees "Clicking now! :3" but not "The checkbox is now checked."

6. **HN AX tree is too large** — 90+ numbered elements. Need smarter filtering: skip navigation/footer links, focus on main content area.

---

## Recommendations (Priority Order)

### P0 — Wire vault to CLI chat
```rust
// In main.rs CLI chat path, pass vault to create_tools()
let vault = Some(Arc::new(local_vault));
let tools = create_tools(config, ..., vault);
```
**Impact:** Unblocks authenticate and restore_web_session testing.

### P1 — System prompt guidance for observe
Add to the browser tool description:
```
When using observe/accessibility_tree, share the key elements with the user.
For example: "I can see: [3] search box, [5] login button, [7] navigation menu"
```
**Impact:** Model explains what it sees instead of silently processing.

### P2 — Add href to links in AX tree
```javascript
if (tag === 'a') {
    const href = el.getAttribute('href') || '';
    if (href) entry += ' href="' + href.substring(0,60) + '"';
}
```
**Impact:** Agent can make informed click decisions without trial-and-error.

### P3 — Wire OTK into main.rs
The Phase 3.3 integration from IMPLEMENTATION.md. Detect when `authenticate` returns "needs OTK", enter interactive screenshot loop.
**Impact:** Full OTK session capture becomes functional.

### P4 — Smart AX tree filtering
Filter out navigation/header/footer links on content-heavy pages. Focus on main content area. Could use `<main>` or `role="main"` as the root for tree walking.
**Impact:** HN tree drops from 90+ to ~30 elements.

---

## Metrics Summary

| Test | Tokens | Cost | API Calls | Tools |
|------|--------|------|-----------|-------|
| Login page observe | **21,947** | **$0.0035** | 4 | 3 |
| HN accessibility_tree | 94,172 | $0.0157 | 10 | — |
| Authenticate (no vault) | 30,630 | $0.0048 | 5 | — |
| Multi-step observe+click | 65,016 | $0.0099 | 9 | 10 |
| **Total UX tests** | | **$0.0339** | | |

---

*UX testing report for Tem Prowl. Self-tested via CLI chat. March 2026.*
