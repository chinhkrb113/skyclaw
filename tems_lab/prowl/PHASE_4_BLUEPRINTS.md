# Phase 4: Web Blueprints

> **Depends on:** Phase 1 (observe action exists)
> **Status:** C=100%, R=0%, K=100%

---

## 4.1 Design

Web Blueprints are standard TEMM1E blueprints stored as `MemoryEntry` with `MemoryEntryType::Blueprint`. They use the existing blueprint system — no new infrastructure. The classifier's `blueprint_hint` routes web tasks to these blueprints via `semantic_tags`.

---

## 4.2 Blueprint Files

Four YAML+Markdown files to be seeded into memory on first Prowl-enabled run.

### Blueprint 1: `web_search`

```yaml
---
id: bp_prowl_search
name: Web Search Pattern
semantic_tags: ["web", "search", "browse", "find", "lookup"]
task_signature: "search for {query} on {site}"
success_count: 0
failure_count: 0
---
## Objective
Search for information on a website using the browser tool.

## Prerequisites
- Browser tool available
- Target URL known or derivable

## Phases

### Phase 1: Navigate (independent)
1. `browser(action="navigate", url="{target_url}")`
2. `browser(action="observe")` — get page structure

### Phase 2: Search (depends: Phase 1)
1. From the accessibility tree, find the search textbox (role=textbox, name contains "search")
2. `browser(action="type", selector="{search_selector}", text="{query}")`
3. Press Enter or click search button
4. `browser(action="observe")` — see results

### Phase 3: Extract (depends: Phase 2)
1. Parse results from accessibility tree (links, text, structured data)
2. Format as a concise summary for the user
3. If results span multiple pages, check if pagination is needed

## Failure Recovery
- If search box not found: try `browser(action="observe", hint="form")` for Tier 2
- If no results: try alternative search terms
- If site blocks: report to user, suggest alternative site

## Verification
- Results contain relevant content matching the query
- At least one actionable result returned to user
```

### Blueprint 2: `web_login`

```yaml
---
id: bp_prowl_login
name: Web Login Flow
semantic_tags: ["web", "login", "authenticate", "sign in", "account"]
task_signature: "log into {service}"
success_count: 0
failure_count: 0
---
## Objective
Authenticate to a web service for the user.

## Phases

### Phase 1: Check existing session (independent)
1. `browser(action="restore_session", service="{service}")` — try stored session
2. If session is valid (no login prompt in tree), DONE — skip remaining phases

### Phase 2: Vault credentials (depends: Phase 1, only if session invalid)
1. `browser(action="navigate", url="{service_login_url}")`
2. `browser(action="authenticate", service="{service}")` — vault injection
3. If vault has credentials, they are injected automatically
4. `browser(action="observe")` — verify authenticated state

### Phase 3: OTK capture (depends: Phase 2, only if no vault credentials)
1. `browser(action="authenticate", service="{service}", method="otk")`
2. Send OTK interactive session link to user
3. Wait for user to complete login and say "done"
4. Session captured automatically

## Failure Recovery
- CAPTCHA detected: send screenshot to user, ask them to solve via OTK session
- 2FA required: escalate to OTK session (user handles 2FA directly)
- Wrong credentials: notify user, offer to update via `/addcred {service}`
- Session expired mid-task: re-run from Phase 1

## Verification
- Accessibility tree shows authenticated state (no "Sign In" / "Log In" buttons)
- User's account info visible in tree (username, avatar, dashboard elements)
```

### Blueprint 3: `web_extract`

```yaml
---
id: bp_prowl_extract
name: Web Data Extraction
semantic_tags: ["web", "extract", "table", "data", "scrape", "read", "get"]
task_signature: "extract {data} from {url}"
success_count: 0
failure_count: 0
---
## Objective
Extract structured data from a web page.

## Phases

### Phase 1: Navigate and observe (independent)
1. `browser(action="navigate", url="{target_url}")`
2. `browser(action="observe")` — get page structure
3. Analyze tree: does it contain the target data?

### Phase 2: Extract (depends: Phase 1)
1. If data is in accessibility tree (text, links, list items) → extract directly
2. If data is in a table → `browser(action="observe", hint="table")` for Tier 2 Markdown
3. If data requires scrolling → scroll and re-observe
4. If data is behind a click (expand, "show more") → click and re-observe

### Phase 3: Structure and deliver (depends: Phase 2)
1. Format extracted data as structured output (Markdown table, JSON, or bullet list)
2. If multi-page, paginate and extract from each page
3. Return formatted results to user

## Failure Recovery
- Data not visible: check if login required (→ web_login blueprint)
- Dynamic content not loading: wait longer, retry observe
- Anti-bot block: report to user

## Verification
- Extracted data matches user's request
- Data is structured and readable
- No credential or sensitive data in output (scrubber applied)
```

### Blueprint 4: `web_compare`

```yaml
---
id: bp_prowl_compare
name: Multi-Site Comparison
semantic_tags: ["web", "compare", "price", "shop", "aggregate", "best", "cheapest"]
task_signature: "compare {item} across sites"
success_count: 0
failure_count: 0
---
## Objective
Compare information across multiple websites. This is a natural Hive decomposition candidate.

## Phases

### Phase 1-N: Search each site (independent — parallelizable)
For each target site:
1. Navigate to site
2. Search for the item
3. Extract structured data (price, name, availability, rating)
4. Store results

### Phase N+1: Aggregate (depends: all Phase 1-N)
1. Collect all results
2. Compare by user's criteria (price, rating, availability)
3. Rank results
4. Format as comparison table

## Notes
- When Hive is active, the Queen decomposes this into N independent browse tasks + 1 aggregation
- Each Tem gets its own browser context (isolated sessions)
- Progressive delivery: partial results sent as each site completes
- If a site blocks or fails, skip it and note in results (don't block other sites)

## Failure Recovery
- Site blocked: skip, note in results
- No results on a site: skip, note in results
- All sites fail: report to user with specific errors per site

## Verification
- At least 2 sites successfully compared
- Results are structured as a comparison table
- Rankings match user's stated criteria
```

---

## 4.3 Blueprint Seeding

**File:** `crates/temm1e-agent/src/blueprint.rs` (or initialization code)

```rust
const WEB_BLUEPRINTS: &[&str] = &[
    include_str!("../../tems_lab/prowl/blueprints/web_search.md"),
    include_str!("../../tems_lab/prowl/blueprints/web_login.md"),
    include_str!("../../tems_lab/prowl/blueprints/web_extract.md"),
    include_str!("../../tems_lab/prowl/blueprints/web_compare.md"),
];

pub async fn seed_web_blueprints(memory: &dyn Memory) -> Result<(), Temm1eError> {
    for bp_content in WEB_BLUEPRINTS {
        let bp = parse_blueprint(bp_content)?;
        // Check if already exists (by id)
        if memory.search(&bp.id, 1).await?.is_empty() {
            memory.store(MemoryEntry {
                id: bp.id.clone(),
                content: bp_content.to_string(),
                entry_type: MemoryEntryType::Blueprint,
                session_id: "system".to_string(),
                metadata: None,
            }).await?;
        }
    }
    Ok(())
}
```

Call during agent initialization when browser tool is enabled:
```rust
if config.tools.browser {
    seed_web_blueprints(&memory).await?;
}
```

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 4.2 Core Web Blueprints (4 files) | 100% | 0% | 100% | Ready |
| 4.3 Blueprint Registration | 100% | 0% | 100% | Ready |
