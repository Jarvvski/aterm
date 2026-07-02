//! The terminal session: the thin "wire" layer between the headless engine and
//! the UI. It owns the [`aterm_core::Engine`] (the three-thread reader/model
//! pipeline) and the pure [`InputModel`] reducer, and implements
//! [`aterm_ui::UiCallbacks`] so the UI event loop pulls the latest snapshot each
//! frame and pushes keystrokes here, which route to the engine in Shell mode.
//!
//! The engine owns the PTY, the VT terminal model, the OSC scanner, and the block
//! list on its own model thread (ticket T-1.3); this session no longer parses VT
//! bytes on the render thread. The three threads are: (1) the PTY reader thread
//! and (2) the model thread, both inside `aterm-core`'s [`aterm_core::Engine`],
//! and (3) the winit/render main thread that owns this session. The agent turn runs
//! on a fourth executor - the [`AgentRuntime`]'s tokio worker threads (ticket
//! T-5.11) - off the render thread, streaming its steps back through the engine's
//! agent-injector mailbox.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use aterm_agent::{AutonomyState, Secrets};
use aterm_core::{
    keys, BlockList, Engine, IntegrationStatus, PtyDimensions, Snapshot, DEFAULT_SCROLLBACK,
};
use aterm_ui::{
    ImeEvent, KeyPress, NamedKey, OverlayRequest, OverlayWorker, TitleBarView, UiCallbacks, Window,
    DEFAULT_DEBOUNCE,
};

use aterm_core::{HistoryRing, HistoryScope, InputEvent, InputMode, InputModel, Motion, Preedit};

use crate::agent_runtime::{AgentRuntime, TurnHandle};
use crate::config::Config;
use crate::routing::{classify, decide, keystroke_for, Disposition, KeyBinding, RoutingContext};

/// One terminal session.
///
/// **Field order is load-bearing for clean shutdown (ticket T-5.11).** Rust drops
/// struct fields in declaration order, so `agent` (the tokio runtime) and `turn` are
/// declared BEFORE `engine`: an in-flight turn's tokio task owns an
/// [`aterm_core::AgentInjector`] - a clone of the engine's model-mailbox sender - and
/// while that clone is alive the model thread never sees its mailbox disconnect, so
/// [`Engine`]'s drop (which joins the model thread) would hang. Dropping the runtime
/// FIRST cancels that task and releases the injector, so the subsequent `engine` drop
/// observes the disconnect and joins cleanly (preserving aterm-core's zero-hang
/// shutdown invariant). Do NOT move `engine` above `agent`/`turn`.
pub struct Session {
    /// The off-render-thread agent-turn runtime (ticket T-5.11): owns the tokio
    /// executor plus the workspace root and the single [`Secrets`] source a turn is
    /// gated and sandboxed against. Declared first so it (and its injector-holding
    /// tasks) drop BEFORE `engine` - see the struct-level note.
    agent: AgentRuntime,
    /// The in-flight agent turn, if one is running (ticket T-5.11). `Some` while a turn
    /// streams its steps into the timeline; the handle bridges the keyboard to the
    /// loop's approval/cancel seam and tells the router whether Esc should interrupt.
    turn: Option<TurnHandle>,
    engine: Engine,
    input: InputModel,
    window: Option<Arc<Window>>,
    /// The configured mode-toggle hotkey (ticket T-3.3), consulted by
    /// [`crate::routing::classify`] each keystroke. Default `Cmd-/`.
    toggle_key: KeyBinding,
    /// The configured autonomy-cycle hotkey (ticket T-5.11). Default `Cmd-Shift-A`.
    autonomy_key: KeyBinding,
    /// The live, session-scoped autonomy posture (ticket T-5.11). Constructed fresh
    /// per session at the configured baseline, so a runtime widening never carries
    /// into a new session (AC5); the cycle hotkey mutates it (AC4), the always-visible
    /// indicator reads it, and its `policy()` gates each live turn (ticket T-5.11).
    autonomy: AutonomyState,
    /// The shared input-history ring (ticket T-3.7): every submitted line is pushed here
    /// (tagged with the mode it was submitted in), and the T-3.5 ghost-text worker draws
    /// its fish-style suggestions from it. `Arc` so a snapshot is a cheap refcount bump
    /// into each overlay request; `Arc::make_mut` copies-on-write only when the worker is
    /// mid-read. In-memory only (persistence is T-8.3), so it starts empty each session.
    history: Arc<HistoryRing>,
    /// The async, debounced highlight + ghost-text worker (ticket T-3.5). Runs off the
    /// render thread; the input line's overlay is recomputed here after each edit and the
    /// last-good result applied to `input` in [`Self::apply_overlay`], so the render path
    /// never blocks on the highlighter.
    overlay: OverlayWorker,
    /// Monotonic id bumped per overlay request so the freshest result is identifiable
    /// (ticket T-3.5); echoed back on each `OverlayResult`.
    overlay_gen: u64,
    /// Whether the history lens is widened to "all" (both Shell + Agent) for ghost
    /// suggestions (ticket T-3.7 `HistoryScope`). Default off (per-mode lens); no runtime
    /// toggle yet (a later setting).
    widen_history: bool,
    /// The sidebar-toggle hotkey (ticket T-9.2), consulted before the routing brain like
    /// the autonomy-cycle hotkey. Default `Cmd-B`.
    sidebar_key: KeyBinding,
    /// The toggle-sidebar INTENT (ticket T-9.2). The sessions sidebar panel is EPIC-10;
    /// today this bool is the intent the `◧` title-bar glyph / `Cmd-B` flips, which the
    /// panel will consume. Default `false` (ADR-0011: not shown by default on one session).
    sidebar_open: bool,
    /// The active title shown centered in the custom title bar (ticket T-9.2). A
    /// placeholder ("aterm") until EPIC-10 replaces it with the active session name.
    title: String,
    /// The current working directory shown beside the title (home abbreviated to `~`).
    /// Sourced from the process cwd at spawn; OSC-7-driven live updates are a follow-up
    /// (EPIC-10 owns the title-bar/session binding).
    cwd_display: String,
}

