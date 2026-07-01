//! The typed custom tool set (T-5.4): `run_command`, `read_file`, `edit_file`,
//! `write_file`, `list_dir`, `glob`, `grep`.
//!
//! Locked design (see `06-agent-architecture.md` section (b) + Recommendation 5):
//! the agent gets DEDICATED TYPED tools, never a bare bash tool. A typed tool
//! hands the harness structured args it can gate ([`crate::risk`], T-5.5), render
//! and audit (the timeline, T-5.10), and parallelize - which an opaque shell
//! string cannot. In particular [`RunCommand`] carries an argv `Vec<String>`,
//! exec'd with NO shell; there is deliberately no shell-string tool. (Injecting a
//! block into the live interactive shell is a separate, harder-gated sink, T-5.9.)
//!
//! This module defines the CONTRACTS only:
//!
//! - The typed input struct for each tool, with `#[serde(deny_unknown_fields)]`
//!   so the parse mirrors the schema's `additionalProperties: false`.
//! - [`ToolKind`]: the tool discriminant, carrying each tool's stable wire name,
//!   its JSON-Schema `input_schema`, and its [`ToolKind::parallel_safe`] flag (so
//!   the scheduler can fan out the read-only tools and serialize the mutating
//!   ones).
//! - [`ToolRegistry`]: exposes the [`ToolSpec`]s to a provider (T-5.2/T-5.3) and
//!   round-trips a streamed [`ToolCall`] back into the typed [`ToolInput`].
//! - [`ToolDispatch`]: the seam the turn loop (T-5.8) calls; the implementation
//!   (T-5.9) owns the risk gate + sinks + sandbox. Only a test stub lives here.
//!
//! Risk classification (T-5.5), execution (T-5.9), and the sandbox (T-5.7) are
//! out of scope - this ticket is purely the typed surface. Anthropic SERVER-side
//! tools (`web_search`/`web_fetch`) are intentionally NOT modelled here: their
//! declaration is a provider-version-specific wire type (e.g.
//! `web_search_20260209`), not a neutral `input_schema` custom tool, so they are
//! declared by the provider client (T-5.2) rather than this neutral registry.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provider::{ToolCall, ToolSpec};

// ---------------------------------------------------------------------------
// Typed tool inputs. `deny_unknown_fields` mirrors `additionalProperties: false`
// so an extra key is rejected on the round-trip, matching strict tool use.
// ---------------------------------------------------------------------------

/// `run_command` - run an argv (NOT a shell string), exec'd with no shell. NOT
/// parallel-safe (serialized). The classic safety win: structured argv closes
/// the shell-injection channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunCommand {
    /// argv tokens, passed `execvp`-style. Never joined into a shell command.
    pub command: Vec<String>,
    /// Working directory; defaults to the session cwd when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `read_file` - read a file, optionally a `[start, end]` line range. Read-only,
/// parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    pub path: String,
    /// Optional `[start_line, end_line]` (1-indexed, inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[i64; 2]>,
}

/// `edit_file` - exactly-one-match string replacement (the executor, T-5.9, does
/// the staleness check + uniqueness check). NOT parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditFile {
    pub path: String,
    /// Exact text to replace; must match exactly once in the file.
    pub old_str: String,
    /// Replacement text.
    pub new_str: String,
}

/// `write_file` - write (create/overwrite) a file's whole content. NOT
/// parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriteFile {
    pub path: String,
    pub content: String,
}

/// `list_dir` - list a directory. Read-only, parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDir {
    pub path: String,
}

/// `glob` - match a file-name pattern, optionally rooted at `root`. Read-only,
/// parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Glob {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// `grep` - content search, optionally scoped to `path` with `flags`. Read-only,
/// parallel-safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grep {
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<String>,
}

// ---------------------------------------------------------------------------
// Tool discriminant + parsed-input enum.
// ---------------------------------------------------------------------------

/// Which tool a call targets. Carries the stable wire name, the JSON-Schema
/// `input_schema`, and the parallel-safety flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    RunCommand,
    ReadFile,
    EditFile,
    WriteFile,
    ListDir,
    Glob,
    Grep,
}

impl ToolKind {
    /// The default tool set, in advertised order.
    pub const ALL: [ToolKind; 7] = [
        ToolKind::RunCommand,
        ToolKind::ReadFile,
        ToolKind::EditFile,
        ToolKind::WriteFile,
        ToolKind::ListDir,
        ToolKind::Glob,
        ToolKind::Grep,
    ];

