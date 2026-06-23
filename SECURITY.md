# Security Policy

## Supported versions

aterm is pre-1.0 and under active development. Only the latest released version receives
security fixes.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | yes       |
| < 0.1   | n/a       |

## Reporting a vulnerability

Report security issues **privately** - do not open a public issue for a vulnerability.

- Preferred: GitHub's private vulnerability reporting on this repository (the **Security**
  tab, then **Report a vulnerability**).

We'll acknowledge your report as soon as we can and keep you posted on a fix. Please give a
reasonable window before any public disclosure.

## Trust and threat model

aterm is designed to embed an LLM agent that can **propose and run real shell commands on
your machine**.

> **Current status:** as of the Phase-2 scaffold the agent loop is **not yet
> implemented** - the provider clients and the OS sandbox are stubs (EPIC-5) - so agent
> command execution is not yet wired and is not safe to enable. The model below is the
> intended design that those tickets must satisfy.

- **The agent is treated as untrusted.** It may have read attacker-controlled content (web
  pages, file contents, command output), so aterm never runs anything on the model's own
  say-so.
- **Typed tools, no free-text shell.** The model's tools take structured argv tokens; it
  cannot emit an arbitrary shell string.
- **The safety gate is deterministic code, not a prompt.** Every proposed command is parsed
  and classified by a code-side risk classifier; the model's self-reported risk is ignored,
  so prompt injection cannot talk its way past the gate. The gate over-approximates toward
  asking for confirmation.
- **Auto-safe default + mandatory sandbox.** By default, commands the gate proves `Safe`
  (with no shell-active reason) auto-run; `Caution` / `Dangerous` always require explicit
  confirmation. Because that default trust surface is larger, a macOS Seatbelt
  (`sandbox-exec`) sandbox plus `setrlimit` + a timeout-kill is **mandatory** in the design
  (currently a stub - EPIC-5 / T-5.7 - which is why agent execution stays gated off until
  it lands).
- **The gate is a classifier, NOT a sandbox boundary.** It cannot fully inspect what
  arbitrary code (interpreters, build tools, `find -exec`) will do once it runs. The OS
  sandbox is the real boundary.
- **Commands run with your user privileges.** An approved command can do anything you can.
  Review what you approve.
- **Secret handling.** A single source of truth feeds both the gate (a sensitive-path
  deny-set covering `~/.ssh`, `~/.aws`, `.env`, credential files) and an output sanitizer
  that redacts secret values from captured output before it is returned to the model.

If you find a way to defeat the risk gate or exfiltrate secrets, please report it privately
as described above.