/// Apply a T-3.2 IME event to the input model. Pure (no engine / no window), so the
/// composition semantics are unit-testable without spawning a session:
///
/// - `Enabled` - composition may begin; the preedit arrives in following events.
/// - `Preedit` - set the transient composition overlay. winit sends an EMPTY preedit to
///   mean "cleared" (it precedes every `Commit`), so an empty string clears it.
/// - `Commit` - insert the final text as inert characters and clear the preedit (one
///   undo unit; replaces any selection).
/// - `Disabled` - the IME was turned off / focus was lost; drop any dangling preedit.
fn apply_ime(input: &mut InputModel, event: ImeEvent) {
    match event {
        ImeEvent::Enabled => {}
        ImeEvent::Preedit { text, cursor } => {
            if text.is_empty() {
                input.set_preedit(None);
            } else {
                input.set_preedit(Some(Preedit { text, cursor }));
            }
        }
        ImeEvent::Commit(text) => input.commit_ime(&text),
        ImeEvent::Disabled => input.set_preedit(None),
    }
}

/// Render a filesystem path for the title bar, abbreviating a `$HOME` prefix to `~` (the
/// conventional shell display). Falls back to the lossy string form for a non-UTF-8 path.
fn abbreviate_home(path: &std::path::Path) -> String {
    let full = path.to_string_lossy();
    if let Some(home) = std::env::var_os("HOME") {
        let home = std::path::Path::new(&home);
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest.to_string_lossy());
        }
    }
    full.into_owned()
}

/// Map the agent's autonomy tier onto the UI-local indicator enum. The crate boundary
/// keeps `aterm-ui` free of `aterm-agent`, so the app does this translation (ticket
/// T-5.11), exactly as it maps `InputMode` onto the routing chip's `PromptMode`.
fn ui_autonomy(mode: aterm_agent::AutonomyMode) -> aterm_ui::AutonomyMode {
    match mode {
        aterm_agent::AutonomyMode::AskAlways => aterm_ui::AutonomyMode::AskAlways,
        aterm_agent::AutonomyMode::AutoSafe => aterm_ui::AutonomyMode::AutoSafe,
        aterm_agent::AutonomyMode::AutoRunInSession => aterm_ui::AutonomyMode::AutoRunInSession,
    }
}

impl Session {
    /// Spawn a login shell over the three-thread engine and build the session with
    /// the configured hotkeys and the baseline autonomy posture. A fresh session
    /// always starts at `cfg.default_autonomy` (the AUTO-SAFE baseline), so a prior
    /// session's runtime widening never carries over (ticket T-5.11 AC5).
    pub fn spawn(cfg: &Config) -> Result<Self, aterm_core::PtyError> {
        let dims = PtyDimensions {
            cols: cfg.initial_cols,
            rows: cfg.initial_rows,
            pixel_width: 0,
            pixel_height: 0,
        };
        let engine = Engine::spawn_login_shell(dims, DEFAULT_SCROLLBACK)?;
        // The agent works in - and confines its writes to - the current working
        // directory; the single Secrets deny-set feeds the gate, the sandbox, and the
        // sanitizer alike (ticket T-5.11; key custody is T-8.3, out of scope).
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Capture the cwd display (home abbreviated to `~`) for the title bar BEFORE `root`
        // is moved into the agent runtime (which confines the agent's writes to it).
        let cwd_display = abbreviate_home(&root);
        let agent = AgentRuntime::new(root, Secrets::new()).map_err(aterm_core::PtyError::Io)?;
        Ok(Self {
            engine,
            input: InputModel::new(),
            window: None,
            toggle_key: cfg.toggle_mode,
            autonomy_key: cfg.autonomy_cycle,
            autonomy: AutonomyState::new(cfg.default_autonomy),
            agent,
            turn: None,
            history: Arc::new(HistoryRing::new()),
            overlay: OverlayWorker::new(DEFAULT_DEBOUNCE),
            overlay_gen: 0,
            widen_history: false,
            sidebar_key: cfg.toggle_sidebar,
            sidebar_open: false,
            title: "aterm".to_string(),
            cwd_display,
        })
    }

