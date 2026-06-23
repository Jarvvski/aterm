# ADR-0006: Safety - deterministic risk gate, auto-safe default, mandatory Seatbelt sandbox

## Status

Accepted

## Context

aterm's agent runs commands and edits files on the user's machine and reads untrusted output
(command stdout/stderr, file contents, web/MCP results) - every read is a prompt-injection
vector. The dossier ([06-agent-architecture.md](../research/06-agent-architecture.md))
established a four-layer, defense-in-depth model where no single layer is trusted, and
identified the prior prototype's deterministic code-side risk gate, its single Secrets
source, and its output sanitizer as the most reusable assets in the old codebase. The owner
locked the autonomy default to AUTO-SAFE ON, which enlarges the default trust surface and
therefore raises the bar on the gate's over-approximation and makes OS-level confinement
mandatory rather than optional.

## Decision

- **A deterministic, code-side risk gate** (ported from the prototype's `CommandLineRisk`/
  `Risk` nearly verbatim). It parses each proposed command's argv (zsh-aware: resolves the
  head past env-assignment prefixes / precommand modifiers; detects shell metacharacters,
  redirects, chaining, history-expansion, leading-tilde, fork-bombs). It **never trusts the
  model's self-reported risk**, **over-approximates toward `RequireConfirm`/`Dangerous`**, and
  **splits multi-line buffers per line and takes the MAX risk**. Tools are typed (argv arrays,
  no shell) so the gate sees structured args, not an opaque string ([ADR-0005](0005-agent-loop-and-providers.md)).
- **Autonomy default: AUTO-SAFE ON.** Commands the gate proves `Safe` *and* that carry no
  shell-active reason auto-run by default. `Caution`/`Dangerous` always require explicit
  confirmation. Auto-run never clears shell-active strings (the `SHELL_ACTIVE_REASONS`
  belt-and-suspenders refusal). Because the default trust surface is larger, the gate must
  over-approximate toward `RequireConfirm`.
- **A single Secrets source feeds BOTH the gate and the OutputSanitizer.** One list of
  `sensitivePaths` (credential files, startup files, cloud-metadata IP, k8s SA token mount,
  aterm's own key-bearing config) and `secretValues`. The gate matches command tokens/paths
  against `sensitivePaths` (case-insensitive, since macOS FS is case-insensitive); the
  `OutputSanitizer` redacts `secretValues` from captured output **before truncation**
  (soft-wrap-aware). One source so the two defenses cannot drift - the single most important
  structural invariant.
- **A mandatory macOS Seatbelt sandbox** via `sandbox-exec`, behind a `Sandbox` trait, plus
  `setrlimit` (CPU time, address space, open files) and process-group timeout-kill. The gate
  is a classifier, not a security boundary; the sandbox is the boundary. It is mandatory, not
  optional, precisely because auto-safe enlarges the default trust surface.
- **Prompt-injection defense is layered:** the deterministic gate is the primary control
  (it classifies the *parsed command*, not the model's persuasive rationale); backed by
  output sanitization before feedback, structural separation of tool results (data role) from
  operator instructions (the non-spoofable `role: "system"` channel), the auto-safe-but-
  confirm-everything-else default, and the sandbox as backstop. System-prompt hardening is
  necessary but not sufficient on its own.
- Every gated command renders in the single wall-clock timeline as a proposal with
  human-readable risk reasons (port `RiskGloss`), Approve/Deny, and the current autonomy mode
  visibly indicated.

## Consequences

- A false positive from over-approximation costs one confirmation; a miss could leak a secret
  or destroy data - so the gate is deliberately biased toward `RequireConfirm`.
- The single Secrets source guarantees the gate's deny-set and the sanitizer's redaction set
  can never diverge.
- The sandbox is a hard dependency on `sandbox-exec`, which is deprecated with no published
  Apple removal date. Accepted with eyes open: it is the only documented mechanism to apply a
  Seatbelt profile to an arbitrary process, has no replacement, and is used by Anthropic's own
  sandbox-runtime. The `Sandbox` trait lets a future native-API or VM backend swap in;
  `setrlimit` + timeout-kill + the gate remain even if Seatbelt is pulled.
- The gate is a best-effort classifier, not a complete boundary: residual RCE via an
  un-enumerated interpreter or a novel shell construct is possible - which is exactly why the
  sandbox is mandatory and prompt-injection defense is layered, not absolute.
- Auto-run, even confined and gate-`Safe`, leaves a bounded residual risk; the session-scoped
  default and the sandbox bound the blast radius, and we do not claim full immunity.

## Alternatives considered

- **ask-always as the shipped default.** This was the dossier's original recommendation;
  the owner locked AUTO-SAFE ON instead, accepting the larger trust surface and compensating
  with stricter over-approximation and a mandatory sandbox.
- **Trusting the model's self-reported risk level.** Rejected outright: the command comes from
  an LLM that may have read injected output; risk is classified from the parsed tokens only.
- **A bare bash tool for the agent.** Rejected: it gives the gate only an opaque string; typed
  argv tools are what make deterministic classification, parallel-safety, and audit possible.
- **App Sandbox instead of `sandbox-exec`.** Rejected for v1: it requires a `.app` bundle +
  code-signing entitlement and is far less granular than a generated Seatbelt `.sb` profile;
  the `Sandbox` trait keeps it available as a future backend.
- **No OS sandbox, gate-only.** Rejected: the gate is explicitly a classifier, not a boundary;
  under an auto-safe default an OS boundary is mandatory.
