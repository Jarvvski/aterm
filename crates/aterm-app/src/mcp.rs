//! T-6.3 app wiring: run MCP auto-discovery at session startup, CONNECT the
//! discovered local stdio servers over the T-6.2 client, and collect the remote
//! servers for the T-6.1 connector. The pure discovery + neutral model live in
//! `aterm_agent::mcp::discovery`; this module owns only the process side.
//!
//! Bounded + fail-soft, NOT non-blocking: [`provision`] is awaited synchronously
//! at startup (see [`crate::agent_runtime::AgentRuntime::new`]), so a wedged
//! discovered server delays the window opening by at most one [`CONNECT_TIMEOUT`]
//! (servers connect concurrently), after which it is logged and skipped. A missing
//! binary or a crash surfaces the same way - never a hang, never a panic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aterm_agent::{discover, Discovery, McpServer, McpToolSpec, ProcessTransport, StdioMcpClient};

/// Per-server connect budget (spawn + `initialize` + `tools/list`). A slow or
/// wedged server is abandoned after this, so discovery can never stall startup.
/// Matches Codex's default MCP startup timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The provisioned MCP state, built once at startup and shared (cheaply, by
/// `Arc`) into every agent turn. Empty when nothing is configured - the common
/// case, and a pure passthrough at dispatch time.
#[derive(Default)]
pub struct McpProvision {
    /// Live local stdio clients (T-6.2). Each owns its spawned server child and
    /// kills it on drop, so the fleet is torn down with the runtime.
    pub clients: Vec<Arc<StdioMcpClient<ProcessTransport>>>,
    /// The tools those clients advertise, registered alongside the native set so
    /// the turn loop gates each MCP call to confirmation like a native mutation.
    pub tools: Vec<McpToolSpec>,
    /// Remote servers (T-6.1), each built DENY-BY-DEFAULT; fed to the Anthropic
    /// provider's connector. Inert under the OpenAI/mock providers.
    pub remote: Vec<McpServer>,
}

impl McpProvision {
    /// Whether any server (local or remote) was provisioned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty() && self.remote.is_empty()
    }
}

/// Run auto-discovery rooted at `cwd`, then connect what it found. The pure
/// discovery half (`.mcp.json` project walk-up + the `$HOME`/`$XDG_*`/`~/.claude.json`
/// user fallback chain) is delegated to [`aterm_agent::discover`]; connecting is
/// [`connect`]. MUST be awaited inside a tokio runtime context.
pub async fn provision(cwd: PathBuf) -> McpProvision {
    connect(discover(cwd)).await
}

