# System Prompt Token Analysis

**Date:** 2026-03-20
**Objective:** Identify and quantify all token consumers in the system prompt + tool definitions to find optimization opportunities.

---

## 1. Architecture Overview: Two Prompt Paths

TEMM1E has **two distinct system prompt construction paths**:

### Path A: `build_system_prompt()` in `src/main.rs` (Gateway/Telegram)
- Used for all gateway-dispatched messages (Telegram, Discord, Slack)
- Monolithic prompt: `SYSTEM_PROMPT_BASE` + dynamic provider/model context + vision rules + current config + self-config rules + secret handling + MCP instructions + custom tool authoring instructions
- **This is the path used in live production** and the one consuming ~25K tokens

### Path B: `SystemPromptBuilder` in `crates/temm1e-agent/src/prompt_optimizer.rs` (V2 tiered)
- Used when `prompt_tier` is set (V2 optimizations enabled)
- Conditional sections based on tier (Minimal/Basic/Standard/Full)
- **Much smaller** -- tested to stay under 1,500 tokens with 6 tools
- Only activated when the classifier routes through the V2 path

### Path C: Fallback in `crates/temm1e-agent/src/context.rs` line 818
- Used when `system_prompt` is `None` and no V2 tier
- Generates a minimal ~500-token prompt with tool list + workspace + guidelines

**Key insight:** Path A (main.rs) is the dominant cost driver. The V2 `SystemPromptBuilder` already solves much of this, but only when V2 optimizations are enabled.

---

## 2. Token Breakdown: Path A (main.rs `build_system_prompt()`)

Using the project's own heuristic: **1 token ~ 4 characters**.

### 2.1 Base System Prompt (`SYSTEM_PROMPT_BASE`)

| Section | Chars | Est. Tokens |
|---------|-------|-------------|
| Identity + tool list overview | 310 | 78 |
| Key rules (shell output, send_message, browser, language) | 780 | 195 |
| Persistent memory instructions | 680 | 170 |
| **Subtotal** | **1,770** | **~443** |

### 2.2 Dynamic Sections (appended by `build_system_prompt()`)

