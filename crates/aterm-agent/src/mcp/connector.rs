//! T-6.1: the Anthropic Messages-API MCP connector - consume REMOTE (public
//! HTTPS) MCP servers where Anthropic brokers the connection and executes the
//! tool calls SERVER-SIDE. The cheapest "consume MCP" path (no client code, no
//! local process); the tradeoffs are that it is limited to tool calls (not MCP
//! prompts/resources), requires public HTTPS (Streamable HTTP or SSE), and is
//! **NOT ZDR-eligible** - data routes through Anthropic. Privacy-sensitive users
//! should prefer the local stdio client (T-6.2), which stays on-device.
//!
//! # Where the gate lives on this path
//!
//! Because the tools run server-side, we cannot pause an individual call mid-turn
//! to confirm it: by the time an `mcp_tool_use` block streams back, the matching
//! `mcp_tool_result` already exists. The gate is therefore applied at REQUEST-
//! BUILD time as a **deny-by-default** per-tool allow/deny policy ([`McpToolPolicy`]),
//! emitted as the toolset's `default_config` + `configs`. A denylisted or
//! unlisted (hence disabled) tool is never offered to the model, so it can never
//! run - the connector analogue of "gated, not silently run". Returned blocks are
//! additionally sanitized before rendering and classified by name
//! ([`classify_mcp_tool`]) so the timeline shows the same risk vocabulary as a
//! native call.
//!
//! The `2025-04-04` connector version is deprecated; we pin
//! [`MCP_CONNECTOR_BETA`] (`mcp-client-2025-11-20`), whose shape references each
//! server by exactly one `mcp_toolset` tool. [`validate_connector_body`] enforces
//! that 1:1 invariant locally so a missing/duplicate toolset becomes an error
//! here instead of a 400 from the API.

use serde_json::{json, Map, Value};

use crate::risk::{Risk, RiskAssessment, RiskReason};

/// The connector beta header value. Pinned; the `2025-04-04` version is
/// deprecated. Sent as `anthropic-beta` whenever a request carries MCP servers.
pub const MCP_CONNECTOR_BETA: &str = "mcp-client-2025-11-20";

/// A remote MCP server consumed via the Anthropic connector. `url` MUST be public
/// HTTPS (Streamable HTTP or SSE); local stdio servers use the client in
/// `mcp::stdio` (T-6.2), NOT this path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    /// The connector-visible server name; referenced 1:1 by its `mcp_toolset`.
    pub name: String,
    /// Public HTTPS endpoint.
    pub url: String,
    /// Optional bearer token the connector forwards to the server (secret custody
    /// is T-8.3; this only holds it).
    pub authorization_token: Option<String>,
    /// Per-tool enable policy (deny-by-default).
    pub tool_policy: McpToolPolicy,
}

impl McpServer {
    /// A server with the safe deny-by-default policy (no tools enabled until you
    /// [`allow`](McpToolPolicy::allow_only) them).
    #[must_use]
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            authorization_token: None,
            tool_policy: McpToolPolicy::default(),
        }
    }

    /// Attach a bearer token the connector forwards to the server.
    #[must_use]
    pub fn with_authorization_token(mut self, token: impl Into<String>) -> Self {
        self.authorization_token = Some(token.into());
        self
    }

    /// Set the per-tool enable policy.
    #[must_use]
    pub fn with_tool_policy(mut self, policy: McpToolPolicy) -> Self {
        self.tool_policy = policy;
        self
    }

    /// The `mcp_servers[]` entry: `{type:"url", url, name, authorization_token?}`.
    #[must_use]
    pub fn server_json(&self) -> Value {
        let mut obj = json!({
            "type": "url",
            "url": self.url,
            "name": self.name,
        });
        if let Some(token) = &self.authorization_token {
            obj["authorization_token"] = json!(token);
        }
        obj
    }

    /// The matching `mcp_toolset` tool entry (exactly one per server), carrying
    /// the deny-by-default allow/deny config so a disabled tool can never run.
    #[must_use]
    pub fn toolset_json(&self) -> Value {
        json!({
            "type": "mcp_toolset",
            "mcp_server_name": self.name,
            "default_config": { "enabled": self.tool_policy.default_enabled },
            "configs": Value::Object(self.tool_policy.configs_json()),
        })
    }
}

/// Per-tool enable policy for a connector toolset. **Deny-by-default** is the safe
/// posture: with `default_enabled = false`, ONLY the tools named in `allow` are
/// enabled, so an unknown or destructive tool can never be invoked server-side.
/// `deny` always wins over `allow`. Set `default_enabled = true` only when you
/// trust every tool the server exposes (discouraged - it enables tools you have
/// not seen).
///
/// The derived [`Default`] is the safe posture: `default_enabled = false`, empty
/// allow/deny - i.e. no tool is enabled until you [`allow`](McpToolPolicy::allow_only) it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolPolicy {
    /// The `default_config.enabled` value. `false` (the default) denies every
    /// tool not explicitly allowed.
    pub default_enabled: bool,
    /// Tools to force-enable (meaningful under deny-by-default).
    pub allow: Vec<String>,
    /// Tools to force-disable. Always wins over `allow`.
    pub deny: Vec<String>,
}

