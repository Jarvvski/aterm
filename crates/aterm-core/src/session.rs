//! Concurrent terminal-session ownership (ticket T-10.1).
//!
//! [`SessionList`] is the lifecycle seam above [`Engine`](crate::Engine). It keeps
//! ordered, independently running engines behind stable identities and centralizes
//! the active-session invariant so callers cannot retain an id that was just closed.
//!
//! Resource ceiling: `N` live sessions own `N` PTY reader threads and `N` model
//! threads. One lifecycle reaper performs blocking engine joins away from the single
//! render thread, which reads only the active session. No shared model queue lets one
//! session's flood consume another's capacity.

use crate::{Engine, ShellKind};

/// Stable identity for one terminal session. Values are never reused within a
/// [`SessionList`], including after the corresponding session is closed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(u64);

/// One independently running terminal session.
pub struct Session {
    id: SessionId,
    name: String,
    engine: Engine,
}

impl Session {
    /// Stable identity assigned by the owning [`SessionList`].
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Human-readable default name derived from the hosted shell.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrow the independently running terminal engine owned by this session.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

/// Ordered ownership of every live terminal session and the active selection.
///
/// Creating a session activates it. Closing the active session selects the
/// previous neighbor when one exists, otherwise the next. An empty list has no
/// active id; every non-empty list always has exactly one valid active id.
pub struct SessionList {
    sessions: Vec<Session>,
    active: Option<usize>,
    next_id: u64,
    reap_tx: Option<std::sync::mpsc::Sender<Engine>>,
    reaper: Option<std::thread::JoinHandle<()>>,
}

impl SessionList {
    /// Start with no sessions. The first call to [`Self::create`] establishes the
    /// active-session invariant.
    #[must_use]
    pub fn new() -> Self {
        let (reap_tx, reap_rx) = std::sync::mpsc::channel::<Engine>();
        let reaper = std::thread::Builder::new()
            .name("aterm-session-reaper".to_string())
            .spawn(move || {
                while let Ok(engine) = reap_rx.recv() {
                    drop(engine);
                }
            })
            .expect("spawn terminal session reaper");
        Self {
            sessions: Vec::new(),
            active: None,
            next_id: 1,
            reap_tx: Some(reap_tx),
            reaper: Some(reaper),
        }
    }

    /// Take ownership of an independently running engine and make it active.
    pub fn create(&mut self, engine: Engine) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("terminal session id space exhausted");
        let name = default_name(engine.shell_kind()).to_string();
        self.sessions.push(Session { id, name, engine });
        self.active = Some(self.sessions.len() - 1);
        id
    }

    /// Close `id`, selecting a valid neighbor and transferring blocking engine
    /// teardown to the lifecycle reaper. Returns `false` without changing the list
    /// when `id` is unknown.
    pub fn close(&mut self, id: SessionId) -> bool {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return false;
        };
        let was_active = self.active == Some(index);
        let removed = self.sessions.remove(index);

        if was_active {
            self.active = if self.sessions.is_empty() {
                None
            } else if index > 0 {
                Some(index - 1)
            } else {
                Some(0)
            };
        } else if self.active.is_some_and(|active| active > index) {
            self.active = self.active.map(|active| active - 1);
        }
        let Session { engine, .. } = removed;
        if let Some(tx) = &self.reap_tx {
            let _ = tx.send(engine);
        }
        true
    }

    /// Select `id`. Returns `false` without changing the selection when `id` is
    /// unknown.
    pub fn set_active(&mut self, id: SessionId) -> bool {
        if let Some(index) = self.sessions.iter().position(|session| session.id == id) {
            self.active = Some(index);
            true
        } else {
            false
        }
    }

    /// The active session, or `None` only when the list is empty.
    #[must_use]
    pub fn active(&self) -> Option<&Session> {
        self.sessions.get(self.active?)
    }

    /// Iterate in stable display order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Session> {
        self.sessions.iter()
    }
}

