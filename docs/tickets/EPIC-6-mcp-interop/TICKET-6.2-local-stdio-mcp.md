---
id: T-6.2
epic: EPIC-6-mcp-interop
title: Local stdio MCP client
status: ready-for-agent
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