impl McpToolPolicy {
    /// Deny-by-default, enabling exactly the named tools. The recommended way to
    /// scope a connector server.
    #[must_use]
    pub fn allow_only(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            default_enabled: false,
            allow: tools.into_iter().map(Into::into).collect(),
            deny: Vec::new(),
        }
    }

    /// Force-disable the named tools (in addition to any existing deny list).
    #[must_use]
    pub fn deny(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.deny.extend(tools.into_iter().map(Into::into));
        self
    }

    /// Whether a specific tool name is enabled under this policy. `deny` wins.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        if self.deny.iter().any(|d| d == name) {
            return false;
        }
        if self.default_enabled {
            true
        } else {
            self.allow.iter().any(|a| a == name)
        }
    }

    /// The `configs` map: one explicit `{enabled}` entry per tool the policy names
    /// (allow ∪ deny), so a denied tool is provably disabled in the request body.
    #[must_use]
    fn configs_json(&self) -> Map<String, Value> {
        let mut names: Vec<&String> = self.allow.iter().chain(self.deny.iter()).collect();
        names.sort();
        names.dedup();
        names
            .into_iter()
            .map(|n| (n.clone(), json!({ "enabled": self.is_enabled(n) })))
            .collect()
    }
}

/// A best-effort risk classification of a REMOTE MCP tool, by NAME only (we
/// cannot see its argv). Used purely to render the same risk vocabulary in the
/// timeline as a native call; it does NOT change what runs (the allow/deny policy
/// does). Over-approximated to a Caution baseline with [`RiskReason::McpTool`] -
/// a remote tool's local effects are unverifiable, so it can never read as
/// plainly Safe.
#[must_use]
pub fn classify_mcp_tool(_name: &str) -> RiskAssessment {
    RiskAssessment {
        level: Risk::Caution,
        reasons: vec![RiskReason::McpTool],
    }
}

/// A connector configuration error, caught BEFORE the request is sent so a
/// malformed toolset becomes a local error rather than a 400 from the API.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum McpConfigError {
    /// A server name was empty.
    #[error("MCP server name must not be empty")]
    EmptyName,
    /// A server url was not public HTTPS.
    #[error("MCP server `{0}` url must be public HTTPS")]
    NotHttps(String),
    /// Two servers shared a name (their toolsets would be ambiguous).
    #[error("duplicate MCP server name `{0}`")]
    DuplicateName(String),
    /// A declared `mcp_servers` entry had no matching `mcp_toolset` (the request
    /// would 400).
    #[error("MCP server `{0}` has no matching mcp_toolset (would 400)")]
    MissingToolset(String),
    /// An `mcp_toolset` referenced a server not declared in `mcp_servers`.
    #[error("mcp_toolset references unknown MCP server `{0}`")]
    UnknownServerRef(String),
    /// A server had more than one `mcp_toolset`.
    #[error("MCP server `{0}` has more than one mcp_toolset")]
    DuplicateToolset(String),
}

/// Validate a set of connector servers before assembling a request: non-empty
/// unique names and public HTTPS urls. We always generate exactly one toolset per
/// server, so the 1:1 invariant holds by construction; [`validate_connector_body`]
/// re-checks the assembled body as a belt-and-suspenders guard.
///
/// # Errors
/// Returns [`McpConfigError`] on an empty/duplicate name or a non-HTTPS url.
pub fn validate_servers(servers: &[McpServer]) -> Result<(), McpConfigError> {
    let mut seen: Vec<&str> = Vec::new();
    for s in servers {
        if s.name.trim().is_empty() {
            return Err(McpConfigError::EmptyName);
        }
        if !s.url.starts_with("https://") {
            return Err(McpConfigError::NotHttps(s.name.clone()));
        }
        if seen.contains(&s.name.as_str()) {
            return Err(McpConfigError::DuplicateName(s.name.clone()));
        }
        seen.push(&s.name);
    }
    Ok(())
}

