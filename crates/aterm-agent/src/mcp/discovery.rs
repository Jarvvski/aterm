//! T-6.3: MCP config auto-discovery - read the standard, well-known MCP server
//! config and wire discovered servers into the two consume paths so aterm "just
//! works" as a HOST for external agents (the wedge in
//! `docs/research/11-competitive-landscape.md` rec 4). v1 reads the `mcpServers`
//! JSON schema (Claude Code's `.mcp.json` + `~/.claude.json`, and any host sharing
//! that schema); Codex's `~/.codex/config.toml` (TOML) is a DEFERRED follow-up
//! (it needs a TOML parser + a distinct schema), so a Codex-only user sees no
//! servers until then.
//!
//! # The standard we follow
//!
//! There is no `agents/` directory standard for MCP - `AGENTS.md` (OpenAI, an
//! Agentic-AI-Foundation founding project alongside MCP and goose) is
//! instructions-only and defines nothing about servers. What the ecosystem shares
//! is a *schema, not a location*: the **`mcpServers` JSON map** every host reads
//! (`.mcp.json`, `.cursor/mcp.json`, `~/.claude.json`, ...). A server entry is
//! either STDIO (`command` + `args` + `env`) or REMOTE (`type` + `url` +
//! `headers`). We adopt that one schema (zero new deps - `serde_json` only) and
//! read it from a fixed, ordered set of well-known files.
//!
//! # Where we look
//!
//! - **Project** (checked-in, more specific): `.mcp.json`, walking UP from the cwd
//!   to the filesystem root - the nearest one wins.
//! - **User** (global) - the FIRST existing file wins (a fallback chain):
//!   1. `$HOME/mcp.json`
//!   2. `$XDG_HOME/mcp.json` (non-standard XDG var, honored only if set)
//!   3. `$XDG_CONFIG_HOME/mcp/mcp.json` (defaults to `~/.config/mcp/mcp.json`)
//!   4. `~/.claude.json` (the Claude Code fallback; its top-level `mcpServers`)
//!
//! A **project** server shadows a **user** server of the same name (more specific
//! wins), with a diagnostic. Precedence and the walk-up are pure functions; only
//! the file reads touch I/O.
//!
//! # Safety (this is why discovery is deny-by-default too)
//!
//! Discovery never widens the trust surface on its own:
//! - A discovered STDIO server's tools are registered as native tools and the turn
//!   loop over-approximates every MCP call to `RequireConfirm` (see
//!   [`crate::turn::gate_tool`]) - so a discovered tool can never auto-run.
//! - A discovered REMOTE server is built with the **deny-by-default**
//!   [`McpToolPolicy`](crate::mcp::connector::McpToolPolicy) (no tool enabled until
//!   the user allow-lists it), because a connector tool runs server-side and cannot
//!   be paused mid-turn. This is AC (c): destructive tools are never auto-enabled.
//! - A remote `url` that is not public HTTPS is DROPPED at discovery (with a
//!   diagnostic) - the connector requires HTTPS and would otherwise 400 the turn.
//! - Config-supplied secrets (`env` values, an `Authorization` header) are the
//!   user's own and are forwarded to the server they configured; they are NEVER
//!   logged (summaries print names/hosts/commands only, never values).
//!
//! # Toggling (AC d)
//!
//! A server is disabled by either the config entry's own `"disabled": true` field
//! or the [`DISABLE_ENV`] env override (a comma/space-separated name list) - the
//! interim mechanism until the EPIC-8 settings UI. Disabled servers stay in the
//! [`Discovery`] model (so a future panel can show and re-enable them) but are not
//! connected. [`Discovery::summary_lines`] is the "see" surface (logged at startup).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::mcp::connector::{McpServer, McpToolPolicy};
use crate::mcp::stdio::StdioServerConfig;

/// Env override naming servers to force-disable (comma/whitespace separated).
/// The interim "toggle" until the EPIC-8 config UI, alongside the per-entry
/// `"disabled": true` field.
pub const DISABLE_ENV: &str = "ATERM_MCP_DISABLE";

/// The neutral shared config filename (the `$HOME` / XDG locations).
const MCP_JSON: &str = "mcp.json";
/// The project-level, checked-in config filename (walked UP from the cwd).
const PROJECT_MCP_JSON: &str = ".mcp.json";
/// The Claude Code user config, read as the last user-level fallback (its
/// top-level `mcpServers` map; per-project entries are not read in v1).
const CLAUDE_JSON: &str = ".claude.json";

// ---- neutral discovered model ----------------------------------------------

/// Where a server definition was found. `Project` is more specific and wins on a
/// name collision with `User`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerScope {
    /// A user-level (global) config file.
    User,
    /// A project-level `.mcp.json` (checked-in, walked up from the cwd).
    Project,
}

impl ServerScope {
    /// A short label for summaries/logs.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ServerScope::User => "user",
            ServerScope::Project => "project",
        }
    }
}

/// The transport a discovered remote server speaks, from the config's `type`
/// field. Informational only - the connector brokers both as a `type:"url"`
/// server; we keep it for the summary and forward-compat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTransport {
    /// Legacy Server-Sent-Events transport (`"type": "sse"`).
    Sse,
    /// Streamable HTTP (`"type": "http"` / `"streamable-http"`); the modern
    /// default, also assumed when `type` is absent on a `url` entry.
    Http,
}

/// A discovered server's transport plus its launch/connect details, normalized
/// across the source format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveredTransport {
    /// A local stdio server (T-6.2): spawn `command args` with `env`.
    Stdio {
        /// The executable to spawn.
        command: String,
        /// Its arguments.
        args: Vec<String>,
        /// Extra environment (sorted by key for determinism).
        env: Vec<(String, String)>,
    },
    /// A remote HTTPS server (T-6.1, via the Anthropic connector).
    Remote {
        /// The declared (or assumed) transport.
        transport: RemoteTransport,
        /// The public HTTPS endpoint.
        url: String,
        /// The bearer token for the connector, from the `Authorization` header.
        /// After `parse_mcp_json` this holds the RAW header value; discovery then
        /// expands `${VAR}`s and strips the `Bearer ` prefix (see the DISC-4 note),
        /// so post-[`discover`] it is the bare token.
        authorization_token: Option<String>,
    },
}

