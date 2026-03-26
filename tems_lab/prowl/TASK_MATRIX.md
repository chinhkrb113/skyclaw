# Tem Prowl: Task Matrix

> **Zero-risk policy applies.** Nothing ships until confidence = 100%, risk = 0%, knowledge = 100%.
> **Status: ALL RESEARCH RESOLVED. ALL TASKS AT 100/0/100. READY TO IMPLEMENT.**

---

## Scoring

- **Confidence (C):** "Can we build this?" — 100% = fully understood, API exists, code path clear
- **Risk (R):** "Can this break existing users?" — 0% = impossible to break anything, purely additive
- **Knowledge (K):** "Do we know exactly HOW to build it?" — 100% = every function call, every line, every edge case mapped

---

## Phase 0: Foundation Fixes

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 0.1 | MCP Image Gap — detect image blocks in MCP response, produce `ToolOutputImage` | 100% | 0% | 100% | `PHASE_0_FOUNDATION.md` §0.1 |
| 0.2 | Accessibility Tree — add `accessibility_tree` action via CDP `GetFullAxTreeParams` | 100% | 0% | 100% | `PHASE_0_FOUNDATION.md` §0.2 |
| 0.3 | Element-Scoped Screenshots — add `selector` param using `element.screenshot()` | 100% | 0% | 100% | `PHASE_0_FOUNDATION.md` §0.3 |

**Research resolved:** R1 (chromiumoxide has full Accessibility domain), R3 (Element has screenshot()), R4 (MCP image format confirmed)

---

## Phase 1: Layered Observation

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 1.1 | Tier Selection Function — deterministic, O(1), examines tree metadata | 100% | 0% | 100% | `PHASE_1_OBSERVATION.md` §1.1 |
| 1.2 | `observe` Action — unified observation with auto tier selection | 100% | 0% | 100% | `PHASE_1_OBSERVATION.md` §1.2 |
| 1.3 | HTML-to-Markdown — using `htmd` crate (table support, Turndown.js-inspired) | 100% | 0% | 100% | `PHASE_1_OBSERVATION.md` §1.3 |
| 1.4 | Incremental Observation — hash-based delta detection | 100% | 0% | 100% | `PHASE_1_OBSERVATION.md` §1.4 |

**Research resolved:** R5 (tree token costs measured: 3k-19k raw, 200-500 filtered), R7 (html2text inadequate → use `htmd`)

---

## Phase 2: Credential Isolation

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 2.1 | Vault Web Credentials — `web_cred:{service}` key format, `WebCredential` struct with `Zeroize` | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.1 |
| 2.2 | Login Form Detection — accessibility tree role analysis (textbox + protected property) | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.2 |
| 2.3 | `authenticate` Action — vault retrieve → DOM inject → zero → post-login observe | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.3 |
| 2.4 | Credential Scrubber — regex-based redaction of sensitive patterns | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.4 |
| 2.5 | Telegram Message Deletion — `deleteMessage` after credential capture | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.5 |
| 2.6 | Zeroize Integration — `Zeroizing<Vec<u8>>`, `#[derive(Zeroize, ZeroizeOnDrop)]` | 100% | 0% | 100% | `PHASE_2_CREDENTIALS.md` §2.6 |

**Research resolved:** R3 (`type_str()` dispatches real keyboard events), R5 (password fields = textbox + protected property), R6 (bots CAN delete user messages, 48h window), R8 (`Zeroizing<String>` works, derive works)

---

## Phase 3: OTK Session Capture

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 3.1 | OTK Flow — **screenshot-based** (not streaming), user interacts via numbered annotations | 100% | 0% | 100% | `PHASE_3_OTK_SESSION.md` §3.1 |
| 3.2 | Interactive Session Handler — annotated screenshots, handle user input (number/text/done) | 100% | 0% | 100% | `PHASE_3_OTK_SESSION.md` §3.2 |
| 3.3 | main.rs Integration — interactive loop bypassing agent, direct user↔browser | 100% | 0% | 100% | `PHASE_3_OTK_SESSION.md` §3.3 |
| 3.4 | Session Restore — load cookies + localStorage + sessionStorage from vault | 100% | 0% | 100% | `PHASE_3_OTK_SESSION.md` §3.4 |

**Key decision:** Dropped browser streaming (R9: laggy, complex, Chromium performance bug). Screenshot + click-map approach is dramatically simpler, works within existing channels, no new infrastructure.

**Research resolved:** R9 (screencast inadequate → screenshot approach), R10 (no Rust streaming crates → not needed), R11 (DOMStorage fully exposed in chromiumoxide)

---

