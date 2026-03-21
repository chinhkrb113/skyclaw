# Tem Prowl: Multi-Step Web Browsing Benchmark

**Date:** 2026-03-20
**Model:** gemini-3-flash-preview
**Target site:** https://the-internet.herokuapp.com (Checkboxes page)
**Feature under test:** Incremental observation hashing (hash-based delta detection)

---

## Test Matrix

Three tests were run to evaluate Tem Prowl's `observe` action vs `screenshot` for multi-step browser tasks, with a specific focus on the incremental hashing optimization.

| Test | Method | Task | Purpose |
|------|--------|------|---------|
| A | `observe` | Navigate, click Checkboxes, click checkbox, confirm | Real multi-step workflow |
| B | `screenshot` | Same task with screenshots instead of observe | Baseline comparison |
| C | `observe` (repeat) | Observe same page 3 times without changes | Isolate incremental hashing |

---

## Results Summary

| Metric | Test A: Observe | Test B: Screenshot | Test C: Hash Test |
|--------|----------------:|-------------------:|------------------:|
| **API Calls** | 10 | 14 | 7 |
| **Input Tokens** | 78,068 | 115,713 | 44,528 |
| **Output Tokens** | 616 | 682 | 371 |
| **Combined Tokens** | 78,684 | 116,395 | 44,899 |
| **Tools Used** | 14 | 18 | 5 |
| **Total Cost** | $0.0121 | $0.0178 | $0.0069 |
| **Task Completed** | Yes | Yes | Yes |
| **Incremental Hash Triggered** | No | N/A | Yes (2x) |

**Total benchmark cost: $0.0368** (well under $1.00 budget)

---

## Test A: Multi-Step Observe

**Prompt:** Navigate to the-internet.herokuapp.com, observe the page, click Checkboxes link, observe again, click checkbox 1, observe again to confirm state change.

**Result:** Task completed correctly in 10 API calls. The agent:
1. Navigated to homepage and observed (accessibility tree returned)
2. Clicked the Checkboxes link
3. Observed the Checkboxes page (new tree returned -- page content changed)
4. Clicked checkbox 1
5. Observed again (new tree returned -- checkbox state changed)
6. Reported all steps accurately

**Incremental hashing:** Did NOT trigger. This is **correct behavior** -- each observation was on a genuinely different page state (homepage -> checkboxes page -> checkboxes page with toggled checkbox). The hash changed each time, so full observation was returned each time.

**Browser note:** One browser connection loss occurred mid-session (line 56: "Browser connection lost -- relaunching"). The agent recovered automatically and continued the task.

---

## Test B: Multi-Step Screenshot

**Prompt:** Same task but requesting screenshots instead of observe.

**Result:** Task completed correctly in 14 API calls (40% more than observe). The agent:
1. Navigated and took screenshot (homepage.png -- 167,552 bytes)
2. Clicked Checkboxes link
3. Took screenshot (checkboxes_page.png -- 38,372 bytes)
4. Clicked checkbox 1
5. Took screenshot (checkbox_clicked_final.png -- 38,596 bytes)
6. Sent all 3 files to user, reported all steps accurately

**Key difference:** Screenshots were injected as vision images into the conversation context, adding significant token overhead. Each screenshot added ~1,000+ vision tokens.

---

## Test C: Incremental Hashing Isolation

**Prompt:** Navigate to /checkboxes, observe 3 times without any changes between observations.

**Result:** Incremental hashing triggered exactly as expected:
- **Observation 1:** Full accessibility tree returned (TreeWithDom tier, form selector detected)
- **Observation 2:** `[Page unchanged since last observation]` (5 tokens)
- **Observation 3:** `[Page unchanged since last observation]` (5 tokens)

**Debug log evidence:**
```
[DEBUG] Browser observe -- layered observation (hint: None, retry: false)
[DEBUG] Observe: tier selected (tier: TreeWithDom { selector: "form" })
...
[DEBUG] Browser observe -- layered observation (hint: None, retry: false)
[DEBUG] Observe: page unchanged since last observation
...
[DEBUG] Browser observe -- layered observation (hint: None, retry: false)
[DEBUG] Observe: page unchanged since last observation
```

The hash comparison happens at the tree text level before any tier analysis, making the short-circuit extremely fast.

---

## Analysis

### Observe vs Screenshot Comparison (Test A vs Test B)

| Metric | Observe | Screenshot | Delta | Savings |
|--------|--------:|----------:|------:|--------:|
| API Calls | 10 | 14 | -4 | 28.6% fewer |
| Input Tokens | 78,068 | 115,713 | -37,645 | 32.5% fewer |
| Combined Tokens | 78,684 | 116,395 | -37,711 | 32.4% fewer |
| Cost | $0.0121 | $0.0178 | -$0.0057 | 32.0% cheaper |
| Tools Used | 14 | 18 | -4 | 22.2% fewer |

**Observe delivers 32% token savings** over screenshots on this multi-step workflow, even without incremental hashing triggering (since every observation was on a changed page).

### Where Incremental Hashing Shines

Test C demonstrates the extreme case: when a page has NOT changed between observations, the hash-based delta detection returns `[Page unchanged since last observation]` (5 tokens) instead of the full tree (~200+ tokens). This is a **97.5% reduction per unchanged observation**.

In real-world scenarios, this matters for:
- **Polling loops** -- agent waits for a page to update after an action
- **Verification steps** -- agent re-observes to confirm nothing changed
- **Error recovery** -- agent re-observes after a failed click to check state
- **Multi-tab workflows** -- returning to a previously observed tab

### Token Breakdown by Method

```
Observe (full tree, Checkboxes page):
  Accessibility tree:  ~200 tokens
  DOM detail:          ~50 tokens
  Total:               ~250 tokens

Screenshot (Checkboxes page):
  Image (38KB PNG):    ~1,000+ vision tokens
  Total:               ~1,000+ tokens

Observe (incremental, unchanged):
  "[Page unchanged]":  5 tokens
  Total:               5 tokens
```

### Correctness

All three tests completed the task correctly:
- Both observe and screenshot methods correctly identified checkbox states
- The agent accurately reported initial state (checkbox 1 unchecked, checkbox 2 checked)
- The agent accurately reported final state (both checkboxes checked)
- Incremental hashing correctly returned full content on first observe, then short-circuited on unchanged pages

---

## Conclusion

1. **Observe is 32% cheaper than screenshots** for multi-step browser tasks, even when every page state is different (no hashing benefit).

2. **Incremental observation hashing works correctly.** When the same page is observed multiple times without changes, the hash-based delta detection short-circuits and returns 5 tokens instead of ~250, a 97.5% reduction per observation.

3. **The savings compound with task complexity.** Simple 3-step tasks show 32% savings. Workflows with polling, verification, or repeated observations on unchanged pages would see dramatically higher savings as the incremental hash eliminates redundant content.

4. **No correctness trade-off.** Both methods produced identical task outcomes. The observe method provides structured accessibility data that the LLM interprets correctly for UI interaction tasks.

5. **Browser resilience held.** One browser connection loss during Test A was automatically recovered with a relaunch, and the task completed successfully.