/// One discovered MCP server, neutral across the config source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredServer {
    /// The server's logical name (the `mcpServers` map key; the routing key).
    pub name: String,
    /// Where it was found (project shadows user).
    pub scope: ServerScope,
    /// The file it came from (for the "see" summary; never a secret).
    pub source: PathBuf,
    /// Whether it should be connected (false if `"disabled": true` or named in
    /// [`DISABLE_ENV`]). A disabled server is retained in the model but skipped.
    pub enabled: bool,
    /// Its transport + connect details.
    pub transport: DiscoveredTransport,
}

impl DiscoveredServer {
    /// Whether this is a local stdio server (wires to T-6.2).
    #[must_use]
    pub fn is_stdio(&self) -> bool {
        matches!(self.transport, DiscoveredTransport::Stdio { .. })
    }

    /// Whether this is a remote server (wires to the T-6.1 connector).
    #[must_use]
    pub fn is_remote(&self) -> bool {
        matches!(self.transport, DiscoveredTransport::Remote { .. })
    }

    /// The [`StdioServerConfig`] to hand [`crate::mcp::stdio::StdioMcpClient::connect`],
    /// or `None` for a remote server. `cwd` is left default (the process cwd).
    #[must_use]
    pub fn as_stdio_config(&self) -> Option<StdioServerConfig> {
        match &self.transport {
            DiscoveredTransport::Stdio { command, args, env } => Some(StdioServerConfig {
                name: self.name.clone(),
                command: command.clone(),
                args: args.clone(),
                cwd: None,
                env: env.clone(),
            }),
            DiscoveredTransport::Remote { .. } => None,
        }
    }

    /// The connector [`McpServer`] (T-6.1) with the safe **deny-by-default**
    /// [`McpToolPolicy`] - no discovered remote tool is enabled until the user
    /// allow-lists it (AC c) - or `None` for a stdio server.
    #[must_use]
    pub fn as_connector_server(&self) -> Option<McpServer> {
        match &self.transport {
            DiscoveredTransport::Remote {
                url,
                authorization_token,
                ..
            } => {
                let mut server = McpServer::new(&self.name, url.clone())
                    .with_tool_policy(McpToolPolicy::default());
                if let Some(token) = authorization_token {
                    server = server.with_authorization_token(token.clone());
                }
                Some(server)
            }
            DiscoveredTransport::Stdio { .. } => None,
        }
    }

    /// A single non-secret summary line for logs / a future settings panel.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let state = if self.enabled { "enabled" } else { "disabled" };
        let detail = match &self.transport {
            DiscoveredTransport::Stdio { command, .. } => format!("stdio: {command}"),
            DiscoveredTransport::Remote { transport, url, .. } => {
                let scheme = match transport {
                    RemoteTransport::Sse => "sse",
                    RemoteTransport::Http => "http",
                };
                format!("remote/{scheme}: {}", host_of(url))
            }
        };
        format!(
            "  - {name} [{scope}, {state}] {detail}",
            name = self.name,
            scope = self.scope.label(),
        )
    }
}

/// The bare `host[:port]` of a url for a NON-SECRET summary. Drops the query
/// (`?...`), the fragment (`#...`), AND crucially any `userinfo@` authority
/// prefix - a url like `https://svc:token@host/mcp` must never surface its
/// embedded credential in a log line (the review's SAFETY-1 leak).
fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Keep only what follows the last `@` (strip `user:pass@`).
    authority
        .rsplit('@')
        .next()
        .unwrap_or(authority)
        .to_string()
}

// ---- the discovery result --------------------------------------------------

/// The outcome of auto-discovery: the merged server model, the files actually
/// read, and any tolerated diagnostics (a malformed file / skipped entry never
/// fails discovery - it degrades honestly with a note).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    /// The merged servers (project shadowing user), sorted by name for
    /// determinism. Includes DISABLED servers (skip them at connect time).
    pub servers: Vec<DiscoveredServer>,
    /// The config files that existed and were read.
    pub sources: Vec<PathBuf>,
    /// Human-readable notes (skipped entries, shadowing, dropped non-HTTPS
    /// remotes, unresolved `${VAR}`s). Never contains a secret value.
    pub diagnostics: Vec<String>,
}

impl Discovery {
    /// The servers that should actually be connected (enabled only).
    pub fn enabled(&self) -> impl Iterator<Item = &DiscoveredServer> {
        self.servers.iter().filter(|s| s.enabled)
    }

    /// The enabled stdio configs (wire to T-6.2).
    #[must_use]
    pub fn stdio_configs(&self) -> Vec<StdioServerConfig> {
        self.enabled()
            .filter_map(DiscoveredServer::as_stdio_config)
            .collect()
    }

    /// The enabled connector servers (wire to T-6.1), each deny-by-default.
    #[must_use]
    pub fn connector_servers(&self) -> Vec<McpServer> {
        self.enabled()
            .filter_map(DiscoveredServer::as_connector_server)
            .collect()
    }

    /// Non-secret summary lines (the "see" surface, AC d): one header + one line
    /// per server. Empty-safe.
    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        if self.servers.is_empty() {
            return vec!["MCP auto-discovery: no servers configured".to_string()];
        }
        let mut lines = vec![format!(
            "MCP auto-discovery: {} server(s) from {} file(s)",
            self.servers.len(),
            self.sources.len()
        )];
        lines.extend(self.servers.iter().map(DiscoveredServer::summary_line));
        lines
    }
}

// ---- errors ----------------------------------------------------------------

/// Why a single config file could not be parsed. A parse failure is tolerated
/// (recorded as a diagnostic), never a panic and never a hard discovery failure.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DiscoveryError {
    /// The file was not valid JSON / not the expected shape.
    #[error("could not parse MCP config `{path}`: {message}")]
    Parse {
        /// The offending file.
        path: PathBuf,
        /// The serde error text.
        message: String,
    },
}

// ---- raw serde model (lenient) ---------------------------------------------

/// The `mcpServers` wrapper. Lenient: unknown top-level keys (`projects`,
/// `enabledMcpjsonServers`, tool-specific settings, ...) are ignored so we can
/// read another host's larger config file without choking.
#[derive(Debug, Deserialize)]
struct RawFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: BTreeMap<String, RawServer>,
}

/// One raw server entry. Every field optional; classification is by which of
/// `command` / `url` is present. Unknown fields (`timeout`, `bearer_token_env_var`,
/// `enabled_tools`, ...) are ignored.
///
/// `args`/`env` values are typed [`Value`], NOT `String`, on purpose: real-world
/// configs write numbers/bools (e.g. `"env": { "PORT": 8080 }`), and typing them
/// as `String` would fail the WHOLE-file deserialize on the first non-string
/// value - silently dropping every sibling server (the review's DISC-1). We
/// instead coerce scalars and skip non-scalars per-entry in [`classify`].
#[derive(Debug, Default, Deserialize)]
struct RawServer {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<Value>,
    #[serde(default)]
    env: BTreeMap<String, Value>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    disabled: bool,
}