/// Connect the enabled servers of an already-run [`Discovery`]: spawn + handshake
/// each stdio server (T-6.2) and collect the remote connector servers (T-6.1),
/// wiring each discovered server through the SAME risk gate as a native tool.
/// Split from [`provision`] so a test can inject a HERMETIC discovery (temp dirs,
/// not the developer's real `~/.claude.json`). MUST be awaited inside a tokio
/// runtime context (it spawns server children and awaits their handshake).
pub async fn connect(discovery: Discovery) -> McpProvision {
    // "See" (AC d): one non-secret line per discovered server at startup.
    for line in discovery.summary_lines() {
        log::info!("{line}");
    }
    for note in &discovery.diagnostics {
        log::warn!("mcp discovery: {note}");
    }

    let mut provision = McpProvision {
        remote: discovery.connector_servers(),
        ..McpProvision::default()
    };

    // Connect the stdio servers CONCURRENTLY, each bounded by CONNECT_TIMEOUT so a
    // single dead server costs one timeout, not the sum. Failures are skipped.
    let mut set = tokio::task::JoinSet::new();
    for cfg in discovery.stdio_configs() {
        set.spawn(async move {
            let name = cfg.name.clone();
            match tokio::time::timeout(CONNECT_TIMEOUT, StdioMcpClient::connect(&cfg)).await {
                Ok(Ok((client, tools))) => Some((Arc::new(client), tools, name)),
                Ok(Err(e)) => {
                    log::warn!("mcp: local server `{name}` failed to connect: {e}");
                    None
                }
                Err(_) => {
                    log::warn!(
                        "mcp: local server `{name}` connect timed out after {}s (skipped)",
                        CONNECT_TIMEOUT.as_secs()
                    );
                    None
                }
            }
        });
    }
    let mut connected: Vec<(
        String,
        Arc<StdioMcpClient<ProcessTransport>>,
        Vec<McpToolSpec>,
    )> = Vec::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(Some((client, tools, name))) = joined {
            log::info!(
                "mcp: connected local server `{name}` ({} tool(s))",
                tools.len()
            );
            connected.push((name, client, tools));
        }
    }
    // JoinSet completion order is nondeterministic; sort by server name so tool
    // registration order (and thus which server wins a cross-server tool-name
    // collision at `ToolRegistry::parse`, first-by-name) is deterministic - and
    // WARN on any such collision so a silent misroute never goes unnoticed (HON-4).
    connected.sort_by(|a, b| a.0.cmp(&b.0));
    let mut tool_owner: HashMap<String, String> = HashMap::new();
    for (name, client, tools) in connected {
        for t in &tools {
            if let Some(prev) = tool_owner.insert(t.name.clone(), name.clone()) {
                log::warn!(
                    "mcp: tool `{}` is exposed by both `{prev}` and `{name}`; calls route to \
                     `{prev}` (first by server name)",
                    t.name
                );
            }
        }
        provision.clients.push(client);
        provision.tools.extend(tools);
    }

    if provision.is_empty() {
        log::debug!("mcp: no servers connected");
    } else {
        log::info!(
            "mcp: {} local tool(s) + {} remote server(s) available to the agent",
            provision.tools.len(),
            provision.remote.len()
        );
    }
    provision
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use aterm_agent::{discover_with, DiscoveryEnv};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique temp dir without `Date`/rand (both unavailable / racy).
    fn tempdir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aterm-mcp-provision-{:?}-{}",
            std::thread::current().id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A minimal real stdio MCP server: answers `initialize` (id 1) and
    /// `tools/list` (id 2), exposing one `ping` tool. Written to a temp `.sh`.
    const FAKE_SERVER: &str = r#"while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}' ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"ping","description":"p","inputSchema":{"type":"object"}}]}}' ;;
  esac
done
"#;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_wires_a_real_stdio_server_skips_a_bad_one_and_collects_remotes() {
        // AC(a)/(b)/(d) end-to-end: a discovered stdio server is spawned + handshaken +
        // its tools registered; a sibling whose binary is missing is skipped WITHOUT
        // dropping the good one; a discovered remote server is collected (deny-by-default).
        let root = tempdir();
        let home = root.join("home"); // empty -> no user-level config bleeds in
        let proj = root.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&proj).unwrap();

        let script = proj.join("server.sh");
        std::fs::write(&script, FAKE_SERVER).unwrap();

        std::fs::write(
            proj.join(".mcp.json"),
            format!(
                r#"{{ "mcpServers": {{
                    "good":    {{ "command": "sh", "args": ["{script}"] }},
                    "zbad":    {{ "command": "aterm-nonexistent-cmd-xyz-t63" }},
                    "docsrem": {{ "type": "http", "url": "https://mcp.example.com" }}
                }} }}"#,
                script = script.display()
            ),
        )
        .unwrap();

        // A HERMETIC discovery: temp project cwd + an empty home, so the real
        // ~/.claude.json is never read and no real process is spawned.
        let env = DiscoveryEnv {
            cwd: proj.clone(),
            home,
            xdg_home: None,
            xdg_config_home: None,
            disabled: Vec::new(),
        };
        let discovery = discover_with(&env, &|_| None);
        // Sanity: discovery saw all three (before connect drops the unreachable one).
        assert_eq!(discovery.servers.len(), 3);

        let provision = connect(discovery).await;

        // The good stdio server connected and registered its one tool; the bad one
        // was skipped (not a panic, not a dropped sibling).
        assert_eq!(provision.clients.len(), 1, "only `good` connects");
        assert_eq!(provision.tools.len(), 1);
        assert_eq!(provision.tools[0].name, "ping");
        assert_eq!(provision.tools[0].server, "good");
        // The remote server was collected for the connector, deny-by-default.
        assert_eq!(provision.remote.len(), 1);
        assert_eq!(provision.remote[0].name, "docsrem");
        assert!(!provision.remote[0].tool_policy.default_enabled);
        assert!(!provision.is_empty());

        let _ = std::fs::remove_dir_all(Path::new(&root));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_of_an_empty_discovery_is_an_empty_passthrough() {
        let provision = connect(Discovery::default()).await;
        assert!(provision.is_empty());
        assert!(provision.clients.is_empty());
        assert!(provision.tools.is_empty());
        assert!(provision.remote.is_empty());
    }
}
