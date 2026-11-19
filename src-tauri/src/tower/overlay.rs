//! The tower overlay: Handy's overlay window in a `tower` state, fed by a
//! `tower-overlay` event (`src/overlay/TowerOverlay.tsx` draws it). It shows on
//! both machines whenever the server is not idle, and briefly for messages.

use super::client::TowerMessage;
use super::events::{ServerState, ServerView};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// How long a message holds before the overlay returns to the session (or
/// hides).
pub const MESSAGE_HOLD: Duration = Duration::from_millis(2500);

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Recording,
    Held,
    Finishing,
    Message,
}

/// Payload of the `tower-overlay` event. Never carries text.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    pub kind: Kind,
    /// Recording time so far; the overlay ticks it forward itself.
    pub elapsed_ms: u64,
    pub raw: bool,
    pub message: Option<TowerMessage>,
    /// Message severity, for its colour.
    pub error: bool,
}

/// What the overlay should show now, or `None` to hide it.
pub fn decide(view: &ServerView, message: Option<TowerMessage>, now: Instant) -> Option<Payload> {
    if let Some(m) = message {
        return Some(Payload {
            kind: Kind::Message,
            elapsed_ms: 0,
            raw: false,
            message: Some(m),
            error: m.is_error(),
        });
    }
    let (snap, at) = view.display()?;
    let kind = match snap.state {
        ServerState::Recording => Kind::Recording,
        ServerState::Held => Kind::Held,
        ServerState::Finishing => Kind::Finishing,
        ServerState::Idle | ServerState::Unknown => return None,
    };
    let elapsed_ms = if kind == Kind::Recording {
        snap.elapsed_ms + now.saturating_duration_since(at).as_millis() as u64
    } else {
        snap.elapsed_ms
    };
    Some(Payload {
        kind,
        elapsed_ms,
        raw: snap.mode.as_deref() == Some("raw"),
        message: None,
        error: false,
    })
}

/// Order of `apply` calls, and the newest one that reached the window.
static DECIDED: AtomicU64 = AtomicU64::new(0);
static APPLIED: AtomicU64 = AtomicU64::new(0);

/// Whether update `seq` may still touch the window: `false` once a newer
/// update has (it would undo that one).
fn claim(applied: &AtomicU64, seq: u64) -> bool {
    applied.fetch_max(seq, Ordering::SeqCst) < seq
}

/// Shows or hides the overlay. `shown` tracks whether the tower put it up, so
/// an idle server never hides an overlay that belongs to something else.
///
/// Both run on the main thread, in the order decided. Upstream's show hops to
/// the main thread while its hide runs on the caller's: a hide decided just
/// after a show (the stop result, then the stream's `idle` a few ms later) ran
/// first, the late show then cancelled the hide's delayed `window.hide()`, and
/// the "finishing" spinner stayed up after the text was pasted.
pub fn apply(app: &AppHandle, payload: Option<Payload>, shown: &mut bool) {
    let hide = match payload {
        Some(_) => false,
        None if *shown => true,
        None => return,
    };
    *shown = !hide;
    let seq = DECIDED.fetch_add(1, Ordering::SeqCst) + 1;
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !claim(&APPLIED, seq) {
            log::debug!("Skipping a stale tower overlay update");
            return;
        }
        match payload {
            Some(p) => {
                let _ = handle.emit_to("recording_overlay", "tower-overlay", &p);
                // Already on the main thread, so this shows inline.
                crate::overlay::show_overlay_state(&handle, "tower");
            }
            None => crate::overlay::hide_recording_overlay(&handle),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tower::events::Snapshot;

    fn snap(state: ServerState, mode: &str, elapsed_ms: u64) -> Snapshot {
        Snapshot {
            state,
            session: Some("s_1".into()),
            mode: Some(mode.into()),
            elapsed_ms,
            max_ms: 600_000,
            reason: None,
        }
    }

    fn connected_view(s: Snapshot) -> ServerView {
        let mut v = ServerView::default();
        v.connected = true;
        v.apply(s);
        v
    }

    #[test]
    fn a_stale_update_never_overrides_a_newer_one() {
        let applied = AtomicU64::new(0);
        // In order: each one applies.
        assert!(claim(&applied, 1));
        assert!(claim(&applied, 2));
        // Show #3 is overtaken by hide #4: the late show must not run.
        assert!(claim(&applied, 4));
        assert!(!claim(&applied, 3));
        assert!(!claim(&applied, 4), "an update applies once");
        assert!(claim(&applied, 5));
    }

    #[test]
    fn recording_ticks_elapsed_forward() {
        let v = connected_view(snap(ServerState::Recording, "raw", 8_300));
        let p = decide(&v, None, Instant::now() + Duration::from_secs(2)).unwrap();
        assert_eq!(p.kind, Kind::Recording);
        assert!(p.raw);
        assert!(p.elapsed_ms >= 10_300, "{}", p.elapsed_ms);
    }

    #[test]
    fn held_and_finishing_show_idle_hides() {
        let now = Instant::now();
        let v = connected_view(snap(ServerState::Held, "clean", 51_000));
        assert_eq!(decide(&v, None, now).unwrap().kind, Kind::Held);
        let v = connected_view(snap(ServerState::Finishing, "clean", 0));
        assert_eq!(decide(&v, None, now).unwrap().kind, Kind::Finishing);
        let v = connected_view(Snapshot {
            state: ServerState::Idle,
            session: None,
            mode: None,
            elapsed_ms: 0,
            max_ms: 600_000,
            reason: Some("stopped".into()),
        });
        assert_eq!(decide(&v, None, now), None);
    }

    #[test]
    fn a_message_wins_over_the_session() {
        let v = connected_view(snap(ServerState::Recording, "clean", 0));
        let p = decide(&v, Some(TowerMessage::Busy), Instant::now()).unwrap();
        assert_eq!(p.kind, Kind::Message);
        assert_eq!(p.message, Some(TowerMessage::Busy));
        assert!(!p.error);
        let p = decide(&v, Some(TowerMessage::Unreachable), Instant::now()).unwrap();
        assert!(p.error);
    }

    #[test]
    fn a_dead_stream_shows_nothing_but_local_state() {
        let mut v = connected_view(snap(ServerState::Recording, "clean", 0));
        v.set_disconnected();
        assert_eq!(decide(&v, None, Instant::now()), None);
        assert!(!v.session_active());
        v.set_local(Some(snap(ServerState::Finishing, "clean", 0)));
        assert_eq!(
            decide(&v, None, Instant::now()).unwrap().kind,
            Kind::Finishing
        );
        assert!(v.session_active());
        v.set_local(None);
        assert_eq!(decide(&v, None, Instant::now()), None);
    }

    #[test]
    fn the_stream_replaces_an_optimistic_state() {
        let mut v = ServerView::default();
        v.connected = true;
        v.set_local(Some(snap(ServerState::Recording, "clean", 0)));
        assert_eq!(
            decide(&v, None, Instant::now()).unwrap().kind,
            Kind::Recording
        );
        v.apply(Snapshot {
            state: ServerState::Idle,
            session: None,
            mode: None,
            elapsed_ms: 0,
            max_ms: 0,
            reason: Some("cancelled".into()),
        });
        assert_eq!(decide(&v, None, Instant::now()), None);
    }
}
