---
id: T-2.1
epic: EPIC-2-shell-integration-blocks
title: OSC-133/OSC-7 pre-parser filter with nonce gating
status: done
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

# Notes

2026-06-24 (agent): Landed. Rebuilt `aterm-core::osc` from the stateless per-chunk
scaffold into a stateful pre-parser filter that closes the real gaps vs the ACs:

- **Per-mark offsets** - `ScanResult.marks` is now `Vec<(usize, Mark)>`; the scanner
  tracks a cumulative clean-stream position so each mark carries an absolute offset
  into the logical output the emulator sees (keeps marks in lockstep with the grid
  for T-2.5). The engine's `Model::process_output` passes these straight to the
  segmenter; the redundant `stream_offset` field was removed.
- **Cross-chunk split stitching** - `scan(&mut self)` carries a bounded `partial`
  buffer (`MAX_OSC_LEN = 8192`; over-long unterminated sequences flush to passthrough,
  where alacritty's own OSC bound takes over). Handles the ESC|`]` boundary split.
- **OSC 633 (VS Code)** ingest - A/B/C/D -> prompt kinds, `E` -> command line with
  `\xHH`/`\\` decoding (the AC), `P;Cwd=` -> cwd; **OSC 1337** recognized as telemetry
  (no mark). **OSC 133** `C[;cmdline=]` emits a `CommandLine` mark; `D[;exit]` parses
  the exit code. New `Mark::CommandLine(String)`; the `BlockSegmenter` arm is a no-op
  (capturing into `Block.command` is T-2.5).
- **Nonce gate** via an exact `;`-delimited `aterm_nonce=<nonce>` field; untrusted
  mode (the engine's default until the T-2.2 shim handshake) trusts any well-formed
  mark. BEL + ST terminators both parse.

**Adversarial review found a CRITICAL security flaw (now fixed; this is why the
ticket exists).** `read_osc` originally terminated a body only on BEL or `ESC \`,
absorbing an embedded `ESC ]` (a fresh OSC introducer) as a body byte. Combined with
the cross-chunk stitch, untrusted command output could print an UNTERMINATED OSC and
absorb the shell's next genuine nonce-stamped mark, either (a) **leaking the secret
nonce** into the passthrough (grid/scrollback the agent's LLM reads) when the
swallowing OSC was non-strippable (e.g. OSC 7), or (b) **forging a trusted mark** by
borrowing the genuine mark's nonce (the whole merged body was nonce-checked) - both
collapse the prompt-injection defense, with zero knowledge of the secret. Four
reviewer agents independently reproduced it.

Fix: `read_osc` now returns `Done | Aborted | Incomplete`; a fresh `ESC` that is not
the `ESC \` ST aborts the in-progress OSC (per ECMA-48), and `scan` discards the
malformed prefix and re-anchors at the embedded `ESC`, so a following genuine mark is
parsed as its OWN body and nonce-checked in isolation. The invariant: a new `ESC ]`
can never be merged into a prior OSC body. `contains_nonce` was also tightened from
an unanchored substring search to an exact `;`-delimited field (defense-in-depth -
not independently exploitable, but removes the `notaterm_nonce=`/prefix-value
footgun). Five security regression tests encode the exact reproduced PoCs
(exfiltration, nonce-borrow forgery, untrusted desync, single-chunk embedded-ESC,
anchored-nonce). A focused re-verification (200k-iteration randomized split-feed fuzz
+ ~18 framing-attack traces) could not break the fixed code.

**Contract for T-2.2 (flagged by review):** the shim MUST emit each nonce-bearing
mark atomically with its `ESC ]` introducer - never the nonce value as raw bytes
detached from an introducer - and T-2.2 should assert this in the shim's tests. The
filter's guarantee depends on it.

67 `aterm-core` tests pass (21 OSC), `fmt`/`clippy`/`build`/`test` all green, no
flakiness across reruns. No version bump / CHANGELOG entry: internal engine filter,
no user-visible behaviour change yet (the engine still runs in untrusted mode until
T-2.2 wires the nonce).

2026-06-25 (agent): Status flipped `ready-for-agent` -> `done` (the label was
stale - implementation landed 2026-06-24 per the entry above). Re-audited every AC
against a passing test before flipping: AC1 `prompt_lifecycle_marks_at_correct_offsets_and_stripped`
(asserts exact clean-stream offsets + zero-width strip), AC2 `mark_split_across_two_chunks_is_stitched`
+ `split_at_esc_bracket_boundary_is_stitched`, AC3 `both_terminators_parse`, AC4
`nonce_gating_trusts_strips_only_our_marks` (trusted `with_nonce` mode, both
directions: absent/wrong dropped + passed through, correct stripped) +
`nonce_match_is_anchored_to_full_field`, AC5 `osc633_e_decodes_vscode_escaping`
(real `\x3b`/`\\`/`\xAB` decode), AC6 = 26 OSC tests green under
`cargo test -p aterm-core`. Scanner confirmed wired into `engine.rs`
(`self.osc.scan(bytes)` in `Model::process_output`).
