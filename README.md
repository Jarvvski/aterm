# aterm

A native macOS GPU terminal in Rust. aterm pairs **Warp's controlled-UI
behavior** (command blocks, structured output, an agent that can act) with **iA
Writer's minimalism** (one paper-calm surface, a single typeface family, no
chrome you do not need) behind a **unified input field** that is one keystroke
away from being either a shell prompt or an agent prompt - your typed text
survives the switch. It is native Rust on `winit` + `wgpu`, built around a
guaranteed **60fps floor** (120fps on ProMotion), with a deterministic safety
gate sitting between the model and your shell.

**Status: Phase-2 scaffold.** The workspace compiles, runs, and is green; the
engine (PTY/VT/blocks/OSC marks), the agent safety spine (risk gate, secrets,
sanitizer, approval policy, sandbox seam), the design tokens, and a
window+wgpu+glyphon renderer are real. The LLM provider clients and the agentic
turn loop are compiling stubs behind traits (EPIC-5); see the TODOs.

## Build & run

Tooling is driven by [mise](https://mise.jdx.dev):

```
mise install          # pin the Rust toolchain
mise run check        # cargo check --workspace
mise run build        # cargo build --workspace
mise run test         # cargo test --workspace
mise run run          # cargo run -p aterm-app  (opens a window backed by a live shell PTY)
mise run fmt          # cargo fmt --all
mise run lint         # cargo clippy --workspace --all-targets
mise run bench        # cargo bench -p aterm-bench
```

Plain cargo works too, e.g. `cargo run -p aterm-app`.

## Crate map

```
app -> {ui, agent};  ui -> {core, tokens};  agent -> core;  bench -> {core}
```

| Crate           | Role                                                                                          |
| --------------- | --------------------------------------------------------------------------------------------- |
| `aterm-core`    | Engine: PTY (`portable-pty`), VT/grid (`alacritty_terminal`), block model, OSC-133/OSC-7 marks, shell-integration shim, and the pure unified-input `InputModel` reducer. No UI, no LLM. |
| `aterm-tokens`  | Design tokens as typed Rust consts (paper-light + dark themes, ANSI-16 palettes, type/spacing scales, font families). Leaf crate. |
| `aterm-agent`   | The safety spine: `ShellCommand` parse, `DefaultRiskClassifier`, `Secrets`, `OutputSanitizer`, `ApprovalPolicy`, `Sandbox` trait. Plus the `LlmProvider` trait, provider stubs, and the `AgentTurn` skeleton. |
| `aterm-ui`      | Renderer seam: `winit` window + `wgpu` device/surface, a `Renderer` trait, a `glyphon` grid text fast-path, widget stubs. |
| `aterm-app`     | The `aterm` binary. Wires ui+agent+core, owns the window + 3-thread model and the unified-input routing (the `InputModel` reducer itself lives in `aterm-core`). |
| `aterm-bench`   | `criterion` benches (VT parse / OSC scan / block segmentation throughput). |

## Documentation

The `docs/` tree is the source of truth for intent and is **owned elsewhere** -
do not edit it from build tooling:

- `docs/research/` - the research dossier this design is drawn from.
- `docs/adr/` - architecture decision records.
- `docs/design/` - the design system (`design-system.md`) and its machine
  mirror (`tokens.toml`), which `aterm-tokens` reifies.
- `docs/tickets/` - the EPIC/ticket backlog referenced by `TODO(ticket …)`
  markers throughout the code.

Project conventions and agent guidance live in `CLAUDE.md`.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the jj workflow, toolchain, and crate
boundaries, and [`CHANGELOG.md`](CHANGELOG.md) for what's landed. Security policy and the
agent threat model are in [`SECURITY.md`](SECURITY.md).

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE). Bundled fonts under `assets/fonts/`
are iM Writing Nerd Font (SIL OFL 1.1) - see `assets/fonts/OFL-LICENSE.md`.
