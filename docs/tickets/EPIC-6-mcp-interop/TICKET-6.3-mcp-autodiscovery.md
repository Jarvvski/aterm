---
id: T-6.3
epic: EPIC-6-mcp-interop
title: MCP config auto-discovery
status: ready-for-agent
labels: [agent, mcp]
depends_on: [T-6.2]
---

# Goal

Auto-discover MCP server configuration so aterm "just works" as a host for Camp B agents (Claude Code / Codex) - reading standard MCP config locations and wiring discovered servers into the local stdio client and/or the connector.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) section (c); [11-competitive-landscape.md](../../research/11-competitive-landscape.md) (the wedge includes being the best HOST for external agents via MCP auto-discovery + OSC-133).

# Implementation notes

- Crate: `aterm-agent`. Module `mcp::discovery`.
- Read standard MCP config files (project-level and user-level; mirror the conventions Claude Code / common MCP hosts use - confirm current schema). Parse server definitions (stdio command/args/env, or remote url + auth).
- Wire stdio servers into T-6.2 and remote HTTPS servers into the connector (T-6.1). Apply the gate/denylist uniformly.
- Surface discovered servers in the UI (which are connected, which tools enabled). Do not auto-enable destructive tools (default denylist + confirmation).

# Acceptance criteria

- A project-level MCP config is discovered and its stdio servers are connected automatically.
- A remote server in config is wired through the connector.
- Discovered tools default to gated/denylisted for destructive operations.
- The user can see and toggle discovered servers.

# Out of scope

- Hosting aterm AS an MCP server (post-MVP, out of v1).
- The connector/stdio implementations (T-6.1/T-6.2).
