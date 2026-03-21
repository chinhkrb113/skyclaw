# Phase 6: Benchmark Suite (TBench)

> **Depends on:** All previous phases
> **Status:** C=100%, R=0%, K=100%

---

## 6.1 Infrastructure

**Location:** `tems_lab/prowl/bench/`

**Pattern:** Same as existing Lambda benchmarks — Python scripts that launch TEMM1E, send messages via CLI chat, capture output, measure metrics.

**Core measurement framework:**

```python
# tems_lab/prowl/bench/tbench.py

import subprocess, time, json, re

class TBenchRunner:
    def __init__(self, binary="./target/release/temm1e"):
        self.binary = binary
        self.results = []

    def run_task(self, task_name, messages, timeout=120, expect_tools=None):
        """Run a single benchmark task via CLI chat."""
        start = time.time()

        # Build input script
        script = "\n".join(
            f"sleep 10\necho '{msg}'" for msg in messages
        ) + "\nsleep 10\necho '/quit'"

        # Run via subshell pipe
        proc = subprocess.run(
            f"({script}) | {self.binary} chat 2>&1",
            shell=True, capture_output=True, text=True, timeout=timeout
        )

        elapsed = time.time() - start
        output = proc.stdout

        # Parse metrics from output
        result = {
            "task": task_name,
            "wall_clock_ms": int(elapsed * 1000),
            "output_length": len(output),
            "success": self._check_success(output, expect_tools),
            "tool_calls": self._count_tool_calls(output, "browser"),
            "tokens": self._extract_tokens(output),
            "cost_usd": self._extract_cost(output),
            "errors": self._extract_errors(output),
        }
        self.results.append(result)
        return result

    def report(self, suite_name):
        """Generate benchmark report."""
        total = len(self.results)
        passed = sum(1 for r in self.results if r["success"])
        total_tokens = sum(r["tokens"] for r in self.results)
        total_cost = sum(r["cost_usd"] for r in self.results)
        avg_time = sum(r["wall_clock_ms"] for r in self.results) / total if total > 0 else 0

        report = {
            "suite": suite_name,
            "total_tasks": total,
            "passed": passed,
            "failed": total - passed,
            "pass_rate": f"{passed/total*100:.1f}%",
            "total_tokens": total_tokens,
            "total_cost_usd": round(total_cost, 4),
            "avg_wall_clock_ms": int(avg_time),
            "results": self.results,
        }

        with open(f"tems_lab/prowl/bench/results/{suite_name}.json", "w") as f:
            json.dump(report, f, indent=2)

        return report
```

---

## 6.2 TBench-Simple (50 tasks)

**What it tests:** Basic web browsing — navigate, observe, extract, search.

**File:** `tems_lab/prowl/bench/tbench_simple.py`

### Task categories:

**Navigation (10 tasks):**
```python
tasks = [
    ("nav_wikipedia", "Go to wikipedia.org and tell me the featured article title"),
    ("nav_github", "Go to github.com/temm1e-labs/temm1e and tell me the star count"),
    ("nav_hackernews", "Go to news.ycombinator.com and list the top 3 stories"),
    ("nav_weather", "Go to wttr.in/Tokyo and tell me tomorrow's weather"),
    # ... 6 more navigation tasks
]
```

**Observation tiers (10 tasks):**
```python
# Tier 1: accessibility tree sufficient
("obs_t1_links", "Go to example.com and list all links on the page"),
# Tier 2: needs DOM for tables
("obs_t2_table", "Go to en.wikipedia.org/wiki/Rust_(programming_language) and extract the version history table"),
# Tier 3: needs screenshot for visual
("obs_t3_visual", "Go to google.com and describe the layout of the page"),
```

**Search (10 tasks):**
```python
("search_wiki", "Search Wikipedia for 'stigmergy' and give me the first paragraph"),
("search_github", "Search GitHub for 'rust web scraper' and list the top 3 repos"),
```

**Extraction (10 tasks):**
```python
("extract_price", "Go to books.toscrape.com and tell me the price of the first book"),
("extract_list", "Go to news.ycombinator.com and extract all story titles as a list"),
```

**Form interaction (10 tasks):**
```python
("form_search", "Go to duckduckgo.com and search for 'Tem Prowl'"),
("form_login_test", "Go to the-internet.herokuapp.com/login and tell me what fields are on the form"),
```

