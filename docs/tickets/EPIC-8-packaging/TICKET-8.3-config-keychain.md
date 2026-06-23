---
id: T-8.3
epic: EPIC-8-packaging
title: Config load + API-key Keychain custody
status: ready-for-agent
labels: [app, agent, safety]
depends_on: [T-5.6]
---

# Goal

Load aterm config (TOML) and store the provider API key(s) in the macOS Keychain (BYOK) rather than a plaintext config file - the most defensible custody and what satisfies Anthropic's Commercial Terms for BYOK.

# Context

- Research: [06-agent-architecture.md](../../research/06-agent-architecture.md) open-question #3 (API key custody - Keychain recommended); [12-licensing.md](../../research/12-licensing.md) (BYOK with Keychain storage satisfies Anthropic's Commercial Terms). The Secrets source (T-5.6) already treats env/config/Keychain values as sensitive.

# Implementation notes

- Crate: `aterm-app` (config + custody) / `aterm-agent` (consumes the key via the Secrets source).
- Config: a TOML file in aterm's config dir (provider selection, theme, hotkey, autonomy default, MCP servers). Provider default Anthropic; model `claude-opus-4-8`.
- API key: store/retrieve from the macOS Keychain (e.g. `security-framework` / `keyring` crate). The key is fed to the provider client (T-5.2/T-5.3) at construction. Record the Keychain item + aterm's own config path in the Secrets `sensitive_paths` so the gate/sanitizer protect them.
- Fallback order if needed: Keychain (primary) -> env var -> config file (all three treated as SecretAccess by the gate). Default and recommend Keychain.

# Acceptance criteria

- A key stored in the Keychain is retrieved and used by the agent client; no plaintext key on disk by default.
- Config (provider/theme/hotkey/autonomy/MCP) loads and applies.
- The Keychain item + config path are in the Secrets deny-set (gate refuses to read them, sanitizer redacts the value).
- Missing key surfaces a clear setup prompt, not a crash.

# Out of scope

- The Secrets source itself (T-5.6); signing (T-8.4).
