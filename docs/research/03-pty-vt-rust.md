---
title: Terminal Engine: PTY + VT Emulation + Block Model in Rust
domain: pty-vt-rust
status: research
---

# Terminal Engine: PTY + VT Emulation + Block Model in Rust

## TL;DR

- **Use `alacritty_terminal` (0.26.0, Apache-2.0) as the VT/grid engine, not a from-scratch grid.** It is the most battle-tested *reusable* terminal core in Rust, is exactly what Zed's GPU-rendered terminal uses [1][12][14], and it bundles the hard parts: a Williams-state-machine parser (`vte` 0.15), a reflowing scrollback `Grid<Cell>`, alternate-screen handling, line-level damage tracking, and a `RenderableContent` cell iterator the renderer can walk every frame [4][6][7]. wezterm's `wezterm-term` is *not published to crates.io* and has *no API-stability guarantee* [3], so it is a non-starter as a dependency; `termwiz` (its published cousin) is a TUI-building toolkit, not a drop-in emulator core.
- **Drive the PTY with `portable-pty` (0.9.0, MIT)** — the wezterm crate, 1M+ downloads/month, cross-platform (macOS/Linux + Windows ConPTY) [2][9]. Prefer it over alacritty's *own* `tty` module so we own spawn/resize/signal/process-group semantics and keep the Linux/Windows door open per the hard requirement. (alacritty_terminal *does* ship a `tty` module with `Pty`/`Shell`/`new()` and an `event_loop`; we deliberately use only its `Term`/`Grid`/parser layer [8].)
- **Block model = a Warp-style `BlockList` layered *on top of* the VT grid, not inside it.** Warp confirms the exact shape we want: typed blocks keyed to commands via shell-integration OSC hooks; full-screen apps live *outside* the block list in a separate alt-screen grid; the renderer only needs each block's *height* (indexed by a `SumTree` for O(log n) viewport queries) [10][11]. Agent transcript entries are just more block types in the same list.
- **OSC 133/OSC 7 marks must be intercepted by us, before/around the emulator — `alacritty_terminal` does NOT parse OSC 133 (issue #5850 still open) [13].** The prior Kotlin prototype's `ShellIntegrationParser` (a pre-emulator filter that strips our marks to zero-width and tags each with an offset into the clean passthrough text) is the correct pattern to port in spirit; in Rust this becomes a thin `vte::Perform`/byte filter ahead of `Term`.
- **Protect the 60fps floor with a 3-thread split + output coalescing:** a blocking PTY-reader thread, a parser/model thread that owns the `Term`, and the render thread that only reads a snapshot. Coalesce bursts on a ~tick boundary before issuing a frame — a real GPUI terminal froze on `cat`-ing a large file until a **4ms batching interval** was added to merge PTY events before rendering [15]. Backpressure is implicit: the reader blocks on a bounded channel when the model can't keep up; we never let the render thread touch the PTY.
- **Net stack recommendation:** `portable-pty 0.9` + `alacritty_terminal 0.26` (+ its bundled `vte 0.15`) + our own `BlockList`/OSC-mark layer. All deps are MIT/Apache-2.0 and absorb cleanly into a **GPLv3** product [16][17].

## Findings

### A. Spawning / controlling a hidden login shell via PTY

Three realistic options:

**1. `portable-pty` 0.9.0 (wezterm), MIT [2][9].** Latest release 2025-02-11. Cross-platform by design: a `PtySystem` trait with native Unix and Windows-ConPTY backends selected at runtime via `native_pty_system()`. Public surface used in practice [2]:
- `native_pty_system().openpty(PtySize { rows, cols, pixel_width, pixel_height })` → `PtyPair { master, slave }`.
- `CommandBuilder::new("zsh")` then `.env()`, `.cwd()`, `.args()`; `pair.slave.spawn_command(cmd)` → a `Box<dyn Child>` exposing `process_id()`, `try_wait()`, `wait()`, `kill()`.
- `pair.master.try_clone_reader()` → `Box<dyn Read + Send>` (the off-thread reader); `pair.master.take_writer()` → input sink; `pair.master.resize(PtySize{..})` issues the SIGWINCH-equivalent ioctl.
- On Unix, `MasterPty` exposes the underlying fd (`as_raw_fd`) so we can do `tcsetattr`, send signals to the foreground process group, etc.

`portable-pty` handles `login_tty`/`setsid`/controlling-terminal setup internally on Unix, which is the fiddly part for a *login* shell. For aterm we want a login shell (`zsh -l` or argv0 `-zsh`) so `/etc/zprofile` + the user's profile load; `CommandBuilder` lets us set argv0/args to request login behavior, and we layer our ZDOTDIR shim via `.env("ZDOTDIR", shim_dir)` exactly as the prototype does (no rc edits).

**2. `pty-process` 0.5.3, MIT [docs.rs].** Thinner, Unix-only, no Windows path. Nice property: native **async** via the `async`/`tokio` feature — the `Pty` implements `tokio::io::AsyncRead`/`AsyncWrite`, and `OwnedReadPty`/`OwnedWritePty` give a clean split-halves API. Wraps `tokio::process::Command`. Good if we commit to a tokio reactor for I/O, but it forecloses the Windows door we are asked to keep open, and gives us less control over ConPTY later.

**3. Roll our own on `rustix`/`nix` (`openpty`, `login_tty`).** `rustix` has a `pty` module and there is a `rustix-openpty` helper crate; `nix::pty` is the classic route. This is what you reach for only if `portable-pty` is too opinionated. It means hand-writing `forkpty`/`setsid`/`TIOCSCTTY`/`TIOCSWINSZ` and the Windows ConPTY dance ourselves — substantial, error-prone, and exactly the wheel `portable-pty` already reinvented well. Not recommended for v1.

**Resize / signals / process groups.** Resize is `master.resize(PtySize)` (ioctl `TIOCSWINSZ` → kernel raises `SIGWINCH` in the slave session) — must be debounced on the model thread when the window is dragged. Signals (Ctrl-C/Ctrl-Z) are normally produced by writing the control byte to the master, letting the line discipline turn it into `SIGINT`/`SIGTSTP` for the foreground group; for agent-initiated cancellation we may also need an explicit `killpg(pgid, SIGINT)` using the fd. Foreground-pgid tracking (which the prototype lacks cleanly) is needed to know *what* a given Ctrl-C will hit; wezterm's procinfo handling is the reference (Zed even vendors a single struct from a wezterm fork for this [12]).

### B. VT / ANSI emulation — `alacritty_terminal` vs `wezterm-term`/`termwiz` vs `vte` + custom grid

**`alacritty_terminal` 0.26.0, Apache-2.0 [1][4].** Released 2026-04-06; ~9k LOC; ~82k downloads/month; used by Zed [12][14]. Built on `vte 0.15`, `base64 0.22`, `bitflags 2.4`, `unicode-width`, `parking_lot`, with `rustix` (Unix) / `windows-sys` (Windows) for its optional `tty` module [4]. Key public API:
- `Term<T: EventListener>` — owns all terminal state. Constructed `Term::new(config, &dimensions, event_proxy)`. Implements `vte::ansi::Handler`, so you *feed it bytes through the parser* rather than calling it directly.
- The parser: a `vte::ansi::Processor` (re-exported under `alacritty_terminal::vte::ansi`) whose `advance(&mut handler, &bytes)` drives the state machine and calls back into `Term`'s `Handler` impl (`input`, `goto`, `set_color`, `osc_dispatch`, etc.) [7][13].
- `term.grid()` → `&Grid<Cell>`; `term.renderable_content()` → `RenderableContent` with public fields: `display_iter: GridIterator<'_, Cell>`, `cursor: RenderableCursor`, `display_offset: usize`, `selection: Option<SelectionRange>`, `colors: &Colors`, `mode: TermMode` [6]. The renderer walks `display_iter` each frame; each `Cell` carries char + fg/bg + flags (bold/italic/wide/underline). This is the per-frame read path and it is cheap.
- `term.resize(TermSize)` reflows the grid + scrollback (default 10 000 lines, configurable to 100 000) [src/grid/resize.rs, config docs]. **Caveat:** reflow has known sharp edges — content jumbling and scrollback-line loss when resizing while scrolled back (alacritty issues #4419, #8576), and reflow is O(n) over a large grid and has caused visible "freeze during reflow" reports (#2213, #2567). For aterm this matters less because *finished blocks store their own immutable row snapshot* (the prototype already does this) and we re-wrap blocks ourselves; only the live grid goes through alacritty reflow.
- **Damage tracking:** `TermDamage::Full` or `TermDamage::Partial(TermDamageIterator)` of `LineDamageBounds { line, left, right }` [4] — lets the renderer mark only dirty rows dirty, feeding straight into a 60fps dirty-rect or dirty-line strategy.
- **Alternate screen:** handled internally; `?1049h/l` swaps to the alt grid; `term.mode()` exposes `TermMode::ALT_SCREEN` so we can detect "a full-screen app is live" and switch the UI into pass-through mode.
- **Events:** `EventListener::send_event(Event)` where `Event` includes `Title(String)`, `ResetTitle`, `PtyWrite(String)` (the terminal needs to reply to the PTY, e.g. DA/DSR queries — we must wire this back to the master writer), `ClipboardStore`/`ClipboardLoad`, `Bell`, `MouseCursorDirty`, `CursorBlinkingChange`, `Exit` [4]. This is the hook for clipboard (OSC 52), title, bell, and crucially the reply channel.

**`wezterm-term` + `termwiz`.** `wezterm-term` is the closest *semantic* match (it has the richest real-world escape coverage and a `Surface`/`Line`/`Cell` model with a `Change` delta log) but it is **deliberately unpublished with no API stability** [3], so depending on it means vendoring a moving fork — the same maintenance tax Zed pays and is trying to shed [12]. `termwiz` *is* published (5.3M downloads) and has an excellent escape parser/encoder and `Surface` with change-log deltas, but it is positioned as a toolkit for *building* TUIs/emulators, not a ready `Term` you feed bytes and read a grid from; we'd still be assembling the grid/scrollback/alt-screen glue ourselves.

**`vte` 0.15 alone + custom grid [5][7].** `vte` is *just* the Williams state machine (the `Parser` + your `Perform` impl; `vte::ansi` adds the higher-level `Processor`/`Handler` that alacritty uses). Maximum control, zero opinions about the grid — which is attractive for a "controlled UI, not a real terminal." But it means we re-implement the entire `Grid`, scrollback, reflow, wide-char/combining handling, tab stops, all CSI/SGR semantics, and alt-screen — i.e. re-build the 9k LOC alacritty already debugged across years of real-world escape sequences. The honest assessment: a custom grid is the *purist* answer for a controlled UI but a poor ROI against the 60fps-first, ship-it mandate.

**Why alacritty_terminal still fits a "controlled UI, not a real terminal."** The block model does not require *less* VT correctness — vim, ssh, `git log`, `npm` progress bars all emit real escapes that must render correctly inside a block. What the controlled UI changes is *where the grid boundaries are* and *what wraps the grid* (a block list + our own chrome), not whether we faithfully emulate VT. alacritty gives us a correct grid; we own everything above it.

### C. Grid + scrollback + BLOCK data model

**Layering.** Keep alacritty's `Term`/`Grid` as the *live* VT surface. Around it, maintain our own `BlockList` (the Warp pattern [10][11]):
- Shell-integration OSC 133 `A→B→C→D` marks delimit command lifecycles; OSC 7 reports cwd. We **intercept these ourselves** (see D below) — alacritty will not [13].
- On `OSC 133;C` (output start) open a `RunningBlock` whose body is the *live grid*; on `OSC 133;D` (command end) snapshot the output rows into an immutable `CommandBlock { output: Vec<RowRun>, exit_code, … }`. This is exactly the prototype's `Block`/`RunningBlock`/`CommandBlock` model (`core/.../terminal/Block.kt`) and it is sound — port the data shapes, re-implement on the alacritty grid.
- Finished blocks store their *own* immutable row snapshot keyed to block-relative y, so they are immune to later grid reflow/eviction — the prototype's most important design choice, and what lets us avoid alacritty's reflow sharp edges for history.
- **Storage:** Warp splits `GridStorage` (mutable, only the live/active region the cursor can still write) from `FlatStorage` (packed bytes + sparse style intervals indexed by byte offset, for immutable scrollback — no reflow cost, only a row index rebuild) [11]. For v1 a `Vec<RowRun>` per finished block is fine; the FlatStorage idea is the optimization to reach for if memory under huge logs bites.
- **Renderer contract:** each block exposes only its pixel `height`; a `SumTree` (balanced tree, per-block height sums at interior nodes) turns viewport intersection into O(log n) [11]. Virtualize twice: pick blocks intersecting the viewport, then visible rows within each. This is how the block list scales to thousands of blocks at 60fps.
- **Unified timeline:** agent transcript entries are additional block *variants* in the same list, wall-clock ordered — matches the prototype's "single timeline" keep-idea and Warp's "rich content blocks plug into the same BlockList" [11].

### D. Full-screen TUIs (vim/htop/ssh) via the alternate screen

Follow Warp exactly: **full-screen apps live outside the block list.** When `?1049h` enters the alt screen (detect via `term.mode() & TermMode::ALT_SCREEN`), switch the UI to render the alt grid as a single full-window surface (its own grid, no scrollback, replaced wholesale on exit) [10][11]. While alt-screen is active:
- Route keyboard/mouse/scroll straight to the PTY (pass-through), not to the block input box.
- Do **not** fabricate blocks from any OSC 133 marks a TUI might emit (the prototype notes this: alt-screen suppression must be decided at *fire time*, reading the current alt-screen flag, because the toggling CSI is still unparsed passthrough when the mark is first seen — keep that ordering discipline).
- On exit (`?1049l`), discard the alt grid and resume the block list where it left off. The command that entered alt-screen becomes a compact `Interactive` block ("ran vim · 12s") with no captured output — again the prototype's `BlockKind.Interactive`.

### E. Reading PTY output OFF the render thread (threads + channels + backpressure)

The 60fps floor is won or lost here. Recommended topology (thread-based, not async — PTY reads are blocking and the broll/Zellij precedent favors threads + channels + atomics [15][anatomy]):

1. **Reader thread:** owns `master.try_clone_reader()`, loops `read()` into a ~64 KiB buffer (Zellij uses a 65 536-byte buffer [anatomy]); sends `Bytes` over a **bounded** channel. Bounded = backpressure: if the model can't keep up under a flood (`yes`, `cat hugefile`), the reader blocks on `send`, which blocks `read`, which lets the PTY's kernel buffer apply flow control to the producer. No unbounded memory growth.
2. **Model thread:** owns the `Term` and `BlockList`. Drains the channel, feeds bytes through our OSC-133 filter then `Processor::advance(&mut term, &bytes)`, updates blocks, and publishes an immutable *snapshot* (or a damage set) to the renderer via a triple-buffer / `arc-swap` / `parking_lot::Mutex<Snapshot>`. **Coalesce here:** don't wake the renderer per chunk; merge everything available within a tick.
3. **Render thread (GPU):** reads the latest snapshot/damage, draws at vsync (60/120fps ProMotion). Never touches the PTY or blocks on the model.

**Coalescing is mandatory, with a concrete precedent:** a GPUI-based terminal *froze* rendering when running `cat` on a large file until the author added a **4ms batching interval to coalesce PTY events before rendering** [15]. Adopt the same: a small debounce/tick (~4-8ms, i.e. comfortably under the 16.6ms/60fps and 8.3ms/120fps budgets) between "bytes arrived" and "issue frame," so a megabyte burst becomes one parse pass + one frame, not thousands. Pair with FrankenTUI's idea of a frame-time budget that *degrades fidelity* if a frame would blow 16ms [15] (e.g. skip intermediate scroll states during a flood).

**Output-rate guard:** under sustained flood, additionally cap *visible* refresh (parse everything for correctness, but only snapshot→render at the display rate). This decouples "bytes/sec the shell produces" from "frames/sec we draw," which is the whole game for the 60fps guarantee.

### Licensing

aterm is **GPLv3**. Apache-2.0 (`alacritty_terminal`) is one-way compatible *into* GPLv3, and MIT (`portable-pty`, `vte`, `pty-process`) is GPLv3-compatible; the combined work ships as GPLv3 [16][17]. No blockers. (Same family of deps the prototype's THIRD-PARTY-NOTICES already tracks.)

## Recommendations for aterm

1. **VT engine: `alacritty_terminal = "0.26"`. (High)** Most reusable, correct, GPU-renderer-proven (Zed) core; gives us `Grid<Cell>`, reflow, alt-screen, damage, and `RenderableContent` for free [1][6][12].
2. **PTY: `portable-pty = "0.9"`, use it instead of alacritty's bundled `tty` module. (High)** Owns spawn/resize/signal/ConPTY; keeps Linux/Windows open per the hard requirement [2][9]. We feed its reader bytes into alacritty's parser ourselves.
3. **Threading: 3 threads (reader / model+Term / render) over bounded channels; coalesce PTY bursts on a ~4-8ms tick before each frame. (High)** Directly mitigates the documented `cat`-flood freeze [15] and enforces the 60fps floor by decoupling byte-rate from frame-rate.
4. **Block model: own `BlockList` + `SumTree` height index on top of the grid; finished blocks store immutable row snapshots; agent entries are block variants. (High)** Mirrors Warp [10][11] and the prototype's proven `Block` model; avoids alacritty reflow sharp edges for history.
5. **OSC 133/7 interception: a pre-parser byte filter (port `ShellIntegrationParser` in spirit) that strips our marks to zero-width and tags offsets, *then* hand clean bytes to `Term`. (High)** alacritty does not parse OSC 133 [13]; do it ourselves and keep the alt-screen-suppression decision at fire time.
6. **Full-screen apps: separate alt-screen surface outside the block list, gated on `TermMode::ALT_SCREEN`; pass-through input; compact `Interactive` block on exit. (High)** The Warp model [10][11].
7. **Do NOT build a custom grid on bare `vte` for v1; revisit only if alacritty's grid blocks a UI requirement. (Med)** The control benefit doesn't beat re-debugging 9k LOC of escape handling under a 60fps-first mandate.
8. **Wire `Event::PtyWrite` back to the master writer. (High)** Terminal query replies (DA/DSR/cursor-position) flow through here; dropping them breaks programs that probe the terminal.
9. **Track the foreground process-group id (via the master fd / procinfo). (Med)** Needed to make Ctrl-C and agent-cancel hit the right process; the prototype is weak here.

## Risks & unknowns

- **alacritty reflow correctness/perf on large live grids** — known issues #2213/#2567/#4419/#8576 (jumbling, scrollback loss, slow reflow). Mitigated for *history* by our own block snapshots, but the *live* grid still uses alacritty reflow on window resize; needs a perf check on a maximized 4K window. Not yet measured by us.
- **alacritty_terminal API stability across 0.x.** It is pre-1.0 (0.26.0) and has shipped many breaking minors; pin exactly and budget for upgrade churn. The `vte` re-export path (`alacritty_terminal::vte::ansi::Processor`) and exact `Handler`/`Event` signatures should be re-verified against the pinned version's docs before coding — I confirmed the *shape* from docs/source [4][6][7] but not every signature against 0.26.0 specifically.
- **No first-class OSC 133 in alacritty** [13] means our marker filter is load-bearing and must be exactly correct against real shells (zsh/bash/fish), incl. split sequences across reads and the BEL-vs-ST terminator cases the prototype already handles.
- **`portable-pty` 0.9 fd exposure for pgid/signals** — I confirmed it exposes the master and a reader/writer; exact API for "send signal to foreground group" / raw fd on macOS should be re-verified in code (likely via `as_raw_fd` + `nix::sys::signal::killpg`).
- **Threading vs async choice** — I recommend threads (blocking reads, simplest backpressure), but if the rest of aterm is tokio-centric, `pty-process` (async, Unix-only) is the alternative; that trades the Windows path for reactor uniformity. Decision interacts with the renderer/runtime choice (other researcher's domain).
- **Coalesce interval (4-8ms)** is a starting heuristic from one GPUI report [15]; needs tuning against the actual renderer and ProMotion 120fz, not taken as gospel.
- **`wezterm-term` re-evaluation:** if alacritty's grid proves too limiting for the controlled UI, vendoring `wezterm-term` (or `termwiz`) is the fallback, accepting the unpublished/no-stability tax [3]. Not chosen now.

## Open questions for the product owner

- **Threads vs tokio for I/O?** Affects the PTY crate choice (`portable-pty` blocking vs `pty-process` async) and must align with the renderer/runtime decision in the rendering-stack domain.
- **Login shell semantics:** launch as a true login shell (`-zsh`/`zsh -l`, loads `/etc/zprofile` + profile) or interactive-only? Affects env/cwd inheritance and the ZDOTDIR shim.
- **Scrollback budget per block / globally:** alacritty defaults to 10 000 lines for the live grid; how much history do finished blocks retain before eviction, and do we adopt Warp's FlatStorage for huge logs in v1 or defer?
- **Windows in scope for v1 at all,** or "not precluded later" only? If truly deferred, `pty-process` (Unix-only, async) becomes viable and simpler.
- **Resize policy while scrolled back / mid-command:** match alacritty's behavior or impose our own block-aware reflow (re-wrap stored snapshots) to dodge the known reflow bugs?

## Sources

1. alacritty_terminal — crates.io: https://crates.io/crates/alacritty_terminal
2. portable_pty — docs.rs: https://docs.rs/portable-pty
3. Publish `wezterm-term` crate · Issue #6663 — github.com/wezterm/wezterm: https://github.com/wezterm/wezterm/issues/6663
4. alacritty_terminal — lib.rs (version 0.26.0, deps, license): https://lib.rs/crates/alacritty_terminal
5. alacritty/vte — github (parser, Perform trait): https://github.com/alacritty/vte
6. RenderableContent — docs.rs: https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/term/struct.RenderableContent.html
7. vte 0.15 — docs.rs: https://docs.rs/crate/vte/latest
8. alacritty_terminal Term API — docs.rs: https://docs.rs/alacritty_terminal/0.26.0/alacritty_terminal/term/struct.Term.html
9. portable-pty — lib.rs (0.9.0, 2025-02-11, MIT, ConPTY): https://lib.rs/crates/portable-pty
10. Full-screen apps — Warp docs: https://docs.warp.dev/terminal/more-features/full-screen-apps/
11. The Block Model Behind Warp's Agentic Development Environment — warp.dev: https://www.warp.dev/blog/block-model-behind-warps-agentic-development-environment
12. Consider removing wezterm fork from dependencies · Issue #8604 — github.com/zed-industries/zed: https://github.com/zed-industries/zed/issues/8604
13. Consider adding OSC 133 · Issue #5850 — github.com/alacritty/alacritty: https://github.com/alacritty/alacritty/issues/5850
14. Replace Alacritty Terminal with VTE · Issue #10791 — github.com/zed-industries/zed: https://github.com/zed-industries/zed/issues/10791
15. Building a GPU-Accelerated Terminal Emulator with Rust and GPUI — dev.to (4ms coalescing, cat-flood freeze): https://dev.to/zhiwei_ma_0fc08a668c1eb51/building-a-gpu-accelerated-terminal-emulator-with-rust-and-gpui-4103
16. Apache License v2.0 and GPL Compatibility — apache.org: https://www.apache.org/licenses/GPL-compatibility.html
17. Open source license compatibility — GPLv3 and Apache 2.0 — thehyve.nl: https://www.thehyve.nl/articles/open-source-software-licenses-part-2
18. pty-process — docs.rs (0.5.3, MIT, tokio async, Unix): https://docs.rs/pty-process
19. PTY and Process Management — wezterm DeepWiki: https://deepwiki.com/wezterm/wezterm/4.5-pty-and-process-management
20. Anatomy of a Terminal Emulator — poor.dev (Zellij, 64KiB read buffer): https://poor.dev/blog/terminal-anatomy/
