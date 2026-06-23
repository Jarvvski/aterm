---
title: Licensing & Legal Go/No-Go
domain: licensing
status: research
---

# Licensing & Legal Go/No-Go

> Practical licensing analysis for engineers and AI agents building aterm. **Not formal legal advice.** Where this dossier says "get counsel," it means it. Verify license fields with `cargo deny`/`cargo about` in CI, not by trusting this document at build time - upstream crates change their `license` field between versions.

## TL;DR

- **GO overall.** Nothing in the planned stack is a hard blocker for shipping a GPLv3 macOS app, but two items need active handling before they bite: the GPUI render-stack contamination and the AGPL-adjacency of Warp's now-open client.
- **GPUI is a render-stack-defining risk, not a clean Apache-2.0 dep.** GPUI's `Cargo.toml` says `Apache-2.0`, but a default release build statically links GPL-3.0-or-later code via `gpui -> sum_tree -> ztracing -> zlog/ztracing_macro` [1]. For aterm this is *license-compatible* (we are GPLv3 anyway) but it is a landmine for any future relicensing/commercial fork and contradicts GPUI's stated license. **This is the single biggest input to the render-stack decision** and argues for wgpu + a text stack we control over GPUI.
- **wgpu + cosmic-text/swash/parley + alacritty_terminal + portable-pty + winit are all permissive (MIT and/or Apache-2.0)** and impose only notice-retention obligations - fully GPLv3-compatible, no copyleft conflict [2][3][4][5][6][7].
- **Warp is now AGPL-v3 + MIT (warpui), NOT proprietary** (as of the Apr 2026 open-sourcing) [8][9]. The behavior-cloning premise is fine, but the prior threat model ("don't copy proprietary code") is now an *AGPL-contamination* threat: do not read or copy Warp's AGPL source into aterm's GPLv3 codebase. Clean-room the behavior; cite only public docs/screenshots.
- **iM Writing Nerd Font: confirmed bundleable** under SIL OFL 1.1 in a GPLv3 (or commercial) app, provided we ship the OFL text + copyright, do not sell the font standalone, and keep the renamed "iMWriting" primary name (the OFL Reserved Font Names are "iA Writer" and "Plex") [10][11][12].
- **Anthropic API: ship BYOK (user's own API key).** The Commercial Terms forbid using the API "to build a competing product" and forbid reselling without approval; consumer OAuth tokens (Pro/Max) may not power a third-party app [13][14][15]. BYOK with the user's own Console API key is the clean, compliant path.

## Findings

### (a) Cloning Warp's behavior vs its expression

**Legal baseline.** Copyright protects *expression* (specific code, asset files, UI artwork, prose), not *ideas, methods, or UX patterns*. Independently reimplementing a behavior - a controlled UI wrapping a hidden PTY, command "blocks," a unified input that routes to shell-or-agent, an approval-gated agent loop - is permissible. What is not permissible: copying source code, copying asset files (icons, fonts-as-shipped, themes), or using Warp's trademarks/trade dress in a way that implies affiliation.

**The threat model changed in April 2026.** The project prompt frames Warp as proprietary ("we must not copy Warp's code/assets"). That framing is now out of date: Warp open-sourced its terminal **client** on 2026-04-28. The repository `github.com/warpdotdev/Warp` is licensed **AGPL v3 for the bulk of the code, with the `warpui_core` and `warpui` crates under MIT** [8][9]. Oz (the agent), server infrastructure, and Warp Drive remain proprietary/cloud-bound [8].

This flips the practical risk:

- **Old risk:** copying closed-source code you can't see. (Low, because you can't see it.)
- **New risk:** *reading* Warp's AGPL source and letting it influence aterm's code. AGPL-v3 code cannot be combined into aterm's GPLv3 codebase and re-licensed under plain GPLv3 - AGPL adds the network-use/SaaS source-disclosure obligation that GPLv3 lacks. (GPLv3 and AGPLv3 are one-directionally compatible via the §13 bridge, but the result carries AGPL obligations for the AGPL portions; you cannot strip them.) Pulling AGPL snippets into aterm would contaminate aterm with AGPL terms it did not choose.
- **The MIT-licensed `warpui`/`warpui_core` crates are the exception** - those are permissive and could, in principle, be depended on or referenced. But there is no reason to: aterm's iA aesthetic is deliberately different from Warp's, and taking a dependency on Warp's UI crates couples aterm to a competitor's release cadence and trade dress.

