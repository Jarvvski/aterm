---
id: T-5.6
epic: EPIC-5-agent-loop-safety
title: Single Secrets source + OutputSanitizer
status: ready-for-agent
labels: [agent, safety]
depends_on: []
---

# Goal

Port the single Secrets source (one list of sensitive paths + secret values) feeding BOTH the risk gate's deny-set and the OutputSanitizer that redacts secret values before truncation - so the two defenses cannot drift. This single-source invariant is the most important structural property to preserve.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (d).2 + (f).2 + Recommendation 4. Prototype: `Secrets.kt` (sensitivePaths + secretValues), `OutputSanitizer.kt` (soft-wrap-aware redaction, size bound, shared Secrets).

# Implementation notes

- Crate: `aterm-agent`. Module `secrets`. Pure logic.
- One `Secrets` struct: `sensitive_paths` (credential files `.ssh/`, `.aws/`, `.env`, `ANTHROPIC_API_KEY` config, cloud-metadata IP `169.254.169.254`, k8s SA token mount, aterm's own config holding the key in plaintext) and `secret_values` (actual key strings). Single source consumed by the gate (T-5.5) via a borrow/handle, not a copy.
- Path matching: case-insensitive substring (macOS FS is case-insensitive).
- `OutputSanitizer`: redact `secret_values` from all captured output BEFORE it re-enters the model context and BEFORE size truncation; soft-wrap-aware (tolerate a `\n` between any two chars of a secret). Bound output size.

# Acceptance criteria

- The gate and sanitizer reference the SAME `Secrets` instance (a test mutates the source and observes both reflect it - they cannot drift).
- A secret value split across a soft-wrap (`\n` inserted mid-token) is still redacted.
- Output exceeding the size bound is truncated AFTER redaction (no secret leaks via the truncated tail).
- Sensitive-path matching is case-insensitive.
- Port-parity with the prototype's sanitizer test cases.

# Out of scope

- Where the API key is actually stored (T-8.3 Keychain) - the Secrets source records what's sensitive, not the custody mechanism.
- The gate (T-5.5) and execution (T-5.9).
