//! MCP (Model Context Protocol) interop - EPIC-6. aterm is a client now, a host
//! later. Two consume paths, one gate:
//!
//! - [`connector`] (T-6.1): REMOTE (public HTTPS) MCP servers via the Anthropic
//!   Messages-API MCP connector. Anthropic brokers the connection and runs the
//!   tools SERVER-SIDE; we send `mcp_servers` + a matching `mcp_toolset` per
//!   server (beta header [`connector::MCP_CONNECTOR_BETA`]). The gate is applied
//!   at request-build time as a deny-by-default per-tool allow/deny policy, since
//!   a server-side call cannot be intercepted mid-turn. NOT ZDR-eligible - data
//!   routes through Anthropic; privacy-sensitive users should prefer local stdio.
//! - `stdio` (T-6.2): LOCAL stdio MCP servers (the common dev case), where we run
//!   our own MCP client in Rust, spawn the server, and surface each tool as a
//!   native tool in the turn loop - gated + sandboxed + sanitized exactly like a
//!   native tool, fully on-device.
//!
//! [`discovery`] (T-6.3) reads the standard `mcpServers` JSON config from the
//! well-known locations and produces a neutral model that wires stdio servers to
//! the `stdio` client and remote servers to the `connector` - deny-by-default on
//! both paths - so aterm auto-hosts the servers a user already configured for
//! Claude Code (`.mcp.json` / `~/.claude.json`). Codex's TOML config is a deferred
//! follow-up (see [`discovery`]).
//!
//! Hosting an MCP server (exposing aterm's own gated tools to other agents) is
//! post-MVP and out of v1 scope (see `docs/research/06-agent-architecture.md`).

pub mod connector;
pub mod discovery;
pub mod stdio;
