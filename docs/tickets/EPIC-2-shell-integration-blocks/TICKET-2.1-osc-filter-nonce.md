---
id: T-2.1
epic: EPIC-2-shell-integration-blocks
title: OSC-133/OSC-7 pre-parser filter with nonce gating
status: ready-for-agent
labels: [core, shell-integration]
depends_on: [T-1.2]
---

# Goal

Implement the pre-parser byte filter that intercepts OSC 133 (A/B/C/D) + OSC 7 marks before they reach `alacritty_terminal`, strips them to zero-width, tags each with an offset into the clean passthrough text, enforces a per-session nonce, and opportunistically ingests OSC 633/1337.

# Context

- Research: [04-shell-integration.md](../../research/04-shell-integration.md) sections 1, 5 and Recommendations 1, 5-6; [03-pty-vt-rust.md](../../research/03-pty-vt-rust.md) section D and Recommendation 5 (alacritty does NOT parse OSC 133 - issue #5850 - so this filter is load-bearing; handle split sequences across reads and BEL-vs-ST terminators exactly).

# Implementation notes

- Crate: `aterm-core`. Module `osc` (the `ShellIntegrationParser` port in spirit, rebuilt in Rust as a thin byte filter / `vte::Perform` ahead of `Term`).
- Sits between the coalescer (T-1.4) and `Term::feed` (T-1.2): consumes raw bytes, emits (clean bytes, [Mark { kind, offset, attrs, nonce }]). The block state machine (T-2.5) fires events once the emulator has drained to a mark's offset - keep marks in lockstep with the grid.
- Parse OSC 133 A/B/C[;cmdline=ENC]/D[;<code>], OSC 7 (`file://host/path`, percent-decode), tolerating both BEL (`\a`) and ST (`\x1b\\`) terminators and sequences split across read boundaries (stateful scanner).
- Nonce: every mark must carry `tag=NONCE` matching the per-session random nonce (set by the shim, T-2.2). Marks with absent/mismatched nonce are dropped (or flagged for a child sub-session) - this defeats nested-shell/program spoofing and framework double-marks (starship/p10k emit un-nonced marks -> dropped).
- Opportunistic ingest: map OSC 633 A->A, B->B, C->C, D->D, E->cmdline (honor its nonce/escaping), P;Cwd->OSC7-equivalent; ingest OSC 1337 `ShellIntegrationVersion` as telemetry. Do not depend on 633/1337.
- The alt-screen suppression *decision* is made at fire-time by T-2.5 (reads the drained alt-screen flag); this filter only detects and tags.

# Acceptance criteria

- Feeding a real captured zsh prompt cycle (A->B->C->D with OSC 7) yields the correct marks at correct offsets, and the clean byte stream has the marks removed (zero-width - cursor math unaffected).
- A mark split across two read chunks is parsed correctly.
- Both BEL- and ST-terminated marks parse.
- A mark with a wrong/absent nonce is dropped; a correctly-nonced mark passes.
- An OSC 633;E with the VS Code escaping (`\x3b`, `\xAB`, `\\`) decodes to the correct command text.
- `cargo test -p aterm-core` covers the state machine across the fixtures.

# Out of scope

- Installing the shims that emit the marks (T-2.2, T-2.3).
- The BlockList + lifecycle state machine (T-2.4, T-2.5).
