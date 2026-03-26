# Discord Integration Plan — v3.2.1

> Status: **PLANNING**
> Branch: `v321`
> Date: 2026-03-22

## Problem Statement

The `DiscordChannel` implementation exists in `crates/temm1e-channels/src/discord.rs` (1,188 lines) and is fully complete — `Channel` trait, `FileTransfer` trait, serenity event handler, allowlist, admin commands, file upload/download, 19 unit tests. But `src/main.rs` has **zero Discord wiring**. The channel code compiles but never gets instantiated, started, or connected to the message pipeline.

User feedback confirms: "It's like having a driver installed but never plugging in the device."

---

## Current Architecture (Telegram-only)

### Channel init (main.rs:1350–1387)
```
channels: Vec<Arc<dyn Channel>> = Vec::new()
primary_channel: Option<Arc<dyn Channel>> = None
tg_rx: Option<mpsc::Receiver<InboundMessage>> = None

1. Auto-inject Telegram config from TELEGRAM_BOT_TOKEN env var
2. TelegramChannel::new(tg_config)?
3. tg.start().await?
4. tg_rx = tg.take_receiver()
5. channels.push(tg_arc.clone())
6. primary_channel = Some(tg_arc.clone())
```

### Message forwarding (main.rs:1655–1672)
```
(msg_tx, msg_rx) = mpsc::channel::<InboundMessage>(32)

// Wire Telegram → unified channel
if let Some(tg_rx) {
    tokio::spawn(while tg_rx.recv() → msg_tx.send())
}
```

### Response routing (main.rs:1768–1987)
```
if let Some(sender) = primary_channel.clone() {
    // sender is used for ALL outbound messages
    // hardcoded to primary_channel (= Telegram)
}
```

### Critical Design Gap
`sender` = `primary_channel` = a **single** `Arc<dyn Channel>`. There is no per-message channel lookup. If Discord messages enter the pipeline, responses would be sent via Telegram (wrong channel!) or fail silently if Telegram isn't configured.

---

## Implementation Plan

### Task 0: Channel-agnostic startup — remove Telegram hard dependency

**File:** `src/main.rs`

**The problem:** The entire message processing loop (lines 1768–3991) is gated behind:
```rust
if let Some(sender) = primary_channel.clone() {  // line 1768
    // ~2,200 lines of message dispatch, worker spawning, interceptor, etc.
}
```
If `primary_channel` is `None` (no Telegram configured, Discord not wired yet), the **entire dispatcher never starts**. Messages arrive via `msg_rx` but nobody reads them. The bot is technically running but completely deaf.

Additionally, line 4015 hardcodes:
```rust
println!("  Status: Onboarding — send your API key via Telegram");
```

**The fix — three parts:**

**Part A: Replace `if let Some(sender)` gate with channel map presence check.**

Currently the `sender` serves two roles:
1. Gate: "do we have any channel at all?" → should be `!channel_map.is_empty()`
2. Response routing: "where do I send replies?" → should be `channel_map.get(&msg.channel)`

After the channel map (Task 2), change line 1768 from:
```rust
if let Some(sender) = primary_channel.clone() {
```
to:
```rust
let channel_map_arc: Arc<HashMap<String, Arc<dyn Channel>>> = Arc::new(channel_map);
if !channel_map_arc.is_empty() {
```

Inside the worker (line 1987), replace:
```rust
let sender = sender.clone();
```
with:
```rust
let channel_map_worker = channel_map_arc.clone();
let primary_fallback = primary_channel.clone();
```

And at message dispatch time, resolve the sender per-message:
```rust
let sender: Arc<dyn Channel> = channel_map_worker
    .get(&msg.channel)
    .cloned()
    .or_else(|| primary_fallback.clone())
    .expect("channel_map is non-empty, checked at gate");
```

This also applies to the interceptor sender (line 1849):
```rust
let icpt_sender = channel_map_worker
    .get(&msg.channel)
    .cloned()
    .or_else(|| primary_fallback.clone())
    .expect("channel_map is non-empty");
```

**Part B: Fix the onboarding status message.**

Change line 4015 from:
```rust
println!("  Status: Onboarding — send your API key via Telegram");
```
to:
```rust
let channel_names: Vec<&str> = channel_map_arc.keys().map(|s| s.as_str()).collect();
if channel_names.is_empty() {
    println!("  Status: No channels configured — set TELEGRAM_BOT_TOKEN or DISCORD_BOT_TOKEN");
} else {
    println!("  Status: Onboarding — send your API key via {}", channel_names.join(" or "));
}
```