/// Coerce a JSON scalar to the string a launched process expects. A string is
/// taken as-is; a number/bool is stringified (`8080` -> `"8080"`); null / array /
/// object have no sensible argv/env representation and yield `None` (skipped with
/// a diagnostic, never a crash).
fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

// ---- parse (pure) -----------------------------------------------------------

/// A parsed file: the servers it yielded plus any per-entry diagnostics.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedFile {
    /// The servers parsed from this file (unexpanded values).
    pub servers: Vec<DiscoveredServer>,
    /// Notes about skipped/dropped entries in this file.
    pub diagnostics: Vec<String>,
}

/// Parse one `mcpServers` JSON document. PURE (no env, no I/O): `${VAR}`
/// expansion is a separate pass ([`expand_vars`]). A structurally-broken FILE is
/// an `Err`; a broken single ENTRY is skipped with a diagnostic (one bad server
/// never drops the rest).
///
/// # Errors
/// [`DiscoveryError::Parse`] if the document is not the `mcpServers` shape.
pub fn parse_mcp_json(
    text: &str,
    scope: ServerScope,
    source: &Path,
) -> Result<ParsedFile, DiscoveryError> {
    let raw: RawFile = serde_json::from_str(text).map_err(|e| DiscoveryError::Parse {
        path: source.to_path_buf(),
        message: e.to_string(),
    })?;
    let mut out = ParsedFile::default();
    for (name, rs) in raw.mcp_servers {
        match classify(&name, rs, scope, source, &mut out.diagnostics) {
            Ok(server) => out.servers.push(server),
            Err(reason) => out.diagnostics.push(format!(
                "skipped server `{name}` in {}: {reason}",
                source.display()
            )),
        }
    }
    Ok(out)
}

/// Turn one raw entry into a [`DiscoveredServer`], or an `Err(reason)` to skip the
/// whole entry. Per-field coercion notes (a dropped non-scalar arg/env) go to
/// `diagnostics` and do NOT drop the server. The `authorization_token` set here is
/// the RAW `Authorization` header value (unexpanded, `Bearer` prefix intact);
/// [`expand_server`] expands and normalizes it (see the DISC-4 note).
fn classify(
    name: &str,
    rs: RawServer,
    scope: ServerScope,
    source: &Path,
    diagnostics: &mut Vec<String>,
) -> Result<DiscoveredServer, String> {
    let enabled = !rs.disabled;
    // A `command` means stdio; a `url` means remote. `command` wins if (wrongly)
    // both are present - a local process is the safer interpretation.
    if let Some(command) = rs.command.filter(|c| !c.trim().is_empty()) {
        // Coerce scalar args/env; skip (with a note) anything without a string form.
        let mut args = Vec::with_capacity(rs.args.len());
        for (i, a) in rs.args.iter().enumerate() {
            match scalar_to_string(a) {
                Some(s) => args.push(s),
                None => diagnostics.push(format!(
                    "server `{name}`: arg[{i}] is not a scalar, dropped"
                )),
            }
        }
        let mut env = Vec::with_capacity(rs.env.len());
        for (k, v) in &rs.env {
            match scalar_to_string(v) {
                Some(s) => env.push((k.clone(), s)),
                None => diagnostics.push(format!(
                    "server `{name}`: env `{k}` is not a scalar, dropped"
                )),
            }
        }
        return Ok(DiscoveredServer {
            name: name.to_string(),
            scope,
            source: source.to_path_buf(),
            enabled,
            transport: DiscoveredTransport::Stdio { command, args, env },
        });
    }
    if let Some(url) = rs.url.filter(|u| !u.trim().is_empty()) {
        // The connector requires public HTTPS; a non-HTTPS remote would 400 the
        // turn, so drop it here. Report only scheme + host (userinfo-stripped) so a
        // hardcoded credential in the url never lands in the diagnostic (SAFETY-2).
        if !url.starts_with("https://") {
            return Err(format!(
                "remote url must be public HTTPS (dropped `{}://{}`)",
                url.split("://").next().unwrap_or("?"),
                host_of(&url)
            ));
        }
        let transport = match rs.r#type.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("sse") => RemoteTransport::Sse,
            // Missing `type` on a url: assume Streamable HTTP (the modern default).
            Some("http" | "streamable-http" | "streamable_http") | None => RemoteTransport::Http,
            Some(other) => return Err(format!("unknown remote transport `type`: `{other}`")),
        };
        let authorization_token = authorization_value_from_headers(&rs.headers);
        return Ok(DiscoveredServer {
            name: name.to_string(),
            scope,
            source: source.to_path_buf(),
            enabled,
            transport: DiscoveredTransport::Remote {
                transport,
                url,
                authorization_token,
            },
        });
    }
    Err("must set `command` (stdio) or `url` (remote)".to_string())
}

/// The RAW `Authorization` header value (case-insensitive key), unexpanded and
/// with any `Bearer ` prefix intact. The `Bearer`-strip is deferred to
/// [`normalize_bearer`] AFTER `${VAR}` expansion, so a fully-variable value like
/// `Authorization: "${MCP_AUTH}"` (where `MCP_AUTH` resolves to `Bearer xyz`) is
/// handled, not dropped for lacking a literal prefix (the review's DISC-4).
fn authorization_value_from_headers(headers: &BTreeMap<String, String>) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Normalize a (post-expansion) `Authorization` value into the connector's bearer
/// `authorization_token`: strip an optional `Bearer ` prefix (case-insensitive);
/// an empty result yields `None`. A non-Bearer scheme is kept verbatim - the
/// connector forwards it as-is - so no auth is silently lost.
fn normalize_bearer(value: &str) -> Option<String> {
    let v = value.trim();
    let token = v
        .strip_prefix("Bearer ")
        .or_else(|| v.strip_prefix("bearer "))
        .unwrap_or(v)
        .trim();
    (!token.is_empty()).then(|| token.to_string())
}

// ---- ${VAR} / ${VAR:-default} expansion (pure) ------------------------------