**Practical do/don't:**

- DO clean-room: design from public docs (`docs.warp.dev`), marketing pages, screenshots, and your own usage. Document that the behavior spec was written from public sources.
- DO reimplement behaviors/patterns freely (block model, OSC-133 integration, unified input toggle, agent transcript).
- DO use shell-integration standards directly - OSC 133 (FinalTerm/iTerm2 semantic prompts) and OSC 7 (cwd reporting) are open conventions, not Warp IP. The prior prototype's ZDOTDIR shim approach is itself a standard technique.
- DON'T open Warp's AGPL source files and transcribe or paraphrase-into-code from them.
- DON'T copy Warp's name, logo, icon, color identity, or any phrasing that implies "Warp-compatible"/"a Warp clone" in a trademark-confusing way. "Warp" is a Warp Labs trademark; nominative reference ("behaves like Warp") in docs is fine, branding is not.
- DON'T ship any Warp asset (themes, icons, fonts as-distributed-by-Warp).
- Trade dress: the iA-minimal visual language is your defense here - it is materially distinct from Warp's look, which reduces any trade-dress argument to near-zero.

### (b) iM Writing Nerd Font under SIL OFL 1.1

**Confirmed: aterm may bundle, embed, and redistribute the fonts in a GPLv3 (or even commercial) app.** The SIL OFL 1.1 is GPL-compatible and was explicitly designed so fonts can be bundled with software under any license.

The upstream iA fonts ship under **SIL Open Font License Version 1.1 (26 Feb 2007)** with **two Reserved Font Names declared: "iA Writer" (© 2018 Information Architects Inc.) and "Plex" (© 2017 IBM Corp.)** [10]. Verbatim from the iA-Fonts `LICENSE.md` for iA Writer Duo: it is OFL-1.1 with those RFNs [10].

OFL 1.1 permitted uses, with the exact conditions [11][12]:

1. **Bundling/embedding/redistribution/selling-with-software is allowed:** "Original or Modified Versions of the Font Software may be bundled, redistributed and/or sold with any software, provided that each copy contains the above copyright notice and this license." So aterm shipping the font inside the `.app` bundle is fine.
2. **Cannot be sold by itself:** the font may not be sold standalone, only bundled. aterm sells/ships software, not fonts - condition met.
3. **Must ship the OFL text + copyright notice** with each copy. aterm must include the OFL 1.1 license text and the iA + IBM copyright lines in the distributed bundle (e.g. a `licenses/` directory or an in-app acknowledgements screen).
4. **Reserved Font Name rule:** a *Modified Version* may not use "iA Writer" or "Plex" as its name. The Nerd Fonts patch is a **modified version** (it adds glyphs), which is exactly why Nerd Fonts renamed it to **"iMWriting"** to comply [12]. The Nerd Fonts naming convention yields family names like **"iMWriting Mono Nerd Font Mono" (NFM, constant advance), "iMWriting Duo/Quattro Nerd Font Propo (NFP)"** for proportional variants [12]. aterm using "iMWriting"/"iM Writing" as the primary font name is correct and required.

**Subtlety the prompt should note:** aterm bundles the *Nerd Fonts-patched* fonts, which are themselves a Modified Version. The OFL share-alike requires that **the fonts (including the patched ones) remain under OFL** and travel with the OFL text - it does **not** require aterm's own code to be OFL/GPL. Font copyleft and code copyleft are independent. Also: do not invent a new family name that re-uses "iA Writer" or "Plex"; "iMWriting" is already a compliant derivative name, so the simplest path is to ship Nerd Fonts' patched binaries unmodified with their existing names. If aterm re-patches/subsets the fonts itself, it produces a new Modified Version and must (a) keep it OFL, (b) keep the OFL text + copyright, and (c) not use the reserved names - "iMWriting" remains safe to keep.