    /// Number of command blocks segmented so far (used by tests / status line).
    pub fn block_count(&self) -> usize {
        self.engine.block_count()
    }

    /// Flip the toggle-sidebar INTENT (ticket T-9.2) and return the new state. The
    /// sessions sidebar panel is EPIC-10; today this is the intent path the `Cmd-B` hotkey
    /// drives (and that the `◧` title-bar glyph will drive once mouse hit-testing lands).
    /// EPIC-10 reads `sidebar_open` to show/hide the panel. Each call flips the bool.
    pub fn toggle_sidebar(&mut self) -> bool {
        self.sidebar_open = !self.sidebar_open;
        log::debug!(
            "sidebar -> {}",
            if self.sidebar_open { "open" } else { "closed" }
        );
        self.sidebar_open
    }

    /// Drain the VT engine's window events so its channel does not grow. Most are
    /// surfaced for later wiring (title -> window title); for now we log and
    /// otherwise discard them. (DA/DSR/CPR replies are no longer here - the engine
    /// writes them straight back to the PTY on the model thread; ticket T-1.9.)
    fn drain_terminal_events(&mut self) {
        use aterm_core::TerminalEvent;
        while let Ok(event) = self.engine.terminal_events().try_recv() {
            match event {
                TerminalEvent::Title(title) => log::debug!("title: {title}"),
                TerminalEvent::Bell => log::debug!("bell"),
                other => log::trace!("terminal event: {other:?}"),
            }
        }
    }

    /// Build the routing context from live state. `degraded` (integration `None`),
    /// `alt_screen`, `foreground_reading_stdin` (a foreground process group other than
    /// the hidden shell owns the terminal), `agent_turn_active` (a live agent turn is
    /// running, ticket T-5.11), and `preedit_active` (an IME composition is in progress,
    /// ticket T-3.2 - so Enter confirms the candidate and never submits) are all sourced
    /// live.
    fn routing_context(&mut self) -> RoutingContext {
        let degraded = matches!(
            self.engine.integration_status().status,
            IntegrationStatus::None
        );
        let alt_screen = self.engine.latest_snapshot().alt_screen;
        RoutingContext {
            mode: self.input.mode(),
            preedit_active: self.input.preedit().is_some(),
            degraded,
            alt_screen,
            foreground_reading_stdin: self.engine.foreground_is_foreign(),
            agent_turn_active: self.turn.as_ref().is_some_and(TurnHandle::is_active),
        }
    }