    /// The stable wire name the model sees and emits in a `tool_use` block.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            ToolKind::RunCommand => "run_command",
            ToolKind::ReadFile => "read_file",
            ToolKind::EditFile => "edit_file",
            ToolKind::WriteFile => "write_file",
            ToolKind::ListDir => "list_dir",
            ToolKind::Glob => "glob",
            ToolKind::Grep => "grep",
        }
    }

    /// Resolve a wire name back to its kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<ToolKind> {
        ToolKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// Whether the scheduler may run this tool concurrently with others. Only the
    /// read-only tools are parallel-safe; anything that mutates state (run_command,
    /// edit_file, write_file) is serialized.
    #[must_use]
    pub fn parallel_safe(self) -> bool {
        matches!(
            self,
            ToolKind::ReadFile | ToolKind::ListDir | ToolKind::Glob | ToolKind::Grep
        )
    }

    /// One-line tool description sent to the model.
    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            ToolKind::RunCommand => {
                "Run a program. `command` is an argv array (NOT a shell string) executed \
                 directly with no shell. Use separate array elements for each argument."
            }
            ToolKind::ReadFile => {
                "Read a UTF-8 text file. Optionally pass `range` as [start_line, end_line] \
                 (1-indexed, inclusive) to read only part of the file."
            }
            ToolKind::EditFile => {
                "Replace `old_str` with `new_str` in a file. `old_str` must match exactly once."
            }
            ToolKind::WriteFile => "Create or overwrite a file with `content`.",
            ToolKind::ListDir => "List the entries of a directory.",
            ToolKind::Glob => "Find files matching a glob `pattern`, optionally rooted at `root`.",
            ToolKind::Grep => {
                "Search file contents for a regular-expression `pattern`, optionally scoped \
                 to `path` with ripgrep-style `flags`."
            }
        }
    }

    /// The JSON-Schema `input_schema` for this tool. Each is an object schema with
    /// `additionalProperties: false` so strict tool use validates the input
    /// exactly; only genuinely-required fields are in `required`. A provider client
    /// (T-5.2/T-5.3) maps this neutral schema onto its own strict dialect.
    ///
    /// Constraints OUTSIDE the strict structured-output subset are deliberately
    /// omitted - array length (`minItems`/`maxItems`), numeric bounds, string
    /// length/`pattern`/`format` - because a strict request carrying them is
    /// rejected and our hand-rolled clients have no SDK keyword-stripping layer.
    /// The invariants those keywords would express (argv non-empty, `range` is
    /// exactly two integers) are enforced at the parse layer instead (the typed
    /// struct + [`ToolKind::parse_input`]; e.g. `range: Option<[i64; 2]>` rejects a
    /// 1- or 3-element array). The [`tests`] module guards this with
    /// `schemas_use_only_strict_supported_keywords`.
    #[must_use]
    pub fn input_schema(self) -> Value {
        match self {
            ToolKind::RunCommand => json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Non-empty argv tokens, run with no shell (execvp-style)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory; defaults to the session cwd."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            ToolKind::ReadFile => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read." },
                    "range": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "description": "Optional [start_line, end_line], 1-indexed, inclusive (exactly two integers)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            ToolKind::EditFile => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to edit." },
                    "old_str": {
                        "type": "string",
                        "description": "Exact text to replace; must match exactly once."
                    },
                    "new_str": { "type": "string", "description": "Replacement text." }
                },
                "required": ["path", "old_str", "new_str"],
                "additionalProperties": false
            }),
            ToolKind::WriteFile => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to write." },
                    "content": { "type": "string", "description": "Full file content." }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            ToolKind::ListDir => json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list." }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            ToolKind::Glob => json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern, e.g. **/*.rs." },
                    "root": { "type": "string", "description": "Directory to search from." }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            ToolKind::Grep => json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search." },
                    "path": { "type": "string", "description": "File or directory to scope the search." },
                    "flags": { "type": "string", "description": "ripgrep-style flags, e.g. -i." }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        }
    }

    /// The provider-neutral [`ToolSpec`] to advertise. All custom typed tools are
    /// `strict`.
    #[must_use]
    pub fn spec(self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
            strict: true,
        }
    }

    /// Parse a raw `tool_use.input` JSON object into the typed [`ToolInput`] for
    /// this kind. A malformed or extra-field input is a [`ToolError::InvalidInput`],
    /// never a panic.
    pub fn parse_input(self, input: &Value) -> Result<ToolInput, ToolError> {
        let v = input.clone();
        let parsed = match self {
            ToolKind::RunCommand => serde_json::from_value(v).map(ToolInput::RunCommand),
            ToolKind::ReadFile => serde_json::from_value(v).map(ToolInput::ReadFile),
            ToolKind::EditFile => serde_json::from_value(v).map(ToolInput::EditFile),
            ToolKind::WriteFile => serde_json::from_value(v).map(ToolInput::WriteFile),
            ToolKind::ListDir => serde_json::from_value(v).map(ToolInput::ListDir),
            ToolKind::Glob => serde_json::from_value(v).map(ToolInput::Glob),
            ToolKind::Grep => serde_json::from_value(v).map(ToolInput::Grep),
        };
        parsed.map_err(|e| ToolError::InvalidInput {
            tool: self.name(),
            message: e.to_string(),
        })
    }
}

