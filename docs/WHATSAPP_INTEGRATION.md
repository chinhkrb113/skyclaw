# WhatsApp Channel Integration — Research & Architecture

> **Status**: Research complete — awaiting decision
> **Date**: 2026-03-22
> **Target**: v3.3.0+

---

## Executive Summary

WhatsApp has 2B+ users and is the dominant messaging platform in Latin America, South Asia, Africa, and parts of Europe. Adding WhatsApp as a channel would significantly expand TEMM1E's reach.

**However**, there is a critical policy blocker: as of **January 15, 2026**, Meta prohibits general-purpose AI chatbots on WhatsApp Business API. TEMM1E would need to be positioned as a business-specific tool (customer support, order management, etc.) rather than an open-ended AI assistant.

**Recommended approach**: WhatsApp Business Cloud API with direct `reqwest` + `serde` implementation (no third-party crate dependency). The API surface is small (~5 core endpoints) compared to Telegram (100+) or Discord (complex gateway).

---

## Table of Contents

1. [WhatsApp Business Cloud API](#1-whatsapp-business-cloud-api)
2. [On-Premise API (Sunset)](#2-on-premise-api-sunset)
3. [Rust Crates & Third-Party Options](#3-rust-crates--third-party-options)
4. [Unofficial Approaches](#4-unofficial-approaches)
5. [Architecture Design](#5-architecture-design)
6. [Compliance & Legal](#6-compliance--legal)
7. [Comparison with Telegram & Discord](#7-comparison-with-telegram--discord)
8. [Recommendation & Phased Plan](#8-recommendation--phased-plan)

---

## 1. WhatsApp Business Cloud API

Meta's official cloud-hosted API. Meta runs the infrastructure; TEMM1E communicates via REST (HTTPS) calls to the Graph API and receives inbound messages via webhooks.

### Setup Requirements

1. Create a Meta Business Account and verify the business
2. Create an App in Meta for Developers dashboard
3. Add the WhatsApp product to the app
4. Register a phone number (can use Meta's test number initially)
5. Generate a System User access token (permanent)
6. Configure webhook URL with a verify token
7. Subscribe to the `messages` webhook field

### Core API

**Base endpoint**: `https://graph.facebook.com/v23.0/{PHONE_NUMBER_ID}/messages`

**Authentication**: Bearer token (`Authorization: Bearer <ACCESS_TOKEN>`)

**Required permissions**: `whatsapp_business_management`, `whatsapp_business_messaging`, `whatsapp_business_manage_events`

| Operation | Method | Endpoint |
|-----------|--------|----------|
| Send message | POST | `/{PHONE_NUMBER_ID}/messages` |
| Upload media | POST | `/{PHONE_NUMBER_ID}/media` |
| Get media URL | GET | `/{MEDIA_ID}` |
| Download media | GET | `{MEDIA_URL}` (temp URL, valid 5 min) |
| Mark as read | POST | `/{PHONE_NUMBER_ID}/messages` |

**Send message payload**:
```json
{
  "messaging_product": "whatsapp",
  "recipient_type": "individual",
  "to": "<PHONE_NUMBER_E164>",
  "type": "text",
  "text": { "body": "Hello" }
}
```

### Webhook Format

**Verification (GET)**: Meta sends `hub.mode`, `hub.verify_token`, `hub.challenge`. Validate token, echo back `hub.challenge` with HTTP 200.

**Inbound message (POST)**:
```json
{
  "object": "whatsapp_business_account",
  "entry": [{
    "changes": [{
      "field": "messages",
      "value": {
        "messaging_product": "whatsapp",
        "metadata": { "display_phone_number": "...", "phone_number_id": "..." },
        "contacts": [{ "profile": { "name": "..." }, "wa_id": "..." }],
        "messages": [{
          "from": "<PHONE_NUMBER>",
          "id": "<MESSAGE_ID>",
          "timestamp": "...",
          "type": "text",
          "text": { "body": "message content" }
        }]
      }
    }]
  }]
}
```

**Requirements**: Valid TLS certificate (self-signed NOT accepted), publicly accessible HTTPS URL.

### Media Limits

| Type | Formats | Max Size |
|------|---------|----------|
| Audio | AAC, MP3, OGG (OPUS) | 16 MB |
| Image | JPEG, PNG | 5 MB |
| Video | MP4, 3GP (H.264) | 16 MB |
| Document | PDF, DOCX, XLSX, etc. | 100 MB |
| Sticker | WebP | 100–500 KB |

### Rate Limits

- **Throughput**: Default 80 msg/s per phone number, upgradable to 1,000 msg/s
- **Daily conversation caps** (business portfolio level):
  - New/unverified: 250/24h
  - Tier 1: 2,000/24h → Tier 2: 10,000/24h → Tier 3: 100,000/24h → Unlimited
- Auto-upgrade: use 50%+ of limit in 7 days with high quality

### Pricing (Post July 1, 2025)

| Category | Charged? | Notes |
|----------|----------|-------|
| **Service** (user-initiated, within 24h window) | **FREE** | No charge for replies within service window |
| **Utility** (order confirmations, shipping) | Per message | Volume-based tier discounts |
| **Authentication** (OTPs, logins) | Per message | Volume-based tier discounts |
| **Marketing** (promotions, product recs) | Per message | Always charged, region-dependent |

Example marketing rates: North America ~$0.025/msg, Germany ~$0.14/msg, Brazil ~$0.06/msg.

### 24-Hour Service Window

This is the most important architectural constraint:
- **User messages first** → 24h free reply window opens
- **Business initiates** → MUST use an approved template message (costs money, requires Meta approval)
- **Ad-click originating** → 72h free window

For an AI agent: the typical pattern is user messages first, bot replies freely within the window. If the window expires and the bot needs to reach out, a pre-approved template is required.

---

## 2. On-Premise API (Sunset)

**Status: DEAD as of October 23, 2025.**

- Final version: v2.53 (January 2024)
- No new signups after July 1, 2024
- Client expired October 23, 2025
- Migration to Cloud API mandatory

**Zero reason to consider this.**

---

## 3. Rust Crates & Third-Party Options

### Rust Crates (crates.io)

| Crate | Version | Downloads | Notes |
|-------|---------|-----------|-------|
| `whatsapp-cloud-api` | 0.5.4 | 31K | Longest-lived. Graph API v20.0. 28 stars. |
| `whatsapp-business-rs` | 0.5.0 | 2.8K | Most feature-rich. Built-in webhook server, batch processing, multi-tenant. Tokio-based. |
| `whatsapp_handler` | 0.2.0 | 1K | Webhook processing + sending. Recently updated (Mar 2026). |
| `whatsapp-rust` | 0.4.3 | 5K | **Unofficial WA Web protocol** (NOT Cloud API). Pure Rust, Signal Protocol. See Section 4. |

**Assessment**: All Cloud API crates are thin wrappers with low adoption (<50 commits each). The API surface is small enough (~5 endpoints) that a direct `reqwest` + `serde` implementation is cleaner, avoids dependency risk, and gives full control — matching how TEMM1E handles other integrations.

### Third-Party Services

| Service | Type | Notes |
|---------|------|-------|
| **Twilio** | Official BSP | Meta fees + $0.005/msg markup. Full compliance. |
| **360dialog** | Official BSP | Meta fees + subscription. CRM integrations. |
| **GREEN-API** | Unofficial | ~$6/mo per session. No templates needed. **Account ban risk.** |

### Baileys (Node.js) as Sidecar

[WhiskeySockets/Baileys](https://github.com/WhiskeySockets/Baileys) — most mature unofficial WA Web library. Could run as a Node.js sidecar with REST API wrapper. Adds operational overhead (Node.js runtime, process management, IPC latency). Not recommended for production.

---

## 4. Unofficial Approaches

### whatsapp-rust (Pure Rust)

[github.com/jlucaso1/whatsapp-rust](https://github.com/jlucaso1/whatsapp-rust) — 494 stars, 740 commits, v0.4.3 (March 2026).

Pure Rust reimplementation of WhatsApp Web multi-device protocol. Modular design with pluggable storage and transport backends. Maps well to TEMM1E's trait-based architecture.

ZeroClaw already has an open issue ([#965](https://github.com/zeroclaw-labs/zeroclaw/issues/965)) for adding WhatsApp Web channel via this crate.

### whatsmeow (Go)

[github.com/tulir/whatsmeow](https://github.com/tulir/whatsmeow) — gold standard for unofficial WA Web. Would require Go sidecar.

### Risks

1. **Account ban**: Meta actively detects and bans accounts using unofficial clients
2. **Terms of Service violation**: Explicitly prohibited, legal liability
3. **DMCA exposure**: Meta has issued cease-and-desist orders
4. **Reliability**: Protocol changes can break clients with no notice
5. **No recourse**: Banned numbers cannot be recovered

**Verdict**: Unsuitable for production. Could be offered as an experimental `whatsapp-web` channel with clear warnings, similar to ZeroClaw's approach.

---

## 5. Architecture Design

### Channel Trait Implementation

```rust
pub struct WhatsAppChannel {
    phone_number_id: String,
    access_token: String,        // System User token (long-lived)
    verify_token: String,        // Webhook verification secret
    api_version: String,         // e.g., "v23.0"
    http_client: reqwest::Client,
    allowlist: Arc<RwLock<Vec<String>>>,   // Phone numbers (E.164)
    admin: Arc<RwLock<Option<String>>>,
    tx: mpsc::Sender<InboundMessage>,
    rx: Option<mpsc::Receiver<InboundMessage>>,
    shutdown: Arc<AtomicBool>,
    // 24h service window tracking
    last_inbound: Arc<RwLock<HashMap<String, Instant>>>,
}
```

### Webhook vs Polling

**Webhook is the only option.** WhatsApp Cloud API does not support long-polling (unlike Telegram). The TEMM1E gateway must be publicly accessible with valid TLS.

This means:
- Gateway must be publicly reachable (or behind a reverse proxy/tunnel)
- Valid TLS certificate required (Let's Encrypt works)
- Webhook verification handshake must be handled at startup
- Closest analog in our codebase: Slack (also webhook-based)

### Message Flow

```
Meta Cloud API → HTTPS POST /webhook/whatsapp
  → axum handler verifies signature (X-Hub-Signature-256)
  → Parse webhook payload → extract message
  → Check allowlist (phone number)
  → Check 24h window (for reply type selection)
  → tx.send(InboundMessage) → Gateway router → Agent loop
  → Agent response → WhatsAppChannel.send_message()
  → POST https://graph.facebook.com/v23.0/{PHONE_NUMBER_ID}/messages
```

### Integration Touchpoints

| File | Change |
|------|--------|
| `crates/temm1e-channels/src/whatsapp.rs` | New — Channel + FileTransfer impl |
| `crates/temm1e-channels/Cargo.toml` | Add `whatsapp` feature flag |
| `crates/temm1e-channels/src/lib.rs` | Add `"whatsapp"` to `create_channel()` |
| `crates/temm1e-core/src/types/config.rs` | Add `[channel.whatsapp]` config section |
| `src/main.rs` | Wire webhook route, channel init |
| `temm1e.toml` | Add example `[channel.whatsapp]` |

### Config Schema

```toml
[channel.whatsapp]
enabled = true
phone_number_id = "${WHATSAPP_PHONE_NUMBER_ID}"
access_token = "${WHATSAPP_ACCESS_TOKEN}"
verify_token = "${WHATSAPP_VERIFY_TOKEN}"
api_version = "v23.0"
allowlist = []                    # Empty = first user auto-whitelisted
file_transfer = true
# webhook_path is auto-set to /webhook/whatsapp on the gateway
```

### Key Architectural Differences from Telegram/Discord

| Aspect | Telegram | Discord | WhatsApp |
|--------|----------|---------|----------|
| Connection | Polling OR webhook | WebSocket gateway | Webhook ONLY |
| User ID | Numeric | Snowflake | Phone number (E.164) |
| Initiation | Bot can message anytime | Bot can message anytime | Templates required outside 24h |
| Setup | Instant (BotFather) | Instant (Dev Portal) | Business verification (days) |

---

## 6. Compliance & Legal

### January 2026 AI Policy — CRITICAL BLOCKER

**Effective January 15, 2026**, Meta prohibits "general-purpose AI chatbots" on WhatsApp Business API:

- **BANNED**: AI model providers using WhatsApp as a distribution channel. Bots that answer arbitrary questions or serve as general conversation companions.
- **ALLOWED**: Business-specific AI chatbots for customer support, order tracking, booking, lead qualification, authentication.
- **The test**: "The chatbot's role must be ancillary to a legitimate business service, not the centerpiece."

**Impact on TEMM1E**: An open-ended AI agent on WhatsApp would violate this policy. The WhatsApp channel must be positioned as a business-specific tool, not a general AI assistant. This is a **significant constraint** that does not apply to Telegram or Discord.

### Other Requirements

- Business must be verified through Meta Business Manager
- Explicit user opt-in required before business-initiated messages
- Double opt-in best practice (provide number → confirmation → active confirm)
- GDPR compliant (Meta acts as data processor under Art. 28)
- Messages retained on Meta servers max 30 days for delivery
- **Blocked countries**: Cuba, Iran, North Korea, Syria, sanctioned Ukrainian regions

---

## 7. Comparison with Telegram & Discord

### What's Harder About WhatsApp

1. **Business verification gate** — days/weeks before production messaging
2. **Template approval** — business-initiated messages need pre-approved templates
3. **24-hour window** — cannot freely message users outside the service window
4. **Webhook-only** — must expose public HTTPS endpoint (no polling fallback)
5. **AI policy** — general-purpose AI agents explicitly banned
6. **Per-message costs** — template messages have real dollar cost
7. **No mature Rust SDK** — must build more infra vs teloxide/serenity
8. **Phone number requirement** — dedicated number, not registered on WA consumer

### What's Easier About WhatsApp

1. **Simple API surface** — ~5 core endpoints vs Telegram's 100+ or Discord's complex gateway
2. **Large file support** — 100 MB documents (vs 50 MB Telegram, 25 MB Discord)
3. **Massive user base** — 2B+ users, dominant in many markets
4. **Built-in encryption** — transparent (Cloud API handles it)
5. **High throughput** — up to 1,000 msg/s (service messages within 24h are free)

---

## 8. Recommendation & Phased Plan

### Primary: WhatsApp Business Cloud API (Direct Implementation)

Build `WhatsAppChannel` implementing `Channel` + `FileTransfer` traits using `reqwest` for HTTP and the existing `axum` gateway for webhook reception.

**Do NOT** use unofficial `whatsapp-rust` for production (ban risk).
**Do NOT** depend on `whatsapp-business-rs` or similar crates (too immature).

### Phased Rollout

| Phase | Scope | Effort |
|-------|-------|--------|
| **Phase 1** | Text send/receive, webhook handling, allowlist, 24h window tracking | ~2 days |
| **Phase 2** | Media handling (images, documents, audio, video) via FileTransfer trait | ~1 day |
| **Phase 3** | Template message support for business-initiated conversations | ~1 day |
| **Phase 4** | Interactive messages (buttons, lists) for structured interactions | ~1 day |

### Decision Points Needed

1. **Policy compliance**: How do we position TEMM1E on WhatsApp? Business-specific tool only? This limits the use case significantly.
2. **Business verification**: Who registers the Meta Business Account? Individual users or TEMM1E Labs?
3. **Template strategy**: What default templates do we pre-register for common agent patterns?
4. **Unofficial channel**: Do we also ship an experimental `whatsapp-web` channel using `whatsapp-rust` with clear ToS warnings? (ZeroClaw is considering this.)
5. **Priority**: Given the policy blocker, should WhatsApp implementation wait until the policy landscape clarifies?

---

## Sources

- [WhatsApp Cloud API — Meta for Developers](https://developers.facebook.com/docs/whatsapp/cloud-api/)
- [Messages API Reference](https://developers.facebook.com/docs/whatsapp/cloud-api/reference/messages/)
- [Webhook Setup Guide](https://developers.facebook.com/docs/whatsapp/cloud-api/guides/set-up-webhooks/)
- [Pricing Updates July 2025](https://developers.facebook.com/docs/whatsapp/pricing/updates-to-pricing/)
- [On-Premises API Sunset](https://developers.facebook.com/docs/whatsapp/on-premises/sunset)
- [WhatsApp Business Policy](https://business.whatsapp.com/policy)
- [General-Purpose Chatbot Ban (TechCrunch)](https://techcrunch.com/2025/10/18/whatssapp-changes-its-terms-to-bar-general-purpose-chatbots-from-its-platform/)
- [whatsapp-rust (GitHub)](https://github.com/jlucaso1/whatsapp-rust)
- [whatsmeow (GitHub)](https://github.com/tulir/whatsmeow)
- [Baileys (GitHub)](https://github.com/WhiskeySockets/Baileys)