**Part C: Guard against zero channels.**

If neither `TELEGRAM_BOT_TOKEN` nor `DISCORD_BOT_TOKEN` is set and no `[channel.*]` config exists, the bot has no way to receive messages. Currently this silently starts with no channels. We should warn loudly:

```rust
if channel_map.is_empty() {
    tracing::warn!(
        "No messaging channels configured. Set TELEGRAM_BOT_TOKEN or DISCORD_BOT_TOKEN, \
         or add [channel.telegram] / [channel.discord] to config."
    );
    // Still proceed — gateway health endpoint works, user might add config later
}
```

**Deployment matrix after this change:**

| `TELEGRAM_BOT_TOKEN` | `DISCORD_BOT_TOKEN` | Result |
|:-----:|:-----:|--------|
| set | set | Both channels start. `primary_channel` = Telegram (first configured). Both work independently. |
| set | unset | Telegram only. Identical to current v3.2.0 behavior. |
| unset | set | **Discord only. NEW — this was impossible before.** |
| unset | unset | No channels. Warning logged. Gateway health endpoint still works. |

**Risk:** MEDIUM — restructures the main dispatch gate. But the logic inside the `if` block is unchanged — we're only changing what controls entry into it (from "Telegram exists" to "any channel exists") and how `sender` is resolved (from "always Telegram" to "look up by msg.channel").

**Why this must be Task 0:** Every other task assumes the dispatcher can start without Telegram. Without this fix, wiring Discord (Task 3) and forwarding its messages (Task 4) is pointless — the messages enter `msg_rx` but the dispatcher that reads `msg_rx` never starts because `primary_channel` is `None` in Discord-only deployments.

---

### Task 1: Feature flag — add `discord` to default features

**File:** `Cargo.toml` (root, line 176)

```diff
- default = ["telegram", "browser", "mcp", "codex-oauth"]
+ default = ["telegram", "discord", "browser", "mcp", "codex-oauth"]
```

**Why:** Users shouldn't need `--features discord`. Discord is as core as Telegram. The serenity/poise deps only compile when the feature is active, so build time increases only when the `discord` feature is present. Since we're making it default, this is the trade-off.

**Risk:** LOW — additive only. Increases compile time by ~15-20s (serenity crate). No runtime effect if no `[channel.discord]` config exists.

---

### Task 2: Channel map — replace single `primary_channel` with per-name lookup

**File:** `src/main.rs`

This is the **highest-risk** change. Currently:
- `primary_channel: Option<Arc<dyn Channel>>` — single channel, always Telegram
- `sender` in the worker loop = `primary_channel.clone()` — all replies go to one channel

**New design:**
```rust
// Replace primary_channel with a channel map
let channel_map: Arc<HashMap<String, Arc<dyn Channel>>> = ...;

// In the worker, look up the sender by msg.channel:
let sender = channel_map.get(&msg.channel)
    .or_else(|| channel_map.values().next()) // fallback to any channel
    .expect("no channels configured");
```

**Detailed changes:**

1. **Declare channel map** (after line 1352):
   ```rust
   let mut channel_map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
   let mut primary_channel: Option<Arc<dyn Channel>> = None;
   ```

2. **Telegram init** (line 1383) — also insert into map:
   ```rust
   channels.push(tg_arc.clone());
   channel_map.insert("telegram".to_string(), tg_arc.clone());
   primary_channel = Some(tg_arc.clone());
   ```

3. **Wrap in Arc** before the worker loop:
   ```rust
   let channel_map: Arc<HashMap<String, Arc<dyn Channel>>> = Arc::new(channel_map);
   ```

4. **Worker loop** (line 1987) — replace `let sender = sender.clone()` with channel map lookup:
   ```rust
   let channel_map_worker = channel_map.clone();
   // Inside the worker:
   let sender = channel_map_worker
       .get(&msg.channel)
       .cloned()
       .or(primary_channel.clone())
       .expect("No channel configured for response routing");
   ```

5. **`censored_channel`** (line 1426) — still wraps `primary_channel` for tool output censoring. This is fine — tool-triggered messages (shell output censoring) always go to whichever channel triggered the tool call. But the `SecretCensorChannel` wrapper needs to become channel-aware too, or we accept that tool censoring only works on the primary channel for now.

