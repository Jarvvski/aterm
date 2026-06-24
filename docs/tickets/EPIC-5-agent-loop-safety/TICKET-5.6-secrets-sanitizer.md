---
id: T-5.6
epic: EPIC-5-agent-loop-safety
title: Single Secrets source + OutputSanitizer
status: done
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

# Resolution

**done 2026-06-24** (jj, not pushed). Crate `aterm-agent`, module `secrets`
(+ the `sanitizer` it feeds and the gate seam in `risk`/`policy`/`turn`). Pure
logic, Linux-runnable. The scaffold already carried a `Secrets`/`OutputSanitizer`
that *looked* done; this ticket made the single-source invariant real, fixed the
matching semantics to the prototype's, and proved it under adversarial review.

**The single source (AC1 - the load-bearing property).** Before, `Secrets` held
only the redaction `values`; the path deny-set was a `static` const reached via
an *associated* `Secrets::argv_touches_secret`, so the gate and the sanitizer did
NOT share one instance and the deny-set could not be mutated. Now ONE `Secrets`
struct holds BOTH `sensitive_paths` (seeded from `SENSITIVE_PATHS`, extendable via
`add_sensitive_path`) and `values`. Path matching is a `&self` method; the gate
borrows the same `&Secrets` the sanitizer borrows. The borrow seam is threaded
through `DefaultRiskClassifier::classify(cmd, &Secrets)` →
`ApprovalPolicy::decide(line, &Secrets)` → `AgentTurn` (which already held one
`&Secrets`). `turn::gate_and_sanitizer_cannot_drift_single_secrets_source` is the
red-capable end-to-end proof: add a sensitive path + a secret value to one
instance, and `cat <path>` (which would otherwise auto-run) is refused with
`SecretPathAccess` AND the value is redacted - from the same instance.

**Matching rule = case-insensitive substring (AC4 + AC5 port-parity).** The
scaffold used a 3-branch (path-contains / dotfile-ext / basename-exact) matcher;
the prototype's `Secrets.isSensitivePath` is a pure case-insensitive substring
(`token.lowercase().contains(pattern.lowercase())`). The 3-branch matcher both
diverged from parity AND created a fail-open (see below), so it was replaced by
the prototype's substring rule. `is_sensitive_path` is now also pure (the old
`normalize_home` `$HOME` env read was dropped; the prototype's relative fragments
make it unnecessary). The deny-set was ported from the prototype's curated list
(incl. the ticket-mandated provider-key env-var *names* and `aterm/config.toml`,
plus the IMDS IP and k8s SA mount as substring patterns) and kept a strict
superset of the pre-port coverage (re-adding `secrets.yaml`/`secrets.yml`/
`credentials`, see remediation #2). `benign_tokens_not_sensitive` guards that
substring matching does not over-fire on ordinary tokens (`monkey` ⊅ `.key`).

**Sanitizer (AC2 + AC3) was already correct** and confirmed against the prototype
`OutputSanitizer.kt`: redact-before-truncate ordering and soft-wrap (`\n`)
tolerance. (Mine over-tolerates `\r`/spaces too - the safe direction.) The
prototype's default 100 KiB cap + truncation marker are deferred to the consumer
that actually bounds output (T-5.9/T-5.10); AC3's *ordering* + no-tail-leak is
satisfied and tested (`redacts_before_truncation`).

**ACs:** AC1 `gate_and_sanitizer_cannot_drift_single_secrets_source` (turn.rs);
AC2 `tolerates_softwrap_newline_in_secret`/`_with_spaces`; AC3
`redacts_before_truncation`; AC4 `sensitive_path_match_is_case_insensitive` (now
covers the bare-filename shape via `ID_RSA`/`AUTHORIZED_KEYS`, reachable only
through their own pattern); AC5 substring rule + ported deny-set +
`cloud_and_k8s_metadata_endpoints_are_sensitive` +
`provider_key_env_names_and_aterm_config_are_sensitive`. 54 `aterm-agent` tests,
all green; clippy clean at CI parity (`-D warnings`). No version bump/CHANGELOG
(internal engine API, agent loop still stubbed, no user-visible surface - matches
T-3.1/T-3.7).

**Adversarial review (two passes, ultracode).** Pass 1 (5 lenses): confirmed the
IMDS `169.254.169.254` entry was a DEAD rule under the basename matcher (a real
`curl http://169.254.169.254/latest/meta-data/...` never matched) + an AC4
test-coverage gap - both fixed by the substring switch; 4 findings dismissed.
Pass 2 (3 lenses, on the remediation): confirmed (a) a regression I introduced -
the prototype re-seed dropped `secrets.yaml`/`secrets.yml`/`credentials`, now
re-added with a red gate test `risk::bare_credential_files_are_secret_path_access`;
and (b) a pre-existing classifier fail-open - bare `env`/`printenv` auto-runs and
dumps the environment (incl. the agent's key). (b) is `risk.rs` classifier logic
(T-5.5's explicit domain; the prototype gates it via an `ENV_DUMP` *head* rule in
`Risk.kt`, not in the Secrets source), is latent (no execution wired), and was
recorded as a **Pre-work finding on T-5.5** rather than half-ported here. Two
findings dismissed (the `.env.example` false positive is sanctioned
over-approximation; a doc-overclaim subsumed by (b), but the comment was tightened
anyway).

**Scope held:** no classifier port-parity work (T-5.5), no sandbox (T-5.7), no
execution (T-5.9). The gate's `classify` rule set is unchanged in logic - only its
consumption of the single `Secrets` source and the deny-set/matcher it reads.
