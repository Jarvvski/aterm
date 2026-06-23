---
id: T-3.4
epic: EPIC-3-unified-input
title: Key encoder (Kitty protocol + DECCKM) for raw passthrough
status: ready-for-agent
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
