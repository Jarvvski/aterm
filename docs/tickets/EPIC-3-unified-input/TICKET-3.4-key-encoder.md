---
id: T-3.4
epic: EPIC-3-unified-input
title: Key encoder (Kitty protocol + DECCKM) for raw passthrough
status: done
labels: [core, input]
depends_on: [T-3.3]
---

# Goal

Encode keystrokes to the correct PTY byte sequences for raw passthrough (alt-screen TUIs, in-flight stdin), honoring the Kitty keyboard protocol and application-cursor mode (DECCKM), via a neutral key model + a mode-aware encoder.

# Context

- Research: [05-unified-input-ux.md](../../research/05-unified-input-ux.md) section 1 (Kitty: the prototype already implemented `KittyKeyboard.kt`/test; neutral `KeyStroke`/`NamedKey` + a mode-aware encoder honoring DECCKM is the right shape - re-implement in Rust).

# Implementation notes

- Crate: `aterm-core`. Module `keys`.
- A neutral key model (`KeyStroke { key: NamedKey | Char, mods }`) decoupled from winit, and an encoder that produces bytes for: legacy mode, DECCKM application-cursor mode (arrow keys send `SS3` vs `CSI`), and the Kitty keyboard protocol disambiguation when the program has requested it (query `Term` mode flags).
- Port the prototype's encoding tables; verify against the pinned `alacritty_terminal` mode flags for which protocol is active.
- The router (T-3.3) calls this only on the raw/alt-screen/in-flight paths; committed-command submit (Shell mode Enter) writes the full command line + newline, not per-key encoding.

# Acceptance criteria

- Arrow keys encode to `CSI` in normal mode and `SS3` in DECCKM mode.
- Ctrl-C encodes to `0x03`, Ctrl-Z to `0x1a` (driving line-discipline signals).
- Kitty-protocol disambiguation produces the correct extended sequences when the program enabled it.
- A round-trip test in a TUI fixture (e.g. an app reading raw keys) sees the expected bytes.
- Port parity: the Rust encoder matches the prototype's `KittyKeyboardTest` cases.

# Out of scope

- Deciding *when* to use raw passthrough (T-3.3).
- Foreground-pgid signal delivery (T-1.9).

# Resolution

**2026-06-25 (agent): Done.** New pure module `aterm-core::keys` - the neutral
key model + mode-aware encoder. All 5 ACs met by headless tests (10 unit tests).

- `KeyStroke` + `NamedKey` (ported from the prototype's `Input.kt`, decoupled from
  winit). `KeyEncodeFlags { app_cursor, disambiguate }` with `from_term_mode` mapping
  `TermMode::APP_CURSOR` (DECCKM) and `TermMode::DISAMBIGUATE_ESC_CODES` (Kitty).
- `encode(stroke, flags)` tries the Kitty `CSI u` promotion (`encode_kitty`, a
  verbatim port of `KittyKeyboard.encode`) then falls back to `encode_legacy`.
- **AC1** arrows -> `CSI` (`ESC[A`) normal / `SS3` (`ESC O A`) under DECCKM (+ Home/End);
  **AC2** Ctrl-C -> `0x03`, Ctrl-Z -> `0x1a` (`cp & 0x1f`); **AC3** Kitty disambiguation
  (Shift+Enter -> `ESC[13;2u`, Ctrl+I -> `ESC[105;5u`); **AC4** a TUI-fixture round-trip
  test asserts the concatenated bytes; **AC5** the `encode_kitty` cases mirror the
  prototype's `KittyKeyboardTest`.
- **Scope decision (documented):** the prototype's INBOUND Kitty `filter` (the app's
  query + the push/pop/set flag stack, per-screen) is owned natively by the pinned
  `alacritty_terminal` (it parses those sequences and exposes the flags on `TermMode`),
  so this ticket ports only the OUTBOUND encode and reads the live flags - rather than
  re-implementing the negotiation. The legacy/DECCKM tables (which JediTerm provided in
  the prototype) are implemented here per standard xterm conventions.

A 3-lens adversarial review found one real defect (now fixed + regression-tested):
`alt`/`meta` + an un-encodable code point (a surrogate / `> U+10FFFF`) emitted a lone
dangling `ESC` (PTY-stream-corrupting); `encode_legacy` now drops an un-encodable code
point entirely. `mise run fmt && lint && build && test` green; clippy clean at
`-D warnings`; `aterm-core` 181 tests. No version bump (internal engine module, no
user-visible surface yet).

The encoder is a ready lib API; its ADOPTION on the router's raw/alt-screen/in-flight
path (replacing the `raw_key_bytes` arrow stubs in `aterm-app::session`) is T-3.3's
remaining work (it also needs the keyboard-modifier seam + a live `TermMode` accessor).