/// Expand `${VAR}` and `${VAR:-default}` in `input` using `lookup`. An
/// unresolved `${VAR}` with no default is left VERBATIM and its name pushed to
/// `unresolved` (never guess a value). `$${` is an escape for a literal `${`;
/// a bare `$$` (not followed by `{`) is NOT special and copies through verbatim
/// (the review's DISC-2). A missing closing brace is left verbatim. The closing
/// brace is matched with brace DEPTH, so a nested `${...}` inside a `:-default`
/// is handled, not truncated at the first `}` (DISC-3). NEVER logs a value.
#[must_use]
pub fn expand_vars(
    input: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
    unresolved: &mut Vec<String>,
) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        // `$${` escapes to a literal `${`; a bare `$$` is not special.
        if bytes[i] == b'$' && i + 2 < bytes.len() && bytes[i + 1] == b'$' && bytes[i + 2] == b'{' {
            out.push_str("${");
            i += 3;
            continue;
        }
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(close) = matching_close_brace(&input[i + 2..]) {
                let inner = &input[i + 2..i + 2 + close];
                let (name, default) = match inner.split_once(":-") {
                    Some((n, d)) => (n.trim(), Some(d)),
                    None => (inner.trim(), None),
                };
                match lookup(name) {
                    Some(val) => out.push_str(&val),
                    None => match default {
                        // A default may itself contain `${...}`; expand it recursively.
                        Some(d) => out.push_str(&expand_vars(d, lookup, unresolved)),
                        None => {
                            unresolved.push(name.to_string());
                            out.push_str(&input[i..i + 2 + close + 1]); // verbatim ${...}
                        }
                    },
                }
                i += 2 + close + 1;
                continue;
            }
        }
        // Not a variable start: copy this char (respecting UTF-8 boundaries).
        let ch_len = utf8_len(bytes[i]);
        out.push_str(&input[i..i + ch_len]);
        i += ch_len;
    }
    out
}

/// Given the text AFTER an opening `${`, return the byte offset of the matching
/// `}`, accounting for nested `${ ... }` (so `A:-x${Y}z}` matches the FINAL `}`).
/// `None` if unbalanced (no matching close).
fn matching_close_brace(rest: &str) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            b'{' if i > 0 && bytes[i - 1] == b'$' => depth += 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Byte length of the UTF-8 char starting with `first`.
fn utf8_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        _ => 4,
    }
}

/// Expand every `${VAR}` in a discovered server's launch/connect strings in
/// place, recording unresolved names as diagnostics (by NAME, never value).
fn expand_server(
    server: &mut DiscoveredServer,
    lookup: &dyn Fn(&str) -> Option<String>,
    diagnostics: &mut Vec<String>,
) {
    let mut unresolved = Vec::new();
    match &mut server.transport {
        DiscoveredTransport::Stdio { command, args, env } => {
            *command = expand_vars(command, lookup, &mut unresolved);
            for a in args.iter_mut() {
                *a = expand_vars(a, lookup, &mut unresolved);
            }
            for (_, v) in env.iter_mut() {
                *v = expand_vars(v, lookup, &mut unresolved);
            }
        }
        DiscoveredTransport::Remote {
            url,
            authorization_token,
            ..
        } => {
            *url = expand_vars(url, lookup, &mut unresolved);
            // Expand the raw Authorization value, THEN strip the `Bearer ` prefix -
            // so a fully-variable header (`"${MCP_AUTH}"` -> `Bearer xyz`) resolves
            // correctly (DISC-4). An empty result drops the token.
            if let Some(raw) = authorization_token.as_ref() {
                let expanded = expand_vars(raw, lookup, &mut unresolved);
                *authorization_token = normalize_bearer(&expanded);
            }
        }
    }
    for name in unresolved {
        diagnostics.push(format!(
            "server `{}`: unresolved ${{{}}} (left verbatim)",
            server.name, name
        ));
    }
}

// ---- file location (pure over injected inputs) ------------------------------

/// The ordered user-level candidate paths (first EXISTING wins). Pure over the
/// resolved home + XDG values so it is unit-testable without touching real env.
#[must_use]
pub fn user_config_candidates(
    home: &Path,
    xdg_home: Option<&Path>,
    xdg_config_home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut v = vec![home.join(MCP_JSON)]; // 1. $HOME/mcp.json
    if let Some(x) = xdg_home {
        v.push(x.join(MCP_JSON)); // 2. $XDG_HOME/mcp.json
    }
    // 3. $XDG_CONFIG_HOME/mcp/mcp.json (default ~/.config/mcp/mcp.json)
    let config_home = xdg_config_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".config"));
    v.push(config_home.join("mcp").join(MCP_JSON));
    v.push(home.join(CLAUDE_JSON)); // 4. ~/.claude.json (fallback)
    v
}

/// The nearest project `.mcp.json` walking UP from `cwd`, or `None`. Pure over an
/// injected existence predicate.
#[must_use]
pub fn project_config_from(cwd: &Path, mut exists: impl FnMut(&Path) -> bool) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let candidate = dir.join(PROJECT_MCP_JSON);
        if exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// The first existing user-level candidate, or `None`. Pure over an injected
