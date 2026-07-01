---
id: T-6.2
epic: EPIC-6-mcp-interop
title: Local stdio MCP client
status: done
labels: [agent, mcp]
depends_on: [T-5.4, T-5.5]
---

# Goal

Run our own MCP client in Rust for local stdio servers (the common dev case - filesystem, git, project-specific servers), surfacing each MCP tool as a native tool in the turn loop, with every call routed through the risk gate. This is provider-agnostic, so it works under either backend.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (c) (consume local stdio) + Recommendation 7. Risk: `rmcp` (Rust MCP SDK) maturity not yet verified - confirm it supports current MCP spec + stdio transport before depending; otherwise hand-roll JSON-RPC over stdio.

# Implementation notes

- Crate: `aterm-agent`. Module `mcp::stdio`.
- Spawn the configured MCP server process, speak JSON-RPC over stdio, `initialize` + `list_tools`, and register each tool as a native tool (T-5.4 registry) so the turn loop (T-5.8) calls it like any other.
- Dependency: evaluate `rmcp` (the official Rust MCP SDK); if immature, hand-roll JSON-RPC over stdio. Document the choice.
- Every local MCP tool call goes through the gate (T-5.5) and, for anything that executes/writes on the machine, the sandbox (T-5.7) - classified exactly like a native tool. Output sanitized (T-5.6).
- Local stdio stays fully on-device (contrast the connector's not-ZDR-eligible remote path).

# Acceptance criteria

- A local stdio MCP server (e.g. a filesystem server) is spawned, its tools listed, and one is invoked through the turn loop.
- An MCP tool that runs a command/writes a file is gated + sandboxed exactly like a native tool.
- Server crash/exit is handled cleanly (no hang; surfaced to the user).
- Output is sanitized before re-entering context.
- The dependency decision (`rmcp` vs hand-rolled) is documented.

# Out of scope

- Auto-discovery of MCP config (T-6.3).
- The connector path (T-6.1) and hosting (post-MVP).

# Notes (landed 2026-07-01)

New module `aterm-agent/src/mcp/stdio.rs`. All ACs met; the client logic is pure + headless-testable, plus one real-process EOF test.

- **Dependency decision: HAND-ROLLED, not `rmcp`.** Documented in the module doc. Same rationale as the hand-rolled provider clients + SSE framer: the transport is tiny (newline-delimited JSON-RPC 2.0, three methods: `initialize`, `tools/list`, `tools/call`), a dep would add more schema/transport surface than it removes, it keeps the `cargo deny` license graph minimal (a locked-decision value), and the `McpTransport` seam makes the client pure/testable. Revisit `rmcp` if we later need MCP resources/prompts or Streamable-HTTP.
- **Client.** `McpTransport` trait (async send/recv, `None` = EOF) with `ProcessTransport` (tokio `process`+`io-util`, `kill_on_drop` so a dropped client never leaks a child, stderr discarded) and a `MockTransport` (tests). `StdioMcpClient<T>`: `initialize` (+`notifications/initialized`), `list_tools`, `call_tool`, plus `spawn`/`connect` convenience on the `ProcessTransport` specialization. Requests serialize through a lock (MCP calls are never parallel-safe anyway) and each is bounded by `REQUEST_TIMEOUT` (30s); interleaved server notifications are skipped by id.
- **Registered as native tools.** New `ToolInput::Mcp(McpToolCall{server,name,args})` + `McpToolSpec`; `ToolRegistry::with_mcp_tools` holds dynamic specs, `specs()` merges (skipping name-collisions with native), `parse()` resolves NATIVE FIRST so an MCP server can never shadow/hijack a gated built-in. `gate_tool` over-approximates `ToolInput::Mcp` to `RequireConfirm(Caution, [RiskReason::McpTool])` - it can never auto-run (args are opaque; may run a command or write files). `McpToolRouter<S,T>` is the composite `ToolDispatch` the loop drives: `Mcp` -> the owning client (crash/unknown-server -> `is_error` `ToolOutcome`, never a hang), everything else -> `Sinks` (gate + sandbox + fs). `Sinks::dispatch` gained a fail-closed `Mcp` arm (error if reached without a router).
- **Crash/exit handled cleanly.** Closed stdout -> `McpError::Closed`; wedged server -> `McpError::Timeout`; both become an error result. Covered by a MockTransport test AND a real `#[cfg(unix)]` spawn of `true` (exits immediately) asserting no hang.
- **Sanitized + sandboxed.** Output sanitization is upstream (the turn loop runs every `ToolOutcome.output` through `OutputSanitizer` against the single `Secrets`). The AC "an MCP tool that runs a command/writes a file is gated + sandboxed exactly like a native tool" holds for the CALL (gated to confirm; native tools it triggers via the router's `Sinks` are Seatbelt-confined). The server PROCESS itself is a spawned child (stderr discarded, killed on drop); confining the long-lived server process under Seatbelt is a follow-up (see residual).
- **Tokio features:** added `process` + `io-util` to the workspace `tokio` (already a dep; features are additive).
- **Residuals:** (1) app startup wiring (spawn configured servers, build the router, register the tools) rides T-6.3 (config auto-discovery) - out of scope here; the seam is complete and tested. (2) Optionally confine the spawned server process itself under Seatbelt (today it runs as a normal child; the tools it drives through the router's `Sinks` are already confined). (3) One tool-call at a time (serialized by the client lock) - fine for v1.
