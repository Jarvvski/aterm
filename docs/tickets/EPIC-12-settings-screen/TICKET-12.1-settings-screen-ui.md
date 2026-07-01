---
id: T-12.1
epic: EPIC-12-settings-screen
title: Settings screen UI - typographic preference rows
status: ready-for-agent
labels: [ui, design, settings]
depends_on: [T-9.1]
---

# Goal

Render the Settings ("Preferences") screen exactly as the vision mock shows it: a
calm, typographic list of preference rows separated by `hairline` rules, each row a
label + one-line description on the left and a segmented control (or a font-size
stepper) on the right, closing with a version footer. This is the on-screen surface
only; binding the controls to live config and persisting them is T-12.2.

# Context

- Visual source of record: [`docs/design/vision-mock/AtermWindow.dc.html`](../../design/vision-mock/AtermWindow.dc.html), the `screen="settings"` state - an uppercase "PREFERENCES" label, then `hairline`-separated rows (Theme, Font size, Default provider, Autonomy), then the footer line "aterm <version> - themes stay calm, config stays typographic".
- [ADR-0011](../../adr/0011-vision-mock-north-star.md) adopts the vision mock as the UI north star; a rendered settings screen was not previously scoped in any epic (only T-8.3 config load exists, headless).
- [`07-ia-design-language.md`](../../research/07-ia-design-language.md) - the "config stays calm and typographic" ethos: settings is just more of the same restrained surface, not a distinct chrome-heavy preferences window.
- Consumes the reconciled tokens/palette from [T-9.1](../EPIC-9-vision-reskin/TICKET-9.1-design-token-reconciliation.md) (warm palette, `hairline`, `fg.*` hierarchy, `accent.primary` for the active segment).

# Implementation notes

- Crate: `aterm-ui`. Build the screen from `aterm-tokens` (post-T-9.1). No hardcoded hex.
- Layout per the mock: an uppercase `type.label`/`fg.faint` "PREFERENCES" heading; each preference is a row with a `hairline` top rule, the label in `fg.primary` and its one-line description in `fg.faint`/`type.caption` on the left, the control right-aligned.
- Rows to render (control state is stubbed/local here; real binding is T-12.2):
  - **Theme** - segmented Dark / Light.
  - **Font size** - a stepper: `-` button, current value "N px", `+` button (mock clamps roughly 12-18).
  - **Default provider** - segmented Anthropic / OpenAI / Local (the model backing the agent loop; keep the labels consistent with the locked multi-provider `LlmProvider` seam).
  - **Autonomy** - segmented Ask each time / Auto-run safe / Full auto. The locked default is AUTO-SAFE ON, so "Auto-run safe" is the default-selected segment.
- **Reusable widgets** to factor out (both used again elsewhere): a `segmented control` (the `.seg` treatment - dim `fg.dim` label, `accent.primary` when active, hover to `fg.primary`) and a `stepper` (two hairline-bordered square buttons around a centered value).
- **Version footer**: source the version from the `[workspace.package].version` of record in `Cargo.toml` at build time (e.g. `env!("CARGO_PKG_VERSION")`); do NOT hardcode a version string.
- Respect the 60fps floor: this is a static list; no per-frame allocation, no decorative motion beyond the allowed `.seg` color transition.

# Acceptance criteria

- [ ] The Settings screen renders all four rows + heading + footer to spec in both themes, matching the mock's structure (hairline row rules, left label+description, right control).
- [ ] The segmented control and the font-size stepper are reusable widgets, token-driven, with no hardcoded hex.
- [ ] The active segment uses `accent.primary`; inactive segments use the dim/hover `.seg` treatment.
- [ ] The version footer reflects the `Cargo.toml` version of record automatically (not a literal).
- [ ] Autonomy defaults to the "Auto-run safe" segment (matches the locked AUTO-SAFE default).
- [ ] No frame-budget regression (the T-1.8 no-per-frame-allocation assertion holds).

# Out of scope

- Persisting or live-applying any setting (T-12.2 owns config binding + persistence).
- The shared frame/token re-skin (EPIC-9), the sessions sidebar/title bar (EPIC-10), and the editor surface (EPIC-11).
- Keychain / secret custody mechanics (T-8.3 owns those).