impl Default for SessionList {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for SessionList {
    fn drop(&mut self) {
        if let Some(tx) = &self.reap_tx {
            for Session { engine, .. } in self.sessions.drain(..) {
                let _ = tx.send(engine);
            }
        }
        self.reap_tx.take();
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
    }
}

fn default_name(shell: ShellKind) -> &'static str {
    match shell {
        ShellKind::Zsh => "zsh",
        ShellKind::Bash => "bash",
        ShellKind::Fish => "fish",
        ShellKind::Other => "terminal",
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::time::{Duration, Instant};

    use crate::{Engine, PtyDimensions, DEFAULT_SCROLLBACK};

    use super::{Session, SessionList};

    fn test_engine() -> Engine {
        Engine::spawn_command(
            "/bin/cat",
            &[],
            PtyDimensions {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            DEFAULT_SCROLLBACK,
        )
        .expect("test PTY should spawn")
    }

    fn flood_engine() -> Engine {
        Engine::spawn_command(
            "/bin/sh",
            &["-c", "yes | head -c 2097152; sleep 2"],
            PtyDimensions {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            DEFAULT_SCROLLBACK,
        )
        .expect("finite flood PTY should spawn")
    }

    fn timeline_engine(marker: &str) -> Engine {
        let nonce = "ATERMSESSION0";
        let script = format!(
            "printf '\\033]133;A;aterm_nonce={nonce}\\007'; \
             printf '\\033]133;C;aterm_nonce={nonce}\\007'; \
             printf '{marker}\\n'; \
             printf '\\033]133;D;0;aterm_nonce={nonce}\\007'; \
             sleep 2"
        );
        Engine::spawn_command_with_integration(
            "/bin/sh",
            &["-c", &script],
            PtyDimensions {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            },
            DEFAULT_SCROLLBACK,
            crate::ShellKind::Bash,
            Some(nonce),
            Duration::from_secs(1),
        )
        .expect("integrated timeline PTY should spawn")
    }

    fn snapshot_text(session: &Session) -> String {
        session
            .engine()
            .latest_snapshot()
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect()
    }

    fn wait_for_text(session: &Session, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if snapshot_text(session).contains(expected) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("session snapshot never contained {expected:?}");
    }

    #[test]
    fn session_list_creates_switches_and_closes_with_stable_ids() {
        let mut sessions = SessionList::new();

        let first = sessions.create(test_engine());
        let second = sessions.create(test_engine());

        assert_ne!(first, second, "each session receives a distinct id");
        assert_eq!(
            sessions.active().map(|session| session.name()),
            Some("terminal"),
            "an unrecognized command host gets the terminal default name"
        );
        assert_eq!(
            sessions.active().map(|session| session.id()),
            Some(second),
            "a newly created session becomes active"
        );

        assert!(sessions.set_active(first));
        assert_eq!(sessions.active().map(|session| session.id()), Some(first));
        assert!(sessions.close(first));
        assert_eq!(
            sessions.active().map(|session| session.id()),
            Some(second),
            "closing the first active session selects its next neighbor"
        );

        assert!(sessions.close(second));
        assert!(sessions.active().is_none());

        let third = sessions.create(test_engine());
        assert_ne!(third, first, "closed ids are never reused");
        assert_ne!(third, second, "closed ids are never reused");

        let fourth = sessions.create(test_engine());
        assert!(sessions.close(fourth));
        assert_eq!(
            sessions.active().map(|session| session.id()),
            Some(third),
            "closing the last active session prefers its previous neighbor"
        );

        let fifth = sessions.create(test_engine());
        assert!(sessions.close(third));
        assert_eq!(
            sessions.active().map(|session| session.id()),
            Some(fifth),
            "closing an earlier inactive session keeps the active selection valid"
        );
    }

    #[test]
    fn background_session_output_is_retained_until_it_becomes_active() {
        let mut sessions = SessionList::new();
        let background = sessions.create(test_engine());
        let foreground = sessions.create(test_engine());

        let background_session = sessions
            .iter()
            .find(|session| session.id() == background)
            .expect("background session exists");
        background_session
            .engine()
            .send_input(b"background-marker\n".to_vec());

        let foreground_session = sessions.active().expect("foreground session is active");
        foreground_session
            .engine()
            .send_input(b"foreground-marker\n".to_vec());
        wait_for_text(foreground_session, "foreground-marker");
        assert!(
            !snapshot_text(foreground_session).contains("background-marker"),
            "the active render source never exposes another session's grid"
        );

        assert!(sessions.set_active(background));
        let revealed = sessions.active().expect("background session is now active");
        wait_for_text(revealed, "background-marker");
        assert_eq!(revealed.id(), background);
        assert_ne!(revealed.id(), foreground);
    }

    #[test]
    fn background_flood_does_not_block_another_sessions_reader() {
        let mut sessions = SessionList::new();
        let flooding = sessions.create(flood_engine());
        let responsive = sessions.create(test_engine());

        let flooding_session = sessions
            .iter()
            .find(|session| session.id() == flooding)
            .expect("flooding session exists");
        let saturation_deadline = Instant::now() + Duration::from_secs(2);
        while flooding_session
            .engine()
            .metrics()
            .max_queue_depth
            .load(std::sync::atomic::Ordering::Relaxed)
            < crate::engine::READER_QUEUE_DEPTH
            && Instant::now() < saturation_deadline
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            flooding_session
                .engine()
                .metrics()
                .max_queue_depth
                .load(std::sync::atomic::Ordering::Relaxed),
            crate::engine::READER_QUEUE_DEPTH,
            "the background reader reached its bounded-channel backpressure ceiling"
        );

        let responsive_session = sessions.active().expect("responsive session is active");
        let response_started = Instant::now();
        responsive_session
            .engine()
            .send_input(b"still-responsive\n".to_vec());
        wait_for_text(responsive_session, "still-responsive");
        assert!(
            response_started.elapsed() < Duration::from_millis(500),
            "a saturated background reader must not delay another session"
        );
        assert_eq!(responsive_session.id(), responsive);

        let (closed_tx, closed_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(sessions);
            closed_tx
                .send(())
                .expect("teardown observer remains available");
        });
        assert!(
            closed_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a flooded background session must tear down without hanging the reaper"
        );
    }

    #[test]
    fn background_session_timeline_is_revealed_when_selected() {
        let mut sessions = SessionList::new();
        let background = sessions.create(timeline_engine("background-block"));
        let foreground = sessions.create(test_engine());

        let background_session = sessions
            .iter()
            .find(|session| session.id() == background)
            .expect("background session exists");
        let deadline = Instant::now() + Duration::from_secs(2);
        while background_session.engine().latest_blocks().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            sessions.active().map(|session| session.id()),
            Some(foreground),
            "background timeline progress does not change the render source"
        );

        assert!(sessions.set_active(background));
        let blocks = sessions
            .active()
            .expect("background session is selected")
            .engine()
            .latest_blocks();
        let text: String = blocks
            .iter()
            .filter_map(|block| block.as_command())
            .flat_map(|block| block.output.iter())
            .flat_map(|row| row.cells.iter().map(|cell| cell.c))
            .collect();
        assert!(
            text.contains("background-block"),
            "switching reveals the accumulated background timeline, got {text:?}"
        );
    }

    #[test]
    fn close_reaps_the_engine_without_waiting_for_agent_injector_clones() {
        let mut sessions = SessionList::new();
        let id = sessions.create(test_engine());
        let injector = sessions
            .active()
            .expect("session is active")
            .engine()
            .agent_injector()
            .expect("engine is running");
        let (closed_tx, closed_rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            assert!(sessions.close(id));
            drop(sessions);
            closed_tx.send(()).expect("test receiver remains open");
        });

        let closed = closed_rx.recv_timeout(Duration::from_secs(1));
        drop(injector);
        assert!(
            closed.is_ok(),
            "close and teardown must finish while an injector clone still exists"
        );
    }
}