### Metrics per task:
- Wall clock time (target: <30s for simple tasks)
- Token count (target: <5,000 for Tier 1 tasks)
- Tool calls (count of browser actions)
- Success (binary: did it return the correct answer?)
- Observation tier used (verify Tier 1 default for simple pages)

### Validation:
Each task has an `expect` function that checks the output:
```python
def validate_nav_wikipedia(output):
    return "featured article" in output.lower() or len(output) > 50

def validate_search_wiki(output):
    return "stigmergy" in output.lower()
```

---

## 6.3 TBench-Auth (30 tasks)

**What it tests:** Credential isolation, vault injection, session management.

**File:** `tems_lab/prowl/bench/tbench_auth.py`

**Test site:** `the-internet.herokuapp.com` (Heroku test app with login page, known credentials: tomsmith/SuperSecretPassword!)

### Task categories:

**Vault credential injection (10 tasks):**
```python
# Pre-store test credentials in vault
# Then test login flow
("auth_vault_login", "Log into the-internet.herokuapp.com/login using stored credentials"),
("auth_vault_verify", "After login, what does the secure area page say?"),
("auth_vault_wrong", "Try to log into the-internet.herokuapp.com with credentials for 'nonexistent_service'"),
```

**Credential scrubber verification (10 tasks):**
```python
# Inject known credential values into test pages, verify they don't appear in LLM context
("scrub_url_param", "Navigate to example.com?token=secret123 and tell me the URL"),
# Verify output contains "[REDACTED]" not "secret123"
```

**Session management (10 tasks):**
```python
("session_save", "Log into the test site and save the session"),
("session_restore", "Restore the test site session and verify still logged in"),
("session_expired", "Try to restore a session for 'expired_service' (pre-populated with old cookies)"),
```

### Critical security validation:
```python
def validate_no_credential_leak(output, known_credentials):
    """MUST PASS: credential bytes never appear in agent output."""
    for cred in known_credentials:
        assert cred not in output, f"CREDENTIAL LEAK: '{cred}' found in output!"
    return True
```

This is the most important test in the entire suite. One failure = stop everything.

---

## 6.4 TBench-Swarm (30 tasks)

**What it tests:** Parallel browsing, speedup, progressive delivery, failure isolation.

**File:** `tems_lab/prowl/bench/tbench_swarm.py`

### Task categories:

**Multi-site comparison (10 tasks):**
```python
("swarm_3sites", "Compare the top story on news.ycombinator.com, reddit.com/r/programming, and lobste.rs"),
("swarm_4sites", "Find the title of the homepage on wikipedia.org, bbc.com, nytimes.com, and reuters.com"),
```

**Speedup measurement (10 tasks):**
```python
# Run same task in single-agent mode and swarm mode, compare wall clock
def measure_speedup(task, messages):
    # Single agent
    t_single = runner.run_task(f"{task}_single", messages, env={"HIVE_ENABLED": "false"})
    # Swarm
    t_swarm = runner.run_task(f"{task}_swarm", messages, env={"HIVE_ENABLED": "true"})

    speedup = t_single["wall_clock_ms"] / t_swarm["wall_clock_ms"]
    token_ratio = t_swarm["tokens"] / max(t_single["tokens"], 1)

    return {
        "speedup": speedup,
        "token_ratio": token_ratio,
        "single_ms": t_single["wall_clock_ms"],
        "swarm_ms": t_swarm["wall_clock_ms"],
    }
```

**Failure isolation (5 tasks):**
```python
# Tasks where one site is known to fail (timeout, block)
("swarm_partial_fail", "Compare data from site-a.com, site-b.com (will timeout), and site-c.com"),
# Verify: sites A and C return results despite B failing
```

**Progressive delivery (5 tasks):**
```python
# Verify that partial results arrive before full completion
# Check timestamps of intermediate messages vs final message
```

### Metrics:
- Speedup factor (target: >2x for 3+ sites)
- Token cost ratio swarm/single (target: <1.0 for 3+ sites)
- Failure isolation (target: 100% — other Tems unaffected)
- Progressive delivery latency (target: first partial result within 50% of total time)

---

## 6.5 TBench-Adversarial (20 tasks)

**What it tests:** Anti-bot, dynamic content, popups, SPAs.

**File:** `tems_lab/prowl/bench/tbench_adversarial.py`

### Task categories:

**Dynamic content (5 tasks):**
```python
# SPAs that load content via JS
("adv_spa", "Go to a React-based site and extract the dynamically loaded content"),
```

**Popups and overlays (5 tasks):**
```python
("adv_cookie_banner", "Navigate to a site with a cookie consent banner and accept it"),
("adv_popup", "Navigate to a site with a popup overlay and close it"),
```

**Anti-bot detection (5 tasks):**
```python
("adv_cloudflare", "Navigate to a Cloudflare-protected site and extract the page title"),
# Expected: either succeeds (stealth works) or gracefully reports the block
```

**Rate limiting (5 tasks):**
```python
("adv_rate_limit", "Make 5 rapid requests to a rate-limited endpoint"),
# Expected: handles 429 gracefully, backs off, retries
```

### Validation:
- Graceful degradation: agent NEVER crashes, always reports what happened
- No silent failures: every blocked/failed task produces a user-facing message
- Anti-bot: measure pass rate (target: >80% for standard protection)

---

## 6.6 TBench-Resilience (20 tasks)

**What it tests:** Crash recovery, timeout handling, convergence bounds.

**File:** `tems_lab/prowl/bench/tbench_resilience.py`

### Task categories:

**Browser crash recovery (5 tasks):**
```python
# Inject a crash mid-task (kill Chrome process, corrupt page state)
# Verify: agent detects, recovers, reports error — does NOT crash the process
```

**Timeout handling (5 tasks):**
```python
# Navigate to extremely slow pages (10+ second load)
# Verify: timeout triggers, agent reports, moves on
```

**Session expiry mid-task (5 tasks):**
```python
# Start with valid session, invalidate cookies mid-task
# Verify: agent detects login page, reports session expired
```

**Convergence bounds (5 tasks):**
```python
# Tasks designed to trigger retries
# Verify: agent terminates within R*k attempts (R=3 default)
# Verify: anti-loop detection triggers if same action repeated 3x
```

### Metrics:
- Recovery rate (target: >90%)
- MTBF proxy (no crashes across full suite)
- Convergence: all tasks terminate within bound

---

## 6.7 Running the Full Suite

**File:** `tems_lab/prowl/bench/run_all.sh`

```bash
#!/bin/bash
set -e

# Build release binary
cargo build --release --bin temm1e --features prowl

# Ensure clean state
rm -f ~/.temm1e/memory.db

# Source env (without ANTHROPIC_API_KEY for onboarding test, with for others)
grep -E "^[A-Z_]+=" .env | sed 's/^/export /' > /tmp/prowl_env.sh
source /tmp/prowl_env.sh

# Create results directory
mkdir -p tems_lab/prowl/bench/results

# Run suites
echo "=== TBench-Simple (50 tasks) ==="
python3 tems_lab/prowl/bench/tbench_simple.py

echo "=== TBench-Auth (30 tasks) ==="
python3 tems_lab/prowl/bench/tbench_auth.py

echo "=== TBench-Swarm (30 tasks) ==="
python3 tems_lab/prowl/bench/tbench_swarm.py

echo "=== TBench-Adversarial (20 tasks) ==="
python3 tems_lab/prowl/bench/tbench_adversarial.py

echo "=== TBench-Resilience (20 tasks) ==="
python3 tems_lab/prowl/bench/tbench_resilience.py

# Generate combined report
python3 tems_lab/prowl/bench/generate_report.py
```

**Combined report** aggregates all suites into `tems_lab/prowl/bench/results/TBENCH_REPORT.md` with:
- Per-pillar scores (the 10 Pillars from the paper)
- Pass/fail counts per suite
- Token and cost totals
- Speedup measurements
- Security validation results (credential leak = instant fail)

---

## Summary

| Task | C | R | K | Status |
|------|---|---|---|--------|
| 6.1 Infrastructure | 100% | 0% | 100% | Ready |
| 6.2 TBench-Simple | 100% | 0% | 100% | Ready |
| 6.3 TBench-Auth | 100% | 0% | 100% | Ready |
| 6.4 TBench-Swarm | 100% | 0% | 100% | Ready |
| 6.5 TBench-Adversarial | 100% | 0% | 100% | Ready |
| 6.6 TBench-Resilience | 100% | 0% | 100% | Ready |

---

*Benchmark suite for Tem Prowl. 150 tasks across 5 suites. March 2026.*