/// A tool exposed by a local stdio MCP server (T-6.2), registered so the turn
/// loop calls it like any other tool. `server` names the server it came from (for
/// dispatch routing); `input_schema` is the server's `inputSchema` (advertised
/// as-is, `strict:false`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolSpec {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A call to a local MCP server tool (T-6.2). Its `args` are opaque JSON we cannot
/// statically classify, so the gate over-approximates it to RequireConfirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolCall {
    pub server: String,
    pub name: String,
    pub args: Value,
}

/// A parsed, typed tool call's input. The variant identifies the tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolInput {
    RunCommand(RunCommand),
    ReadFile(ReadFile),
    EditFile(EditFile),
    WriteFile(WriteFile),
    ListDir(ListDir),
    Glob(Glob),
    Grep(Grep),
    /// A local stdio MCP server tool call (T-6.2). Dispatched by an
    /// [`crate::mcp::stdio::McpToolRouter`], gated like a native mutation.
    Mcp(McpToolCall),
}

impl ToolInput {
    /// The native kind this input belongs to, or `None` for an MCP tool (which has
    /// no native [`ToolKind`]).
    #[must_use]
    pub fn kind(&self) -> Option<ToolKind> {
        Some(match self {
            ToolInput::RunCommand(_) => ToolKind::RunCommand,
            ToolInput::ReadFile(_) => ToolKind::ReadFile,
            ToolInput::EditFile(_) => ToolKind::EditFile,
            ToolInput::WriteFile(_) => ToolKind::WriteFile,
            ToolInput::ListDir(_) => ToolKind::ListDir,
            ToolInput::Glob(_) => ToolKind::Glob,
            ToolInput::Grep(_) => ToolKind::Grep,
            ToolInput::Mcp(_) => return None,
        })
    }

    /// Whether this call may run concurrently with others. An MCP tool's effects
    /// are unknown, so it never runs concurrently (serialized like a mutation).
    #[must_use]
    pub fn parallel_safe(&self) -> bool {
        match self {
            ToolInput::ReadFile(_)
            | ToolInput::ListDir(_)
            | ToolInput::Glob(_)
            | ToolInput::Grep(_) => true,
            ToolInput::RunCommand(_)
            | ToolInput::EditFile(_)
            | ToolInput::WriteFile(_)
            | ToolInput::Mcp(_) => false,
        }
    }
}

/// Why a [`ToolCall`] could not be turned into a typed [`ToolInput`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ToolError {
    /// The model named a tool the registry does not expose.
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    /// The input failed to validate against the tool's schema.
    #[error("invalid input for tool `{tool}`: {message}")]
    InvalidInput { tool: &'static str, message: String },
}

/// The set of tools advertised to the provider + the parse seam. Holds which
/// [`ToolKind`]s are enabled (the default set is all of them); a future config can
/// trim it.
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    kinds: Vec<ToolKind>,
    /// Dynamically-registered local MCP server tools (T-6.2), advertised
    /// alongside the native kinds. An MCP tool whose name collides with a native
    /// tool is shadowed by the native one at parse time (it can never hijack the
    /// gated native path).
    mcp: Vec<McpToolSpec>,
}

impl ToolRegistry {
    /// The full default tool set.
    #[must_use]
    pub fn with_default_tools() -> Self {
        Self {
            kinds: ToolKind::ALL.to_vec(),
            mcp: Vec::new(),
        }
    }

