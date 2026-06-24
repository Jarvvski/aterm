---
id: T-5.5
epic: EPIC-5-agent-loop-safety
title: Deterministic risk gate (zsh-aware argv parse)
status: ready-for-agent
labels: [agent, safety]
depends_on: []
---

# Goal

Port the prototype's deterministic, code-side risk gate to Rust nearly verbatim: parse each proposed command's argv (zsh-aware), over-approximate toward RequireConfirm/Dangerous, never trust the model, split multi-line buffers and take MAX risk, and implement the graduated `ApprovalPolicy` with the AUTO-SAFE default and the shell-active belt-and-suspenders.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (d).1 + (f) + Recommendation 4, 10. Locked decisions: **AUTO-SAFE ON by default** - commands proven Safe (and carrying no shell-active reason) auto-run; Caution/Dangerous always require explicit confirmation. Because the default trust surface is larger, the gate MUST over-approximate toward RequireConfirm. The gate is a classifier, NOT a security boundary (sandbox in T-5.7 is the boundary).
- Prototype reference: `CommandLineRisk.kt`, `Risk.kt`, `DefaultApprovalPolicy` (`SHELL_ACTIVE_REASONS`), `RiskGloss.kt`.

# Implementation notes

- Crate: `aterm-agent`. Module `risk`. Pure logic, no network, heavily unit-tested.
- Parse argv ourselves, zsh-aware: resolve the head (skip env-assignment prefixes / precommand modifiers), detect shell metacharacters, redirects, chaining (`&&`/`||`/`;`/`|`), history-expansion (`^`), leading-tilde expansion, fork-bombs.
- Risk levels: `Safe` / `Caution` / `Dangerous` + a set of typed reasons. Over-approximate: reading API key/Keychain/known credential paths, `env`/`printenv`, interpreter-with-inline-code (`python -c`, `node -e`, `sh -c`), `eval`/`source`, build tools, `find -exec` -> Dangerous. The path deny-set comes from the single Secrets source (T-5.6) so gate + sanitizer cannot drift.
- Multi-line buffers: split per line, classify each, take the MAX (`classify_command_buffer`) - a benign first line cannot smuggle a dangerous second past a head-keyed rule via an embedded `\n`.
- Remote (SSH) over-approximation: a `RemoteContext` forces a `RemoteExecution` Caution baseline (never auto-runs); unknown remote cwd over-approximates relative-path args to `SecretAccess`.
- `ApprovalPolicy`: `ask-always` -> `auto-safe` (DEFAULT: auto-approve only `Safe` AND no shell-active reason) -> `auto-run-in-session` (session-scoped widening that still refuses shell-active strings). The `SHELL_ACTIVE_REASONS` set forces RequireConfirm even at Safe for any shell metacharacter/`~`/redirect/chaining/history-expansion.
- `RiskGloss`: human-readable reason text for the approval UI (T-5.11).

# Acceptance criteria

- Port-parity unit tests mirror the prototype's classifier cases (metachars, redirects, interpreters, credential paths, fork-bombs).
- AUTO-SAFE default: a plain `ls -la` auto-runs; `cat ~/.ssh/id_rsa`, `python -c '...'`, `rm -rf ~`, anything with `|`/`>`/`&&`/`~`/`$()` requires confirmation even though it might look Safe.
- Multi-line buffer takes MAX risk; an embedded `\n` cannot downgrade.
- A `RemoteContext` never auto-runs.
- The gate consumes the path deny-set from the Secrets source (T-5.6), not a private copy.
- Never reads/trusts a model-reported risk level.

# Out of scope

- Secrets source + sanitizer (T-5.6), sandbox (T-5.7), execution (T-5.9), approval UI (T-5.11).

# Pre-work findings (must fix when this ticket is worked)

**Env-dump fail-open in the scaffold classifier (found during T-5.6 review,
2026-06-24).** The scaffold `risk.rs` lists `env` and `printenv` in
`SAFE_PROGRAMS` and has NO env-dump head rule, so a bare `env` / `printenv`
classifies `Safe` with no reasons and AUTO-RUNS - dumping the whole environment,
including the agent's own `ANTHROPIC_API_KEY`/`OPENAI_API_KEY` when the env-var
key fallback is in use (see `Secrets::from_env`). `printenv AWS_SECRET_ACCESS_KEY`
(any credential var) is the same fail-open: the T-5.6 deny-set carries the two
provider key *names*, but per-token substring matching only catches the
explicit-arg read of *those two literal names*, never the bare verb form nor
other credential vars. This is the canonical credential-exfil shape and it is a
fail-open MISS (the unsafe direction).

The prototype gates this in the classifier, not the Secrets source:
`Risk.kt` has `ENV_DUMP = setOf("env", "printenv")` and
`if (parsed.head in ENV_DUMP) reasons.add(RiskReason.SecretAccess)` (Dangerous),
asserted by `RiskClassifierTest.kt::envDumpIsDangerous`. **Fix when porting:**
remove `env`/`printenv` from `SAFE_PROGRAMS`, add an `ENV_DUMP` head rule
escalating to `Dangerous`/`SecretPathAccess`, and add red-capable tests
(`env`, `printenv`, `printenv AWS_SECRET_ACCESS_KEY` → Dangerous + RequireConfirm;
a benign carrier like `echo env` stays Safe). Latent today (no execution path is
wired - T-5.9 not landed - and T-5.9 depends on this ticket), but it MUST be
closed before any command-execution sink goes live.
