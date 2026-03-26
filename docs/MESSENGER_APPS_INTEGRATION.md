# Messenger Apps Integration Guide

> Reference document for integrating any new messaging platform into TEMM1E.
> Last verified against codebase: v3.2.0 (2026-03-22)

This document describes every touchpoint, convention, and architectural constraint involved in adding a new messenger channel. It is the canonical reference — use it as a checklist whether you're adding Slack, WhatsApp, Zalo, Signal, LINE, Matrix, or anything else.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [The Channel Contract](#2-the-channel-contract)
3. [Touchpoint Checklist](#3-touchpoint-checklist)
4. [Crate-Level Work](#4-crate-level-work)
5. [Runtime Wiring (main.rs)](#5-runtime-wiring-mainrs)
6. [Platform-Specific Considerations](#6-platform-specific-considerations)
7. [Security Invariants](#7-security-invariants)
8. [Testing Requirements](#8-testing-requirements)
9. [Platform Reference Table](#9-platform-reference-table)

---

## 1. Architecture Overview

### Message lifecycle

```
┌───────────────┐     ┌──────────┐     ┌───────────┐     ┌──────────┐
│ Platform API  │────▶│ Channel  │────▶│ Unified   │────▶│ Gateway  │
│ (Discord,     │     │ .start() │ tx  │ msg_tx    │     │ Dispatch │
│  Telegram..)  │     │ listener │────▶│ msg_rx    │────▶│ Loop     │
└───────────────┘     └──────────┘     └───────────┘     └────┬─────┘
                                                              │
                      ┌──────────┐     ┌───────────┐     ┌────▼─────┐
                      │ Platform │◀────│ channel   │◀────│ Agent    │
                      │ API      │     │ .send_    │     │ Runtime  │
                      │          │     │ message() │     │          │
                      └──────────┘     └───────────┘     └──────────┘
```

### Key concepts

- **Channel**: A `trait Channel` implementation that connects to one platform. Lives in `crates/temm1e-channels/src/`.
- **Inbound path**: Platform event → `InboundMessage` → `mpsc::Sender<InboundMessage>` (channel-internal tx) → forwarder task → unified `msg_tx` → dispatcher loop.
- **Outbound path**: Agent response → `OutboundMessage` → `channel_map.get(&msg.channel)` → `Channel.send_message()` → Platform API.
- **Channel map**: `HashMap<String, Arc<dyn Channel>>` keyed by channel name (e.g., `"telegram"`, `"discord"`). Routes responses back to the originating platform. (As of v3.2.1+)
- **Primary channel**: First channel initialized. Used as fallback for non-channel messages (heartbeats) and for `SecretCensorChannel` tool output wrapping.
- **Per-chat isolation**: Each unique `chat_id` gets its own `ChatSlot` with dedicated worker task, conversation history, interrupt flag, and cancel token. Chat IDs are platform-specific and don't collide across channels.

### What the channel does NOT do

- **No AI logic.** Channel receives text, forwards it. Receives response, sends it. Zero intelligence.
- **No credential management.** API keys are per-installation (`credentials.toml`), not per-channel. Onboarding happens once.
- **No session management.** The dispatcher handles sessions. The channel just tags messages with `channel: "channelname"`.

---

## 2. The Channel Contract

### Trait: `Channel` (`crates/temm1e-core/src/traits/channel.rs`)

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;                                              // "slack", "whatsapp", etc.
    async fn start(&mut self) -> Result<(), Temm1eError>;                // Connect to platform, spawn listener
    async fn stop(&mut self) -> Result<(), Temm1eError>;                 // Graceful disconnect
    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError>;  // Send response
    fn file_transfer(&self) -> Option<&dyn FileTransfer>;                // File capability (Some or None)
    fn is_allowed(&self, user_id: &str) -> bool;                         // Allowlist check
    async fn delete_message(&self, chat_id: &str, message_id: &str)      // Delete sensitive msgs (default: no-op)
        -> Result<(), Temm1eError> { Ok(()) }
}
```

### Trait: `FileTransfer` (`crates/temm1e-core/src/traits/channel.rs`)

```rust
#[async_trait]
pub trait FileTransfer: Send + Sync {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError>;
    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError>;
    async fn send_file_stream(&self, chat_id: &str, stream: BoxStream<'_, Bytes>, metadata: FileMetadata) -> Result<(), Temm1eError>;
    fn max_file_size(&self) -> usize;
}
```

### Types your channel produces/consumes

```rust
// Inbound — your channel creates these
InboundMessage {
    id: String,              // Platform message ID (unique per platform)
    channel: String,         // MUST be your channel name, e.g. "slack"
    chat_id: String,         // Platform chat/channel/conversation ID
    user_id: String,         // Numeric platform user ID (NEVER username)
    username: Option<String>,// Display name (informational only, never for auth)
    text: Option<String>,    // Message text content
    attachments: Vec<AttachmentRef>,  // File references for lazy download
    reply_to: Option<String>,// Platform message ID this replies to
    timestamp: DateTime<Utc>,// When the message was sent
}

// Outbound — your channel receives these
OutboundMessage {
    chat_id: String,         // Where to send (your platform's ID format)
    text: String,            // Response text
    reply_to: Option<String>,// Platform message ID to reply to (if supported)
    parse_mode: Option<ParseMode>,  // Markdown, Html, or Plain
}

// File attachment reference (for lazy download)
AttachmentRef {
    file_id: String,         // Platform-specific file ID or URL
    file_name: Option<String>,
    mime_type: Option<String>,
    size: Option<usize>,
}
```

---

## 3. Touchpoint Checklist

Every new messenger channel touches these files. No exceptions.

### Crate layer (channel implementation)

| # | File | What to do |
|---|------|-----------|
| 1 | `crates/temm1e-channels/src/<name>.rs` | New file. Implement `Channel` + `FileTransfer` traits. |
| 2 | `crates/temm1e-channels/src/lib.rs` | Add `#[cfg(feature = "<name>")] pub mod <name>;` + re-export + factory match arm (both `cfg` and `cfg(not)` variants). |
| 3 | `crates/temm1e-channels/Cargo.toml` | Add feature flag under `[features]`. Add optional platform SDK deps under `[dependencies]`. |
| 4 | `Cargo.toml` (root) | Add `<name> = ["temm1e-channels/<name>"]` under `[features]`. Add to `default` list if always-on. |

### Runtime layer (main.rs wiring)

| # | Location in main.rs | What to do |
|---|---------------------|-----------|
| 5 | After Telegram/Discord init (~line 1388+) | Add `#[cfg(feature = "<name>")]` block: env var auto-inject + channel init + `take_receiver()` + `channel_map.insert()` + set `primary_channel` if first. |
| 6 | After unified channel creation (~line 1672+) | Add `#[cfg(feature = "<name>")]` forwarder: `tokio::spawn(while rx.recv() → msg_tx.send())`. |
| 7 | Onboarding status message (~line 4015) | Ensure status message includes new channel name (should be automatic if using `channel_map.keys()`). |
| 8 | Zero-channels guard | Ensure warning includes new env var name. |

### Documentation layer

| # | File | What to do |
|---|------|-----------|
| 9 | `docs/channels/<name>.md` | Platform-specific setup guide (bot creation, token, permissions, intents). |
| 10 | `docs/ops/configuration.md` | Add env var to config reference table. |
| 11 | This document | Add row to Platform Reference Table (Section 9). |
| 12 | `README.md` | Update channel list in features section. |

---

## 4. Crate-Level Work

### 4.1 Channel struct pattern

Every channel follows the same structural pattern:

```rust
pub struct <Name>Channel {
    // Platform client (set after connection)
    client: Arc<RwLock<Option<PlatformClient>>>,

    // Auth
    token: String,

    // Access control
    allowlist: Arc<RwLock<Vec<String>>>,
    admin: Arc<RwLock<Option<String>>>,

    // Message pipeline
    tx: mpsc::Sender<InboundMessage>,        // Cloned into event handler
    rx: Option<mpsc::Receiver<InboundMessage>>, // Taken once by gateway

    // Lifecycle
    listener_handle: Option<tokio::task::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}
```

### 4.2 `new()` constructor

```rust
pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
    let token = config.token.clone()
        .ok_or_else(|| Temm1eError::Config("<Name> channel requires a token".into()))?;

    let (tx, rx) = mpsc::channel(256);

    // Load persisted allowlist from ~/.temm1e/<name>_allowlist.toml
    // Fall back to config.allowlist if no persisted file
    let (allowlist, admin) = load_persisted_or_config(config);

    Ok(Self {
        client: Arc::new(RwLock::new(None)),
        token,
        allowlist: Arc::new(RwLock::new(allowlist)),
        admin: Arc::new(RwLock::new(admin)),
        tx,
        rx: Some(rx),
        listener_handle: None,
        shutdown: Arc::new(AtomicBool::new(false)),
    })
}
```

### 4.3 `take_receiver()` — CRITICAL

```rust
pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
    self.rx.take()
}
```

The gateway calls this **once** after `start()`. It drains this receiver into the unified `msg_tx`. If you forget this method, messages from your channel will never reach the dispatcher.

### 4.4 `start()` — listener pattern

```rust
async fn start(&mut self) -> Result<(), Temm1eError> {
    let tx = self.tx.clone();
    let shutdown = self.shutdown.clone();
    let token = self.token.clone();
    // Clone all shared state the handler needs...

    let handle = tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);

        loop {
            if shutdown.load(Ordering::Relaxed) { break; }

            // Connect to platform API
            match PlatformClient::connect(&token).await {
                Ok(client) => {
                    backoff = Duration::from_secs(1); // Reset on success

                    // Event loop — process incoming messages
                    while let Some(event) = client.next_event().await {
                        if shutdown.load(Ordering::Relaxed) { break; }

                        let inbound = convert_to_inbound(event); // → InboundMessage
                        if tx.send(inbound).await.is_err() {
                            break; // Receiver dropped
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to connect to <Platform>");
                }
            }

            if shutdown.load(Ordering::Relaxed) { break; }

            // Exponential backoff reconnection (1s → 2s → 4s → ... → 60s cap)
            tracing::warn!(backoff_secs = backoff.as_secs(), "<Platform> disconnected, reconnecting");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    });

    self.listener_handle = Some(handle);
    tracing::info!("<Name> channel started");
    Ok(())
}
```

**Mandatory elements:**
- Exponential backoff with 60s cap
- `shutdown` flag check at every loop iteration
- `tx.send()` error = receiver dropped = break
- Log reconnection attempts

### 4.5 `send_message()` — outbound pattern

```rust
async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
    let client = /* get platform client from shared state */;

    let chat_id = parse_chat_id(&msg.chat_id)?;

    // Handle platform message size limits
    let chunks = split_message(&msg.text, PLATFORM_MAX_LENGTH);

    for (i, chunk) in chunks.iter().enumerate() {
        let mut builder = PlatformMessage::new(chat_id, chunk);

        // Reply threading — first chunk only
        if i == 0 {
            if let Some(ref reply_id) = msg.reply_to {
                if let Ok(mid) = reply_id.parse::<PlatformMessageId>() {
                    builder = builder.reply_to(mid);
                }
            }
        }

        client.send(builder).await.map_err(|e| {
            Temm1eError::Channel(format!("Failed to send <Platform> message: {e}"))
        })?;
    }

    Ok(())
}
```

### 4.6 Allowlist pattern

```rust
fn is_allowed(&self, user_id: &str) -> bool {
    let list = self.allowlist.read().unwrap_or_else(|p| p.into_inner());

    if list.is_empty() {
        return false;  // DF-16: empty allowlist = deny all
    }

    // Wildcard: "*" = allow everyone
    if list.iter().any(|a| a == "*") {
        return true;
    }

    // Match on numeric user ID only (CA-04: never match usernames)
    list.iter().any(|a| a == user_id)
}
```

### 4.7 Allowlist persistence

Each channel persists its allowlist independently at `~/.temm1e/<name>_allowlist.toml`:

```toml
admin = "123456789"
users = ["123456789", "987654321"]
```

**Why per-channel files:** Allowlists are platform-specific (Telegram user IDs vs Discord snowflakes vs Slack member IDs). They can't share a single file.

**Load order:** persisted file → config `allowlist` field → empty (auto-whitelist first user).

### 4.8 Admin commands (in event handler)

Every channel should intercept these commands before forwarding to the agent:

| Command | Action |
|---------|--------|
| `/allow <user_id>` | Add user to allowlist, persist |
| `/revoke <user_id>` | Remove user from allowlist, persist |
| `/users` | List all allowed users with admin marker |

Only the admin (first user auto-whitelisted) can run these. Non-admin gets: `"Only the admin can use this command."` Reply in-platform, don't forward to agent.

### 4.9 Factory registration

In `crates/temm1e-channels/src/lib.rs`:

```rust
#[cfg(feature = "<name>")]
pub mod <name>;

#[cfg(feature = "<name>")]
pub use <name>::<Name>Channel;

// In create_channel():
#[cfg(feature = "<name>")]
"<name>" => Ok(Box::new(<Name>Channel::new(config)?)),

#[cfg(not(feature = "<name>"))]
"<name>" => Err(Temm1eError::Config(
    "<Name> support is not enabled. Compile with --features <name>".into(),
)),
```

---

## 5. Runtime Wiring (main.rs)

This is the part the channel skill doesn't cover. Without this, your channel compiles but never runs.

### 5.1 Env var auto-inject

Pattern: if no `[channel.<name>]` exists in config, check `<NAME>_BOT_TOKEN` env var.

```rust
#[cfg(feature = "<name>")]
{
    if !config.channel.contains_key("<name>") {
        if let Ok(token) = std::env::var("<NAME>_BOT_TOKEN") {
            if !token.is_empty() {
                config.channel.insert(
                    "<name>".to_string(),
                    temm1e_core::types::config::ChannelConfig {
                        enabled: true,
                        token: Some(token),
                        allowlist: vec![],
                        file_transfer: true,
                        max_file_size: None,
                    },
                );
                tracing::info!("Auto-configured <Name> from <NAME>_BOT_TOKEN env var");
            }
        }
    }
}
```

**Convention:** env var name = `<NAME>_BOT_TOKEN` (e.g., `TELEGRAM_BOT_TOKEN`, `DISCORD_BOT_TOKEN`, `SLACK_BOT_TOKEN`).

### 5.2 Channel init

```rust
#[cfg(feature = "<name>")]
let mut <name>_rx: Option<mpsc::Receiver<InboundMessage>> = None;

#[cfg(feature = "<name>")]
{
    if let Some(<name>_config) = config.channel.get("<name>") {
        if <name>_config.enabled {
            let mut ch = temm1e_channels::<Name>Channel::new(<name>_config)?;
            ch.start().await?;
            <name>_rx = ch.take_receiver();
            let ch_arc: Arc<dyn temm1e_core::Channel> = Arc::new(ch);
            channels.push(ch_arc.clone());
            channel_map.insert("<name>".to_string(), ch_arc.clone());
            if primary_channel.is_none() {
                primary_channel = Some(ch_arc.clone());
            }
            tracing::info!("<Name> channel started");
        }
    }
}
```

**Key:** `primary_channel` is set by the first channel that initializes. Order in code = priority. Currently: Telegram > Discord > (your channel).

### 5.3 Message forwarding

After the unified `msg_tx`/`msg_rx` channel is created:

```rust
#[cfg(feature = "<name>")]
if let Some(mut <name>_rx) = <name>_rx {
    let tx = msg_tx.clone();
    task_handles.push(tokio::spawn(async move {
        while let Some(msg) = <name>_rx.recv().await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    }));
}
```

This bridges the channel's internal mpsc into the unified pipeline. Every channel gets its own forwarder task.

### 5.4 Response routing

Handled by the channel map — no per-channel code needed. The dispatcher resolves the sender:

```rust
let sender = channel_map.get(&msg.channel)  // "slack" → SlackChannel
    .cloned()
    .or_else(|| primary_channel.clone())     // fallback for heartbeats
    .expect("channel_map non-empty");
```

Your channel's `send_message()` is called automatically when the agent responds to a message that originated from your channel.

---

## 6. Platform-Specific Considerations

### 6.1 Message size limits

| Platform | Text limit | File limit | Handling |
|----------|-----------|------------|----------|
| Telegram | 4,096 chars | 50 MB upload, 20 MB download | Split at newline/space boundary |
| Discord | 2,000 chars | 25 MB (non-Nitro), 100 MB (Nitro) | Split at newline/space boundary |
| Slack | 40,000 chars (block kit) | 1 GB (paid) | Rarely needs splitting |
| WhatsApp | 65,536 chars | 16 MB (media dependent) | Split at paragraph boundary |
| Signal | ~65,536 chars | 100 MB | Split at paragraph boundary |
| Zalo | 2,000 chars | 25 MB | Split at newline/space boundary |
| LINE | 5,000 chars | varies by type | Split at newline/space boundary |

Always implement `split_message()` with intelligent boundary detection (newline > space > hard cut).

### 6.2 Reply threading

| Platform | How | Notes |
|----------|-----|-------|
| Telegram | `reply_to_message_id` parameter | Always works |
| Discord | `MessageReference` | Silently ignored if original deleted |
| Slack | `thread_ts` parameter | Creates/continues thread |
| WhatsApp | `context.message_id` | Quoted reply |
| Signal | Quoted reply via protocol | Requires original message |

Use `OutboundMessage.reply_to` (always populated by the dispatcher). If your platform doesn't support replies, ignore it silently.

### 6.3 Markdown/formatting

| Platform | Supports | Notes |
|----------|----------|-------|
| Telegram | MarkdownV2, HTML | Choose based on `parse_mode` |
| Discord | Native Markdown | Send text as-is |
| Slack | mrkdwn (Slack's variant) | `*bold*`, `_italic_`, `` `code` `` |
| WhatsApp | Limited: `*bold*`, `_italic_`, `` `code` `` | Strip unsupported markup |
| Signal | None (plain text only) | Strip all markup |

Handle `parse_mode` in `send_message()`. When in doubt, send as plain text.

### 6.4 Bot creation / token acquisition

Document platform-specific setup in `docs/channels/<name>.md`. At minimum:
1. Where to create the bot/app (developer portal URL)
2. What permissions/scopes/intents are needed
3. How to get the token
4. How to invite/install the bot

### 6.5 User ID formats

| Platform | Format | Example | Notes |
|----------|--------|---------|-------|
| Telegram | Signed 64-bit int | `123456789`, `-1001234567890` | Groups are negative |
| Discord | Unsigned 64-bit snowflake | `1234567890123456789` | 17-19 digits |
| Slack | String | `U0123ABCDEF` | Alphanumeric member ID |
| WhatsApp | Phone number | `14155552671` | E.164 without `+` |
| Signal | UUID | `a1b2c3d4-...` | V4 UUID |
| Zalo | Numeric | `1234567890` | 10 digits |
| LINE | String | `U1a2b3c4d5e...` | 33-char alphanumeric |

**Critical:** Allowlist matches on `user_id`, which must be numeric or platform-canonical. NEVER match on display names (CA-04 — usernames can be changed, enabling allowlist bypass).

**Chat ID collision across platforms:** Practically impossible due to different ID spaces (signed vs unsigned vs string vs UUID). But session IDs already namespace by channel: `format!("{}-{}", msg.channel, msg.chat_id)`. Conversation history keys (`chat_history:{chat_id}`) do NOT namespace by channel yet — acceptable because ID spaces don't overlap, but planned for future cleanup.

---

## 7. Security Invariants

These apply to ALL channels. Violating any of these is a blocking issue.

| Rule | Code | Why |
|------|------|-----|
| **DF-16:** Empty allowlist = deny all | `if list.is_empty() { return false; }` | Prevents open-access bots from zero-config deployments |
| **CA-04:** Match user ID only, never username | `list.iter().any(\|a\| a == user_id)` | Usernames can be changed, enabling allowlist bypass |
| **Credential scrubbing:** Never log tokens at info level | `tracing::debug!` with masking | Tokens in logs = compromised tokens |
| **Sensitive message deletion:** Implement `delete_message()` | Delete after API key ingestion | Prevent credential exposure in chat history |
| **File name sanitization:** Strip directory components | `Path::file_name()` only | Prevent path traversal via malicious filenames |
| **UTF-8 safety:** Never slice user text with `&str[..N]` | Use `char_indices()` | Multi-byte chars (Vietnamese, CJK, emoji) cause panics |

---

## 8. Testing Requirements

### 8.1 Minimum test coverage

Every channel implementation must include:

```rust
#[cfg(test)]
mod tests {
    // Construction
    test_new_requires_token()            // Missing token → Err
    test_new_with_token_succeeds()       // Valid config → Ok
    test_name_returns_correct_string()   // name() == "<name>"

    // Allowlist
    test_empty_allowlist_denies_all()    // DF-16
    test_allowlist_allows_listed_user()  // Exact match
    test_allowlist_denies_unlisted()     // Not in list
    test_wildcard_allows_all()           // "*" = allow everyone
    test_wildcard_with_entries()         // ["*", "123"] = allow everyone

    // Channel trait
    test_implements_channel_trait()      // let _: &dyn Channel = &ch;
    test_file_transfer_returns_some()    // file_transfer().is_some()

    // Message handling
    test_split_message_under_limit()     // Short msg → 1 chunk
    test_split_message_over_limit()      // Long msg → multiple chunks
    test_split_at_boundary()             // Splits at newline/space, not mid-word

    // FileTransfer (if applicable)
    test_max_file_size()                 // Returns platform limit
}
```

### 8.2 Compilation gates

```bash
# Feature-gated build
cargo check -p temm1e-channels --features <name>
cargo test -p temm1e-channels --features <name>
cargo clippy -p temm1e-channels --features <name> -- -D warnings

# Full workspace with all features
cargo check --workspace --all-features
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Verify exclusion doesn't break other features
cargo check --workspace --no-default-features --features telegram,browser,mcp,codex-oauth
```

---

## 9. Platform Reference Table

Quick lookup for platform SDK crates, auth patterns, and quirks.

| Platform | Rust SDK crate | Auth method | Env var | Quirks |
|----------|---------------|-------------|---------|--------|
| Telegram | `teloxide` | Bot token (BotFather) | `TELEGRAM_BOT_TOKEN` | Long-polling default. File download needs separate HTTP call. |
| Discord | `serenity` + `poise` | Bot token (Developer Portal) | `DISCORD_BOT_TOKEN` | Requires MESSAGE_CONTENT intent. Gateway + REST split. |
| Slack | `reqwest` (REST) or `slack-morphism` | Bot token (OAuth) | `SLACK_BOT_TOKEN` | Socket Mode for events, or Event Subscriptions (needs public URL). |
| WhatsApp | `reqwest` (Cloud API) | System user token | `WHATSAPP_TOKEN` | Requires Meta Business verification. Webhook-based (needs public URL). 24h messaging window. |
| Zalo | `reqwest` (REST) | OA access token (OAuth) | `ZALO_OA_TOKEN` | Webhook-based. Vietnamese market. OA (Official Account) required. |
| Signal | `presage` or `libsignal` | Phone number registration | `SIGNAL_PHONE` | No official bot API. Must use Signal protocol directly. Complex setup. |
| LINE | `reqwest` (Messaging API) | Channel access token | `LINE_CHANNEL_TOKEN` | Webhook-based. Reply tokens expire in 1 minute. |
| Matrix | `matrix-sdk` | Access token or SSO | `MATRIX_TOKEN` | Federated. E2EE support requires device verification. |
| Viber | `reqwest` (REST) | Auth token (Admin Panel) | `VIBER_AUTH_TOKEN` | Webhook-based. 7,000 char limit. |

### Webhook-based vs long-polling platforms

**Long-polling** (Telegram, Discord): The channel's `start()` spawns a task that polls the platform API. No public URL needed. Works behind NAT/firewall.

**Webhook-based** (Slack, WhatsApp, Zalo, LINE, Viber): The platform sends HTTP requests to YOUR server. Requires:
1. A publicly accessible URL (use TEMM1E's gateway or a tunnel)
2. Registering the webhook URL with the platform
3. An HTTP handler in your channel's `start()` — either:
   - Register a route on TEMM1E's existing axum gateway (`temm1e-gateway`)
   - Or spawn a separate listener (less preferred, more ports)

**Recommendation for webhook platforms:** Add a route to the existing gateway at `/webhook/<name>` (e.g., `/webhook/slack`). The gateway already runs on port 8080. Your channel's `start()` can register a handler without spawning a separate server.

---

## Appendix: Existing implementations as reference

| Channel | File | Lines | Complexity | Best reference for |
|---------|------|-------|-----------|-------------------|
| CLI | `crates/temm1e-channels/src/cli.rs` | 240 | Simple | Minimal trait impl, no network |
| Telegram | `crates/temm1e-channels/src/telegram.rs` | 906 | Medium | Long-polling, file transfer, allowlist |
| Discord | `crates/temm1e-channels/src/discord.rs` | 1,188 | Medium | Gateway-based, reconnection, reply threading |
| Slack | `crates/temm1e-channels/src/slack.rs` | 1,385 | Complex | Webhook/Socket Mode, block kit |

**Start by reading Telegram** — it's the cleanest, most representative pattern. Then check Discord for reconnection and reply threading. Slack for webhook patterns.