    /// Apply an editing key to the [`InputModel`], reporting an [`EditOutcome`]:
    /// `erased` (a backspace actually removed a char, so the Shell-echo mirror only
    /// sends DEL when something was deleted) and `text_changed` (the buffer TEXT changed,
    /// as opposed to a pure caret motion). Enter/Tab/Escape never reach here (the routing
    /// brain dispatches them). `text_changed` gates the async overlay recompute: a pure
    /// caret motion cannot change the highlight or the ghost prefix, so it must NOT fire a
    /// request - otherwise a held motion key (OS key-repeat) would keep resetting the
    /// debounce and starve a pending recompute (review finding).
    fn apply_edit_key(&mut self, named: Option<NamedKey>, text: Option<&str>) -> EditOutcome {
        match named {
            Some(NamedKey::Backspace) => {
                let erases = self.input.caret() > 0 || !self.input.selection().is_empty();
                self.input.reduce(InputEvent::Backspace);
                // Backspace changes the text iff it actually erased something.
                EditOutcome {
                    erased: erases,
                    text_changed: erases,
                }
            }
            Some(NamedKey::Delete) => {
                // Delete is a no-op only at end-of-buffer with no selection.
                let deletes = !self.input.selection().is_empty()
                    || self.input.caret() < self.input.text().chars().count();
                self.input.reduce(InputEvent::Delete);
                EditOutcome::changed(deletes)
            }
            Some(NamedKey::ArrowLeft) => {
                self.input.reduce(InputEvent::Move(Motion::Left, false));
                EditOutcome::motion()
            }
            Some(NamedKey::ArrowRight) => {
                // zsh-autosuggestions semantics (ticket T-3.5): Right at end-of-line
                // accepts the ghost tail (a TEXT change); otherwise it is a plain caret
                // move. `accept_ghost` is a no-op unless the caret is at the end with a
                // live suggestion, so this also correctly no-ops when Right at the end has
                // nothing to accept.
                let accepted = self.input.accept_ghost();
                if !accepted {
                    self.input.reduce(InputEvent::Move(Motion::Right, false));
                }
                EditOutcome::changed(accepted)
            }
            Some(NamedKey::ArrowUp) => {
                self.input.reduce(InputEvent::Move(Motion::Up, false));
                EditOutcome::motion()
            }
            Some(NamedKey::ArrowDown) => {
                self.input.reduce(InputEvent::Move(Motion::Down, false));
                EditOutcome::motion()
            }
            Some(NamedKey::Home) => {
                self.input.reduce(InputEvent::Move(Motion::Home, false));
                EditOutcome::motion()
            }
            Some(NamedKey::End) => {
                // End at end-of-line accepts the ghost (T-3.5, a TEXT change); else it is
                // a plain end-of-line motion (see the `ArrowRight` note).
                let accepted = self.input.accept_ghost();
                if !accepted {
                    self.input.reduce(InputEvent::Move(Motion::End, false));
                }
                EditOutcome::changed(accepted)
            }
            Some(NamedKey::Space) => {
                self.input.reduce(InputEvent::Insert(" ".to_string()));
                EditOutcome::changed(true)
            }
            // Tab requests shell completion: it acts on the PTY (the shell's own
            // completer), not the input line, so the model is left untouched here -
            // the `\t` byte is sent by `raw_key_bytes` in the Shell-mode mirror.
            // Freed from the toggle now that `Cmd-/` is the real chord (T-3.3).
            Some(NamedKey::Tab) => EditOutcome::motion(),
            // Esc with no agent turn (and integrated, not alt-screen) is ordinary
            // input; the input box has no Esc action yet, so it is a no-op.
            Some(NamedKey::Escape) => EditOutcome::motion(),
            _ => {
                let inserted = text.filter(|t| !t.is_empty());
                if let Some(t) = inserted {
                    self.input.reduce(InputEvent::Insert(t.to_string()));
                }
                EditOutcome::changed(inserted.is_some())
            }
        }
    }

    /// The bytes a key sends to the PTY for the **Shell-mode prompt-echo mirror**
    /// only (the raw passthrough path now encodes via `routing::keystroke_for` +
    /// `keys::encode`). `erased` gates Backspace -> DEL so an empty input box does
    /// not echo a stray DEL. Arrows/Delete/Home/End send nothing here on purpose:
    /// at the prompt those move the `InputModel` caret, not the shell's line - the
    /// shell-echo mirror is a deliberately minimal cooked stand-in until the T-3.6
    /// widget is the source of truth.
    fn raw_key_bytes(named: Option<NamedKey>, text: Option<&str>, erased: bool) -> Option<Vec<u8>> {
        match named {
            Some(NamedKey::Enter) => Some(b"\r".to_vec()),
            Some(NamedKey::Backspace) => erased.then(|| vec![0x7f]),
            Some(NamedKey::Space) => Some(b" ".to_vec()),
            Some(NamedKey::Tab) => Some(b"\t".to_vec()),
            Some(
                NamedKey::Delete
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::Home
                | NamedKey::End,
            ) => None,
            _ => text
                .filter(|t| !t.is_empty())
                .map(|t| t.as_bytes().to_vec()),
        }
    }

    /// Queue an async recompute of the input overlay (highlight + ghost) for the current
    /// line (ticket T-3.5). `immediate` short-circuits the worker's debounce for instant
    /// feedback (space / paste / mode toggle / IME commit / ghost accept). This never
    /// computes here - it sends a single request to the off-thread worker, so the key
    /// path stays free of the highlighter.
    fn request_overlay(&mut self, immediate: bool) {
        self.overlay_gen = self.overlay_gen.wrapping_add(1);
        let mode = self.input.mode();
        self.overlay.request(OverlayRequest {
            generation: self.overlay_gen,
            text: self.input.text().to_string(),
            mode,
            scope: HistoryScope::for_mode(mode, self.widen_history),
            history: Arc::clone(&self.history),
            immediate,
        });
    }

    /// Drain the overlay worker and apply the freshest result to the input model (ticket
    /// T-3.5), called each wake via [`UiCallbacks::tick`]. The model self-guards
    /// staleness - [`InputModel::ghost_tail`] re-derives the tail against the live text,
    /// and the highlight self-corrects on the next result - so applying the latest
    /// available overlay is always safe even if the buffer advanced since the request.
    fn apply_overlay(&mut self) {
        if let Some(res) = self.overlay.poll() {
            self.input.set_highlight(res.highlight);
            self.input.set_ghost(res.ghost);
        }
    }