    /// Register local MCP server tools (T-6.2) to advertise alongside the native
    /// set. Chainable. A registered tool whose name matches a native tool is
    /// shadowed by the native one (both advertised once, native wins at parse).
    #[must_use]
    pub fn with_mcp_tools(mut self, specs: Vec<McpToolSpec>) -> Self {
        self.mcp.extend(specs);
        self
    }

    /// The enabled tool kinds, in advertised order.
    #[must_use]
    pub fn kinds(&self) -> &[ToolKind] {
        &self.kinds
    }

    /// The registered local MCP tools, in advertised order.
    #[must_use]
    pub fn mcp_tools(&self) -> &[McpToolSpec] {
        &self.mcp
    }

    /// The [`ToolSpec`]s to advertise to a provider (T-5.2/T-5.3): native kinds
    /// first, then each registered MCP tool (skipping any whose name collides with
    /// a native tool - the native one already covers it and wins at parse).
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self.kinds.iter().map(|k| k.spec()).collect();
        for m in &self.mcp {
            if self.kind_for(&m.name).is_some() {
                continue;
            }
            specs.push(ToolSpec {
                name: m.name.clone(),
                description: m.description.clone(),
                input_schema: m.input_schema.clone(),
                // MCP schemas are not guaranteed to satisfy strict constraints.
                strict: false,
            });
        }
        specs
    }

    /// Resolve an advertised tool name to its native kind.
    #[must_use]
    pub fn kind_for(&self, name: &str) -> Option<ToolKind> {
        self.kinds.iter().copied().find(|k| k.name() == name)
    }

    /// Round-trip a streamed [`ToolCall`] (name + reassembled input JSON) into the
    /// typed [`ToolInput`]. A NATIVE tool name resolves first (an MCP server can
    /// never shadow/hijack a gated native tool); otherwise a registered MCP tool
    /// name maps to [`ToolInput::Mcp`]. Rejects an unknown name and any native
    /// input that does not validate against the tool's schema.
    pub fn parse(&self, call: &ToolCall) -> Result<ToolInput, ToolError> {
        if let Some(kind) = self.kind_for(&call.name) {
            return kind.parse_input(&call.input);
        }
        if let Some(m) = self.mcp.iter().find(|m| m.name == call.name) {
            return Ok(ToolInput::Mcp(McpToolCall {
                server: m.server.clone(),
                name: m.name.clone(),
                args: call.input.clone(),
            }));
        }
        Err(ToolError::UnknownTool(call.name.clone()))
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::with_default_tools()
    }
}

/// The raw outcome of dispatching one tool call. `output` is the unsanitized tool
/// result - the turn loop runs it through [`crate::sanitizer::OutputSanitizer`]
/// before feeding it back to the model or rendering it. `is_error` marks a
/// tool-level failure so the loop can feed back an error result block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
}

impl ToolOutcome {
    /// A successful result.
    #[must_use]
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    /// A tool-level error result (fed back to the model, not a transport error).
    #[must_use]
    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

/// The execution seam the turn loop (T-5.8) drives. An implementation owns the
/// risk gate (T-5.5), the command/file sinks (T-5.9), and the sandbox (T-5.7):
/// it decides whether a proposed call runs, runs it confined, and returns the raw
/// outcome. This ticket defines only the contract; the sole implementation here is
/// a test stub. The turn loop holds a concrete `D: ToolDispatch` (mirroring how it
/// holds a concrete `P: LlmProvider`), so the async-fn-in-trait is not dyn.
#[allow(async_fn_in_trait)]
pub trait ToolDispatch: Send + Sync {
    /// Dispatch one already-parsed, typed tool call. The caller pairs the returned
    /// outcome with the originating `tool_use` id for the timeline join (T-5.10).
    async fn dispatch(&self, input: ToolInput) -> ToolOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: Value) -> ToolCall {
        ToolCall {
            id: "toolu_test".to_string(),
            name: name.to_string(),
            input,
        }
    }

    // ---- schema shape (AC: valid JSON Schema, strict-compatible) ----