### (c) GPLv3 compatibility of likely Rust deps

| Crate | License (verify in CI) | GPLv3-compatible? | Notes |
|---|---|---|---|
| **GPUI** | declared `Apache-2.0`; **de facto GPL-3.0-or-later** via static link [1] | Yes (aterm is GPLv3) - but see warning | Default release build statically links GPL-3.0-or-later object code: `gpui -> sum_tree -> ztracing -> zlog + ztracing_macro` [1]. GPL code is no-op at runtime in non-Zed builds; contamination is likely unintentional. Upstream issue open, **no fix as of this research** [1]. |
| **wgpu** | `MIT OR Apache-2.0` | Yes | Permissive; the conventional GPLv3-safe GPU layer. |
| **alacritty_terminal** | `Apache-2.0` [2] | Yes | Apache-2.0 is one-way GPLv3-compatible (Apache code into a GPLv3 project is fine; not vice-versa). v0.26.x at time of research. |
| **portable-pty** (wezterm) | `MIT` [3] | Yes | Cross-platform PTY traits, runtime-selectable backend. Part of wezterm. |
| **winit** | `Apache-2.0` [4] | Yes | Windowing. |
| **cosmic-text** | `MIT OR Apache-2.0` [5] | Yes | Shaping (HarfRust) + layout + rendering (swash). |
| **swash** | `Apache-2.0 OR MIT` [6] | Yes | Font introspection, shaping, glyph rendering. Author: dfrg. |
| **parley** (+ fontique) | `Apache-2.0 OR MIT` [7] | Yes | Alternative/companion text-layout stack (linebender). |

**Verdict on the render stack:** Every non-GPUI option above is permissive and clean for a GPLv3 app. **The GPUI Apache-2.0 label is materially misleading** - a release binary inherits GPL-3.0-or-later source-availability and share-alike obligations [1]. For aterm specifically this is *not a license violation* (aterm is GPLv3, which satisfies those obligations), but it has two consequences worth a deliberate decision:

1. **It removes GPUI's headline advantage.** GPUI's selling point is "build high-performance desktop apps and ship under any license." That promise is currently broken for default builds [1]. If aterm ever wants to dual-license, relicense, or let a downstream fork go permissive/commercial, GPUI's hidden GPL would block it.
2. **It is upstream-fragile.** The contamination depends on Zed's transitive deps; it could be fixed (the proposed fix is swapping `ztracing` for the standard `tracing` crate) or could worsen. aterm would be tracking someone else's accidental copyleft.

Given aterm's hard 60fps requirement, the recommended path is **wgpu + cosmic-text (or parley) + a thin custom UI layer**, which is what the render-stack researcher should weigh on performance grounds - but from a *licensing* standpoint, that combination is unambiguously clean while GPUI carries an asterisk.

**No AGPL/GPL-incompatible core dep was found** in the candidate list. The only AGPL surface is *Warp's own source* (external, do-not-copy), not a dependency.