/// existence predicate.
#[must_use]
pub fn first_existing(
    candidates: &[PathBuf],
    mut exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

// ---- merge (pure) -----------------------------------------------------------

/// Parse the [`DISABLE_ENV`] value: names separated by commas or whitespace.
#[must_use]
pub fn parse_disable_list(spec: &str) -> Vec<String> {
    spec.split([',', ' ', '\t', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Merge user + project servers (project shadows user by name), apply the
/// force-disable list, and sort by name for determinism. Returns the merged
/// servers plus shadowing/disable diagnostics. PURE.
#[must_use]
pub fn merge(
    user: Vec<DiscoveredServer>,
    project: Vec<DiscoveredServer>,
    disabled: &[String],
) -> (Vec<DiscoveredServer>, Vec<String>) {
    let mut diagnostics = Vec::new();
    let mut by_name: BTreeMap<String, DiscoveredServer> = BTreeMap::new();
    for s in user {
        by_name.insert(s.name.clone(), s);
    }
    for s in project {
        if by_name.contains_key(&s.name) {
            diagnostics.push(format!(
                "project `{}` shadows the user-level server of the same name",
                s.name
            ));
        }
        by_name.insert(s.name.clone(), s);
    }
    let mut servers: Vec<DiscoveredServer> = by_name.into_values().collect();
    for s in &mut servers {
        if disabled.iter().any(|d| d == &s.name) && s.enabled {
            s.enabled = false;
            diagnostics.push(format!("server `{}` disabled via {DISABLE_ENV}", s.name));
        }
    }
    (servers, diagnostics)
}

// ---- top-level discovery (I/O over explicit inputs) -------------------------

/// The environment discovery reads: kept as explicit inputs so the whole pipeline
/// is testable against a temp dir without mutating the real process env.
#[derive(Debug, Clone)]
pub struct DiscoveryEnv {
    /// The current working directory (project walk-up root).
    pub cwd: PathBuf,
    /// The user's home directory (`$HOME`).
    pub home: PathBuf,
    /// `$XDG_HOME` if set (non-standard, honored if present).
    pub xdg_home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` if set.
    pub xdg_config_home: Option<PathBuf>,
    /// Names to force-disable (from [`DISABLE_ENV`]).
    pub disabled: Vec<String>,
}

impl DiscoveryEnv {
    /// Read the discovery environment from the real process env. `home` falls back
    /// to `.` if `$HOME` is unset (discovery then finds nothing user-level).
    #[must_use]
    pub fn from_env(cwd: PathBuf) -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            cwd,
            home: var("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from),
            xdg_home: var("XDG_HOME").map(PathBuf::from),
            xdg_config_home: var("XDG_CONFIG_HOME").map(PathBuf::from),
            disabled: var(DISABLE_ENV)
                .map(|s| parse_disable_list(&s))
                .unwrap_or_default(),
        }
    }
}

/// Read one config file and parse it, expanding `${VAR}` via `lookup`. Returns
/// `None` if the file does not exist / cannot be read; a parse error becomes a
/// diagnostic on `into` (never a hard failure).
fn read_and_parse(
    path: &Path,
    scope: ServerScope,
    lookup: &dyn Fn(&str) -> Option<String>,
    into: &mut Discovery,
) -> Vec<DiscoveredServer> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    into.sources.push(path.to_path_buf());
    match parse_mcp_json(&text, scope, path) {
        Ok(mut parsed) => {
            into.diagnostics.append(&mut parsed.diagnostics);
            for s in &mut parsed.servers {
                expand_server(s, lookup, &mut into.diagnostics);
            }
            parsed.servers
        }
        Err(e) => {
            into.diagnostics.push(e.to_string());
            Vec::new()
        }
    }
}

/// Run auto-discovery against an explicit [`DiscoveryEnv`] and env lookup. This is
/// the testable core; [`discover`] wraps it with the real process env.
#[must_use]
pub fn discover_with(env: &DiscoveryEnv, lookup: &dyn Fn(&str) -> Option<String>) -> Discovery {
    let mut result = Discovery::default();

    // User level: the first existing candidate in the fallback chain.
    let candidates = user_config_candidates(
        &env.home,
        env.xdg_home.as_deref(),
        env.xdg_config_home.as_deref(),
    );
    let user = match first_existing(&candidates, |p| p.exists()) {
        Some(path) => read_and_parse(&path, ServerScope::User, lookup, &mut result),
        None => Vec::new(),
    };

    // Project level: the nearest `.mcp.json` walking up from the cwd.
    let project = match project_config_from(&env.cwd, |p| p.exists()) {
        Some(path) => read_and_parse(&path, ServerScope::Project, lookup, &mut result),
        None => Vec::new(),
    };

    let (servers, mut merge_diags) = merge(user, project, &env.disabled);
    result.diagnostics.append(&mut merge_diags);
    result.servers = servers;
    result
}

/// Auto-discover MCP servers from the standard well-known locations, rooted at
/// `cwd`, using the real process environment for both file location and `${VAR}`
/// expansion. Never fails: a missing/broken file degrades to a diagnostic.
#[must_use]
pub fn discover(cwd: PathBuf) -> Discovery {
    let env = DiscoveryEnv::from_env(cwd);
    discover_with(&env, &|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    // ---- parse: classification ---------------------------------------------

    #[test]
    fn parses_stdio_and_remote_from_one_file() {
        let text = r#"{
            "mcpServers": {
                "fs":     { "command": "npx", "args": ["-y", "server-fs", "/tmp"], "env": { "TOKEN": "x" } },
                "docs":   { "type": "sse", "url": "https://mcp.example.com/sse" },
                "gitmcp": { "url": "https://api.example.com/mcp" }
            }
        }"#;
        let parsed = parse_mcp_json(text, ServerScope::Project, Path::new("/p/.mcp.json")).unwrap();
        assert_eq!(parsed.servers.len(), 3);
        assert!(parsed.diagnostics.is_empty());

        let fs = parsed.servers.iter().find(|s| s.name == "fs").unwrap();
        assert!(fs.is_stdio());
        assert_eq!(fs.scope, ServerScope::Project);
        assert!(fs.enabled);
        match &fs.transport {
            DiscoveredTransport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y", "server-fs", "/tmp"]);
                assert_eq!(env, &[("TOKEN".to_string(), "x".to_string())]);
            }
            DiscoveredTransport::Remote { .. } => panic!("fs should be stdio"),
        }

        let docs = parsed.servers.iter().find(|s| s.name == "docs").unwrap();
        assert!(matches!(
            docs.transport,
            DiscoveredTransport::Remote {
                transport: RemoteTransport::Sse,
                ..
            }
        ));
        // A url with no `type` assumes Streamable HTTP.
        let git = parsed.servers.iter().find(|s| s.name == "gitmcp").unwrap();
        assert!(matches!(
            git.transport,
            DiscoveredTransport::Remote {
                transport: RemoteTransport::Http,
                ..
            }
        ));
    }

    #[test]
    fn a_broken_entry_is_skipped_not_fatal() {
        // Neither command nor url -> skipped with a diagnostic; the good one survives.
        let text = r#"{ "mcpServers": {
            "good": { "command": "echo" },
            "bad":  { "note": "nothing runnable here" }
        }}"#;
        let parsed = parse_mcp_json(text, ServerScope::User, Path::new("/u/mcp.json")).unwrap();
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].name, "good");
        assert_eq!(parsed.diagnostics.len(), 1);
        assert!(parsed.diagnostics[0].contains("bad"));
    }

    #[test]
    fn a_structurally_broken_file_is_a_parse_error() {
        let err =
            parse_mcp_json("{ not json", ServerScope::User, Path::new("/u/mcp.json")).unwrap_err();
        assert!(matches!(err, DiscoveryError::Parse { .. }));
    }

    #[test]
    fn empty_or_missing_mcpservers_yields_no_servers() {
        // Another host's config with no mcpServers key parses to zero, not an error.
        let parsed = parse_mcp_json(
            r#"{ "projects": {} }"#,
            ServerScope::User,
            Path::new("/u/.claude.json"),
        )
        .unwrap();
        assert!(parsed.servers.is_empty());
    }

    #[test]
    fn disabled_field_marks_the_server_off() {
        let text = r#"{ "mcpServers": { "fs": { "command": "npx", "disabled": true } } }"#;
        let parsed = parse_mcp_json(text, ServerScope::Project, Path::new("/p/.mcp.json")).unwrap();
        assert!(!parsed.servers[0].enabled);
    }

    // ---- safety: HTTPS + deny-by-default -----------------------------------

    #[test]
    fn non_https_remote_is_dropped_without_leaking_a_credential() {
        // The connector requires public HTTPS; an http:// remote would 400 the turn,
        // so it is dropped. SAFETY-2: the diagnostic reports scheme+host only - a
        // credential embedded in the url authority never lands in the log.
        let text = r#"{ "mcpServers": { "insecure": {
            "url": "http://user:hardcoded-secret@plaintext.example.com/mcp"
        } } }"#;
        let parsed = parse_mcp_json(text, ServerScope::User, Path::new("/u/mcp.json")).unwrap();
        assert!(parsed.servers.is_empty());
        let diag = parsed.diagnostics.join("\n");
        assert!(diag.contains("HTTPS"));
        assert!(diag.contains("plaintext.example.com"), "host shown: {diag}");
        assert!(
            !diag.contains("hardcoded-secret") && !diag.contains('@'),
            "credential must not leak into the diagnostic: {diag}"
        );
    }

    #[test]
    fn discovered_remote_is_deny_by_default() {
        // AC c: a discovered remote server enables NO tools until allow-listed.
        let text =
            r#"{ "mcpServers": { "docs": { "type": "http", "url": "https://mcp.example.com" } } }"#;
        let parsed = parse_mcp_json(text, ServerScope::User, Path::new("/u/mcp.json")).unwrap();
        let connector = parsed.servers[0].as_connector_server().unwrap();
        assert!(!connector.tool_policy.default_enabled, "deny-by-default");
        assert!(connector.tool_policy.allow.is_empty());
        assert!(!connector.tool_policy.is_enabled("delete_everything"));
    }

    #[test]
    fn authorization_header_is_normalized_after_expansion() {
        // The raw header value is kept at parse time; the `Bearer` strip happens
        // AFTER ${VAR} expansion (DISC-4), so a fully-variable header resolves.
        let mut h = BTreeMap::new();
        h.insert(
            "Authorization".to_string(),
            "Bearer  secret-abc".to_string(),
        );
        h.insert("X-Other".to_string(), "kept-raw".to_string());
        assert_eq!(
            authorization_value_from_headers(&h).as_deref(),
            Some("Bearer  secret-abc"),
            "the whole Authorization value is kept unexpanded"
        );
        assert_eq!(normalize_bearer("Bearer xyz").as_deref(), Some("xyz"));
        assert_eq!(normalize_bearer("bearer  xyz ").as_deref(), Some("xyz"));
        // A non-Bearer scheme is kept verbatim (never silently lost).
        assert_eq!(normalize_bearer("raw-token").as_deref(), Some("raw-token"));
        assert_eq!(normalize_bearer("   "), None);

        // End-to-end: both `Bearer ${TOK}` and a whole-value `${MCP_AUTH}` (which
        // resolves to `Bearer t-2`) yield the bare token on the connector server.
        let dir = tempdir();
        std::fs::write(
            dir.join(".mcp.json"),
            r#"{ "mcpServers": {
                "a": { "url": "https://a.example.com", "headers": { "Authorization": "Bearer ${TOK}" } },
                "b": { "url": "https://b.example.com", "headers": { "Authorization": "${MCP_AUTH}" } }
            } }"#,
        )
        .unwrap();
        let env = DiscoveryEnv {
            cwd: dir.clone(),
            home: dir.join("nohome"),
            xdg_home: None,
            xdg_config_home: None,
            disabled: Vec::new(),
        };
        let lookup = |k: &str| match k {
            "TOK" => Some("t-1".to_string()),
            "MCP_AUTH" => Some("Bearer t-2".to_string()),
            _ => None,
        };
        let disco = discover_with(&env, &lookup);
        let tok = |name: &str| {
            disco
                .servers
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .as_connector_server()
                .unwrap()
                .authorization_token
        };
        assert_eq!(tok("a").as_deref(), Some("t-1"));
        assert_eq!(tok("b").as_deref(), Some("t-2"));
        cleanup(&dir);
    }

    #[test]
    fn summary_line_never_leaks_url_userinfo_credentials() {
        // SAFETY-1: a credential embedded in the url authority must never reach the
        // (info-level) startup summary that provision() logs.
        let s = DiscoveredServer {
            name: "svc".into(),
            scope: ServerScope::User,
            source: "u".into(),
            enabled: true,
            transport: DiscoveredTransport::Remote {
                transport: RemoteTransport::Http,
                url: "https://svc:s3kret-token@api.example.com/mcp?k=v".into(),
                authorization_token: None,
            },
        };
        let line = s.summary_line();
        assert!(line.contains("api.example.com"), "host shown: {line}");
        assert!(
            !line.contains("s3kret-token") && !line.contains('@'),
            "userinfo credential must not leak: {line}"
        );
    }

    #[test]
    fn non_string_env_or_arg_is_coerced_or_skipped_not_fatal() {
        // DISC-1: a numeric/bool value is coerced to a string; a non-scalar is
        // dropped with a diagnostic - and NEITHER fails the whole file (which would
        // drop every sibling server).
        let text = r#"{ "mcpServers": {
            "good": { "command": "srv", "args": ["-p", 8080, true, null], "env": { "PORT": 8080, "OK": true, "OBJ": { "x": 1 } } },
            "also": { "command": "other" }
        } }"#;
        let parsed = parse_mcp_json(text, ServerScope::Project, Path::new("/p/.mcp.json")).unwrap();
        assert_eq!(parsed.servers.len(), 2, "both servers survive a bad value");
        let good = parsed.servers.iter().find(|s| s.name == "good").unwrap();
        match &good.transport {
            DiscoveredTransport::Stdio { args, env, .. } => {
                // 8080 -> "8080", true -> "true", null dropped.
                assert_eq!(args, &["-p", "8080", "true"]);
                assert!(env.contains(&("PORT".to_string(), "8080".to_string())));
                assert!(env.contains(&("OK".to_string(), "true".to_string())));
                assert!(!env.iter().any(|(k, _)| k == "OBJ"), "object env dropped");
            }
            DiscoveredTransport::Remote { .. } => panic!("stdio expected"),
        }
        assert!(parsed.diagnostics.iter().any(|d| d.contains("arg[3]")));
        assert!(parsed.diagnostics.iter().any(|d| d.contains("OBJ")));
    }

    #[test]
    fn as_stdio_config_maps_to_the_t62_launch_config() {
        let text = r#"{ "mcpServers": { "fs": { "command": "npx", "args": ["-y", "s"], "env": { "K": "v" } } } }"#;
        let parsed = parse_mcp_json(text, ServerScope::Project, Path::new("/p/.mcp.json")).unwrap();
        let cfg = parsed.servers[0].as_stdio_config().unwrap();
        assert_eq!(cfg.name, "fs");
        assert_eq!(cfg.command, "npx");
        assert_eq!(cfg.args, vec!["-y".to_string(), "s".to_string()]);
        assert_eq!(cfg.env, vec![("K".to_string(), "v".to_string())]);
        assert!(cfg.cwd.is_none());
    }

    // ---- ${VAR} expansion --------------------------------------------------

    #[test]
    fn expand_vars_only_escapes_double_dollar_before_a_brace() {
        // DISC-2: `$${` escapes to a literal `${`, but a bare `$$` (no following
        // `{`) is NOT special and must survive verbatim (e.g. a shell `$$` pid or a
        // password containing `$$`), not be silently halved to `$`.
        let mut u = Vec::new();
        assert_eq!(expand_vars("$${X}", &no_env, &mut u), "${X}");
        assert_eq!(
            expand_vars("cost is $$5 and pid $$", &no_env, &mut u),
            "cost is $$5 and pid $$"
        );
        assert!(u.is_empty());
    }

    #[test]
    fn expand_vars_matches_nested_braces_in_a_default() {
        // DISC-3: the `:-default` may itself contain `${...}`; the close brace is
        // matched by depth (not the first `}`), and the default is expanded.
        let lookup = |k: &str| (k == "Y").then(|| "inner".to_string());
        let mut u = Vec::new();
        assert_eq!(expand_vars("${A:-x-${Y}-z}", &lookup, &mut u), "x-inner-z");
        assert!(u.is_empty());
    }

    #[test]
    fn expand_vars_resolves_default_and_leaves_unknown_verbatim() {
        let lookup = |k: &str| (k == "TOKEN").then(|| "sekret".to_string());
        let mut unresolved = Vec::new();
        assert_eq!(expand_vars("${TOKEN}", &lookup, &mut unresolved), "sekret");
        assert!(unresolved.is_empty());
        assert_eq!(
            expand_vars("${MISSING:-fallback}", &lookup, &mut unresolved),
            "fallback"
        );
        assert!(unresolved.is_empty());
        // Unknown, no default -> left verbatim + recorded (never guessed).
        assert_eq!(
            expand_vars("a${GONE}b", &lookup, &mut unresolved),
            "a${GONE}b"
        );
        assert_eq!(unresolved, vec!["GONE".to_string()]);
    }

    #[test]
    fn expand_vars_handles_escapes_utf8_and_unclosed() {
        let mut unresolved = Vec::new();
        // `$${` is a literal `${`.
        assert_eq!(
            expand_vars("$${LITERAL}", &no_env, &mut unresolved),
            "${LITERAL}"
        );
        // UTF-8 around a var is preserved.
        let lookup = |k: &str| (k == "X").then(|| "1".to_string());
        assert_eq!(
            expand_vars("héllo ${X} 世界", &lookup, &mut unresolved),
            "héllo 1 世界"
        );
        // An unclosed `${` is copied verbatim.
        assert_eq!(
            expand_vars("${unterminated", &no_env, &mut unresolved),
            "${unterminated"
        );
    }

    #[test]
    fn discovery_expands_server_values() {
        // End-to-end value expansion through discover_with (temp dir, injected env).
        let dir = tempdir();
        let file = dir.join(".mcp.json");
        std::fs::write(
            &file,
            r#"{ "mcpServers": { "fs": { "command": "${BIN}", "args": ["${MISSING:-def}"] } } }"#,
        )
        .unwrap();
        let env = DiscoveryEnv {
            cwd: dir.clone(),
            home: dir.join("nohome"),
            xdg_home: None,
            xdg_config_home: None,
            disabled: Vec::new(),
        };
        let lookup = |k: &str| (k == "BIN").then(|| "/usr/bin/npx".to_string());
        let disco = discover_with(&env, &lookup);
        let fs = disco.servers.iter().find(|s| s.name == "fs").unwrap();
        match &fs.transport {
            DiscoveredTransport::Stdio { command, args, .. } => {
                assert_eq!(command, "/usr/bin/npx");
                assert_eq!(args, &["def"]);
            }
            DiscoveredTransport::Remote { .. } => panic!("stdio expected"),
        }
        cleanup(&dir);
    }

    // ---- file location + precedence ----------------------------------------

    #[test]
    fn user_candidates_are_in_the_agreed_order() {
        let home = Path::new("/home/u");
        let c = user_config_candidates(home, Some(Path::new("/xh")), Some(Path::new("/xc")));
        assert_eq!(
            c,
            vec![
                PathBuf::from("/home/u/mcp.json"),
                PathBuf::from("/xh/mcp.json"),
                PathBuf::from("/xc/mcp/mcp.json"),
                PathBuf::from("/home/u/.claude.json"),
            ]
        );
    }

    #[test]
    fn xdg_config_home_defaults_under_dot_config_when_unset() {
        let c = user_config_candidates(Path::new("/home/u"), None, None);
        assert_eq!(
            c,
            vec![
                PathBuf::from("/home/u/mcp.json"),
                PathBuf::from("/home/u/.config/mcp/mcp.json"),
                PathBuf::from("/home/u/.claude.json"),
            ]
        );
    }

    #[test]
    fn first_existing_is_the_fallback_chain() {
        let cands = user_config_candidates(Path::new("/home/u"), None, None);
        // Only the third (claude fallback) exists.
        let claude = PathBuf::from("/home/u/.claude.json");
        let found = first_existing(&cands, |p| p == claude);
        assert_eq!(found, Some(claude));
    }

    #[test]
    fn project_walk_up_finds_the_nearest() {
        let root = PathBuf::from("/a/.mcp.json");
        let cwd = Path::new("/a/b/c");
        // Only /a/.mcp.json exists; the nearer /a/b and /a/b/c do not.
        let found = project_config_from(cwd, |p| p == root);
        assert_eq!(found, Some(root));
    }

    #[test]
    fn project_walk_up_prefers_the_closer_file() {
        let near = PathBuf::from("/a/b/.mcp.json");
        let cwd = Path::new("/a/b/c");
        // Both /a/.mcp.json and /a/b/.mcp.json exist; the nearer wins.
        let found = project_config_from(cwd, |p| p == near || p == Path::new("/a/.mcp.json"));
        assert_eq!(found, Some(near));
    }

    // ---- merge + disable ---------------------------------------------------

    fn stdio(name: &str, scope: ServerScope) -> DiscoveredServer {
        DiscoveredServer {
            name: name.to_string(),
            scope,
            source: PathBuf::from("x"),
            enabled: true,
            transport: DiscoveredTransport::Stdio {
                command: "c".to_string(),
                args: Vec::new(),
                env: Vec::new(),
            },
        }
    }

    #[test]
    fn project_shadows_user_of_the_same_name() {
        let user = vec![
            stdio("fs", ServerScope::User),
            stdio("only_user", ServerScope::User),
        ];
        let project = vec![stdio("fs", ServerScope::Project)];
        let (merged, diags) = merge(user, project, &[]);
        assert_eq!(merged.len(), 2);
        let fs = merged.iter().find(|s| s.name == "fs").unwrap();
        assert_eq!(fs.scope, ServerScope::Project, "project wins");
        assert!(diags.iter().any(|d| d.contains("shadows")));
    }

    #[test]
    fn disable_env_turns_a_server_off_but_keeps_it_in_the_model() {
        let user = vec![
            stdio("fs", ServerScope::User),
            stdio("notes", ServerScope::User),
        ];
        let (merged, diags) = merge(user, Vec::new(), &["notes".to_string()]);
        let notes = merged.iter().find(|s| s.name == "notes").unwrap();
        assert!(!notes.enabled, "named in the disable list");
        assert!(merged.iter().find(|s| s.name == "fs").unwrap().enabled);
        assert!(diags.iter().any(|d| d.contains("disabled via")));
    }

    #[test]
    fn parse_disable_list_splits_on_commas_and_whitespace() {
        assert_eq!(
            parse_disable_list("a, b  c,,\td"),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
        assert!(parse_disable_list("   ").is_empty());
    }

    // ---- Discovery helpers -------------------------------------------------

    #[test]
    fn discovery_partitions_enabled_stdio_and_connector_servers() {
        let disco = Discovery {
            servers: vec![
                DiscoveredServer {
                    name: "fs".into(),
                    scope: ServerScope::Project,
                    source: "p".into(),
                    enabled: true,
                    transport: DiscoveredTransport::Stdio {
                        command: "npx".into(),
                        args: vec![],
                        env: vec![],
                    },
                },
                DiscoveredServer {
                    name: "docs".into(),
                    scope: ServerScope::User,
                    source: "u".into(),
                    enabled: true,
                    transport: DiscoveredTransport::Remote {
                        transport: RemoteTransport::Http,
                        url: "https://mcp.example.com".into(),
                        authorization_token: None,
                    },
                },
                DiscoveredServer {
                    name: "off".into(),
                    scope: ServerScope::User,
                    source: "u".into(),
                    enabled: false,
                    transport: DiscoveredTransport::Stdio {
                        command: "nope".into(),
                        args: vec![],
                        env: vec![],
                    },
                },
            ],
            sources: vec!["p".into(), "u".into()],
            diagnostics: vec![],
        };
        // The disabled stdio server is excluded from both partitions.
        assert_eq!(disco.stdio_configs().len(), 1);
        assert_eq!(disco.stdio_configs()[0].name, "fs");
        assert_eq!(disco.connector_servers().len(), 1);
        assert_eq!(disco.connector_servers()[0].name, "docs");
        // The summary never leaks a value and covers all three.
        let summary = disco.summary_lines().join("\n");
        assert!(summary.contains("fs") && summary.contains("docs") && summary.contains("off"));
        assert!(summary.contains("disabled"));
    }

    #[test]
    fn end_to_end_project_over_user_via_temp_dir() {
        // A real file-system discovery: user mcp.json + project .mcp.json, project
        // shadows on a name collision, no real env mutated.
        let root = tempdir();
        let home = root.join("home");
        let proj = root.join("proj").join("sub");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            home.join("mcp.json"),
            r#"{ "mcpServers": {
                "fs":    { "command": "user-fs" },
                "notes": { "command": "user-notes" }
            } }"#,
        )
        .unwrap();
        // Project .mcp.json at proj's parent (walk-up finds it), shadows "fs".
        std::fs::write(
            root.join("proj").join(".mcp.json"),
            r#"{ "mcpServers": { "fs": { "command": "project-fs" } } }"#,
        )
        .unwrap();

        let env = DiscoveryEnv {
            cwd: proj.clone(),
            home: home.clone(),
            xdg_home: None,
            xdg_config_home: None,
            disabled: vec!["notes".to_string()],
        };
        let disco = discover_with(&env, &no_env);

        assert_eq!(disco.sources.len(), 2, "read both user + project files");
        let fs = disco.servers.iter().find(|s| s.name == "fs").unwrap();
        assert_eq!(fs.scope, ServerScope::Project);
        match &fs.transport {
            DiscoveredTransport::Stdio { command, .. } => assert_eq!(command, "project-fs"),
            DiscoveredTransport::Remote { .. } => panic!(),
        }
        // "notes" is force-disabled but retained.
        let notes = disco.servers.iter().find(|s| s.name == "notes").unwrap();
        assert!(!notes.enabled);
        // Only the enabled "fs" is offered for connection.
        assert_eq!(disco.stdio_configs().len(), 1);
        assert_eq!(disco.stdio_configs()[0].command, "project-fs");

        cleanup(&root);
    }

    #[test]
    fn discovery_is_empty_and_clean_when_nothing_is_configured() {
        let root = tempdir();
        let env = DiscoveryEnv {
            cwd: root.join("nowhere"),
            home: root.join("nohome"),
            xdg_home: None,
            xdg_config_home: None,
            disabled: Vec::new(),
        };
        let disco = discover_with(&env, &no_env);
        assert!(disco.servers.is_empty());
        assert!(disco.sources.is_empty());
        assert_eq!(
            disco.summary_lines(),
            vec!["MCP auto-discovery: no servers configured"]
        );
        cleanup(&root);
    }

    // ---- tiny temp-dir helpers (no dev-dep; unique per test via thread id) --

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        // Uniqueness without Date/rand: thread id + a monotonic counter address.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let uniq = format!(
            "aterm-mcp-disco-{:?}-{}",
            std::thread::current().id(),
            N.fetch_add(1, Ordering::Relaxed)
        );
        let dir = base.join(uniq);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }
}