    /// Record a submitted line in the shared history ring (ticket T-3.7), tagged with the
    /// mode it was submitted in, so the T-3.5 ghost worker can suggest it later. The ring
    /// drops blank lines itself. Copy-on-write via `Arc::make_mut`: it clones the ring
    /// only if the worker is mid-read of a prior snapshot.
    fn record_history(&mut self, line: &str, mode: InputMode) {
        Arc::make_mut(&mut self.history).push(line, mode, SystemTime::now());
    }
}

/// The result of applying one editing key ([`Session::apply_edit_key`]).
#[derive(Debug, Clone, Copy)]
struct EditOutcome {
    /// A backspace actually removed a character (gates the Shell-echo DEL).
    erased: bool,
    /// The buffer TEXT changed (insert / delete / ghost-accept), as opposed to a pure
    /// caret motion. Gates the async overlay recompute (T-3.5) so motion keys never fire.
    text_changed: bool,
}

impl EditOutcome {
    /// A pure caret motion: nothing erased, no text change.
    fn motion() -> Self {
        Self {
            erased: false,
            text_changed: false,
        }
    }

    /// A non-backspace edit that changed the text iff `changed`.
    fn changed(changed: bool) -> Self {
        Self {
            erased: false,
            text_changed: changed,
        }
    }
}

/// Whether an editing key warrants an IMMEDIATE overlay recompute (short-circuiting the
/// T-3.5 debounce) rather than the debounced default (ticket T-3.5 AC1). Space and paste
/// (a multi-char insertion) want instant feedback; accepting a ghost (`Right`/`End` at
/// end of line) changes the buffer and should reflect at once. Ordinary single-char
/// typing, backspace, and caret motion stay debounced.
fn edit_is_immediate(named: Option<NamedKey>, text: Option<&str>) -> bool {
    matches!(
        named,
        Some(NamedKey::Space | NamedKey::ArrowRight | NamedKey::End)
    ) || (named.is_none() && text.is_some_and(|t| t.chars().count() > 1))
}

impl Drop for Session {
    fn drop(&mut self) {
        // Tear the agent turn down BEFORE the engine's model thread is joined (ticket
        // T-5.11). A live turn's tokio task holds an `AgentInjector` - a clone of the
        // engine's model-mailbox sender - and while it is alive `Engine::drop`'s
        // `model.join()` would wait forever on a mailbox disconnect that can never
        // come, hanging the winit thread. Cancel the turn (fail-closed denies any
        // parked approval and unblocks the loop), then shut the runtime down (bounded),
        // which drops the task and releases the injector. The subsequent field-drop of
        // `engine` then observes the disconnect and joins cleanly. (Field order -
        // `agent`/`turn` before `engine` - is a backstop; this is the explicit teardown.)
        if let Some(turn) = self.turn.take() {
            turn.cancel();
        }
        self.agent.shutdown();
    }
}

impl UiCallbacks for Session {
    fn on_ready(&mut self, window: Arc<Window>) {
        self.window = Some(window);
    }

    fn snapshot_version(&mut self) -> u64 {
        // Cheap: an Arc clone under a short lock, then a field read. The pacing
        // loop calls this every wake to detect new output before deciding whether
        // to pay for the full grid clone in `snapshot`.
        self.engine.latest_snapshot().version
    }

    fn snapshot(&mut self) -> Option<Arc<Snapshot>> {
        self.drain_terminal_events();
        // The engine's model thread owns the parse loop; here we just hand the
        // renderer the latest published snapshot as a cheap `Arc` clone (a refcount
        // bump under a short lock) - NO per-frame deep copy of the grid. This is
        // the consumer side of the engine's zero-alloc publish (ticket T-1.5 AC5).
        Some(self.engine.latest_snapshot())
    }

    fn blocks(&mut self) -> Option<Arc<BlockList>> {
        // The live, virtualized timeline's data (ticket T-2.7): the model thread
        // publishes the block list, here handed to the renderer as a cheap `Arc`
        // clone (a refcount bump under a short lock - NO per-frame deep copy), the
        // consumer side of the model thread's block publish.
        Some(self.engine.latest_blocks())
    }

    fn integration_status(&mut self) -> aterm_core::Integration {
        // The live three-state shell-integration indicator (ticket T-2.6): a cheap
        // atomic load the engine's model thread keeps current. The renderer maps it
        // to a glyph + "why?" tooltip.
        self.engine.integration_status()
    }

    fn input(&self) -> Option<&InputModel> {
        // The unified-input box (ticket T-3.6) reads the live buffer this Session owns
        // and drives in `on_key` (the reducer mutations + the mode toggle). Borrowed, not
        // cloned - the renderer only reads it; this finally makes the in-progress line
        // (and, in Agent mode, the previously feedback-less prompt) visible on screen.
        Some(&self.input)
    }