**Risk:** MEDIUM — touches the core message dispatch loop. Every response flows through `sender`. Must verify:
- Telegram still works identically (regression)
- Heartbeat messages route correctly (channel = "heartbeat" → primary_channel fallback)
- CLI channel still works for `skyclaw chat`
- Onboarding mode still works (agent_state = None path)

---

### Task 3: Discord channel init — mirror Telegram wiring

**File:** `src/main.rs` (after Telegram init, ~line 1388)

```rust
// ── Discord channel ───────────────────────────────
#[cfg(feature = "discord")]
let mut discord_rx: Option<
    tokio::sync::mpsc::Receiver<temm1e_core::types::message::InboundMessage>,
> = None;

#[cfg(feature = "discord")]
{
    // Auto-inject Discord config from env var when no config entry exists
    if !config.channel.contains_key("discord") {
        if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
            if !token.is_empty() {
                config.channel.insert(
                    "discord".to_string(),
                    temm1e_core::types::config::ChannelConfig {
                        enabled: true,
                        token: Some(token),
                        allowlist: vec![],
                        file_transfer: true,
                        max_file_size: None,
                    },
                );
                tracing::info!("Auto-configured Discord from DISCORD_BOT_TOKEN env var");
            }
        }
    }

    if let Some(discord_config) = config.channel.get("discord") {
        if discord_config.enabled {
            let mut discord = temm1e_channels::DiscordChannel::new(discord_config)?;
            discord.start().await?;
            discord_rx = discord.take_receiver();
            let discord_arc: Arc<dyn temm1e_core::Channel> = Arc::new(discord);
            channels.push(discord_arc.clone());
            channel_map.insert("discord".to_string(), discord_arc.clone());
            if primary_channel.is_none() {
                primary_channel = Some(discord_arc.clone());
            }
            tracing::info!("Discord channel started");
        }
    }
}
```

**Key decisions:**
- `DISCORD_BOT_TOKEN` env var mirrors `TELEGRAM_BOT_TOKEN` auto-inject pattern
- If Telegram is already primary, Discord becomes secondary (responses still route correctly via channel map)
- If only Discord is configured, it becomes primary_channel
- Feature-gated with `#[cfg(feature = "discord")]` — no compile error when disabled

**Risk:** LOW — additive code, mirrors proven Telegram pattern exactly.

---

### Task 4: Discord message forwarding — wire rx into unified pipeline

**File:** `src/main.rs` (after Telegram forwarding, ~line 1672)

```rust
// Wire Discord messages into the unified channel
#[cfg(feature = "discord")]
if let Some(mut discord_rx) = discord_rx {
    let tx = msg_tx.clone();
    task_handles.push(tokio::spawn(async move {
        while let Some(msg) = discord_rx.recv().await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    }));
}
```

**Risk:** ZERO — identical to Telegram forwarding pattern. Discord messages already set `channel: "discord"` in the InboundMessage (discord.rs:925).

---

### Task 5: Wildcard allowlist (`"*"`) support

**File:** `crates/temm1e-channels/src/discord.rs`

Currently, `check_allowed()` (line 205-217) returns `false` when the list is empty and does exact ID matching. No wildcard support exists in any channel.

**Change 1 — `check_allowed()` in DiscordChannel** (line 205):
```rust
fn check_allowed(&self, user_id: &str) -> bool {
    let list = match self.allowlist.read() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("Discord allowlist RwLock poisoned, recovering");
            poisoned.into_inner()
        }
    };
    if list.is_empty() {
        return false; // No one whitelisted yet
    }
    // Wildcard: "*" means everyone is allowed
    if list.iter().any(|a| a == "*") {
        return true;
    }
    list.iter().any(|a| a == user_id)
}
```

**Change 2 — Event handler allowlist check** (discord.rs:646-665):
The event handler also does its own allowlist check inline. Must add wildcard check there too:
```rust
if !list.iter().any(|a| a == &user_id || a == "*") {
    // reject
}
```

**Change 3 — Auto-whitelist skip when wildcard is set** (discord.rs:607-643):
When `"*"` is in the allowlist, skip the auto-whitelist-first-user logic:
```rust
if list.is_empty() {
    // auto-whitelist first user...
}
// No change needed — if list has ["*"], it's not empty, so auto-whitelist is skipped
```