/// Assert the 1:1 `mcp_servers` <-> `mcp_toolset` invariant on an ASSEMBLED
/// request body before it is sent: every declared server has exactly one toolset,
/// and every toolset references a declared server. The Messages API 400s
/// otherwise (T-6.1 AC); this turns that into a local, actionable error.
///
/// # Errors
/// Returns [`McpConfigError`] on a missing/duplicate toolset or a toolset that
/// references an undeclared server.
pub fn validate_connector_body(body: &Value) -> Result<(), McpConfigError> {
    let servers: Vec<String> = body
        .get("mcp_servers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("name").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Every `mcp_toolset` tool's `mcp_server_name`, in order (duplicates kept).
    let toolset_refs: Vec<String> = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("mcp_toolset"))
                .filter_map(|t| {
                    t.get("mcp_server_name")
                        .and_then(Value::as_str)
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();

    for server in &servers {
        let count = toolset_refs.iter().filter(|r| *r == server).count();
        if count == 0 {
            return Err(McpConfigError::MissingToolset(server.clone()));
        }
        if count > 1 {
            return Err(McpConfigError::DuplicateToolset(server.clone()));
        }
    }
    for r in &toolset_refs {
        if !servers.contains(r) {
            return Err(McpConfigError::UnknownServerRef(r.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_by_default_enables_only_allowlisted() {
        let policy = McpToolPolicy::allow_only(["search", "fetch"]);
        assert!(policy.is_enabled("search"));
        assert!(policy.is_enabled("fetch"));
        // An unlisted (hence destructive-or-unknown) tool stays disabled.
        assert!(!policy.is_enabled("delete_everything"));
        assert!(!policy.default_enabled);
    }

    #[test]
    fn deny_wins_over_allow() {
        let policy = McpToolPolicy::allow_only(["search", "write_file"]).deny(["write_file"]);
        assert!(policy.is_enabled("search"));
        // Denylisted destructive tool is gated off even though it was allowed.
        assert!(!policy.is_enabled("write_file"));
    }

    #[test]
    fn configs_json_marks_denied_tool_disabled() {
        // T-6.1 AC: a denylisted/destructive MCP tool is gated (disabled), not run.
        let server = McpServer::new("docs", "https://mcp.example.com")
            .with_tool_policy(McpToolPolicy::allow_only(["search"]).deny(["delete"]));
        let toolset = server.toolset_json();
        assert_eq!(toolset["type"], "mcp_toolset");
        assert_eq!(toolset["mcp_server_name"], "docs");
        assert_eq!(toolset["default_config"]["enabled"], false);
        assert_eq!(toolset["configs"]["search"]["enabled"], true);
        assert_eq!(toolset["configs"]["delete"]["enabled"], false);
    }

    #[test]
    fn server_json_shape() {
        let server =
            McpServer::new("docs", "https://mcp.example.com").with_authorization_token("tok-123");
        let s = server.server_json();
        assert_eq!(s["type"], "url");
        assert_eq!(s["url"], "https://mcp.example.com");
        assert_eq!(s["name"], "docs");
        assert_eq!(s["authorization_token"], "tok-123");
    }

    #[test]
    fn server_json_omits_absent_token() {
        let s = McpServer::new("docs", "https://mcp.example.com").server_json();
        assert!(s.get("authorization_token").is_none());
    }

    #[test]
    fn validate_servers_rejects_non_https() {
        let err =
            validate_servers(&[McpServer::new("docs", "http://insecure.example.com")]).unwrap_err();
        assert_eq!(err, McpConfigError::NotHttps("docs".into()));
    }

    #[test]
    fn validate_servers_rejects_empty_and_duplicate_names() {
        assert_eq!(
            validate_servers(&[McpServer::new("", "https://a.example.com")]).unwrap_err(),
            McpConfigError::EmptyName
        );
        assert_eq!(
            validate_servers(&[
                McpServer::new("dup", "https://a.example.com"),
                McpServer::new("dup", "https://b.example.com"),
            ])
            .unwrap_err(),
            McpConfigError::DuplicateName("dup".into())
        );
    }

    #[test]
    fn validate_connector_body_rejects_missing_toolset() {
        // T-6.1 AC: a request missing a toolset for a declared server is rejected
        // before sending (avoid the 400).
        let body = json!({
            "mcp_servers": [{ "type": "url", "url": "https://a.example.com", "name": "docs" }],
            "tools": [],
        });
        assert_eq!(
            validate_connector_body(&body).unwrap_err(),
            McpConfigError::MissingToolset("docs".into())
        );
    }

    #[test]
    fn validate_connector_body_rejects_unknown_ref_and_duplicate() {
        let unknown = json!({
            "mcp_servers": [{ "type": "url", "url": "https://a.example.com", "name": "docs" }],
            "tools": [
                { "type": "mcp_toolset", "mcp_server_name": "docs" },
                { "type": "mcp_toolset", "mcp_server_name": "ghost" },
            ],
        });
        assert_eq!(
            validate_connector_body(&unknown).unwrap_err(),
            McpConfigError::UnknownServerRef("ghost".into())
        );

        let dup = json!({
            "mcp_servers": [{ "type": "url", "url": "https://a.example.com", "name": "docs" }],
            "tools": [
                { "type": "mcp_toolset", "mcp_server_name": "docs" },
                { "type": "mcp_toolset", "mcp_server_name": "docs" },
            ],
        });
        assert_eq!(
            validate_connector_body(&dup).unwrap_err(),
            McpConfigError::DuplicateToolset("docs".into())
        );
    }

    #[test]
    fn validate_connector_body_accepts_matched_pair() {
        let server = McpServer::new("docs", "https://a.example.com")
            .with_tool_policy(McpToolPolicy::allow_only(["search"]));
        let body = json!({
            "mcp_servers": [server.server_json()],
            "tools": [server.toolset_json()],
        });
        assert!(validate_connector_body(&body).is_ok());
    }

    #[test]
    fn classify_is_caution_mcp_tool() {
        let a = classify_mcp_tool("search");
        assert_eq!(a.level, Risk::Caution);
        assert!(a.reasons.contains(&RiskReason::McpTool));
    }
}
