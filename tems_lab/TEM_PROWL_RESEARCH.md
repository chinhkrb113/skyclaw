# Tem Browse: Web-Native Agent Vision — Deep Research Report

> **Branch:** `tem-browse`
> **Date:** 2026-03-19
> **Status:** Research phase — no implementation yet

---

## Executive Summary

The web is the second home for humans — and it should be for AI agents too. Local tools are powerful, but the overwhelming majority of modern interactions (social media, work apps, commerce, banking, communication) happen in browsers. For Tem to be a truly useful agent, it must traverse the web as efficiently as a human — or better.

This report covers three domains:
1. **How the industry builds web-native agents** — architectures, frameworks, commercial products
2. **The authentication problem** — LLM guardrails, OAuth, credential vaults, innovative solutions
3. **Efficiency and innovation** — token optimization, cost, benchmarks, what makes agents fast and reliable

---

## Part 1: The Web-Native Agent Landscape

### 1.1 Browser Control Architectures

There are three fundamental approaches to giving an AI agent a browser:

| Approach | How it works | Who uses it | Token cost | Reliability |
|----------|-------------|-------------|------------|-------------|
| **Pure Vision** | Screenshot → LLM → (x,y) click | Anthropic Computer Use, WebVoyager | Very high (~1500 tokens/screenshot) | Universal but slow |
| **Pure DOM** | HTML/accessibility tree → LLM → selector-based action | Early MCP servers, Playwright MCP | Low | Fast but brittle on complex UIs |
| **Hybrid** | Accessibility tree + DOM + selective screenshots | Browser-use, Stagehand, Operator, Agent-E | Medium | Best balance — industry convergence point |

**The industry has converged on hybrid.** Pure vision is too expensive and slow. Pure DOM misses visual context. The winning stack is:

```
Layer 1: Accessibility tree (always — cheapest, semantic)
Layer 2: Targeted DOM extraction (when tree is insufficient)
Layer 3: Selective screenshots (verification, CAPTCHAs, image-heavy pages)
```

### 1.2 Open-Source Frameworks

#### browser-use (Python)
- Wraps Playwright, dual-mode perception (vision + DOM)
- Assigns numeric indices to interactive elements
- Supports multi-tab natively
- Weakness: heavy screenshot dependency = high token cost

#### Stagehand (Browserbase)
- Three primitives: `act()`, `extract()`, `observe()`
- **DOM chunking innovation**: splits DOM into LLM-context-sized pieces, ranks candidates
- DOM-first, vision only as fallback
- Tied to Browserbase cloud infra

#### Agent-E (Emergence AI)
- Pioneered **accessibility-tree-first** approach
- Hierarchical agent architecture (planner + navigator sub-agents)
- DOM distillation: 10-50x smaller than raw HTML while preserving all interactive elements
- Strong form-filling, weak on non-standard UI components

#### LaVague
- Two-engine design: World Model (plans) + Action Engine (generates Playwright/Selenium code)
- Generates and executes actual automation code — flexible but risky (code injection)
- Cheaper model for code gen, capable model for planning

#### WebVoyager
- Pure vision: annotates screenshots with numbered bounding boxes (Set-of-Mark)
- Works on ANY website regardless of DOM complexity, shadow DOM, canvas
- Extremely expensive: full screenshot + vision processing every step

### 1.3 Commercial Products

#### Anthropic Computer Use
- **Pure vision, desktop-level** — treats browser same as any application
- Coordinate-based clicking (x,y output)
- Universal but expensive (~1500 tokens per screenshot, 5-15s per step)
- **Hard refuses** to interact with credential fields — baked into model training

#### OpenAI Operator (CUA)
- Cloud-hosted Chromium, hybrid vision+DOM
- **"Supervised autonomy"**: pauses and hands control to user for sensitive actions (payments, login)
- Live-streamed viewport for user observation
- Later added 1Password integration for credential filling

#### Google Project Mariner
- **Chrome extension** — runs inside user's actual browser
- Inherits ALL existing sessions and cookies — auth is a non-issue
- Chrome's built-in password manager handles auto-fill
- Best UX for auth, but Chrome desktop only — cannot run headless/server-side

#### Microsoft Copilot Vision
- Edge-native, read-heavy (summarize, extract, answer questions)
- Limited autonomous action compared to others
- Enterprise-grade data governance

### 1.4 MCP Browser Tools

The Model Context Protocol has become the standard integration layer:

| MCP Server | Approach | Key capability |
|-----------|----------|---------------|
| **Playwright MCP** | DOM + accessibility snapshots | Full Playwright API, multi-context, headed/headless |
| **Browserbase MCP** | Cloud browsers | Residential proxies, anti-detection, CAPTCHA handling |
| **Puppeteer MCP** | Chrome DevTools Protocol | Tighter Chrome integration, PDF generation |
| **Firecrawl MCP** | Web scraping | HTML→Markdown conversion, read-only but fast and cheap |

### 1.5 Vision vs DOM vs Accessibility Tree

| Dimension | Vision | DOM/HTML | Accessibility Tree |
|-----------|--------|----------|-------------------|
| Token cost | ~1500/screenshot | 500-5000 | 100-500 |
| Visual layout | Excellent | Partial | Not captured |
| Interactive elements | Requires visual parsing | Good but noisy | Excellent (designed for this) |
| Shadow DOM | Transparent (just pixels) | Requires explicit traversal | Flattened automatically |
| iframes | Transparent | Requires context switching | Included in flattened tree |
| Dynamic content | Point-in-time snapshot | May miss JS-rendered | Real-time state |

**Key insight**: The accessibility tree is emerging as the best default representation — 10-100x smaller than HTML, semantically rich, handles Shadow DOM and iframes naturally, and is well-maintained by web developers (it's what screen readers use).

---

## Part 2: The Authentication Problem

### 2.1 The LLM Guardrail Wall

Every major LLM provider refuses to handle credentials:

| Action | Claude | GPT-4/Operator | Gemini |
|--------|--------|----------------|--------|
| Type password into form | **Refuses** | **Refuses** | **Refuses** |
| Read password from screenshot | **Refuses** | **Refuses** | **Refuses** |
| Store API key in context | Warns/refuses | Warns/refuses | Warns/refuses |
| Click "Log in with Google" | Sometimes OK | Sometimes OK | Sometimes OK |
| Use pre-authenticated session | **OK** | **OK** | **OK** |
| Fill 2FA codes | **Refuses** | **Refuses** | **Refuses** |

This is not a bug — it's intentional. Reasons:
- **Prompt injection risk**: malicious site could trick LLM into exfiltrating credentials
- **Context window leakage**: credentials could be extracted via crafted follow-up prompts
- **Training data contamination**: conversation logs with credentials could leak into model weights
- **Liability**: providers don't want to be credential custodians

**This means any browser tool that sends page state to the LLM will hit this wall at every login form.** The auth problem must be solved at the infrastructure layer, not the AI layer.

### 2.2 Solution Landscape

#### OAuth Delegation (Best for API-based services)
The gold standard — agent never sees passwords:
1. Agent generates authorization URL
2. User authenticates directly with the service (in their own browser)
3. Agent receives scoped access token + refresh token
4. Agent operates with the token; refreshes when expired

**Device Authorization Grant (RFC 8628)** is ideal for Tem's messaging-first context — send user a link + code via Telegram, they authorize on their phone, Tem receives tokens.

Limitations: Only works for services with OAuth APIs. Amazon, most e-commerce, many consumer sites have no OAuth for "browse and act as user."

#### Cookie/Session Injection (Best for non-OAuth sites)
1. User logs into target site in their own browser
2. Cookies exported (via extension or devtools)
3. Cookies injected into agent's Playwright browser context
4. Agent browses as the authenticated user

Playwright makes this clean:
```javascript
// Save after manual login
await context.storageState({ path: 'auth.json' });
// Restore in agent's browser
const context = await browser.newContext({ storageState: 'auth.json' });
```

Limitation: Sessions expire, some sites bind sessions to IP/fingerprint, cookie export is technical.

#### Vault + DOM Injection (Strongest separation — recommended for Tem)
1. LLM identifies a login form (recognizes the page layout)
2. LLM signals "I need credentials for service X" to the orchestration layer
3. **Orchestration layer** (not LLM) retrieves encrypted creds from vault, decrypts
4. DOM injection fills fields directly via `page.fill()` — **LLM never sees credential values**
5. LLM continues from post-login state

This maps perfectly to Tem's architecture:
- Vault already exists (`temm1e-vault` with ChaCha20-Poly1305)
- User sends credentials via Telegram → stored encrypted in vault with service name
- Browser tool encounters login → tool layer retrieves from vault → injects via Playwright
- LLM's context only shows: "Credentials for [service] were injected. Page now shows [post-login state]."

#### MCP OAuth (Best for service integrations)
MCP spec includes OAuth 2.0 support:
1. MCP server declares auth requirements in manifest
2. MCP client (Tem) initiates OAuth flow
3. User clicks link in Telegram, authorizes
4. Tokens stored in vault, used for subsequent MCP tool calls

Tem already has MCP support — adding MCP OAuth is the cleanest path for authenticated web services.

#### Operator-Style Handoff (Good UX, limited to visual interaction)
Agent pauses at login, hands control to user, resumes after auth. Works well when there's a visual interface. For Tem's messaging context, this translates to: "I need you to log in. Here's a link to the browser session — please log in and tell me when you're done."

### 2.3 Innovative Approaches

#### Anon (anon.com) — "OAuth for the rest of the internet"
- Provides OAuth-like token delegation for services WITHOUT OAuth (LinkedIn, Instagram, Uber, DoorDash)
- User authenticates once via Anon's browser extension
- Anon maintains session, provides API for agents
- Agent never sees credentials
- Risk: Anon becomes a high-value target

#### Credential Proxy Pattern
- Dedicated microservice between agent and target website
- Agent sends high-level requests ("check my bank balance")
- Proxy handles ALL auth independently
- Returns structured data — LLM never touches auth
- Downside: requires building a proxy per service

#### WebAuthn/Passkeys for Agents (Emerging)
- Agents could have their own FIDO2 passkeys
- User authorizes a passkey for the agent on a specific service
- Agent authenticates using passkey — no password involved
- Status: theoretical, WebAuthn spec doesn't yet accommodate non-human authenticators

#### Trusted Execution Environments (TEEs)
- Run credential-handling code in Intel SGX / AWS Nitro Enclaves
- LLM runs outside TEE, credentials stay inside hardware isolation
- Even if LLM is compromised, credentials are protected
- Status: some enterprise deployments, high operational complexity

### 2.4 Auth Strategy Summary

| Approach | LLM sees creds? | Works headless? | Setup complexity | Best for |
|----------|-----------------|----------------|------------------|----------|
| OAuth delegation | No | Yes | Medium | API-based services |
| Device Auth Grant | No | Yes | Low | Messaging-first agents (Tem!) |
| Cookie injection | No | Yes | High | Non-OAuth sites |
| Vault + DOM injection | No | Yes | Medium | **Primary recommendation** |
| MCP OAuth | No | Yes | Medium | Service integrations |
| Operator handoff | No | Partial | Low | Visual browser sessions |
| Anon-style proxy | No | Yes | Low | Sites without APIs |

**Core principle: the LLM must NEVER see credential material. Authentication is infrastructure, not AI.**

---

## Part 3: Efficiency and Innovation

### 3.1 The Token Tax

Web browsing is expensive. A single screenshot costs ~1500 tokens. A 20-step task with screenshots = 30,000 vision tokens minimum, often exceeding text token cost.

**Optimization hierarchy (most to least savings):**

| Strategy | Token savings | Tradeoff |
|----------|--------------|----------|
| Accessibility tree instead of screenshots | 80-95% | Loses visual layout |
| Low-resolution screenshots (512px) | 60-70% | May miss small text |
| DOM extraction + selective vision | 70-90% | Implementation complexity |
| Context summarization | 40-60% | Potential info loss |
| Action macros (batch steps) | 30-50% fewer LLM calls | Reduced flexibility |
| Viewport cropping (element-level screenshots) | 60-80% | Narrow field of view |

### 3.2 Action Efficiency

**Reactive** (one action per LLM call): Simple but expensive — WebVoyager-style.

**Hierarchical planning** (decompose → execute): 30-50% fewer LLM calls. Planner decomposes task into subtasks, executor handles individual actions. Used by Agent-E, WebPilot.

**Action macros**: "Search for X" = click search box + type query + press Enter. One LLM decision instead of three. Browser-use and Stagehand both implement this.

**Pre-computed templates**: For common patterns (login, search, pagination), skip the LLM entirely and use hardcoded action sequences. LLM only reasons about novel situations.

### 3.3 Model Tiering

**This maps directly to Tem's existing complexity classification:**
- Simple actions (click known button, type text) → cheap model (Haiku, GPT-4o-mini, Gemini Flash)
- Complex reasoning (ambiguous forms, navigation decisions) → capable model (Opus, GPT-4o, Gemini Pro)
- Typical cost reduction: 50-70% with minimal accuracy loss

### 3.4 Caching Strategies

- **Incremental DOM diffing**: After each action, send only the DOM delta, not the full tree. 40-70% context reduction on minor state changes.
- **Element index persistence**: Stable numeric IDs for interactive elements across steps — no re-identification needed.
- **Session context compression**: Summarize earlier steps ("I navigated to Amazon, searched for 'headphones', now on results page") replaces 5-10 detailed observation-action pairs.
- **Selector caching**: For repeated page patterns, cache CSS selectors/schemas after first LLM extraction and reuse.
- **Prompt caching**: Anthropic and OpenAI both support prompt caching — 80-90% savings on cached prefix tokens. Critical for web agents with large, constant system prompts.

### 3.5 Error Recovery

- **Retry with variation**: Failed click → try alternative selector, scroll, wait for dynamic content
- **Fallback to JS execution**: `page.evaluate(() => document.querySelector(...).click())`
- **Self-correction loop**: After each action, evaluate if page state matches expectations. If not, reason about what went wrong. +20-40% task completion in benchmarks.
- **CAPTCHA escalation**: Detect CAPTCHA → hand off to user
- **Anti-bot mitigation**: Realistic fingerprints (puppeteer-extra-plugin-stealth), human-like mouse movements, realistic timing

### 3.6 Benchmark Reality Check

| Benchmark | What it measures | Best AI score | Human score | Gap |
|-----------|-----------------|--------------|-------------|-----|
| **WebArena** | End-to-end tasks on realistic sites | ~45-55% | ~78% | Massive |
| **Mind2Web** | Generalization to unseen websites | ~40-50% step success | N/A | Generalization is hard |
| **VisualWebArena** | Tasks requiring visual understanding | ~20-30% | N/A | Vision-only is not enough |
| **WorkArena** | Enterprise software tasks | ~15-30% | N/A | Complex workflows fail |
| **OSWorld** | General computer use | ~12-20% | N/A | Hardest benchmark |

**Key findings:**
- No agent has consistently exceeded 55% on realistic web tasks — there's a ceiling
- Error compounding: 90% per-step accuracy across 10 steps = 35% end-to-end success
- Vision-capable agents outperform text-only by 10-20% but at 3-5x the cost
- Planning + self-correction significantly outperforms reactive agents

---

## Part 4: Vision for Tem Browse

### 4.1 What Makes Tem Different

Most web agents are designed for one of two contexts:
1. **Desktop copilot** — runs in user's browser (Mariner, Copilot Vision)
2. **Cloud task executor** — runs headless in the cloud (Operator, browser-use)

Tem is neither. Tem is a **messaging-first agent** where the user interacts via Telegram/Discord/Slack. This creates unique constraints AND opportunities:

**Constraints:**
- No user browser to piggyback on (unlike Mariner)
- No live viewport for user observation (unlike Operator)
- User interaction is text/media messages, not mouse clicks
- Must report web browsing results in a messaging-friendly format

**Opportunities:**
- **Async by nature**: User sends "book me a flight to Tokyo" and goes about their day. Tem works in the background and reports back. No one stares at a browser.
- **Multi-service orchestration**: Tem can browse multiple sites in parallel behind the scenes — the user just sees the result.
- **Rich media reporting**: Screenshots, extracted data, summaries — all sent back as messages.
- **Credential vault integration**: Users already trust Tem with API keys via `/addkey`. Extending to web credentials is natural.

### 4.2 Proposed Architecture

```
User (Telegram/Discord/Slack)
  │
  ▼
Channel → Gateway → Agent Runtime
                       │
                       ├─ Task Planner (decomposes web task)
                       │
                       ├─ Browser Pool (Playwright instances)
                       │    ├─ Context 1 (authenticated: Gmail)
                       │    ├─ Context 2 (authenticated: GitHub)
                       │    └─ Context 3 (anonymous browsing)
                       │
                       ├─ Auth Layer (NEVER touches LLM)
                       │    ├─ Vault credentials → DOM injection
                       │    ├─ OAuth token management
                       │    ├─ Session/cookie storage
                       │    └─ MCP OAuth for service integrations
                       │
                       ├─ Observation Layer
                       │    ├─ L1: Accessibility tree (default)
                       │    ├─ L2: Targeted DOM extraction
                       │    └─ L3: Selective screenshots (fallback)
                       │
                       └─ Reporter (formats results for messaging)
                            ├─ Structured data → message cards
                            ├─ Key screenshots → image messages
                            └─ Summaries → text messages
```

### 4.3 Auth Strategy for Tem

**Tier 1 — OAuth Device Flow (immediate)**
For services with OAuth APIs (Google, GitHub, Slack, etc.):
- Tem sends user a link + code via Telegram
- User authorizes on their phone
- Tem receives tokens, stores in vault
- Zero credential exposure

**Tier 2 — Vault + DOM Injection (primary)**
For services without OAuth:
- User sends credentials via `/addcred service_name`
- Tem encrypts and stores in vault (ChaCha20-Poly1305, already exists)
- When browser encounters login form, tool layer injects from vault
- LLM only sees: "Authenticated to [service]. Current page: [description]"

**Tier 3 — Session Import**
For power users:
- Export cookies from their browser
- Send to Tem via file upload
- Tem loads into Playwright browser context
- Works for any site, no credential storage needed

**Tier 4 — MCP OAuth**
For structured service integrations:
- MCP servers declare auth requirements
- Tem handles OAuth flow via messaging channel
- Tokens managed per-MCP-server in vault

### 4.4 Efficiency Strategy for Tem

**Observation hierarchy aligned with complexity classification:**
- Simple tasks (extract text, click link) → accessibility tree only (100-500 tokens)
- Medium tasks (fill forms, navigate SPAs) → accessibility tree + targeted DOM (500-2000 tokens)
- Complex tasks (visual verification, layout-dependent) → add selective screenshots (2000-4000 tokens)

**Model tiering (already built):**
- Simple web actions → cheap model
- Complex web reasoning → capable model
- Budget tracking integrates naturally

**Async advantage:**
- User doesn't watch the browser → no need for real-time screenshots
- Agent can retry, backtrack, and optimize without UX pressure
- Can batch-process multi-site tasks in parallel

### 4.5 Innovation Opportunities

1. **"Login for me" flow**: Tem sends a one-time browser link (like the existing OTK setup page). User logs into the target site in that browser session. Tem captures the authenticated session. User never shares a password — they log in themselves in an ephemeral browser.

2. **Web action recording**: User demonstrates a workflow once (via the linked browser). Tem records it, parameterizes it, and can replay with variations. "Book me a flight" becomes a template after one demonstration.

3. **Hybrid API+Browser**: When a site has an API, use the API (faster, cheaper, more reliable). When it doesn't, fall back to browser. Many sites have undocumented APIs that can be discovered via network interception.

4. **Cross-platform session sync**: User logs into Chrome on their phone → sessions sync to Google account → Tem can use those sessions server-side (with user permission). Builds on Mariner's insight without requiring Chrome desktop.

5. **Progressive disclosure in messaging**: Instead of dumping a full webpage, Tem sends a summary with expandable sections. "I found 3 flights. Cheapest: $450 JAL direct. Want details on the others?" Messaging-native UX.

---

## Part 5: Competitive Landscape Summary

| Product | Architecture | Auth approach | Strengths | Weaknesses | Tem's advantage |
|---------|-------------|---------------|-----------|------------|-----------------|
| Operator | Cloud browser, hybrid vision+DOM | User handoff + 1Password | Managed infra, human-in-loop | Cloud-only, privacy concerns, expensive | Tem is self-hosted, messaging-native |
| Computer Use | Desktop screenshots, pure vision | Hard refusal | Universal, any app | Very expensive, slow, coordinate imprecision | Tem can use accessibility trees (10x cheaper) |
| Mariner | Chrome extension, user's browser | Inherits user sessions | Best auth UX, zero setup | Chrome-only, not headless, not server-side | Tem runs headless, multi-platform |
| browser-use | Playwright, vision+DOM | Cookie injection | Open source, multi-tab | High token cost, fragile on SPAs | Tem adds auth layer + messaging UX |
| Stagehand | Playwright, DOM-first | Session replay | Efficient, composable | Tied to Browserbase | Tem is self-hosted, provider-agnostic |

---

## Appendix: Key Technologies to Evaluate

- **Playwright** (Node.js/Python) — browser automation, accessibility tree, multi-context
- **Firecrawl** — HTML→Markdown for cheap content extraction
- **Chromium headless** — server-side browser for Tem's infra
- **1Password Connect / Bitwarden Secrets Manager** — external vault integration
- **MCP OAuth spec** — standardized auth for tool integrations
- **Set-of-Mark prompting** — numbered bounding boxes for vision-based interaction
- **puppeteer-extra-plugin-stealth** — anti-detection for automated browsers

---

*Research compiled from three parallel deep-dive agents covering web-native architectures, authentication patterns, and efficiency optimization. March 2026.*
