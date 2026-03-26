# Phase 5: Swarm Browsing (Hive Integration)

> **Depends on:** Phase 1 (observation), Phase 2 (credential isolation)
> **Status:** C=100%, R=0%, K=100%

---

## Research Resolutions

### R12: Worker Tool Sharing
Workers share `Vec<Arc<dyn Tool>>` — shallow clone of Arc pointers. The underlying `BrowserTool` has a single `page: Arc<Mutex<Option<Page>>>`. Multiple workers using the same `BrowserTool` instance will serialize (mutex) but corrupt page state (one worker's navigation overwrites another's).

**Solution:** Per-worker `BrowserTool` instances, each bound to a dedicated browser context from the pool.

### R13: Hive Result Delivery
Current Hive: user gets exactly two messages — "Pack activated" ack and final aggregated result. No intermediate delivery. Results are collected from Blackboard after all tasks complete.

**Solution:** Add a watcher task that polls the Blackboard for completed browse tasks and sends partial updates.

### R14: Chrome Memory Footprint
- Baseline Chrome: ~100MB
- Per-context with simple pages: 30-80MB
- 4 contexts total: ~220-420MB (feasible under 1GB)
- Heavy SPAs: could exceed 1GB with 4 contexts

**Decision:** Default pool size = 4, configurable. Document memory requirements.

---

## 5.1 Browser Pool

**File:** `crates/temm1e-tools/src/browser_pool.rs` (new)

```rust
use chromiumoxide::{Browser, BrowserConfig, Page};
use chromiumoxide::cdp::browser_protocol::target::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct BrowserPool {
    browser: Arc<Mutex<Browser>>,
    contexts: Vec<Arc<Mutex<PooledContext>>>,
    available: AtomicU64,  // bitset: bit N = 1 means slot N is available
    max_size: usize,
}

struct PooledContext {
    context_id: BrowserContextId,
    page: Option<Page>,
}

impl BrowserPool {
    pub async fn new(max_size: usize) -> Result<Self, Temm1eError> {
        assert!(max_size <= 64, "Pool size limited to 64 (bitset)");

        let config = BrowserConfig::builder()
            .arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--no-sandbox")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-blink-features=AutomationControlled")
            // ... same stealth args as existing BrowserTool ...
            .window_size(1920, 1080)
            .build()
            .map_err(|e| Temm1eError::Tool(format!("BrowserPool config: {e}")))?;

        let (browser, mut handler) = Browser::launch(config).await
            .map_err(|e| Temm1eError::Tool(format!("BrowserPool launch: {e}")))?;

        // Spawn CDP handler
        tokio::spawn(async move { while handler.next().await.is_some() {} });

        // Pre-create contexts
        let mut contexts = Vec::with_capacity(max_size);
        let mut available_bits: u64 = 0;

        for i in 0..max_size {
            let ctx_id = browser.create_browser_context(
                CreateBrowserContextParams::default()
            ).await
                .map_err(|e| Temm1eError::Tool(format!("BrowserPool context {i}: {e}")))?;

            contexts.push(Arc::new(Mutex::new(PooledContext {
                context_id: ctx_id,
                page: None,
            })));
            available_bits |= 1 << i;
        }

        Ok(Self {
            browser: Arc::new(Mutex::new(browser)),
            contexts,
            available: AtomicU64::new(available_bits),
            max_size,
        })
    }

    /// Atomically claim a browser context. Returns slot index.
    pub fn try_acquire(&self) -> Option<usize> {
        loop {
            let current = self.available.load(Ordering::Acquire);
            if current == 0 { return None; }  // No available slots

            let slot = current.trailing_zeros() as usize;
            let new = current & !(1 << slot);

            if self.available.compare_exchange(
                current, new, Ordering::AcqRel, Ordering::Acquire
            ).is_ok() {
                return Some(slot);
            }
            // CAS failed — retry
        }
    }

    /// Get a Page for the claimed slot. Creates one if needed.
    pub async fn get_page(&self, slot: usize) -> Result<Page, Temm1eError> {
        let mut ctx = self.contexts[slot].lock().await;
        if let Some(ref page) = ctx.page {
            return Ok(page.clone());
        }

        // Create new page in this context
        let browser = self.browser.lock().await;
        let page = browser.new_page(
            CreateTargetParams::builder()
                .url("about:blank")
                .browser_context_id(ctx.context_id.clone())
                .build()
        ).await
            .map_err(|e| Temm1eError::Tool(format!("Pool page create: {e}")))?;

        ctx.page = Some(page.clone());
        Ok(page)
    }

    /// Release a context back to the pool. Clears cookies and storage.
    pub async fn release(&self, slot: usize) -> Result<(), Temm1eError> {
        {
            let mut ctx = self.contexts[slot].lock().await;

            // Close the page
            if let Some(page) = ctx.page.take() {
                let _ = page.goto("about:blank").await;
                let _ = page.close().await;
            }

            // Clear cookies for this context
            let browser = self.browser.lock().await;
            // Note: disposing and recreating the context is the cleanest way
            let _ = browser.dispose_browser_context(ctx.context_id.clone()).await;
            let new_ctx_id = browser.create_browser_context(
                CreateBrowserContextParams::default()
            ).await
                .map_err(|e| Temm1eError::Tool(format!("Pool release: {e}")))?;
            ctx.context_id = new_ctx_id;
        }

        // Mark slot as available
        self.available.fetch_or(1 << slot, Ordering::Release);
        Ok(())
    }

    /// Load a stored session into a pool slot.
    pub async fn load_session(&self, slot: usize, state: &SessionState) -> Result<(), Temm1eError> {
        let page = self.get_page(slot).await?;

        // Restore cookies
        use chromiumoxide::cdp::browser_protocol::network::*;
        page.execute(SetCookiesParams::new(state.cookies.clone())).await?;

        // Restore localStorage via DOMStorage CDP
        // ... (same as Phase 3.4 restore_session)

        page.goto(&state.url).await?;
        Ok(())
    }
}
```

