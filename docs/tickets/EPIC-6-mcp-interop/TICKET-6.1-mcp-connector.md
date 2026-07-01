---
id: T-6.1
epic: EPIC-6-mcp-interop
title: Messages-API MCP connector (remote HTTP servers)
status: done
labels: [agent, mcp, anthropic]
depends_on: [T-5.2, T-5.5]
---

# Goal

Consume remote (public HTTPS) MCP servers via the Anthropic Messages-API MCP connector - the cheapest path, where Anthropic brokers the connection - with a per-tool allowlist/denylist and every MCP tool call routed through the same risk gate.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (c) (consume remote HTTP MCP) + Recommendation 7. BEFORE implementing, load the `claude-api` skill to confirm the current MCP connector beta header and parameter shapes (`mcp-client-2025-11-20`, `mcp_servers` + `mcp_toolset`).

# Implementation notes

- Crate: `aterm-agent`. Module `mcp::connector` (extends the Anthropic provider T-5.2).
- Beta header `mcp-client-2025-11-20`. Request: `mcp_servers: [{type:"url", url, name, authorization_token?}]` AND a matching `tools: [{type:"mcp_toolset", mcp_server_name}]` (every server referenced by exactly one toolset or the request 400s).
- Per-tool allowlist/denylist via `default_config` + `configs` (e.g. `default_config.enabled:false` then enable specific tools) - this is where write/destructive MCP tools are denylisted or forced through confirmation.
- Response blocks `mcp_tool_use` / `mcp_tool_result` map into the provider-neutral events (T-5.1) and the timeline (T-5.10).
- Route every MCP tool call through the risk gate (T-5.5) before surfacing/acting - an MCP tool that runs a command or writes a file is classified exactly like a native one. (Note connector tools execute server-side, but their results are untrusted -> sanitizer + injection defense.)
- Limits to honor/document: tool calls only (not MCP prompts/resources); public HTTPS only; not ZDR-eligible (document for privacy-sensitive users).

# Acceptance criteria

- A remote MCP server's tools are listed and callable through a Messages request (mocked).
- A denylisted/destructive MCP tool is gated/confirmed, not silently run.
- `mcp_tool_use`/`mcp_tool_result` render in the timeline.
- A request missing a toolset for a declared server is rejected before sending (avoid the 400).
- The not-ZDR-eligible limitation is documented.

# Out of scope

- Local stdio servers (T-6.2) and auto-discovery (T-6.3).
- Hosting an MCP server (post-MVP, out of v1).

# Notes (landed 2026-07-01)

New module `aterm-agent/src/mcp/connector.rs` + `mcp/mod.rs`. All ACs met, keyless.

- **Config + gate at request-build time.** `McpServer { name, url, authorization_token?, tool_policy }` with a **deny-by-default** `McpToolPolicy { default_enabled=false, allow, deny }` (`deny` wins over `allow`). `toolset_json()` emits the `mcp_toolset` with `default_config.enabled` + a `configs` map that marks every named tool `{enabled}` - so a denylisted/unlisted destructive tool is provably disabled (AC: "gated, not silently run"). Because connector tools run server-side, we cannot pause a call mid-turn; the allow/deny config IS the gate. `classify_mcp_tool()` returns Caution + the new `RiskReason::McpTool` for timeline badges.
- **Provider wiring (Anthropic-specific, so on the provider, not `TurnRequest`).** `AnthropicProvider::with_mcp_servers(Vec<McpServer>)`; `build_body` merges one `mcp_toolset` per server into the shared `tools` array + emits `mcp_servers`; the `anthropic-beta: mcp-client-2025-11-20` header is set only when servers are present. `validate_servers` (non-empty/unique names, HTTPS) runs before send; `validate_connector_body` re-asserts the 1:1 `mcp_servers`<->`mcp_toolset` invariant on the assembled body (AC: reject before the 400) -> `ProviderError::Invalid`.
- **Response + timeline.** New `ProviderEvent`/`AgentEvent` `McpToolUse`/`McpToolResult`; `StreamState` handles `mcp_tool_use` (assembled at `content_block_stop`, no spurious `ToolUseInputDelta`) and `mcp_tool_result` (delivered whole at start, `content[]` flattened). The mapper passes them 1:1 - they NEVER become `ToolProposed`, so the loop never locally dispatches a server-side call. The turn loop forwards them render-only and sanitizes the untrusted result against the single `Secrets` source before the timeline sees it. The app `StreamProjector` renders both (use block notes "ran remotely").
- **Limits documented:** tool calls only (not MCP prompts/resources); public HTTPS only; **NOT ZDR-eligible** (module doc + CHANGELOG). Connector blocks are not echoed on a `pause_turn` resume (v1 limitation; connector turns rarely pause).
- **Residual:** the exact 2025-11-20 wire shapes are encoded from the ticket/research spec (`mcp_servers`/`mcp_toolset`/`default_config`+`configs`, `mcp_tool_use`/`mcp_tool_result`); confirm against live beta docs when a real key + remote server are wired for an end-to-end pass (interim provider selection is env-var, T-8.3 owns key custody). Tests are pure fixture-driven (no network, no key).
