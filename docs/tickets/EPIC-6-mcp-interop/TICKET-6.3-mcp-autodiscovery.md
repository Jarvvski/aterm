---
id: T-6.3
epic: EPIC-6-mcp-interop
title: MCP config auto-discovery
status: done
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

# Resolution

**2026-07-01 (agent): Done.** A new pure discovery model in `aterm-agent`
(`mcp::discovery`) plus the app-side connect wiring (`aterm-app::mcp` +
`agent_runtime`).

**Standard followed (owner-confirmed this session).** There is no `agents/`
directory MCP standard - `AGENTS.md` is instructions-only. The ecosystem shares a
*schema*, the `mcpServers` JSON map (stdio: `command`/`args`/`env`; remote:
`type`/`url`/`headers`), read from well-known files. v1 adopts that schema with
ZERO new deps (`serde_json` only).

- **Where we look** (owner-specified precedence). User level, first existing wins:
  `$HOME/mcp.json` -> `$XDG_HOME/mcp.json` -> `$XDG_CONFIG_HOME/mcp/mcp.json`
  (default `~/.config/mcp/mcp.json`) -> `~/.claude.json`. Project level: the nearest
  `.mcp.json` walking UP from the cwd. A **project** server shadows a **user** one of
  the same name. Precedence, walk-up, parse, merge, and `${VAR}`/`${VAR:-default}`
  expansion are pure functions.

- **AC (a) project stdio connected automatically:** `aterm-app::mcp::provision` (run
  once in `AgentRuntime::new`, bounded + fail-soft) connects each enabled stdio server
  via the T-6.2 `StdioMcpClient::connect`, registers its tools with
  `ToolRegistry::with_mcp_tools`, and dispatches through `McpToolRouter`. Proven
  end-to-end by `connect_wires_a_real_stdio_server_...` (a real scripted stdio server
  + a bogus one, unix-gated) and the pure `end_to_end_project_over_user_via_temp_dir`.

- **AC (b) remote wired through the connector:** enabled remote servers become
  deny-by-default `McpServer`s and are attached via
  `AnthropicProvider::with_mcp_servers` in `select_provider` (the connector is
  Anthropic-only, T-6.1; inert + logged under OpenAI/mock). Proven by
  `select_provider_attaches_discovered_remote_mcp_only_to_anthropic`.

- **AC (c) discovered tools gated/deny-by-default:** stdio tools are `RequireConfirm`
  via the turn loop's existing MCP over-approximation; remote servers get
  `McpToolPolicy::default()` (no tool enabled until allow-listed). A non-HTTPS remote
  is dropped at discovery (would 400 the connector).

- **AC (d) see + toggle:** disable via a per-entry `"disabled": true` or
  `ATERM_MCP_DISABLE=a,b`; disabled servers stay in the model but are not connected.
  "See" = `Discovery::summary_lines` logged at startup + the `Discovery`/`McpProvision`
  model exposed for a future settings panel (EPIC-8).

**Adversarial review (4-dimension find -> verify workflow; 13 findings, all verified
real, all fixed):**
- **[SAFETY-1/HON-1]** `host_of` leaked url `userinfo@` credentials into the startup
  summary log - now strips userinfo (+ test).
- **[SAFETY-2]** the non-HTTPS diagnostic echoed the full raw url (hardcoded creds) -
  now reports scheme + userinfo-stripped host only (+ test).
- **[DISC-1]** a non-string `env`/`args` value failed the WHOLE-file parse (dropping
  every sibling) - `args`/`env` are now `Value`; scalars are coerced, non-scalars
  dropped per-entry with a diagnostic (+ test).
- **[DISC-2]** every `$$` was escaped to `$` (corrupting legit `$$`) - now only `$${`
  escapes (+ test).
- **[DISC-3]** a `${VAR:-default}` whose default contained `${...}` was truncated at
  the first `}` - now a depth-aware close-brace match + recursive default expansion
  (+ test).
- **[DISC-4]** a whole-value-variable `Authorization: "${MCP_AUTH}"` header was dropped
  (prefix stripped pre-expansion) - the raw value is now kept and the `Bearer` strip
  happens AFTER expansion (+ test).
- **[HON-2]** the "never blocks the window" doc was false (bounded blocking up to the
  connect timeout) - corrected.
- **[HON-3]** the docs claimed Codex support though its TOML config is unread -
  corrected to name it as deferred.
- **[HON-4]** two stdio servers exposing the same tool name misrouted nondeterministically -
  tool ordering is now deterministic (by server name) with a collision warning.
- **[AC-A-1/AC-B-1/AC-D-1]** the connect + provider-selection wiring was untested -
  added the two app-layer tests above.

**HONEST LIMITS.** Startup blocks (bounded) on connecting discovered servers; a wedged
server delays the window by at most one per-server timeout, then is skipped. The
connector (remote) path is Anthropic-only; discovered remote servers are inert (logged)
under OpenAI/mock. `~/.codex/config.toml` (TOML) is a deferred follow-up. No settings
UI yet - "toggle" is the `disabled` field + `ATERM_MCP_DISABLE` (EPIC-8 owns the panel).