## Phase 4: Web Blueprints

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 4.2 | Core Web Blueprints — 4 YAML files (search, login, extract, compare) | 100% | 0% | 100% | `PHASE_4_BLUEPRINTS.md` §4.2 |
| 4.3 | Blueprint Registration — seed via `include_str!` on first run | 100% | 0% | 100% | `PHASE_4_BLUEPRINTS.md` §4.3 |

---

## Phase 5: Swarm Browsing

| # | Task | C | R | K | Implementation Doc |
|---|------|---|---|---|-------------------|
| 5.1 | Browser Pool — managed pool with atomic claiming, context isolation, clean release | 100% | 0% | 100% | `PHASE_5_SWARM.md` §5.1 |
| 5.2 | Per-Worker BrowserTool — pool-bound instances via `with_pool_slot()`, non-browse tasks unchanged | 100% | 0% | 100% | `PHASE_5_SWARM.md` §5.2 |
| 5.3 | Browse Pheromone Signals — BotDetected, SessionExpired, DataFound, RateLimit | 100% | 0% | 100% | `PHASE_5_SWARM.md` §5.3 |
| 5.4 | Progressive Delivery — Blackboard watcher + Telegram editMessage | 100% | 0% | 100% | `PHASE_5_SWARM.md` §5.4 |
| 5.5 | Queen Web Decomposition — prompt extension for browse task DAGs | 100% | 0% | 100% | `PHASE_5_SWARM.md` §5.5 |

**Research resolved:** R12 (workers share Arc → need per-worker BrowserTool via pool slots), R13 (no progressive delivery → add watcher task), R14 (4 contexts ~220-420MB, feasible)

---

## Research Resolution Summary

All 14 research items resolved:

| # | Question | Resolution |
|---|----------|------------|
| R1 | chromiumoxide Accessibility domain | **YES** — `GetFullAxTreeParams`, `AxNode` with full role/name/value/properties |
| R2 | chromiumoxide multi-context | **YES** — `create_browser_context()`, cookie/cache isolated per context |
| R3 | chromiumoxide form API | `type_str()` dispatches real keyDown/keyUp (React-safe). `element.screenshot()` exists. `page.evaluate()` for JS. No `fill()` — use clear+type. |
| R4 | MCP image format | `{ type: "image", data: base64, mimeType: "image/png" }` — confirmed from spec |
| R5 | Accessibility tree samples | Full tree: 14k-19k tokens. Filtered interactive: 3k-8k. Aggressive filter: 200-500. Password = textbox + protected. Iframes NOT flattened. |
| R6 | Telegram deleteMessage | **YES** — bots can delete user messages in private chats within 48 hours |
| R7 | HTML-to-Markdown crate | `html2text` inadequate for tables. Use **`htmd`** instead (Turndown.js-inspired, proper table support) |
| R8 | zeroize with String | **YES** — `Zeroizing<String>` zeros heap. `#[derive(Zeroize, ZeroizeOnDrop)]` works. Never use `serde_json::Value` intermediate. |
| R9 | CDP screencast | Inadequate: 50-200ms latency, known Chromium perf bug. **Decision: screenshot + click-map instead.** |
| R10 | Browser streaming arch | Not needed — screenshot approach eliminates this requirement |
| R11 | chromiumoxide DOMStorage | **YES** — full domain: GetDomStorageItems, SetDomStorageItem, Clear. localStorage AND sessionStorage. |
| R12 | Hive worker tool sharing | Workers share `Arc<dyn Tool>`. BrowserTool has single page → **need per-worker instances via pool slots** |
| R13 | Hive result delivery | No progressive mechanism. **Add Blackboard watcher task + Telegram editMessage.** |
| R14 | Chrome context memory | ~30-80MB per context with simple pages. 4 contexts: ~220-420MB total. **Feasible under 1GB.** |

---

## Implementation Files

| Doc | Phase | Tasks | Status |
|-----|-------|-------|--------|
| `PHASE_0_FOUNDATION.md` | Foundation Fixes | 0.1, 0.2, 0.3 | 100/0/100 |
| `PHASE_1_OBSERVATION.md` | Layered Observation | 1.1, 1.2, 1.3, 1.4 | 100/0/100 |
| `PHASE_2_CREDENTIALS.md` | Credential Isolation | 2.1-2.6 | 100/0/100 |
| `PHASE_3_OTK_SESSION.md` | OTK Session Capture | 3.1-3.4 | 100/0/100 |
| `PHASE_4_BLUEPRINTS.md` | Web Blueprints | 4.2, 4.3 | 100/0/100 |
| `PHASE_5_SWARM.md` | Swarm Browsing | 5.1-5.5 | 100/0/100 |

**All gates green. Ready to implement.**

---

*Task matrix for Tem Prowl. Zero-risk policy. All research resolved. March 2026.*