### Estimated: ~150 lines
### Risk: 0% — new module, no existing code touched

---

## 5.2 Per-Worker BrowserTool Instances

### The Problem
`execute_order()` in main.rs clones `Vec<Arc<dyn Tool>>` for each worker closure. All workers share the same `BrowserTool` Arc — which has a single `Page`.

### The Solution
For browse-tagged tasks, create a per-worker `BrowserTool` variant that holds a pool slot instead of its own browser.

**File:** `crates/temm1e-tools/src/browser.rs`

Add a pool-aware constructor:

```rust
impl BrowserTool {
    /// Create a BrowserTool bound to a specific pool slot.
    /// This instance uses the pool's browser context instead of launching its own.
    pub fn with_pool_slot(pool: Arc<BrowserPool>, slot: usize, vault: Option<Arc<dyn Vault>>) -> Self {
        Self {
            browser: Arc::new(Mutex::new(None)),  // Not used — pool provides pages
            page: Arc::new(Mutex::new(None)),      // Not used
            pool_binding: Some((pool, slot)),       // NEW field
            vault,
            // ... other fields ...
        }
    }
}
```

In `ensure_browser()`, check pool binding first:
```rust
async fn ensure_browser(&self) -> Result<Page, Temm1eError> {
    if let Some((ref pool, slot)) = self.pool_binding {
        return pool.get_page(slot).await;
    }
    // ... existing browser launch logic (for non-pool usage) ...
}
```

**File:** `src/main.rs`

In the Hive `execute_fn` closure, modify tool setup for browse tasks:

