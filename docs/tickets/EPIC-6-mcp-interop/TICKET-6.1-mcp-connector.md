---
id: T-6.1
epic: EPIC-6-mcp-interop
title: Messages-API MCP connector (remote HTTP servers)
status: ready-for-agent
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