    fn autonomy_mode(&self) -> Option<aterm_ui::AutonomyMode> {
        // The always-visible autonomy indicator (ticket T-5.11 AC4): the renderer draws
        // the live session posture as a chip beside the routing chip. Mapped onto the
        // UI-local enum so `aterm-ui` never names an `aterm-agent` type.
        Some(ui_autonomy(self.autonomy.mode()))
    }

    fn title_bar(&self) -> Option<TitleBarView<'_>> {
        // The custom title bar (ticket T-9.2): the active title + cwd, drawn over the
        // reserved top band. Borrowed (not cloned) - the renderer only reads it. EPIC-10
        // replaces `title` with the active session name and makes the `◧` glyph toggle the
        // sidebar panel this session's `sidebar_open` intent already tracks.
        Some(TitleBarView {
            title: &self.title,
            cwd: &self.cwd_display,
        })
    }

    fn on_key(&mut self, key: KeyPress<'_>) -> Option<Vec<u8>> {
        // T-3.3 routing brain. The pure `InputModel` reducer (T-3.1) owns the
        // in-progress line; this layer is the caller that decides where a key goes
        // (the caller-owns-submit contract). `classify` maps the modifier-carrying
        // `KeyPress` to a neutral `KeyInput` (the real `Cmd-/` toggle / `Opt-Enter`),
        // then `decide` applies the disposition gates; we perform the result here.
        // INTERACTIVITY: until the T-3.6 widget is the source of truth, Shell-mode
        // editing keys are still mirrored raw to the PTY so the shell's own line
        // editor echoes them - the byte stream stays what the prior scaffold sent.

        // The autonomy-cycle hotkey (T-5.11) is a SESSION-posture flip, checked before
        // the routing brain - parallel to how the mode toggle flips routing only. It
        // steps the safety tier (taking effect on the next gate decision, AC4) and
        // sends no PTY bytes.
        if self.autonomy_key.matches(&key) {
            self.autonomy.cycle();
            log::info!("autonomy -> {}", self.autonomy.mode().label());
            return None;
        }

        // The sidebar-toggle hotkey (T-9.2) flips the toggle-sidebar intent, also before
        // the routing brain (it changes chrome, never routing or the line). The sidebar
        // panel itself is EPIC-10; this drives the same intent the `◧` glyph will once a
        // pointer path exists. Sends no PTY bytes.
        if self.sidebar_key.matches(&key) {
            self.toggle_sidebar();
            return None;
        }

        // While the agent is parked on an approval (T-5.11), the keyboard ANSWERS it
        // instead of editing the line: Enter / `y` approve (the gated call runs), `n`
        // deny (it is fed back as an error result and the turn continues), Esc cancels
        // the whole turn. Every other key is swallowed so a pending safety decision can
        // never be bypassed by stray input. This IS the click/Esc seam the fail-closed
        // channel-backed `ConfirmHandler` was built for - resolved on the winit thread.
        if let Some(turn) = self.turn.as_ref() {
            if turn.is_active() && turn.has_pending_approval() {
                if matches!(key.named, Some(NamedKey::Escape)) {
                    turn.cancel();
                } else if matches!(key.named, Some(NamedKey::Enter))
                    || key.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'y'))
                {
                    turn.approve_pending();
                } else if key.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'n')) {
                    turn.deny_pending();
                }
                return None;
            }
        }

        let ctx = self.routing_context();
        let bytes = match decide(classify(&key, &self.toggle_key), &ctx) {
            // The IME owns the key while a composition is active (T-3.2): nothing routes
            // or submits. The composition itself is driven by `on_ime` (winit delivers
            // committed/candidate keys as `Ime` events, not `KeyboardInput`); this gate
            // catches any raw key that still arrives mid-composition (notably Enter, so
            // it confirms the candidate instead of submitting - the Zed #23003 trap).
            Disposition::ImeComposing => None,
            // The hotkey flips ONLY the mode; text/selection/undo preserved (T-3.1).
            Disposition::ToggleMode => {
                self.input.reduce(InputEvent::ToggleMode);
                // The overlay is mode-aware (Shell highlights + suggests; Agent is prose):
                // recompute at once so the toggle re-styles without a debounce lag or a
                // flicker of the old mode's overlay (ticket T-3.5 AC4).
                self.request_overlay(true);
                None
            }
            // Interrupt the in-flight turn (Esc): cancel it and fail-closed deny any
            // parked approval so the loop unblocks. Only reached when a turn is active
            // and NOT parked on an approval (that case is handled above, pre-routing).
            Disposition::InterruptAgent => {
                if let Some(turn) = self.turn.as_ref() {
                    turn.cancel();
                    log::info!("agent interrupt (Esc)");
                }
                None
            }
            // Agent submit (T-5.11): hand the line to the live turn runtime, which
            // streams the turn's steps into the timeline through the engine's agent
            // injector, gated by the CURRENT autonomy posture. The turn runs off the
            // render thread - this returns at once.
            Disposition::SubmitAgent => {
                if self.turn.as_ref().is_some_and(TurnHandle::is_active) {
                    // A turn is already running: PRESERVE the typed line (do not drain
                    // the input) so a follow-up is not silently lost; the resubmit is
                    // ignored for now (queueing is a follow-up).
                    log::info!("agent busy; keeping the typed line (resubmit ignored)");
                } else {
                    // No turn active: now it is safe to consume the line and submit it.
                    // `take()` also clears the overlay/ghost for the now-empty buffer.
                    let line = self.input.take();
                    if line.trim().is_empty() {
                        log::debug!("agent submit: empty line ignored");
                    } else {
                        // Record the prompt in shared history (as an Agent submission, even
                        // when Opt-Enter fired it from Shell mode) so ghost text can suggest
                        // it later (tickets T-3.7 / T-3.5).
                        self.record_history(&line, InputMode::Agent);
                        if let Some(injector) = self.engine.agent_injector() {
                            let policy = self.autonomy.policy();
                            log::info!("agent submit ({}): {line}", self.autonomy.mode().label());
                            self.turn = Some(self.agent.start_turn(line, policy, injector));
                        } else {
                            log::warn!("agent submit dropped: the engine is shutting down");
                        }
                    }
                }
                None
            }
            // Shell submit: the shell already received the chars via the Edit mirror
            // below, so submitting is just the carriage return; reset the model (which
            // also clears the overlay/ghost) and record the command in shared history.
            Disposition::SubmitShell => {
                let line = self.input.take();
                self.record_history(&line, InputMode::Shell);
                Some(b"\r".to_vec())
            }
            // Raw passthrough (alt-screen / a running foreground command / degraded
            // ZLE): the keys belong to the foreground program, NOT the input box, so
            // we do not edit the `InputModel`. Encode them to the correct PTY bytes
            // (legacy / DECCKM / Kitty) with the T-3.4 encoder, reading the live
            // key-mode flags off the snapshot.
            Disposition::PassthroughToPty => {
                let snap = self.engine.latest_snapshot();
                let flags = keys::KeyEncodeFlags {
                    app_cursor: snap.app_cursor,
                    disambiguate: snap.disambiguate,
                };
                keystroke_for(&key)
                    .map(|stroke| keys::encode(stroke, flags))
                    .filter(|bytes| !bytes.is_empty())
            }
            // Ordinary editing: update the input line. In Shell mode ALSO mirror raw
            // to the PTY for the shell's echo (until the T-3.6 widget is the source
            // of truth). Agent mode edits the prompt only - no PTY bytes.
            Disposition::Edit => {
                let outcome = self.apply_edit_key(key.named, key.text);
                // Recompute the async highlight/ghost overlay for the edited line off the
                // render thread (ticket T-3.5) - but ONLY when the TEXT changed. A pure
                // caret motion cannot change the highlight or the ghost prefix, and firing
                // on it would reset the worker's debounce (starving a pending recompute) for
                // no benefit. Space/paste/ghost-accept are immediate; typing/delete debounced.
                if outcome.text_changed {
                    self.request_overlay(edit_is_immediate(key.named, key.text));
                }
                if ctx.mode == InputMode::Shell {
                    Self::raw_key_bytes(key.named, key.text, outcome.erased)
                } else {
                    None
                }
            }
        };

        // Mirror to the PTY and surface the bytes so a headless host can observe them.
        if let Some(b) = &bytes {
            self.engine.send_input(b.clone());
        }
        bytes
    }

    fn on_ime(&mut self, event: ImeEvent) {
        // T-3.2 IME feed. Populate/clear the pure `InputModel`'s preedit (a transient
        // overlay that never touches the committed buffer) or commit the final text.
        // `preedit.is_some()` then makes `routing_context` report `preedit_active`, so
        // the routing brain (T-3.3) gives the IME Enter/Tab/Esc and never submits mid-
        // composition (the Zed #23003 trap). No PTY bytes flow here - even in Shell mode
        // the composed text only becomes shell input when the line is submitted.

        // While a turn is parked on an approval the keyboard is locked (see `on_key`) so a
        // pending safety decision cannot be bypassed by stray input; hold IME to the same
        // bar - do not let a composition compose/commit into the buffer during a park. A
        // `Disabled` still passes through so a dangling composition is always cleared
        // (review finding: this closes the IME path around the approval lock).
        if self
            .turn
            .as_ref()
            .is_some_and(|t| t.is_active() && t.has_pending_approval())
            && !matches!(event, ImeEvent::Disabled)
        {
            return;
        }

        let committed = matches!(event, ImeEvent::Commit(_));
        apply_ime(&mut self.input, event);
        // A commit changed the committed buffer; recompute the overlay at once. A bare
        // preedit does not change the buffer (and the renderer hides the ghost while a
        // composition shows), so it needs no recompute.
        if committed {
            self.request_overlay(true);
        }
    }

    fn tick(&mut self) {
        // Apply any off-thread overlay result that landed since the last wake (ticket
        // T-3.5), before the frame is built - so the render path only reads the last-good
        // overlay and never blocks on the highlighter.
        self.apply_overlay();
    }

    fn on_resize(&mut self, cols: u16, rows: u16, width: u32, height: u32) {
        // Pixel dims are advisory (TIOCSWINSZ ws_xpixel/ypixel); clamp rather than
        // silently wrap if a surface somehow exceeds u16.
        self.engine.resize(
            rows,
            cols,
            u16::try_from(width).unwrap_or(u16::MAX),
            u16::try_from(height).unwrap_or(u16::MAX),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The Session-layer IME semantics (ticket T-3.2), tested on a bare `InputModel`
    // through the pure `apply_ime` boundary - no PTY/engine/window needed. The routing
    // gate (`preedit_active` -> Enter never submits) is covered in `routing.rs`
    // (`ime_composition_owns_enter_and_never_submits`); the core mutators in `input.rs`.

    #[test]
    fn preedit_event_populates_then_an_empty_preedit_clears_it() {
        let mut input = InputModel::new();
        input.reduce(InputEvent::Insert("ko".to_string()));
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "ni".to_string(),
                cursor: Some((2, 2)),
            },
        );
        assert!(
            input.preedit().is_some(),
            "an active composition drives preedit_active"
        );
        assert_eq!(input.text(), "ko", "preedit does not touch the buffer");
        // winit sends an empty preedit right before a commit; it must CLEAR, not show.
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: String::new(),
                cursor: None,
            },
        );
        assert!(input.preedit().is_none(), "an empty preedit clears");
    }

    #[test]
    fn commit_inserts_the_final_text_and_clears_preedit() {
        // AC: Commit inserts the final text (inert) and does not leave a dangling preedit.
        let mut input = InputModel::new();
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "に".to_string(),
                cursor: None,
            },
        );
        apply_ime(&mut input, ImeEvent::Commit("日本".to_string()));
        assert_eq!(input.text(), "日本");
        assert!(input.preedit().is_none());
    }

    #[test]
    fn disabled_clears_a_dangling_preedit() {
        let mut input = InputModel::new();
        apply_ime(
            &mut input,
            ImeEvent::Preedit {
                text: "x".to_string(),
                cursor: None,
            },
        );
        apply_ime(&mut input, ImeEvent::Disabled);
        assert!(
            input.preedit().is_none(),
            "blur/disable drops the composition so it cannot wedge the routing gate"
        );
    }

    // --- T-9.2 title-bar cwd display -----------------------------------------

    #[test]
    fn abbreviate_home_replaces_the_home_prefix_with_tilde() {
        // The title bar shows the cwd with `$HOME` collapsed to `~` (the shell convention).
        // Reads $HOME (never sets it), so it is race-free across parallel tests.
        if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            assert_eq!(abbreviate_home(&home), "~", "home itself is exactly ~");
            assert_eq!(
                abbreviate_home(&home.join("projects").join("aterm")),
                "~/projects/aterm",
                "a path under home is ~-abbreviated"
            );
        }
        // A path plainly outside home is shown verbatim (no false abbreviation).
        let outside = std::path::Path::new("/opt/definitely-not-home");
        assert_eq!(abbreviate_home(outside), "/opt/definitely-not-home");
    }

    // --- T-3.5 overlay-immediacy classification ------------------------------

    #[test]
    fn edit_immediacy_short_circuits_space_paste_and_ghost_accept_only() {
        // Space, a multi-char paste, and a ghost accept (Right/End) recompute immediately
        // (AC1); ordinary single-char typing, backspace, and plain motion are debounced.
        assert!(edit_is_immediate(Some(NamedKey::Space), Some(" ")));
        assert!(edit_is_immediate(Some(NamedKey::ArrowRight), None));
        assert!(edit_is_immediate(Some(NamedKey::End), None));
        assert!(
            edit_is_immediate(None, Some("pasted text")),
            "a multi-char insertion is a paste -> immediate"
        );
        // Debounced cases:
        assert!(
            !edit_is_immediate(None, Some("a")),
            "a single typed char is debounced"
        );
        assert!(!edit_is_immediate(Some(NamedKey::Backspace), None));
        assert!(!edit_is_immediate(Some(NamedKey::ArrowLeft), None));
        assert!(!edit_is_immediate(Some(NamedKey::ArrowUp), None));
    }
}