```rust
let execute_fn = move |task: HiveTask, deps: Vec<(String, String)>| {
    let tools = if task.context_tags.contains(&"browse".to_string()) {
        // Acquire a pool slot for this browse task
        let slot = browser_pool.try_acquire()
            .ok_or(Temm1eError::Tool("No browser slots available".into()))?;

        // Create per-worker tool vec with pool-bound BrowserTool
        let mut worker_tools = tools_template.clone();
        // Replace the shared BrowserTool with a pool-bound one
        worker_tools.retain(|t| t.name() != "browser");
        worker_tools.push(Arc::new(BrowserTool::with_pool_slot(
            browser_pool.clone(), slot, vault.clone()
        )));
        worker_tools
    } else {
        tools_template.clone()  // Non-browse tasks: shared tools (existing behavior)
    };

    async move {
        let result = execute_task(task, deps, workers).await;
        // Release pool slot if this was a browse task
        if let Some(slot) = browse_slot {
            browser_pool.release(slot).await?;
        }
        result
    }
};
```

### Risk Assessment
**Only browse-tagged tasks are affected.** Non-browse tasks use the existing shared `tools_template.clone()` path — zero behavior change. The `retain` + `push` pattern replaces only the `BrowserTool` in the per-worker tool vec.

### Estimated: ~50 lines in browser.rs, ~30 lines in main.rs
### Risk: 0% — browse tasks get per-worker tools, non-browse tasks unchanged

---

## 5.3 Browse-Specific Pheromone Signals

**File:** `crates/temm1e-hive/src/types.rs`

Extend `SignalType` enum:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalType {
    // Existing
    Completion,
    Failure,
    Difficulty,
    Urgency,
    Progress,
    HelpWanted,
    // New (Prowl)
    BotDetected,
    SessionExpired,
    DataFound,
    RateLimit,
}

impl SignalType {
    pub fn as_str(&self) -> &'static str {
        match self {
            // ... existing ...
            Self::BotDetected => "bot_detected",
            Self::SessionExpired => "session_expired",
            Self::DataFound => "data_found",
            Self::RateLimit => "rate_limit",
        }
    }

    pub fn default_intensity(&self) -> f64 {
        match self {
            // ... existing ...
            Self::BotDetected => 1.0,
            Self::SessionExpired => 1.0,
            Self::DataFound => 0.8,
            Self::RateLimit => 1.0,
        }
    }

    pub fn default_decay_rate(&self) -> f64 {
        match self {
            // ... existing ...
            Self::BotDetected => 0.004,      // ~3 min half-life
            Self::SessionExpired => 0.012,   // ~1 min half-life
            Self::DataFound => 0.001,        // ~10 min half-life
            Self::RateLimit => 0.002,        // ~5 min half-life
        }
    }
}
```

Also update `read_all_for_target()` in `pheromone.rs` to include new variants in the iteration.

**Emission points in BrowserTool:**

```rust
// After receiving HTTP 403 on navigate:
if response_status == 403 {
    if let Some(pheromone_field) = &self.pheromone_field {
        let domain = extract_domain(&url);
        pheromone_field.emit_default(SignalType::BotDetected, &domain, Some(&worker_id)).await?;
    }
}

// After detecting login page when session was expected:
if expected_authenticated && tree_has_login_prompt {
    if let Some(pf) = &self.pheromone_field {
        pf.emit_default(SignalType::SessionExpired, service, Some(&worker_id)).await?;
    }
}

// After successful data extraction:
if let Some(pf) = &self.pheromone_field {
    pf.emit_default(SignalType::DataFound, &task_id, Some(&worker_id)).await?;
}

// After HTTP 429:
if response_status == 429 {
    if let Some(pf) = &self.pheromone_field {
        pf.emit_default(SignalType::RateLimit, &domain, Some(&worker_id)).await?;
    }
}
```

### Estimated: ~40 lines in types.rs, ~30 lines emission points
### Risk: 0% — additive enum variants, existing signals unaffected

---

## 5.4 Progressive Delivery

### Implementation

Add a watcher task in the Hive `execute_order()` that polls the Blackboard:

**File:** `crates/temm1e-hive/src/lib.rs`

```rust
// In execute_order(), after spawning workers:

// Spawn progress watcher (for browse orders)
let bb_watcher = blackboard.clone();
let progress_tx = progress_sender.clone();  // mpsc channel to main
let total_tasks = task_count;

let watcher_handle = tokio::spawn(async move {
    let mut last_completed = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;

        let completed = bb_watcher.count_completed(order_id).await.unwrap_or(0);
        if completed > last_completed {
            // New completions — send progress update
            let results = bb_watcher.get_completed_results(order_id, last_completed).await
                .unwrap_or_default();
            last_completed = completed;

            let _ = progress_tx.send(ProgressUpdate {
                completed,
                total: total_tasks,
                new_results: results,
            }).await;
        }

        if completed >= total_tasks { break; }
    }
});
```

**In main.rs**, receive progress updates and edit the Telegram message:

```rust
// After sending initial "Pack activated" message, save its message_id
let progress_msg_id = channel.send_message(chat_id, "Pack activated...").await?.message_id;

// In a select loop alongside waiting for execute_order():
loop {
    tokio::select! {
        Some(update) = progress_rx.recv() => {
            let status = format_progress(&update);
            channel.edit_message(chat_id, progress_msg_id, &status).await?;
        }
        result = &mut order_future => {
            // Order complete — send final result
            break;
        }
    }
}
```

**Progress message format:**
```
Searching 4 sites for Tokyo flights...
✓ Google Flights: $450 JAL direct (2.1s)
✓ Kayak: $470 cheapest (3.4s)
⏳ Skyscanner: checking...
⏳ United: checking...
```

### Estimated: ~80 lines (watcher) + ~40 lines (main.rs integration)
### Risk: 0% — watcher is a new optional task, doesn't affect existing Hive flow

---

## 5.5 Queen Web Decomposition

**File:** `crates/temm1e-hive/src/queen.rs`

Add web-specific guidance to the Queen's decomposition prompt:

```rust
const WEB_DECOMPOSITION_GUIDE: &str = r#"
When decomposing web browsing tasks:
- Each different website/domain is an INDEPENDENT subtask (no dependencies between sites)
- Tag browse subtasks with "browse" in context_tags so workers claim browser contexts
- Always add a final "aggregate" or "compare" task that depends on all browse subtasks
- Include the full target URL in each subtask's description
- If a subtask requires authentication, include "auth:{service_name}" in context_tags
- Example decomposition for "compare prices on Amazon, eBay, Walmart":
  t1: "Search Amazon for {item}" [tags: browse] [deps: none]
  t2: "Search eBay for {item}" [tags: browse] [deps: none]
  t3: "Search Walmart for {item}" [tags: browse] [deps: none]
  t4: "Compare results and rank by price" [deps: t1, t2, t3]
"#;
```

Inject this into the Queen's system prompt when the task mentions web/browse/search/compare:

```rust
fn build_queen_prompt(task: &str, tools_available: &[&str]) -> String {
    let mut prompt = QUEEN_BASE_PROMPT.to_string();

    if tools_available.contains(&"browser") {
        prompt.push_str(WEB_DECOMPOSITION_GUIDE);
    }

    prompt
}
```

### Estimated: ~20 lines
### Risk: 0% — additive prompt extension, only when browser tool is available

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 5.1 Browser Pool | 100% | 0% | 100% | Ready |
| 5.2 Per-Worker BrowserTool | 100% | 0% | 100% | Ready — isolated via pool slots |
| 5.3 Pheromone Signals | 100% | 0% | 100% | Ready — 4 new variants |
| 5.4 Progressive Delivery | 100% | 0% | 100% | Ready — watcher + Telegram edit |
| 5.5 Queen Decomposition | 100% | 0% | 100% | Ready — prompt extension |