**CI obligation:** add `cargo deny check licenses` with an allowlist (`MIT`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`, `BSD-*`, `Unicode-*`, `ISC`, `Zlib`, `MPL-2.0` case-by-case) and a denylist that flags any `GPL`/`AGPL`/`*-or-later` in the dependency tree. This is the mechanism that would have caught the GPUI/ztracing chain. Apache-2.0 carries a patent grant + NOTICE-file propagation obligation; retain upstream `NOTICE` files in the distribution.

### (d) Anthropic API ToS for a shipped client app

Default provider is Anthropic Claude; aterm is a full agentic client. Key constraints from Anthropic's Commercial Terms and Usage Policy [13][14][15]:

- **Use the developer API with API keys, not consumer OAuth.** OAuth tokens from Free/Pro/Max plans are "intended exclusively for Claude Code and Claude.ai"; routing a third-party app through them violates the Consumer Terms [15]. aterm must use **Anthropic Console API keys** (or a supported cloud provider).
- **BYOK is the compliant architecture.** Each user supplies their own API key. The Commercial Terms place account responsibility on the "Customer" (key holder) [13]. With BYOK, each user is Anthropic's customer; aterm is just a client. Store the key securely (macOS Keychain; never plaintext, never logged - this dovetails with aterm's single-secrets-source + output-sanitizer design).
- **No "competing product" use.** Commercial Terms §D.4 prohibits using the Services "to build a competing product or service, including to train competing AI models or resell the Services except as expressly approved" [13]. A terminal + agent client that *calls* Claude is not a competing LLM and is the canonical allowed use; do not use API outputs to train a rival model.
- **No reselling without approval.** If aterm ever proxies Claude through aterm-owned keys and bills users, that is reselling and needs explicit Anthropic approval [13]. BYOK sidesteps this entirely.
- **Training on inputs:** Anthropic "may not train models on Customer Content from Services" [13] - a point aterm can surface to users as a privacy reassurance.
- **Usage Policy applies to end output:** aterm should pass through Anthropic's safety/usage constraints; the deterministic code-side risk gate (a "keep" from the prototype) complements but does not replace Anthropic's policy.
- **No mandatory Anthropic branding** in the Commercial Terms, but check Service-Specific Terms and any "Powered by Claude" brand guidelines before using Anthropic marks in-product. Attribution of the model ("uses Claude") in docs is fine; using the Anthropic logo needs adherence to their brand guidelines.

## Recommendations for aterm

1. **Choose wgpu + cosmic-text/swash (or parley) over GPUI for licensing cleanliness.** (Med-High) Rationale: GPUI's real, transitive GPL-3.0 contamination [1] removes its "ship under any license" benefit and makes future relicensing impossible; the permissive stack is unambiguous. Defer the final call to the render-stack researcher on perf grounds, but flag GPUI's license as a real cost, not a footnote.
2. **Treat Warp's repo as AGPL poison for code purposes; clean-room everything.** (High) Rationale: Warp's client is AGPL-v3 since Apr 2026 [8][9]; reading it into aterm risks AGPL contamination of a GPLv3 codebase. Write a one-paragraph "sources" note proving the behavior spec came from public docs.
3. **Ship the Nerd Fonts-patched "iMWriting" binaries unmodified, with the OFL 1.1 text + iA + IBM copyright notices in the bundle.** (High) Rationale: bundling is explicitly OFL-permitted [11][12]; keeping the already-compliant "iMWriting" name avoids the reserved-name problem ("iA Writer"/"Plex") [10]; not re-patching avoids creating a new Modified Version you'd have to manage.
4. **Implement BYOK with macOS Keychain storage; never proxy through aterm-owned keys.** (High) Rationale: cleanest fit with Anthropic's Commercial Terms + Usage Policy [13][15]; avoids reselling/competing-product exposure; aligns with the single-secrets-source design.
5. **Add `cargo deny check licenses` to CI with a GPL/AGPL denylist on transitive deps.** (High) Rationale: this is the mechanism that catches GPUI/ztracing-class surprises before release; license fields drift between crate versions.
6. **Maintain a bundled `THIRD-PARTY-LICENSES`/acknowledgements artifact** (Apache NOTICE files, MIT notices, OFL text). (High) Rationale: Apache-2.0 and OFL both require notice propagation; GPLv3 requires conveying license + source-offer.
7. **Publish aterm under GPLv3 with a clear COPYING + per-file SPDX headers; offer corresponding source.** (High) Rationale: matches the prior prototype and the GPL-compatible dependency set; SPDX headers make `cargo deny`/`cargo about` and downstream audits trivial.

## Risks & unknowns

- **GPUI contamination status is a moving target.** As of this research the upstream issue [1] is open with no fix. If the render-stack researcher prefers GPUI for perf, the team must (a) accept GPLv3 as permanent and irreversible for aterm, and (b) re-check the dep chain on every GPUI bump. Could not verify a fixed release exists.
- **Exact crate license *versions*.** License fields reported here are from crates.io/lib.rs/repo at research time; they can change between versions. `swash` showed `Apache-2.0` in one search and `Apache-2.0 OR MIT` in the authoritative lib.rs/repo listing [6] - treat the dual license as correct but pin and re-verify in CI.
- **Anthropic Service-Specific Terms / brand guidelines** for embedding Claude in a shipped client were not exhaustively read; the competing-product and reselling clauses are clear [13], but branding/attribution specifics need a direct read of current Service-Specific Terms before GA.
- **BYOK + Commercial vs Consumer Terms boundary** is not spelled out for "end user pastes their own Console key into a third-party desktop app." Strongly implied compliant [15], but if aterm ever monetizes, get this confirmed in writing from Anthropic (their own docs recommend contacting them) [13].
- **Trade dress** is a judgment call, not a bright line. The iA-minimal aesthetic makes a Warp trade-dress claim weak, but "judgment call" means a lawyer should glance at the final UI before launch if Warp is named in marketing.
- **AGPL §13 / GPLv3 interaction** is nuanced; the safe rule ("don't copy Warp source") avoids needing to resolve it, but if anyone proposes vendoring Warp's MIT `warpui` crates, route that through counsel first.

## Open questions for the product owner

1. Does aterm want to preserve the *option* to relicense or dual-license later? If yes, GPUI is effectively ruled out and the permissive render stack becomes mandatory, not just recommended.
2. Is aterm strictly BYOK, or will there ever be an aterm-hosted/managed key tier (which triggers Anthropic reselling approval and a very different legal posture)?
3. Will Anthropic Claude be the *only* provider, or pluggable? Multi-provider changes nothing license-wise here but affects which ToS the in-app disclosures must cover.
4. Should aterm ship a built-in "Acknowledgements" UI surface, or a `licenses/` directory in the bundle, or both? (Both is safest for OFL + Apache NOTICE compliance.)
5. Is there appetite to engage counsel for a one-time pre-GA review of (a) the GPLv3/AGPL boundary vs Warp and (b) the Anthropic BYOK monetization question? Both are cheap to resolve early and expensive to unwind late.

## Sources

1. GPUI Apache-2.0 / GPL-3.0 contamination issue - https://github.com/zed-industries/zed/issues/55470
2. alacritty_terminal crate (Apache-2.0) - https://crates.io/crates/alacritty_terminal
3. portable-pty crate (MIT, wezterm) - https://lib.rs/crates/portable-pty
4. winit crate (Apache-2.0) - https://crates.io/crates/winit
5. cosmic-text crate (MIT OR Apache-2.0) - https://github.com/pop-os/cosmic-text/blob/main/Cargo.toml
6. swash crate (Apache-2.0 OR MIT) - https://lib.rs/crates/swash
7. parley crate (Apache-2.0 OR MIT, linebender) - https://github.com/linebender/parley/blob/main/Cargo.toml
8. Warp is now open-source (AGPL client, proprietary Oz/server) - https://www.warp.dev/blog/warp-is-now-open-source
9. Warp repository + license breakdown (AGPL v3 + MIT warpui) - https://github.com/warpdotdev/Warp
10. iA Writer Duo OFL 1.1 LICENSE.md (RFNs: "iA Writer", "Plex") - https://github.com/iaolo/iA-Fonts/blob/master/iA%20Writer%20Duo/LICENSE.md
11. SIL Open Font License 1.1 (bundling/selling/RFN terms) - https://openfontlicense.org/ofl-reserved-font-names/
12. Nerd Fonts iA-Writer patched fonts README ("iMWriting" rename rationale, NFM/NF/NFP naming) - https://github.com/ryanoasis/nerd-fonts/blob/master/patched-fonts/iA-Writer/README.md
13. Anthropic Commercial Terms of Service (competing-product, reselling, no-training-on-customer-content) - https://www.anthropic.com/legal/commercial-terms
14. Anthropic Usage Policy update - https://www.anthropic.com/news/updating-our-usage-policy
15. Anthropic authentication / consumer-OAuth vs API-key + BYOK guidance - https://support.anthropic.com/en/articles/8987200-can-i-use-the-anthropic-api-for-individual-use