    #[test]
    fn every_tool_has_a_strict_object_schema_with_no_additional_properties() {
        for kind in ToolKind::ALL {
            let spec = kind.spec();
            assert!(spec.strict, "{} must advertise strict: true", kind.name());
            assert_eq!(spec.name, kind.name());
            assert!(!spec.description.is_empty());

            let schema = &spec.input_schema;
            assert_eq!(
                schema["type"],
                "object",
                "{} schema must be an object",
                kind.name()
            );
            assert_eq!(
                schema["additionalProperties"],
                false,
                "{} schema must forbid extra properties (strict)",
                kind.name()
            );
            assert!(
                schema["properties"].is_object(),
                "{} schema must list properties",
                kind.name()
            );
            // Every `required` entry must be a declared property.
            let props = schema["properties"].as_object().unwrap();
            for req in schema["required"].as_array().unwrap() {
                let key = req.as_str().unwrap();
                assert!(
                    props.contains_key(key),
                    "{}: required `{key}` is not a declared property",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn schemas_use_only_strict_supported_keywords() {
        // The strict structured-output subset rejects array-length, numeric-bound,
        // and string-length/pattern/format constraints, and our hand-rolled clients
        // (T-5.2/T-5.3) have no SDK layer to strip them - so none may appear in a
        // schema we advertise. NOTE: a key literally named "pattern" whose value is
        // an OBJECT is one of our tool PROPERTIES (glob/grep), not the JSON-Schema
        // `pattern` keyword (which takes a string), so it is allowed.
        const FORBIDDEN: &[&str] = &[
            "minItems",
            "maxItems",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "minLength",
            "maxLength",
            "multipleOf",
            "uniqueItems",
            "minProperties",
            "maxProperties",
            "format",
            "pattern",
        ];

        fn walk(tool: &str, v: &Value) {
            match v {
                Value::Object(map) => {
                    for (k, child) in map {
                        let property_named_pattern = k == "pattern" && child.is_object();
                        assert!(
                            !FORBIDDEN.contains(&k.as_str()) || property_named_pattern,
                            "{tool} schema uses strict-unsupported keyword `{k}`"
                        );
                        walk(tool, child);
                    }
                }
                Value::Array(items) => items.iter().for_each(|i| walk(tool, i)),
                _ => {}
            }
        }

        for kind in ToolKind::ALL {
            walk(kind.name(), &kind.input_schema());
        }
    }

    #[test]
    fn run_command_input_is_an_argv_array_not_a_shell_string() {
        let schema = ToolKind::RunCommand.input_schema();
        assert_eq!(schema["properties"]["command"]["type"], "array");
        assert_eq!(schema["properties"]["command"]["items"]["type"], "string");
        assert_eq!(schema["required"][0], "command");
        // There is no shell-string tool in the set.
        assert!(ToolKind::from_name("bash").is_none());
        assert!(ToolKind::from_name("shell").is_none());
        assert!(ToolKind::from_name("sh").is_none());
    }

    // ---- parallel-safety flags (AC: correct per the table) ----

    #[test]
    fn parallel_safety_flags_match_the_table() {
        // read-only → parallel-safe
        assert!(ToolKind::ReadFile.parallel_safe());
        assert!(ToolKind::ListDir.parallel_safe());
        assert!(ToolKind::Glob.parallel_safe());
        assert!(ToolKind::Grep.parallel_safe());
        // mutating / stateful → serialized
        assert!(!ToolKind::RunCommand.parallel_safe());
        assert!(!ToolKind::EditFile.parallel_safe());
        assert!(!ToolKind::WriteFile.parallel_safe());
    }

    // ---- registry advertises the full set ----

    #[test]
    fn registry_advertises_all_seven_tools() {
        let reg = ToolRegistry::with_default_tools();
        let specs = reg.specs();
        assert_eq!(specs.len(), 7);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "run_command",
                "read_file",
                "edit_file",
                "write_file",
                "list_dir",
                "glob",
                "grep"
            ]
        );
    }

    // ---- round-trip: name + input JSON → typed struct (AC) ----

    #[test]
    fn run_command_round_trips_with_optional_cwd() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call(
                "run_command",
                json!({ "command": ["ls", "-la"], "cwd": "/tmp" }),
            ))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::RunCommand(RunCommand {
                command: vec!["ls".into(), "-la".into()],
                cwd: Some("/tmp".into()),
            })
        );
        assert!(!parsed.parallel_safe());
    }

    #[test]
    fn run_command_round_trips_without_optional_cwd() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call(
                "run_command",
                json!({ "command": ["git", "status"] }),
            ))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::RunCommand(RunCommand {
                command: vec!["git".into(), "status".into()],
                cwd: None,
            })
        );
    }

    #[test]
    fn read_file_round_trips_with_range() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call(
                "read_file",
                json!({ "path": "/etc/hosts", "range": [1, 20] }),
            ))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::ReadFile(ReadFile {
                path: "/etc/hosts".into(),
                range: Some([1, 20]),
            })
        );
        assert!(parsed.parallel_safe());
    }

    #[test]
    fn read_file_range_null_is_none() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call("read_file", json!({ "path": "a", "range": null })))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::ReadFile(ReadFile {
                path: "a".into(),
                range: None,
            })
        );
    }

    #[test]
    fn edit_file_round_trips_all_three_fields() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call(
                "edit_file",
                json!({ "path": "src/lib.rs", "old_str": "a", "new_str": "b" }),
            ))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::EditFile(EditFile {
                path: "src/lib.rs".into(),
                old_str: "a".into(),
                new_str: "b".into(),
            })
        );
    }

    #[test]
    fn grep_round_trips_with_only_required_pattern() {
        let reg = ToolRegistry::default();
        let parsed = reg
            .parse(&call("grep", json!({ "pattern": "TODO" })))
            .unwrap();
        assert_eq!(
            parsed,
            ToolInput::Grep(Grep {
                pattern: "TODO".into(),
                path: None,
                flags: None,
            })
        );
    }

    // ---- error paths ----

    #[test]
    fn unknown_tool_name_is_rejected() {
        let reg = ToolRegistry::default();
        let err = reg
            .parse(&call("bash", json!({ "cmd": "rm -rf /" })))
            .unwrap_err();
        assert_eq!(err, ToolError::UnknownTool("bash".into()));
    }

    #[test]
    fn missing_required_field_is_invalid_input() {
        let reg = ToolRegistry::default();
        // run_command without `command`.
        let err = reg
            .parse(&call("run_command", json!({ "cwd": "/tmp" })))
            .unwrap_err();
        assert!(matches!(
            err,
            ToolError::InvalidInput {
                tool: "run_command",
                ..
            }
        ));
    }

    #[test]
    fn extra_field_is_rejected_matching_additional_properties_false() {
        let reg = ToolRegistry::default();
        // `recursive` is not in the schema; deny_unknown_fields must reject it.
        let err = reg
            .parse(&call(
                "read_file",
                json!({ "path": "a", "recursive": true }),
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            ToolError::InvalidInput {
                tool: "read_file",
                ..
            }
        ));
    }

    #[test]
    fn wrong_typed_command_is_invalid_input() {
        let reg = ToolRegistry::default();
        // `command` must be a string[], not a bare string (no shell-string smuggling).
        let err = reg
            .parse(&call("run_command", json!({ "command": "ls -la" })))
            .unwrap_err();
        assert!(matches!(
            err,
            ToolError::InvalidInput {
                tool: "run_command",
                ..
            }
        ));
    }

    #[test]
    fn read_file_range_must_be_two_elements() {
        let reg = ToolRegistry::default();
        let err = reg
            .parse(&call(
                "read_file",
                json!({ "path": "a", "range": [1, 2, 3] }),
            ))
            .unwrap_err();
        assert!(matches!(
            err,
            ToolError::InvalidInput {
                tool: "read_file",
                ..
            }
        ));
    }

    #[test]
    fn from_name_and_kind_for_agree() {
        let reg = ToolRegistry::default();
        for kind in ToolKind::ALL {
            assert_eq!(ToolKind::from_name(kind.name()), Some(kind));
            assert_eq!(reg.kind_for(kind.name()), Some(kind));
        }
        assert_eq!(ToolKind::from_name("nope"), None);
        assert_eq!(reg.kind_for("nope"), None);
    }

    // ---- dispatch trait is usable by the (future) turn loop ----

    /// A stand-in for the real T-5.9 dispatcher: it records what it was asked to
    /// run and echoes back, proving the turn loop can hold a concrete dispatcher
    /// and await it. The real one injects the gate + sinks.
    struct StubDispatch;

    impl ToolDispatch for StubDispatch {
        async fn dispatch(&self, input: ToolInput) -> ToolOutcome {
            match input {
                ToolInput::RunCommand(rc) => ToolOutcome::ok(rc.command.join(" ")),
                _ => ToolOutcome::error("unsupported in stub"),
            }
        }
    }

    #[tokio::test]
    async fn dispatch_trait_can_be_driven() {
        let reg = ToolRegistry::default();
        let input = reg
            .parse(&call("run_command", json!({ "command": ["echo", "hi"] })))
            .unwrap();
        let dispatcher = StubDispatch;
        let outcome = dispatcher.dispatch(input).await;
        assert_eq!(outcome, ToolOutcome::ok("echo hi"));
    }
}
