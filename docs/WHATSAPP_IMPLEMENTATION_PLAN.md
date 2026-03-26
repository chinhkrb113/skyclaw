# WhatsApp Implementation Plan — Zero Operational Risk

> **Version**: v3.3.0
> **Date**: 2026-03-22
> **Prerequisite**: [WHATSAPP_INTEGRATION.md](./WHATSAPP_INTEGRATION.md) — research findings
> **Scope**: Two feature-gated channels — `whatsapp` (Cloud API) + `whatsapp-web` (unofficial)

---

## Table of Contents

1. [Zero-Risk Guarantee](#1-zero-risk-guarantee)
2. [Architecture Overview](#2-architecture-overview)
3. [Channel A: WhatsApp Cloud API (`whatsapp`)](#3-channel-a-whatsapp-cloud-api)
4. [Channel B: WhatsApp Web (`whatsapp-web`)](#4-channel-b-whatsapp-web)
5. [Shared Infrastructure](#5-shared-infrastructure)
6. [File Touchpoints](#6-file-touchpoints)
7. [Config Schema](#7-config-schema)
8. [Implementation Order](#8-implementation-order)
9. [Test Plan](#9-test-plan)
10. [Failure Modes & Mitigations](#10-failure-modes--mitigations)
11. [Rollback Plan](#11-rollback-plan)

---

## 1. Zero-Risk Guarantee

"Zero operational risk" means: **no existing TEMM1E functionality can break, degrade, or change behavior** as a result of adding WhatsApp support. Here is how we guarantee this:

### Isolation Mechanisms

| Mechanism | What It Protects | How |
|-----------|-----------------|-----|
| **Feature gates** | Build time | `#[cfg(feature = "whatsapp")]` and `#[cfg(feature = "whatsapp-web")]` — code doesn't compile unless opted in. Default features do NOT include either. Zero impact on existing builds. |
| **Separate module files** | Code isolation | `whatsapp.rs` and `whatsapp_web.rs` are new files. No existing file is modified except `lib.rs` (factory arms behind `#[cfg]`), `Cargo.toml` (optional deps), `config.rs` (new config section), and `main.rs` (channel wiring behind `#[cfg]`). |
| **Independent dependencies** | Dependency tree | `reqwest` (already in workspace) for Cloud API. `whatsapp-rust` is optional dep behind feature flag. Neither replaces nor conflicts with `teloxide` or `serenity`. |
| **Channel trait boundary** | Runtime isolation | WhatsApp channels implement the same `Channel` + `FileTransfer` traits as Telegram/Discord. The agent runtime, memory, tools, providers — none of them know or care which channel is active. |
| **mpsc channel boundary** | Message isolation | Each channel has its own `mpsc::Sender<InboundMessage>` → all feed into the unified `msg_rx`. A dead WhatsApp channel simply stops sending. The gateway processes whatever arrives. |
| **catch_unwind wrapper** | Panic containment | The gateway's `process_message()` is already wrapped in `AssertUnwindSafe + catch_unwind()`. A panic in WhatsApp message handling is caught, logged, and turned into an error reply — not a process crash. |
| **AtomicBool shutdown** | Clean teardown | Each channel has its own `shutdown: Arc<AtomicBool>`. Stopping WhatsApp doesn't signal other channels. |
| **Session key namespacing** | Data isolation | Sessions are keyed by `"channel:chat_id:user_id"`. WhatsApp sessions (`"whatsapp:+15551234567:+15551234567"`) never collide with Telegram sessions (`"telegram:12345:12345"`). |

### What Changes in Existing Code (Minimal)

| File | Change | Risk |
|------|--------|------|
| `crates/temm1e-channels/src/lib.rs` | Add 2 match arms in `create_channel()`, behind `#[cfg]` | **ZERO** — `#[cfg(not(feature))]` compiles to nothing |
| `crates/temm1e-channels/Cargo.toml` | Add 2 optional dependencies + 2 feature flags | **ZERO** — optional deps don't compile unless feature enabled |
| `crates/temm1e-core/src/types/config.rs` | Add `WhatsAppConfig` and `WhatsAppWebConfig` structs | **ZERO** — new structs, no existing struct modified. `ChannelConfig` is already generic enough. |
| `src/main.rs` | Add channel init blocks behind `#[cfg]` | **ZERO** — follows exact pattern of Telegram/Discord init blocks |
| `Cargo.toml` (workspace root) | Add `whatsapp` and `whatsapp-web` to feature list (NOT default) | **ZERO** — not in default features |

### What Does NOT Change

- Telegram channel — untouched
- Discord channel — untouched
- CLI channel — untouched
- Agent runtime — untouched
- Provider layer — untouched
- Memory backends — untouched
- Tool system — untouched
- Gateway router — only additive route (webhook)
- Config loader — only additive sections
- Build pipeline — existing `cargo build --release` produces identical binary

---

## 2. Architecture Overview

```
                    ┌─────────────────────────────────────────┐
                    │              TEMM1E Gateway              │
                    │                                          │
  Telegram ────────▶│  tg_rx ──┐                              │
                    │          │                               │
  Discord ─────────▶│  dc_rx ──┤                              │
                    │          ├──▶ msg_rx ──▶ Agent Runtime   │
  WA Cloud ────────▶│  wa_rx ──┤       (unified)              │
  (webhook POST)    │          │                               │
                    │          │                               │
  WA Web ──────────▶│ waw_rx ──┘                              │
  (WebSocket)       │                                          │
                    └─────────────────────────────────────────┘

  Channel A: WhatsApp Cloud API ("whatsapp")
  ├── Inbound:  Meta webhook POST → /webhook/whatsapp → parse → tx.send()
  ├── Outbound: reqwest POST → graph.facebook.com/v23.0/{PHONE}/messages
  └── Auth:     Bearer token (System User access token, long-lived)

  Channel B: WhatsApp Web ("whatsapp-web")
  ├── Inbound:  whatsapp-rust event stream → on_message → tx.send()
  ├── Outbound: whatsapp-rust send_message()
  └── Auth:     QR code scan → persistent session in auth_dir
```

---

## 3. Channel A: WhatsApp Cloud API

### 3.1 Struct

```rust
// crates/temm1e-channels/src/whatsapp.rs

pub struct WhatsAppChannel {
    // Config
    phone_number_id: String,
    access_token: String,
    verify_token: String,
    api_version: String,                       // "v23.0"

    // Runtime
    http_client: reqwest::Client,
    allowlist: Arc<RwLock<Vec<String>>>,        // Phone numbers (E.164)
    admin: Arc<RwLock<Option<String>>>,
    tx: mpsc::Sender<InboundMessage>,
    rx: Option<mpsc::Receiver<InboundMessage>>,
    shutdown: Arc<AtomicBool>,

    // 24h service window tracking
    last_inbound: Arc<RwLock<HashMap<String, chrono::DateTime<chrono::Utc>>>>,

    // File transfer
    file_transfer_enabled: bool,
}
```

### 3.2 Lifecycle

**`new(config)`**:
1. Extract `phone_number_id`, `access_token`, `verify_token` from config (fail if missing)
2. Create `mpsc::channel(256)`
3. Load allowlist from `~/.temm1e/whatsapp_allowlist.toml` (or config)
4. Build `reqwest::Client` with default headers (`Authorization: Bearer {token}`)
5. Return `Self` with `rx: Some(rx)`

**`start()`**:
- Nothing to spawn for Cloud API — messages arrive via webhook (passive).
- Log: `"WhatsApp Cloud API channel ready (webhook-based, no listener needed)"`
- Validate token by calling `GET /v23.0/{phone_number_id}` — log warning if fails, don't fail start.

**`stop()`**:
- Set `shutdown` flag
- Drop `http_client` (cancels in-flight requests)

### 3.3 Webhook Handler

This lives in the gateway, NOT in the channel crate. The gateway routes HTTP to the channel's `tx`.

```rust
// crates/temm1e-gateway/src/whatsapp_webhook.rs

/// GET /webhook/whatsapp — Meta verification handshake
pub async fn whatsapp_verify(
    Query(params): Query<WhatsAppVerifyParams>,
    State(state): State<Arc<WhatsAppWebhookState>>,
) -> impl IntoResponse {
    if params.hub_mode == "subscribe" && params.hub_verify_token == state.verify_token {
        (StatusCode::OK, params.hub_challenge)
    } else {
        (StatusCode::FORBIDDEN, "Invalid verify token".to_string())
    }
}

/// POST /webhook/whatsapp — Inbound messages from Meta
pub async fn whatsapp_inbound(
    State(state): State<Arc<WhatsAppWebhookState>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // 1. Validate X-Hub-Signature-256 (HMAC-SHA256 of body with app secret)
    if !validate_signature(&headers, &body, &state.app_secret) {
        return StatusCode::UNAUTHORIZED;
    }

    // 2. Parse webhook payload
    let payload: WhatsAppWebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse WhatsApp webhook payload");
            return StatusCode::OK; // Always 200 to Meta (avoid retries)
        }
    };

    // 3. Extract messages and forward to channel tx
    for entry in payload.entry {
        for change in entry.changes {
            if change.field != "messages" { continue; }
            if let Some(messages) = change.value.messages {
                for msg in messages {
                    let inbound = InboundMessage {
                        id: msg.id.clone(),
                        channel: "whatsapp".to_string(),
                        chat_id: msg.from.clone(),     // Phone number = chat ID for DMs
                        user_id: msg.from.clone(),      // Phone number as user ID
                        username: change.value.contacts
                            .as_ref()
                            .and_then(|c| c.first())
                            .and_then(|c| c.profile.as_ref())
                            .map(|p| p.name.clone()),
                        text: extract_text(&msg),
                        attachments: extract_attachments(&msg),
                        reply_to: msg.context.as_ref().map(|c| c.message_id.clone()),
                        timestamp: parse_timestamp(&msg.timestamp),
                    };
                    let _ = state.tx.send(inbound).await; // Non-blocking
                }
            }
        }
    }

    StatusCode::OK // ALWAYS 200 — Meta retries on non-200
}
```

**Critical**: Always return `200 OK` to Meta, even on parse errors. Non-200 causes Meta to retry with exponential backoff and eventually disable the webhook.

### 3.4 Send Message

```rust
async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
    // Check 24h window
    let in_window = {
        let windows = self.last_inbound.read()
            .unwrap_or_else(|p| p.into_inner());
        windows.get(&msg.chat_id)
            .map(|t| chrono::Utc::now() - *t < chrono::Duration::hours(24))
            .unwrap_or(false)
    };

    if !in_window {
        // Outside 24h window — would need a template message.
        // For now, log warning and attempt anyway (Meta will reject if invalid).
        tracing::warn!(
            chat_id = %msg.chat_id,
            "Sending outside 24h service window — may require template"
        );
    }

    // Truncate at WhatsApp's 4096 char limit using char_indices (UTF-8 safe)
    let text = if msg.text.len() > 4096 {
        let boundary = msg.text.char_indices()
            .take_while(|(i, _)| *i < 4096)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(4096);
        &msg.text[..boundary]
    } else {
        &msg.text
    };

    let body = serde_json::json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": msg.chat_id,
        "type": "text",
        "text": { "body": text }
    });

    let url = format!(
        "https://graph.facebook.com/{}/{}/messages",
        self.api_version, self.phone_number_id
    );

    let resp = self.http_client.post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| Temm1eError::Channel(format!("WhatsApp send failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Temm1eError::Channel(
            format!("WhatsApp API {status}: {body}")
        ));
    }

    Ok(())
}
```

### 3.5 File Transfer (Cloud API)

**Receive**: Meta sends media messages with a `media_id`. Two-step download:
1. `GET /v23.0/{media_id}` → returns `{ "url": "https://..." }` (temporary, valid 5 min)
2. `GET {url}` with `Authorization: Bearer` → returns raw bytes

**Send**: Upload first, then reference:
1. `POST /v23.0/{phone_number_id}/media` with `multipart/form-data` → returns `{ "id": "..." }`
2. Send message with `type: "document"` / `"image"` / `"video"` referencing the media ID

**Limits**: Images 5 MB, video/audio 16 MB, documents 100 MB.

### 3.6 Webhook Signature Validation

```rust
fn validate_signature(headers: &HeaderMap, body: &[u8], app_secret: &str) -> bool {
    let sig_header = match headers.get("x-hub-signature-256") {
        Some(v) => v.to_str().unwrap_or(""),
        None => return false,
    };

    // Header format: "sha256=<hex>"
    let expected_hex = match sig_header.strip_prefix("sha256=") {
        Some(hex) => hex,
        None => return false,
    };

    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(body);
    let computed = hex::encode(mac.finalize().into_bytes());

    // Constant-time comparison to prevent timing attacks
    computed.len() == expected_hex.len()
        && computed.bytes().zip(expected_hex.bytes()).all(|(a, b)| a == b)
}
```

**Dependencies**: `hmac`, `sha2`, `hex` — all already in the workspace via `temm1e-vault`.

---

## 4. Channel B: WhatsApp Web

### 4.1 Struct

```rust
// crates/temm1e-channels/src/whatsapp_web.rs

pub struct WhatsAppWebChannel {
    // Config
    auth_dir: PathBuf,                         // ~/.temm1e/state/whatsapp_web/
    dm_policy: DmPolicy,                       // Allowlist | AllowAll | DenyAll
    group_policy: GroupPolicy,                 // Respond | Ignore | MentionOnly

    // Runtime
    client: Option<whatsapp_rust::Client>,
    allowlist: Arc<RwLock<Vec<String>>>,        // Phone numbers (E.164)
    admin: Arc<RwLock<Option<String>>>,
    tx: mpsc::Sender<InboundMessage>,
    rx: Option<mpsc::Receiver<InboundMessage>>,
    listener_handle: Option<tokio::task::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    file_transfer_enabled: bool,

    // QR callback for terminal display
    qr_displayed: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
enum DmPolicy { Allowlist, AllowAll, DenyAll }

#[derive(Debug, Clone)]
enum GroupPolicy { Respond, Ignore, MentionOnly }
```

### 4.2 Lifecycle

**`new(config)`**:
1. Extract `auth_dir` (default: `~/.temm1e/state/whatsapp_web/`)
2. Parse `dm_policy` and `group_policy` from config
3. Create `mpsc::channel(256)`
4. Load allowlist from `~/.temm1e/whatsapp_web_allowlist.toml`
5. Ensure `auth_dir` exists (`std::fs::create_dir_all`)

**`start()`**:
1. Create `whatsapp_rust::Client` with SQLite storage in `auth_dir`
2. Register QR code callback → print to terminal (or log if daemon mode)
3. Connect — if session exists in `auth_dir`, auto-reconnect without QR
4. Spawn listener task:
   ```rust
   let handle = tokio::spawn(async move {
       let mut backoff = Duration::from_secs(1);
       loop {
           if shutdown.load(Ordering::Relaxed) { break; }
           match client.next_event().await {
               Ok(Event::Message(msg)) => {
                   backoff = Duration::from_secs(1); // Reset backoff
                   if let Err(e) = handle_wa_web_message(&msg, &tx, &allowlist, &admin, &dm_policy, &group_policy).await {
                       tracing::error!(error = %e, "WhatsApp Web message handler error");
                   }
               }
               Ok(Event::QrCode(qr)) => {
                   display_qr(&qr);
               }
               Ok(Event::Connected) => {
                   tracing::info!("WhatsApp Web connected");
                   backoff = Duration::from_secs(1);
               }
               Ok(Event::Disconnected(reason)) => {
                   tracing::warn!(reason = %reason, "WhatsApp Web disconnected");
                   if shutdown.load(Ordering::Relaxed) { break; }
                   tokio::time::sleep(backoff).await;
                   backoff = (backoff * 2).min(Duration::from_secs(60));
                   // Attempt reconnect
                   if let Err(e) = client.reconnect().await {
                       tracing::error!(error = %e, "WhatsApp Web reconnect failed");
                   }
               }
               Ok(_) => {} // Status, presence, typing — ignore
               Err(e) => {
                   tracing::error!(error = %e, "WhatsApp Web event stream error");
                   if shutdown.load(Ordering::Relaxed) { break; }
                   tokio::time::sleep(backoff).await;
                   backoff = (backoff * 2).min(Duration::from_secs(60));
               }
           }
       }
   });
   ```
5. Store handle in `self.listener_handle`

**`stop()`**:
1. Set `shutdown` flag
2. Call `client.disconnect()` (graceful logout without invalidating session)
3. Await `listener_handle` with 5s timeout
4. Session persists in `auth_dir` for next start

### 4.3 Message Handling

```rust
async fn handle_wa_web_message(
    msg: &whatsapp_rust::Message,
    tx: &mpsc::Sender<InboundMessage>,
    allowlist: &Arc<RwLock<Vec<String>>>,
    admin: &Arc<RwLock<Option<String>>>,
    dm_policy: &DmPolicy,
    group_policy: &GroupPolicy,
) -> Result<(), Temm1eError> {
    // 1. Skip own messages (the bot IS the user's account)
    if msg.is_from_me {
        return Ok(());
    }

    // 2. Determine if DM or group
    let is_group = msg.chat_jid.contains("@g.us");

    // 3. Apply group policy
    if is_group {
        match group_policy {
            GroupPolicy::Ignore => return Ok(()),
            GroupPolicy::MentionOnly => {
                if !msg.mentioned_jids.contains(&msg.own_jid) {
                    return Ok(());
                }
            }
            GroupPolicy::Respond => {} // Process all group messages
        }
    }

    // 4. Extract sender phone number (user_id)
    let user_id = msg.sender_jid
        .split('@')
        .next()
        .unwrap_or(&msg.sender_jid)
        .to_string();

    // 5. Apply DM policy + allowlist
    match dm_policy {
        DmPolicy::DenyAll if !is_group => return Ok(()),
        DmPolicy::Allowlist => {
            // Auto-whitelist first user
            {
                let mut list = allowlist.write().unwrap_or_else(|p| p.into_inner());
                if list.is_empty() {
                    list.push(user_id.clone());
                    let mut adm = admin.write().unwrap_or_else(|p| p.into_inner());
                    *adm = Some(user_id.clone());
                    tracing::info!(user_id = %user_id, "Auto-whitelisted first WhatsApp Web user as admin");
                    // Persist allowlist
                }
            }
            let list = allowlist.read().unwrap_or_else(|p| p.into_inner());
            if !list.iter().any(|a| a == &user_id || a == "*") {
                return Ok(());
            }
        }
        DmPolicy::AllowAll | DmPolicy::Allowlist => {} // AllowAll skips check
        _ => {}
    }

    // 6. Build InboundMessage
    let inbound = InboundMessage {
        id: msg.id.clone(),
        channel: "whatsapp-web".to_string(),
        chat_id: msg.chat_jid.clone(),
        user_id,
        username: msg.push_name.clone(),
        text: msg.text_body().map(String::from),
        attachments: extract_wa_web_attachments(msg),
        reply_to: msg.quoted_message_id.clone(),
        timestamp: msg.timestamp,
    };

    tx.send(inbound).await
        .map_err(|_| Temm1eError::Channel("WhatsApp Web inbound receiver dropped".into()))?;

    Ok(())
}
```

### 4.4 Send Message

```rust
async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
    let client = self.client.as_ref()
        .ok_or_else(|| Temm1eError::Channel("WhatsApp Web client not connected".into()))?;

    // UTF-8 safe truncation (WhatsApp max ~65536 chars for Web)
    let text = safe_truncate(&msg.text, 65536);

    client.send_text_message(&msg.chat_id, text)
        .await
        .map_err(|e| Temm1eError::Channel(format!("WhatsApp Web send failed: {e}")))?;

    Ok(())
}
```

### 4.5 QR Code Display

```rust
fn display_qr(qr_data: &str) {
    // Use qrcode crate to render in terminal
    use qrcode::QrCode;

    if let Ok(code) = QrCode::new(qr_data) {
        let string = code.render::<char>()
            .quiet_zone(true)
            .module_dimensions(2, 1)
            .build();
        println!("\n{string}");
        println!("Scan this QR code with WhatsApp on your phone");
        println!("(Settings → Linked Devices → Link a Device)\n");
    } else {
        tracing::error!("Failed to generate QR code — scan manually: {qr_data}");
    }
}
```

### 4.6 Session Persistence

The `whatsapp-rust` crate handles session persistence via its SQLite storage backend. Sessions are stored in `auth_dir`:

```
~/.temm1e/state/whatsapp_web/
├── session.db          # Signal Protocol keys, device identity
├── session.db-wal      # WAL journal
└── session.db-shm      # Shared memory
```

On restart, if `session.db` exists and is valid, the client reconnects without requiring a new QR scan. If the session is invalidated (user removed linked device), the client emits a `QrCode` event for re-authentication.

---

## 5. Shared Infrastructure

### 5.1 Common Types (in temm1e-channels, shared by both)

```rust
// crates/temm1e-channels/src/whatsapp_common.rs

/// Phone number normalization (E.164 format)
pub fn normalize_phone(phone: &str) -> String {
    phone.chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect()
}

/// UTF-8 safe string truncation (resilience rule — never &str[..N])
pub fn safe_truncate(text: &str, max_chars: usize) -> &str {
    if text.chars().count() <= max_chars {
        return text;
    }
    let boundary = text.char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    &text[..boundary]
}

/// Sanitize filename for path traversal prevention
pub fn sanitize_filename(name: &str) -> String {
    std::path::Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string()
}
```

### 5.2 Allowlist Persistence

Both channels use the same persistence format:

```toml
# ~/.temm1e/whatsapp_allowlist.toml (or whatsapp_web_allowlist.toml)
admin = "+15551234567"
users = ["+15551234567", "+15559876543"]
```

Load/save functions in `crates/temm1e-channels/src/allowlist.rs` (already exists for Telegram/Discord — reuse).

### 5.3 Ambiguous Config Detection

If both `[channel.whatsapp]` and `[channel.whatsapp_web]` are enabled, log a warning:

```rust
if wa_enabled && waw_enabled {
    tracing::warn!(
        "Both WhatsApp Cloud API and WhatsApp Web are enabled. \
         Messages will be processed by BOTH channels. \
         Consider disabling one to avoid duplicate responses."
    );
}
```

This is a warning, not an error — some users may intentionally run both (different phone numbers).

---

## 6. File Touchpoints

### New Files (created from scratch)

| File | Purpose | Lines (est.) |
|------|---------|-------------|
| `crates/temm1e-channels/src/whatsapp.rs` | Cloud API channel impl | ~500 |
| `crates/temm1e-channels/src/whatsapp_web.rs` | WhatsApp Web channel impl | ~600 |
| `crates/temm1e-channels/src/whatsapp_common.rs` | Shared utilities | ~100 |
| `crates/temm1e-gateway/src/whatsapp_webhook.rs` | Webhook handler + verification | ~200 |

### Modified Files (minimal, behind `#[cfg]`)

| File | Change | Lines Changed |
|------|--------|--------------|
| `crates/temm1e-channels/src/lib.rs` | Add `mod whatsapp`, `mod whatsapp_web`, `mod whatsapp_common`, 2 factory arms | ~15 |
| `crates/temm1e-channels/Cargo.toml` | Add `reqwest` (optional), `whatsapp-rust` (optional), `qrcode` (optional), 2 feature flags | ~10 |
| `crates/temm1e-core/src/types/config.rs` | Add `WhatsAppConfig` struct (new, no existing modified) | ~20 |
| `crates/temm1e-gateway/src/server.rs` | Add webhook route registration behind `#[cfg]` | ~10 |
| `crates/temm1e-gateway/src/lib.rs` | Add `mod whatsapp_webhook` behind `#[cfg]` | ~3 |
| `crates/temm1e-gateway/Cargo.toml` | Add `hmac`, `sha2`, `hex` optional deps | ~5 |
| `src/main.rs` | Add channel init blocks behind `#[cfg]` (following Telegram/Discord pattern) | ~40 |
| `Cargo.toml` (root) | Add `whatsapp` and `whatsapp-web` feature flags | ~5 |

**Total lines modified in existing files**: ~108 (all behind `#[cfg]` gates)
**Total new code**: ~1,400 lines across 4 new files

---

## 7. Config Schema

### Cloud API Config

```toml
[channel.whatsapp]
enabled = true
phone_number_id = "${WHATSAPP_PHONE_NUMBER_ID}"
access_token = "${WHATSAPP_ACCESS_TOKEN}"
verify_token = "${WHATSAPP_VERIFY_TOKEN}"
app_secret = "${WHATSAPP_APP_SECRET}"       # For webhook signature validation
api_version = "v23.0"                        # Optional, default "v23.0"
allowlist = []                               # Empty = auto-whitelist first user
file_transfer = true                         # Optional, default true
```

**Environment variables**:
- `WHATSAPP_PHONE_NUMBER_ID` — from Meta Business Manager
- `WHATSAPP_ACCESS_TOKEN` — System User permanent token
- `WHATSAPP_VERIFY_TOKEN` — arbitrary string you choose for webhook verification
- `WHATSAPP_APP_SECRET` — from Meta App Dashboard (for signature validation)

### WhatsApp Web Config

```toml
[channel.whatsapp_web]
enabled = true
auth_dir = "~/.temm1e/state/whatsapp_web"   # Optional, default shown
dm_policy = "allowlist"                       # "allowlist" | "allow_all" | "deny_all"
group_policy = "ignore"                       # "respond" | "ignore" | "mention_only"
allowlist = []                                # Empty = auto-whitelist first user
file_transfer = true                          # Optional, default true
```

No environment variables needed — authentication is interactive (QR code).

### Config Struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    pub enabled: bool,
    pub phone_number_id: Option<String>,
    pub access_token: Option<String>,
    pub verify_token: Option<String>,
    pub app_secret: Option<String>,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_true")]
    pub file_transfer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppWebConfig {
    pub enabled: bool,
    #[serde(default = "default_auth_dir")]
    pub auth_dir: String,
    #[serde(default = "default_dm_policy")]
    pub dm_policy: String,
    #[serde(default = "default_group_policy")]
    pub group_policy: String,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default = "default_true")]
    pub file_transfer: bool,
}
```

---

## 8. Implementation Order

Strict sequential order — each phase must pass all gates before the next begins.

### Phase 1: Shared Foundation (Day 1)

1. Add feature flags to `Cargo.toml` (root + channels crate)
2. Create `whatsapp_common.rs` with shared utilities
3. Add config structs to `config.rs`
4. Add factory arms (behind `#[cfg]`) to `lib.rs`
5. **Gate**: `cargo check --workspace` passes, `cargo clippy`, `cargo fmt`, `cargo test`

### Phase 2: Cloud API Channel (Day 1-2)

1. Implement `WhatsAppChannel` struct and `Channel` trait
2. Implement `send_message()` (text only)
3. Implement allowlist + auto-whitelist first user
4. Create webhook handler in gateway
5. Wire webhook route in `server.rs`
6. Wire channel init in `main.rs`
7. Write unit tests (config validation, allowlist, message construction)
8. **Gate**: Full CI passes. Test webhook with `curl` locally.

### Phase 3: WhatsApp Web Channel (Day 2-3)

1. Add `whatsapp-rust` optional dependency
2. Implement `WhatsAppWebChannel` struct and `Channel` trait
3. Implement QR code display
4. Implement message listener with reconnection
5. Implement `send_message()`
6. Implement `is_from_me` filtering
7. Implement group policy enforcement
8. Wire channel init in `main.rs`
9. Write unit tests
10. **Gate**: Full CI passes. Test QR flow locally.

### Phase 4: File Transfer — Both Channels (Day 3-4)

1. Implement `FileTransfer` for `WhatsAppChannel` (Cloud API media upload/download)
2. Implement `FileTransfer` for `WhatsAppWebChannel` (whatsapp-rust media API)
3. Write unit tests for file operations
4. **Gate**: Full CI passes.

### Phase 5: Integration Testing (Day 4)

1. Test Cloud API with Meta's test phone number
2. Test WhatsApp Web with a personal number (expendable)
3. Test both channels running simultaneously
4. Verify no regression on Telegram/Discord (existing tests)
5. Test shutdown/restart cycle
6. Test session persistence (WhatsApp Web)
7. **Gate**: 10-turn CLI self-test, full integration pass.

---

## 9. Test Plan

### Unit Tests (per-file, no external dependencies)

| Test | File | What It Verifies |
|------|------|-----------------|
| `create_requires_token` | `whatsapp.rs` | Fails without `access_token` |
| `create_requires_phone_id` | `whatsapp.rs` | Fails without `phone_number_id` |
| `channel_name` | `whatsapp.rs` | Returns `"whatsapp"` |
| `empty_allowlist_denies_all` | `whatsapp.rs` | DF-16 enforcement |
| `allowlist_matches_phone_numbers` | `whatsapp.rs` | E.164 matching |
| `wildcard_allowlist` | `whatsapp.rs` | `"*"` allows all |
| `take_receiver_once` | `whatsapp.rs` | Second call returns None |
| `send_body_construction` | `whatsapp.rs` | Correct JSON payload |
| `text_truncation_utf8_safe` | `whatsapp_common.rs` | No byte boundary panics |
| `phone_normalization` | `whatsapp_common.rs` | Strips non-digit chars |
| `filename_sanitization` | `whatsapp_common.rs` | Path traversal prevention |
| `webhook_signature_valid` | `whatsapp_webhook.rs` | HMAC-SHA256 verification |
| `webhook_signature_invalid` | `whatsapp_webhook.rs` | Rejects bad signatures |
| `webhook_signature_missing` | `whatsapp_webhook.rs` | Rejects missing header |
| `webhook_verify_handshake` | `whatsapp_webhook.rs` | Returns challenge on valid token |
| `webhook_verify_rejects_bad_token` | `whatsapp_webhook.rs` | Returns 403 |
| `webhook_payload_parsing` | `whatsapp_webhook.rs` | Extracts messages correctly |
| `web_channel_name` | `whatsapp_web.rs` | Returns `"whatsapp-web"` |
| `web_create_requires_auth_dir` | `whatsapp_web.rs` | Config validation |
| `web_group_policy_ignore` | `whatsapp_web.rs` | Skips group messages |
| `web_skips_own_messages` | `whatsapp_web.rs` | `is_from_me` filter |
| `web_dm_policy_deny_all` | `whatsapp_web.rs` | Rejects all DMs |

### Integration Tests (require network/credentials)

| Test | What It Verifies |
|------|-----------------|
| Cloud API token validation | `GET /v23.0/{phone_id}` succeeds with valid token |
| Webhook receive → message queue | Full inbound pipeline |
| Send text message | Cloud API `POST /messages` returns 200 |
| Media upload + send | Cloud API media pipeline |
| WhatsApp Web connect | QR displayed, session created |
| WhatsApp Web reconnect | Session restored from `auth_dir` |
| WhatsApp Web send | Message delivered via whatsapp-rust |
| Dual channel coexistence | Both channels active, no interference |
| Telegram unaffected | Existing Telegram tests still pass |
| Discord unaffected | Existing Discord tests still pass |

---

## 10. Failure Modes & Mitigations

Every failure mode is mapped to its mitigation. "User risk" items are the user's problem (ToS, bans). "Operational risk" items are TEMM1E's problem — all mitigated to zero.

### Operational Risk (TEMM1E)

| Failure Mode | Impact | Mitigation | Residual Risk |
|-------------|--------|------------|--------------|
| WhatsApp Web disconnects mid-conversation | Messages stop flowing | Exponential backoff reconnect loop (1s → 60s). Listener task auto-reconnects. User sees "WhatsApp Web reconnecting" in logs. | **ZERO** — other channels unaffected |
| Webhook endpoint receives malformed payload | Could panic on bad JSON | `serde_json::from_slice` returns `Err` → log warning, return 200 OK. No panic. | **ZERO** |
| Webhook signature validation fails | Security bypass | Reject with 401. No message processed. | **ZERO** |
| Meta sends duplicate webhooks (retries) | Duplicate messages to agent | Deduplicate by message ID in webhook handler (HashMap with TTL). | **ZERO** |
| `whatsapp-rust` panics internally | Could kill listener task | Task is spawned via `tokio::spawn` — panic kills only that task. Gateway's catch_unwind also applies at `process_message()`. Dead worker detection respawns. | **ZERO** |
| Session DB corrupted (WhatsApp Web) | Can't reconnect | Delete `session.db`, emit new QR code event. User re-scans. Log error clearly. | **ZERO** — self-healing |
| Cloud API rate limit (429) | Messages rejected | Exponential backoff on 429 responses. Queue outbound messages. Log rate limit. | **ZERO** — temporary degradation, self-recovering |
| Meta deprecates API version | Calls fail | `api_version` is configurable (`"v23.0"`). User updates config. Default bumped in releases. | **ZERO** |
| Memory leak from long-lived sessions | Process grows | `last_inbound` HashMap is bounded by unique chat_ids. Sessions are evicted by `SessionManager`'s LRU. | **ZERO** |
| Feature flag not enabled but config present | Confusing error | `create_channel()` returns clear error: "Compile with --features whatsapp" | **ZERO** |
| Both channels enabled on same number | Undefined behavior | Warning logged. Both channels process independently. No crash. | **ZERO** — user misconfiguration, warned |

### User Risk (NOT our problem, but documented)

| Risk | Who Owns It | What We Do |
|------|-------------|-----------|
| WhatsApp Web account ban | User | Document clearly. User uses expendable number. |
| Meta Jan 2026 AI policy violation | User | Document clearly. Cloud API users must position as business tool. |
| WhatsApp ToS violation (unofficial) | User | Document clearly. Feature flag named `whatsapp-web` not `whatsapp`. |
| Phone number compromised | User | We never store the phone number's WhatsApp password. Session keys only. |
| Business verification rejected | User | Out of our control. Document requirements. |

---

## 11. Rollback Plan

If anything goes wrong post-merge:

### Immediate (no code change)
- Disable in config: `[channel.whatsapp] enabled = false`
- Rebuild without feature: `cargo build --release` (features not in default)

### Code rollback
- Revert the merge commit. All WhatsApp code is in new files + `#[cfg]` blocks.
- Zero impact on Telegram/Discord — the code literally doesn't exist without the feature flag.

### Dependency rollback
- `whatsapp-rust` is optional. Removing it from `Cargo.toml` is a one-line change.
- No existing dependency is modified or upgraded.

---

## Appendix: Dependencies Added

### Cloud API (`whatsapp` feature)

| Crate | Version | Purpose | Already in Workspace? |
|-------|---------|---------|----------------------|
| `reqwest` | workspace | HTTP client | YES |
| `hmac` | 0.12 | Webhook signature | YES (via temm1e-vault) |
| `sha2` | 0.10 | Webhook signature | YES (via temm1e-vault) |
| `hex` | 0.4 | Signature hex encoding | YES (via temm1e-vault) |

**Net new dependencies for Cloud API: ZERO.** All already in the workspace.

### WhatsApp Web (`whatsapp-web` feature)

| Crate | Version | Purpose | Already in Workspace? |
|-------|---------|---------|----------------------|
| `whatsapp-rust` | 0.4 | WhatsApp Web protocol | NO — new |
| `qrcode` | 0.14 | Terminal QR display | NO — new |

**Net new dependencies for WhatsApp Web: 2** (+ their transitive deps).