**File:** `crates/temm1e-channels/src/telegram.rs` — check if Telegram's allowlist also needs wildcard support for consistency. (It likely has the same pattern.)

**Risk:** LOW — purely additive check. Empty allowlist behavior unchanged (still denies all per DF-16). Only activates when config explicitly contains `"*"`.

**Config example:**
```toml
[channel.discord]
enabled = true
token = "${DISCORD_BOT_TOKEN}"
allowlist = ["*"]
file_transfer = true
```

---

### Task 6: Reply threading — Discord native replies via `MessageReference`

**File:** `crates/temm1e-channels/src/discord.rs`, `send_message()` (line 308-350)

Currently, `send_message()` creates `CreateMessage::new().content(chunk)` without any reply reference. The `OutboundMessage` struct already has `reply_to: Option<String>` (message.rs:31), and main.rs already populates it with `reply_to: Some(msg.id.clone())` for every response.

**Change:**
```rust
async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
    // ... existing http + channel_id setup ...

    let chunks = split_message(&text, 2000);
    for (i, chunk) in chunks.iter().enumerate() {
        let mut builder = CreateMessage::new().content(chunk);

        // Reply to the original message (first chunk only)
        if i == 0 {
            if let Some(ref reply_id) = msg.reply_to {
                if let Ok(mid) = reply_id.parse::<u64>() {
                    builder = builder.reference_message(
                        serenity::all::MessageReference::from(
                            serenity::all::MessageId::new(mid)
                        )
                    );
                }
            }
        }

        channel_id.send_message(&http, builder).await.map_err(|e| {
            Temm1eError::Channel(format!("Failed to send Discord message: {e}"))
        })?;
    }

    Ok(())
}
```

