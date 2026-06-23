# Contributing to aterm

aterm is early-stage (Phase-2 scaffold), so things move and break. Issues and PRs are
welcome.

## This repo uses Jujutsu (jj)

Developed with [Jujutsu](https://github.com/jj-vcs/jj) in a git-colocated layout. Plain
git is fine for cloning and opening PRs, but the maintainer's day-to-day workflow is
jj-based - see "Landing a change" in [`CLAUDE.md`](CLAUDE.md). Don't run `git` against the
maintainer's working copy; in a colocated repo it can corrupt jj's operation log.

## Toolchain

Pinned with [mise](https://mise.jdx.dev) (`mise.toml`): Rust 1.96. Install mise, then
`mise install` in the repo root. Plain cargo works too.

## Before you open a PR

CI gates fmt, clippy, check, and test on macOS. Run them locally first:

```
mise run fmt      # cargo fmt --all
mise run lint     # cargo clippy --workspace --all-targets
mise run build    # cargo build --workspace
mise run test     # cargo test --workspace
```

## Code style & boundaries

- Rust, formatted by `rustfmt` (`mise run fmt`) and linted by clippy (`mise run lint`).
- **Respect the crate boundaries** (enforced in CI by `cargo deny`): `aterm-app -> {ui,
  agent}`, `aterm-ui -> {core, tokens}`, `aterm-agent -> core`; `aterm-core` and
  `aterm-tokens` are leaves. `aterm-core` must never pull in an LLM SDK.
- **The agent safety gate** (`aterm-agent`: the risk classifier, `Secrets`,
  `OutputSanitizer`) is deterministic and prompt-injection-resistant by design. If you
  touch it, add tests and keep it over-approximating toward asking for confirmation - never
  trust a model's self-reported risk.
- Use the domain vocabulary in `docs/agents/domain.md` in names, tests, and proposals.

## Working from the backlog

Implementation work is tracked as Markdown tickets under `docs/tickets/` (roster in
`INDEX.md`; convention in `docs/agents/issue-tracker.md`). Pick a `ready-for-agent` ticket
whose `depends_on` have landed, read its linked research doc + ADR, and land one focused
change.

## Architecture

[`CLAUDE.md`](CLAUDE.md) is the working contract; `docs/research/00-overview.md` is the
full reasoning; `docs/adr/` records the decisions.

## Reporting bugs and security issues

- General bugs and features: open a GitHub issue.
- Security vulnerabilities: do **not** open a public issue - see [`SECURITY.md`](SECURITY.md).