| Section | Chars | Est. Tokens |
|---------|-------|-------------|
| Supported providers & default models (8 providers) | 620 | 155 |
| Vision (image) support rules | 460 | 115 |
| Current configuration (dynamic, ~3-5 providers) | ~200 | ~50 |
| Self-configuration rules | 440 | 110 |
| Secret handling (MANDATORY) | 920 | 230 |
| MCP self-extension instructions (#[cfg(feature = "mcp")]) | 1,250 | 313 |
| Custom tool authoring instructions | 820 | 205 |
| **Subtotal** | **~4,710** | **~1,178** |

### 2.3 System Prompt Total (text only)

| Component | Est. Tokens |
|-----------|-------------|
| SYSTEM_PROMPT_BASE | 443 |
| Dynamic sections | 1,178 |
| **System prompt text total** | **~1,621** |

---

## 3. Tool Definitions Token Breakdown

Each tool is sent as a `ToolDefinition` with `name`, `description`, and `parameters` (JSON Schema). The provider serializes these into the API request as structured tool definitions.

### 3.1 Per-Tool Analysis (sorted by total size, largest first)

| # | Tool Name | Description (chars) | Schema (serialized JSON chars) | Total Chars | Est. Tokens | Used Every Turn? |
|---|-----------|--------------------:|-------------------------------:|------------:|------------:|:----------------:|
| 1 | **browser** | 1,480 | 920 | 2,400 | **600** | No (browser tasks only) |
| 2 | **self_create_tool** | 340 | 550 | 890 | **223** | No (tool authoring only) |
| 3 | **git** | 400 | 460 | 860 | **215** | No (git tasks only) |
| 4 | **memory_manage** | 340 | 520 | 860 | **215** | Sometimes |
| 5 | **mcp_manage** | 260 | 560 | 820 | **205** | No (MCP admin only) |
| 6 | **self_add_mcp** | 300 | 420 | 720 | **180** | No (MCP install only) |
| 7 | **usage_audit** | 260 | 360 | 620 | **155** | No (usage queries only) |
| 8 | **key_manage** | 280 | 300 | 580 | **145** | No (key management only) |
| 9 | **check_messages** | 310 | 120 | 430 | **108** | No (long tasks only) |
| 10 | **send_file** | 230 | 320 | 550 | **138** | Sometimes |
| 11 | **send_message** | 270 | 240 | 510 | **128** | Sometimes |
| 12 | **web_fetch** | 220 | 280 | 500 | **125** | Sometimes |
| 13 | **shell** | 210 | 280 | 490 | **123** | Yes (core) |
| 14 | **self_extend_tool** | 250 | 200 | 450 | **113** | No (MCP search only) |
| 15 | **lambda_recall** | 310 | 200 | 510 | **128** | No (faded memory only) |
| 16 | **file_read** | 100 | 180 | 280 | **70** | Yes (core) |
| 17 | **file_write** | 160 | 240 | 400 | **100** | Yes (core) |
| 18 | **file_list** | 150 | 200 | 350 | **88** | Sometimes |
| 19 | **mode_switch** | 200 | 230 | 430 | **108** | No (mode changes only) |

### 3.2 Tool Definition Totals

| Category | Tools | Est. Tokens |
|----------|-------|-------------|
| **Core (always needed):** shell, file_read, file_write, file_list, send_message | 5 | **509** |
| **Frequent:** web_fetch, send_file, memory_manage | 3 | **478** |
| **Situational:** browser, git, check_messages, mode_switch, lambda_recall | 5 | **1,047** |
| **Rare/Admin:** self_create_tool, key_manage, usage_audit | 3 | **523** |
| **MCP (feature-gated):** mcp_manage, self_extend_tool, self_add_mcp | 3 | **498** |
| **All tools total** | **19** | **~3,055** |

**Note:** Anthropic and OpenAI both add protocol overhead per tool (~100-200 tokens for framing, `tool_choice` config, etc.). Real-world overhead is likely **~3,500-4,000 tokens** for all tools.

---

## 4. Other Context Injections (per-request)

| Component | Est. Tokens | Frequency |
|-----------|-------------|-----------|
| Blueprint catalog (matched blueprints) | 200-2,000 | When blueprints match |
| Blueprint full body | 500-5,000 | When best blueprint fits 10% budget |
| Blueprint outline | 200-500 | When body too large |
| Lambda memory context | 500-3,000 | Most turns |
| Legacy memory search | 200-1,000 | Fallback when lambda empty |
| Knowledge entries | 100-500 | When knowledge exists |
| Cross-task learnings | 100-300 | When learnings exist |
| Mode injection (PLAY/WORK/PRO) | ~30 | Every turn when mode set |
| Prompt patches (self-tuning) | 100-500 | When approved patches exist |
| Provider protocol overhead | 200-500 | Every turn |
| Message framing overhead | ~500 | Every turn (hardcoded) |

---

## 5. Full Token Budget Estimate: First Turn

For a first-turn message with all tools enabled and no prior context:

| Component | Est. Tokens |
|-----------|-------------|
| System prompt text (Path A) | 1,621 |
| Tool definitions (19 tools) | 3,055 |
| Provider protocol overhead | ~400 |
| Message framing | 500 |
| User message | ~50 |
| **Total input tokens** | **~5,626** |

**Discrepancy with 25K observation:** The 25K figure from experiments likely includes:
1. Provider-side tool definition expansion (Anthropic/OpenAI wrap each tool in XML/JSON structures adding ~50-100 tokens per tool = +1,000-2,000)
2. Multi-turn context: by turn 3-5, conversation history adds 5K-15K tokens
3. Lambda memory + blueprint injections adding 2K-5K tokens
4. The provider's own system prompt overhead and caching metadata

**Realistic first-turn estimate with provider overhead: ~7,000-8,000 tokens**
**Realistic turn-5 estimate: ~15,000-22,000 tokens** (matches the ~25K observation)

---

## 6. Top 5 Biggest Token Consumers

### Rank 1: `browser` tool definition -- ~600 tokens
- Description alone is 1,480 chars listing 16 actions with detailed explanations
- Schema has 14 properties (action, url, selector, x, y, text, script, filename, session_name, hint, service, retry, plus the enum with 16 values)
- **Used only for browser tasks** -- not needed for simple chat or shell commands

### Rank 2: `SYSTEM_PROMPT_BASE` tool descriptions -- redundant with tool definitions
- Lines 327-335 of main.rs manually list tools and their purposes
- These are ALSO described in each tool's `description()` field
- **Double-describing** shell, file_read, file_write, file_list, web_fetch, browser, send_message, send_file, memory_manage

### Rank 3: MCP instructions in system prompt -- ~313 tokens
- Lines 451-477 of main.rs: detailed MCP workflow, when to self-extend, safety rules
- This is in ADDITION to the 3 MCP tool definitions (~498 tokens)
- **Used only when user needs new capabilities** -- rare in most conversations

### Rank 4: Secret handling rules in system prompt -- ~230 tokens
- Lines 427-444 of main.rs: detailed USER/CLAW/PC model, 7 specific rules
- Critical for security but verbose -- could be condensed

### Rank 5: Custom tool authoring instructions -- ~205 tokens
- Lines 481-503 of main.rs: detailed HOW IT WORKS, WHEN TO CREATE, ACTIONS, RULES
- In ADDITION to `self_create_tool` definition (~223 tokens)
- **Used only when creating custom tools** -- rare

---

## 7. Recommendations

### R1: Eliminate System Prompt / Tool Definition Redundancy (Save ~200 tokens)
**Current:** `SYSTEM_PROMPT_BASE` (lines 327-335) lists tools and their purposes, AND each tool has its own `description()`.
**Fix:** Remove the tool listing from `SYSTEM_PROMPT_BASE`. The tool definitions already tell the LLM what each tool does.
**Impact:** -200 tokens, zero behavioral risk.

### R2: Lazy Tool Loading (Save ~1,500-2,000 tokens on casual turns)
**Current:** All 19 tools are sent on every request regardless of task type.
**Fix:** Use the classifier's output to load only relevant tool subsets:
- **Simple chat:** 0 tools (no tool calling needed)
- **File tasks:** shell, file_read, file_write, file_list, send_file, send_message
- **Browser tasks:** browser, send_message, send_file, shell
- **Admin/config:** key_manage, usage_audit, mode_switch
- **MCP:** mcp_manage, self_extend_tool, self_add_mcp (only when MCP feature on AND user requests capability)

The classifier already runs before the agent loop. Adding `tool_hint` to its output is straightforward.
**Impact:** -1,500 to -2,000 tokens for simple/moderate tasks. Saves ~$0.005-0.01 per turn at Claude Sonnet pricing.

### R3: Compress Browser Tool Description (Save ~300 tokens)
**Current:** 1,480-char description with detailed per-action explanations.
**Fix:** Shorten to action list without inline docs. The action names are self-documenting, and the `enum` in the schema already constrains valid actions.

**Before (1,480 chars):**
```
Control a stealth Chrome browser to navigate websites, click elements...
Actions:
- navigate: Go to a URL
- click: Click an element by CSS selector
- click_at: Click at pixel coordinates (x, y) — use after screenshot...
[16 more lines]
```

**After (~400 chars):**
```
Control a stealth Chrome browser. Each call performs one action.
Actions: navigate, click, click_at, type, screenshot, get_text, evaluate, get_html, save_session, restore_session, accessibility_tree, observe, authenticate, restore_web_session, close.
Vision workflow: screenshot -> analyze -> click_at -> repeat.
Observation: use observe for auto-tiered page analysis.
```

**Impact:** -270 tokens from the browser tool alone.

### R4: Move MCP + Custom Tool Instructions to Tool Descriptions (Save ~500 tokens)
**Current:** MCP workflow (313 tokens) and custom tool authoring (205 tokens) are in the system prompt AND in the tool definitions.
**Fix:** Remove from system prompt. The tool descriptions already explain what each tool does. Add a one-line note to the system prompt: "Use self_extend_tool to find new capabilities, self_add_mcp to install them, self_create_tool to author your own."
**Impact:** -500 tokens.

### R5: Condense Secret Handling Rules (Save ~100 tokens)
**Current:** 920 chars (230 tokens) with detailed USER/CLAW/PC model and 7 rules.
**Fix:** Condense to essentials:
```
SECRET HANDLING: Users give you secrets to USE on the server — never echo them back.
Never display API keys, credentials, or tokens in your responses. Say "stored securely" instead.
Never include secrets in shell commands visible to the user.
```
**Impact:** -100 tokens.

### R6: Use V2 `SystemPromptBuilder` Universally (Save ~800 tokens)
**Current:** Path A (main.rs) uses a monolithic prompt; Path B (prompt_optimizer.rs) uses conditional sections.
**Fix:** Route all traffic through `SystemPromptBuilder` with the `Standard` tier as default. This already handles conditional tool sections, file protocol, workspace, etc.
**Impact:** The SystemPromptBuilder's Standard tier with all tools is tested at <1,500 tokens vs Path A's ~1,621. Net savings vary but the real win is that sections are conditionally included.

### R7: Provider-Specific Caching (Save ~3,000 tokens amortized)
**Current:** Full system prompt + tool definitions sent every turn.
**Fix:** Anthropic supports prompt caching. Mark the system prompt and tool definitions as cacheable. After the first turn, they consume 0 input tokens (only cache read tokens at 10% cost).
**Impact:** At ~5,000 system+tool tokens, saving 90% on turns 2+ = ~4,500 tokens * $0.003/1K = **~$0.014 per turn saved**.

---

## 8. Projected Savings Summary

| Optimization | Token Savings | Risk | Effort |
|-------------|---------------|------|--------|
| R1: Remove redundant tool listing | -200 | Zero | 5 min |
| R2: Lazy tool loading by task type | -1,500 to -2,000 | Low | 2-3 hours |
| R3: Compress browser description | -300 | Zero | 15 min |
| R4: Move MCP/custom to tool descriptions | -500 | Zero | 30 min |
| R5: Condense secret handling | -100 | Low | 10 min |
| R6: Universal SystemPromptBuilder | -800 | Low | 1-2 hours |
| R7: Provider-specific caching | -4,500 amortized | Zero | 1 hour |
| **Total (without R7)** | **-3,400 to -3,900** | | |
| **Total (with R7)** | **-7,900 to -8,400** | | |

At Claude Sonnet pricing ($3/M input tokens), saving ~3,500 tokens per turn across an average session (10 turns) saves ~$0.105 per session. With R7 caching, savings reach ~$0.25 per session.

---

## 9. Files Analyzed

| File | Role |
|------|------|
| `src/main.rs` lines 323-507 | `SYSTEM_PROMPT_BASE` + `build_system_prompt()` |
| `crates/temm1e-agent/src/context.rs` | Context builder, tool def injection, budget allocation |
| `crates/temm1e-agent/src/prompt_optimizer.rs` | V2 `SystemPromptBuilder` with tiered sections |
| `crates/temm1e-agent/src/prompt_patches.rs` | Self-tuning prompt patches |
| `crates/temm1e-agent/src/blueprint.rs` | Blueprint injection logic |
| `crates/temm1e-tools/src/lib.rs` | Tool registration factory |
| `crates/temm1e-tools/src/*.rs` | Individual tool definitions (19 tools) |
| `crates/temm1e-mcp/src/{self_extend,self_add,mcp_manage,bridge}.rs` | MCP tool definitions |