**Key decisions:**
- Only the first chunk of a multi-part message is a reply — subsequent chunks are standalone (Discord only allows one reply reference per message)
- If `reply_to` parse fails (non-numeric), silently skip (don't crash)
- Uses `reference_message()` which is serenity's builder for `MessageReference`

**Risk:** LOW — additive. If the referenced message was deleted, Discord silently ignores the reference (doesn't error). Existing behavior unchanged when `reply_to` is `None`.

---

### Task 7: Tests

**New tests needed:**

1. **`crates/temm1e-channels/src/discord.rs`:**
   - `test_wildcard_allowlist` — verify `"*"` allows any user ID
   - `test_wildcard_with_other_entries` — `["*", "123"]` still allows everyone
   - `test_empty_allowlist_still_denies` — regression: empty list = deny all

2. **`src/main.rs` or integration test:**
   - Channel map lookup returns correct channel by name
   - Fallback to primary when channel name not in map
   - Discord messages with `channel: "discord"` route to Discord sender

3. **Compilation gate:**
   - `cargo check --workspace` — all features
   - `cargo check --workspace --no-default-features --features telegram` — Discord excluded, no errors
   - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
   - `cargo test --workspace`

---

## Implementation Order

```
Task 1: Feature flag (Cargo.toml)                     — 1 line, ZERO risk
Task 4: Discord rx forwarding (main.rs)               — 8 lines, ZERO risk
Task 5: Wildcard allowlist (discord.rs)                — ~10 lines, LOW risk
Task 6: Reply threading (discord.rs)                   — ~12 lines, LOW risk
Task 2: Channel map (main.rs)                          — ~30 lines, MEDIUM risk ← most complex
Task 3: Discord channel init (main.rs)                 — ~30 lines, LOW risk (depends on Task 2)
Task 7: Tests                                          — validation gate
```

Tasks 5 and 6 are independent of each other and of Tasks 2/3 — they modify discord.rs only. Tasks 2 and 3 are coupled (channel map must exist before Discord init can insert into it). Task 4 depends on Task 3 (discord_rx only exists after Discord init).

**Revised dependency-correct order:**
```
1. Task 1: Feature flag              (Cargo.toml, zero risk)
2. Task 5: Wildcard allowlist        (discord.rs, independent)
3. Task 6: Reply threading           (discord.rs, independent)
4. Task 2: Channel map               (main.rs, core change)
5. Task 0: Channel-agnostic startup  (main.rs, depends on Task 2 — replaces primary_channel gate with channel_map gate)
6. Task 3: Discord channel init      (main.rs, uses channel map)
7. Task 4: Discord rx forwarding     (main.rs, uses discord_rx from Task 3)
8. Task 7: Tests + compilation gate
```

**Why Task 0 after Task 2:** Task 0 replaces the `if let Some(sender) = primary_channel` gate with `if !channel_map.is_empty()`. The channel map must exist first (Task 2). Tasks 5 and 6 modify discord.rs only and are independent of everything else.

---

## Files Modified

| File | Changes | Risk |
|------|---------|------|
| `Cargo.toml` | Add `"discord"` to default features | ZERO |
| `crates/temm1e-channels/src/discord.rs` | Wildcard allowlist + reply threading | LOW |
| `src/main.rs` | Channel-agnostic startup (Task 0), channel map (Task 2), Discord init (Task 3), Discord rx forwarding (Task 4) | MEDIUM |

---

## Verification Checklist

- [ ] `cargo check --workspace` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo test --workspace` passes (all existing + new tests)
- [ ] Telegram still works identically (regression)
- [ ] CLI chat still works (`skyclaw chat`)
- [ ] Onboarding mode still works (no API key → onboarding)
- [ ] Heartbeat messages route correctly (fallback to primary)
- [ ] Discord channel starts when `DISCORD_BOT_TOKEN` is set
- [ ] Discord wildcard allowlist allows all users
- [ ] Discord replies use native threading (MessageReference)
- [ ] No-discord feature builds cleanly: `cargo check --no-default-features --features telegram,browser,mcp,codex-oauth`
- [ ] Discord-only deploy works: `DISCORD_BOT_TOKEN` set, no `TELEGRAM_BOT_TOKEN` → dispatcher starts, messages process
- [ ] Both channels deploy works: both tokens set → both channels start, responses route to correct channel
- [ ] Zero channels deploy: no tokens → warning logged, gateway health endpoint still works, no crash
- [ ] Onboarding status message shows correct channel names (not hardcoded "Telegram")

---

## Open Questions

1. **Should `SecretCensorChannel` become channel-aware?** Currently wraps `primary_channel` only. If a tool's output contains secrets, the censored reply goes to primary_channel, not necessarily the channel that triggered it. For v3.2.1, accept this limitation — tool censoring works on primary channel. Fix in v3.3.0 with per-message channel routing for tool output.

2. **Concurrent Telegram + Discord?** The plan supports both running simultaneously. Each gets its own rx forwarder into msg_tx. The channel map routes responses back to the originating channel. Session isolation is by `chat_id` which is already channel-specific (Discord snowflake IDs vs Telegram chat IDs won't collide).

3. **Discord config in `temm1e.toml` examples?** Should we ship a commented-out `[channel.discord]` section in the example config? Yes — add to docs/setup/ but not part of this code PR.

---

## Impact Analysis

### Q1: How does this affect the original Telegram flow?

**Answer: Zero behavioral change for Telegram-only deployments.**

Traced through every touchpoint:

1. **Channel init** (main.rs:1377-1387) — Telegram init code is untouched. It still runs first, still creates `TelegramChannel`, still calls `start()`, still takes the receiver. The only addition is that after Telegram init, Discord init runs as a separate block. Telegram is unaware of Discord's existence.

2. **Channel map insertion** — Telegram gets `channel_map.insert("telegram", tg_arc)`. This is additive. The map is a `HashMap<String, Arc<dyn Channel>>` — looking up `"telegram"` returns the same `Arc<dyn Channel>` that `primary_channel` used to return.

3. **Message forwarding** (main.rs:1662-1672) — The Telegram `tg_rx → msg_tx` forwarder is identical. Discord gets its own parallel forwarder. They don't interact.

4. **Response routing** — This is where the change matters. Currently: `sender = primary_channel.clone()` (line 1987). After the change: `sender = channel_map.get(&msg.channel)`. For Telegram messages, `msg.channel == "telegram"` → looks up `"telegram"` in the map → returns the same `Arc<TelegramChannel>`. **Identical Arc, identical behavior.**

5. **Interceptor** (main.rs:1849) — `icpt_sender = sender.clone()`. Same derivation — for Telegram messages, the sender resolves to the Telegram channel. No change.

6. **`send_with_retry`** (main.rs:934) — Generic over `&dyn Channel`. Doesn't care which channel. No change.

7. **Telegram-only deployment** (no `DISCORD_BOT_TOKEN`, no `[channel.discord]` config) — Discord init block is entered but both conditions fail: `config.channel.get("discord")` returns `None`, env var check returns `Err`. No Discord code executes. `channel_map` has only `"telegram"`. Runtime is identical to current.

**Compilation change:** Adding `"discord"` to default features means `serenity` and `poise` crates compile even for Telegram-only users. This adds ~15-20s to cold builds. No runtime cost — the Discord module's code paths are only entered when `config.channel.get("discord")` returns `Some(enabled: true)`.

### Q2: How does this affect installations?

**Existing installations — zero impact:**

- No new env vars required. `DISCORD_BOT_TOKEN` is optional — same pattern as `TELEGRAM_BOT_TOKEN` auto-inject.
- No config migration. `temm1e.toml` is unchanged. No new required sections.
- No new runtime dependencies. serenity is a compile-time dependency behind a feature flag.
- No database schema changes. Memory backend (`memory.db`) is unchanged.
- `credentials.toml` — unchanged. Discord doesn't use the credentials system (it uses its own bot token in config).
- `~/.temm1e/` directory — Discord creates `discord_allowlist.toml` only when the first Discord user messages the bot. For Telegram-only installs, this file is never created.

**New Discord installations — what's needed:**
1. Create a Discord bot at discord.com/developers/applications
2. Set `DISCORD_BOT_TOKEN` env var, OR add `[channel.discord]` to `temm1e.toml`
3. Invite bot to server with MESSAGE_CONTENT intent enabled
4. That's it — bot auto-starts alongside Telegram

**Upgrade path from v3.2.0 → v3.2.1:**
- `cargo build --release` — recompile (serenity now compiles by default)
- No config changes needed
- No data migration
- Drop-in binary replacement works

### Q3: How does this affect Tem's core?

**Answer: Zero core changes. The agent runtime, providers, memory, tools, hive — none are touched.**

Traced through the architecture layers:

| Layer | Affected? | Why |
|-------|-----------|-----|
| `temm1e-core` (traits, types) | NO | `Channel`, `InboundMessage`, `OutboundMessage` already support multi-channel. `msg.channel` field exists. `reply_to` field exists. No new traits or types needed. |
| `temm1e-agent` (runtime) | NO | Agent receives `SessionContext` with `channel: msg.channel.clone()` (main.rs:3287). It already stores channel name. Agent doesn't care if it's "telegram" or "discord" — it processes the message identically. |
| `temm1e-providers` (AI) | NO | Providers receive `CompletionRequest`. They have no concept of channels. |
| `temm1e-memory` (persistence) | NO | Memory keys use `chat_history:{chat_id}`. Chat IDs are channel-specific (Telegram: `-1001234567890`, Discord: `1234567890123456789`). No collision possible — Telegram IDs are signed 64-bit, Discord snowflakes are unsigned 64-bit with different ranges. |
| `temm1e-tools` (shell, browser) | NO | Tools use `PendingMessages` keyed by `chat_id`. Same isolation as memory. |
| `temm1e-hive` (swarm) | NO | Hive sessions use `format!("hive-{}", task.id)` — completely independent of channel. |
| `temm1e-vault` (secrets) | NO | Vault is per-installation, not per-channel. |
| `temm1e-mcp` (MCP) | NO | MCP tools are channel-agnostic. |
| `temm1e-channels` (discord.rs) | YES | Wildcard allowlist + reply threading — contained within discord.rs. |
| `src/main.rs` (gateway) | YES | Channel map + Discord init + Discord rx forwarding. This is the only file with structural changes. |

**The provider-agnostic principle is preserved.** Discord is wired at the channel layer. The agent sees `InboundMessage { channel: "discord", ... }` — it doesn't special-case this. System prompt, tool declarations, complexity classification — all channel-independent.

### Q4: What happens if multiple users chat at once?

**Already handled by the existing per-chat serial executor.** No new concurrency concerns.

Here's how it works (main.rs:1756-2007):

```
chat_slots: HashMap<String, ChatSlot>  ← keyed by chat_id

For each inbound message:
  1. Extract chat_id from msg
  2. Lock chat_slots
  3. If slot exists for this chat_id:
     - If worker is busy → interceptor handles it (line 1835)
     - If worker is idle → message goes to worker channel
  4. If no slot → create new ChatSlot with its own worker task
  5. Each worker has its own: chat_rx, interrupt flag, cancel token,
     conversation history, session context
```

**Scenario matrix after Discord integration:**

| Scenario | Behavior | Risk |
|----------|----------|------|
| Telegram user A + Telegram user B (same time) | Two separate ChatSlots (different chat_ids). Fully parallel. Each gets own worker, own history, own session. | ZERO — existing behavior |
| Discord user C + Discord user D (same time) | Two separate ChatSlots (different Discord channel snowflakes). Same parallel isolation. | ZERO — same as Telegram |
| Telegram user A + Discord user C (same time) | Two separate ChatSlots (Telegram chat_id vs Discord channel_id — different number spaces). Fully parallel. Responses route back to correct channel via channel_map. | ZERO — chat_ids can't collide |
| Same user on both Telegram AND Discord | Two separate ChatSlots (different chat_ids). Two separate conversation histories. Two separate sessions. The bot treats them as two different users. This is correct — the channels are different contexts. | ZERO — intended behavior |
| Discord DM + Discord guild mention (same user) | Two separate ChatSlots (DM channel_id vs guild channel_id). Separate conversations. | ZERO |
| 50 concurrent users across both channels | 50 ChatSlots, 50 worker tasks, 50 independent sessions. Tokio handles this fine. Memory/CPU scales linearly with concurrent chat count, not channel count. | ZERO — existing scaling model |

**Potential edge case — chat_id collision:**

Could a Telegram chat_id ever equal a Discord channel_id?

- Telegram: signed 64-bit, range roughly `-10^18` to `10^18`. Group chats are negative (e.g., `-1001234567890`). User DMs are positive (e.g., `123456789`).
- Discord: unsigned 64-bit snowflake, always positive, starting from ~`2015` epoch. Typical values: `1234567890123456789` (19 digits).

**Collision probability: effectively zero.** Telegram user IDs are 9-10 digits. Discord snowflakes are 17-19 digits. Different number spaces entirely. But for belt-and-suspenders safety, the `session_id` (main.rs:3285) already prefixes with channel name: `format!("{}-{}", msg.channel, msg.chat_id)` → `"telegram-123456"` vs `"discord-1234567890123456789"`. Session isolation is guaranteed.

**However:** `history_key` (main.rs:2009) does NOT prefix with channel: `format!("chat_history:{}", worker_chat_id)`. This means if by astronomical coincidence a Telegram chat_id equaled a Discord channel_id, they'd share conversation history. The fix is trivial — change to `format!("chat_history:{}:{}", msg.channel, worker_chat_id)` — but this is a **migration concern** for existing Telegram history. Recommendation: keep the current key format for v3.2.1 (collision is practically impossible), plan the namespaced key migration for v3.3.0 with a data migration step.

**Interceptor concurrency:** When user A is chatting and user B sends a message while A's task runs, the interceptor spawns a separate task (main.rs:1857). Each interceptor uses the correct `sender` (from channel_map after our change). Discord interceptor replies go to Discord, Telegram interceptor replies go to Telegram. No cross-channel contamination.

### Q5: Does each Discord user have to onboard / provide an API key?

**No. Onboarding is per-installation, not per-user or per-chat.**

The agent runtime is a singleton:
```rust
// main.rs:1530 — ONE shared instance across ALL workers, ALL channels
let agent_state: Arc<tokio::sync::RwLock<Option<Arc<AgentRuntime>>>> = ...;
```

Every `ChatSlot` worker reads from the same `agent_state`. Once ANY user (on ANY channel) completes onboarding, `agent_state.write() = Some(agent)` and every subsequent message from every user on every channel hits the initialized agent.

**Three deployment scenarios:**

| Scenario | What happens |
|----------|-------------|
| Admin sets `ANTHROPIC_API_KEY` in `.env` | Agent initializes at startup. No user ever sees onboarding. Discord users chat immediately. |
| Admin onboards via Telegram first | `credentials.toml` saved. Discord users arriving later get the initialized agent instantly. |
| Admin onboards via Discord first | Same — `credentials.toml` saved. Telegram users get it too. |

**Per-chat isolation is conversation history only** — each chat gets its own memory of what was said, but all chats share the same AI provider, credentials, tools, and agent configuration. There is no per-user key requirement.

**The allowlist is the only per-user gate.** With the wildcard `"*"` support (Task 5), even this gate can be removed for open-access deployments.
