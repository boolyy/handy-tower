//! Hotkey → HTTP mapping for the tower backend. Stateless with respect to the
//! session (the other machine may have started it), hold-or-toggle per key:
//!
//! - key down → `POST /record/toggle?mode=…` (auto-repeat downs are ignored);
//! - `started` → remember when the key went down; on key up, a hold of at
//!   least `hold_threshold_ms` sends `POST /record/stop?mode=…`
//!   (push-to-talk), a short tap leaves it recording;
//! - `stopped` → paste the text and ignore that key's release.
//!
//! [`Keys`] is the pure part; [`run`] executes it in order on one task, so a
//! release is always handled after the response to its own press.

use super::client::{Mode, Toggled};
use std::time::{Duration, Instant};
use tauri::AppHandle;

pub const DICTATE_BINDING: &str = "tower_dictate";
pub const RAW_BINDING: &str = "tower_raw";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TowerKey {
    Dictate,
    Raw,
}

impl TowerKey {
    pub fn from_binding(id: &str) -> Option<Self> {
        match id {
            DICTATE_BINDING => Some(TowerKey::Dictate),
            RAW_BINDING => Some(TowerKey::Raw),
            _ => None,
        }
    }

    pub fn mode(self) -> Mode {
        match self {
            TowerKey::Dictate => Mode::Clean,
            TowerKey::Raw => Mode::Raw,
        }
    }
}

#[derive(Debug)]
pub enum Input {
    Key {
        key: TowerKey,
        pressed: bool,
        at: Instant,
    },
    /// CLI flag / SIGUSR2: a plain toggle with no key to hold.
    ExternalToggle,
}

#[derive(Default)]
struct KeyState {
    down: bool,
    pressed_at: Option<Instant>,
    /// The session this key's press started, until its release.
    started: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    Toggle(Mode),
    /// Push-to-talk release of `session`.
    Stop {
        mode: Mode,
        session: String,
    },
}

#[derive(Default)]
pub struct Keys {
    dictate: KeyState,
    raw: KeyState,
}

impl Keys {
    fn state(&mut self, key: TowerKey) -> &mut KeyState {
        match key {
            TowerKey::Dictate => &mut self.dictate,
            TowerKey::Raw => &mut self.raw,
        }
    }

    pub fn press(&mut self, key: TowerKey, at: Instant) -> Plan {
        let s = self.state(key);
        if s.down {
            return Plan::Nothing; // auto-repeat
        }
        s.down = true;
        s.pressed_at = Some(at);
        s.started = None;
        Plan::Toggle(key.mode())
    }

    /// The press's toggle answered: `Some(session)` if it started recording.
    pub fn toggled(&mut self, key: TowerKey, started: Option<String>) {
        self.state(key).started = started;
    }

    /// `ended` says whether a session is known to be over (see
    /// `ServerView::session_ended`).
    pub fn release(
        &mut self,
        key: TowerKey,
        at: Instant,
        hold_threshold: Duration,
        ended: impl Fn(&str) -> bool,
    ) -> Plan {
        let s = self.state(key);
        if !s.down {
            return Plan::Nothing; // a release we never saw go down
        }
        s.down = false;
        let Some(session) = s.started.take() else {
            return Plan::Nothing; // the press stopped, or failed
        };
        let held = s
            .pressed_at
            .map_or(Duration::ZERO, |p| at.saturating_duration_since(p));
        if held < hold_threshold {
            return Plan::Nothing; // a tap: keep recording until the next press
        }
        if ended(&session) {
            // Stale release (e.g. the key-up was swallowed while the cursor was
            // on the other screen): that session is over, and stopping now
            // would end someone else's.
            log::debug!("Ignoring a push-to-talk release for an ended tower session");
            return Plan::Nothing;
        }
        Plan::Stop {
            mode: key.mode(),
            session,
        }
    }
}

/// Executes inputs in order until the channel closes.
pub async fn run(app: AppHandle, mut rx: tokio::sync::mpsc::UnboundedReceiver<Input>) {
    let mut keys = Keys::default();
    while let Some(input) = rx.recv().await {
        match input {
            Input::Key {
                key,
                pressed: true,
                at,
            } => {
                if let Plan::Toggle(mode) = keys.press(key, at) {
                    let started = toggle(&app, mode).await;
                    keys.toggled(key, started);
                }
            }
            Input::Key {
                key,
                pressed: false,
                at,
            } => {
                let threshold =
                    Duration::from_millis(crate::settings::get_settings(&app).hold_threshold_ms);
                let plan = keys.release(key, at, threshold, |s| super::session_ended(&app, s));
                if let Plan::Stop { mode, .. } = plan {
                    stop(&app, mode).await;
                }
            }
            Input::ExternalToggle => {
                toggle(&app, Mode::Clean).await;
            }
        }
    }
}

/// Sends a toggle and delivers its result. Returns the session it started.
async fn toggle(app: &AppHandle, mode: Mode) -> Option<String> {
    let client = super::client_for_request(app)?;
    match client.toggle(mode).await {
        Ok(Toggled::Started { session }) => {
            log::info!("Tower recording started ({})", mode.as_str());
            super::on_started(app, &session, mode);
            Some(session)
        }
        Ok(Toggled::Stopped(stopped)) => {
            log::info!("Tower recording stopped by toggle ({})", mode.as_str());
            super::deliver(app, Ok(stopped));
            None
        }
        Err(e) => {
            log::info!("Tower toggle failed: {e}");
            super::deliver(app, Err(e));
            None
        }
    }
}

async fn stop(app: &AppHandle, mode: Mode) {
    let Some(client) = super::client_for_request(app) else {
        return;
    };
    super::on_stopping(app);
    let result = client.stop(mode).await;
    match &result {
        Ok(_) => log::info!("Tower recording stopped on release ({})", mode.as_str()),
        Err(e) => log::info!("Tower stop failed: {e}"),
    }
    super::deliver(app, result);
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_millis(300);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn never_ended(_: &str) -> bool {
        false
    }

    #[test]
    fn bindings_map_to_modes() {
        assert_eq!(
            TowerKey::from_binding("tower_dictate").map(TowerKey::mode),
            Some(Mode::Clean)
        );
        assert_eq!(
            TowerKey::from_binding("tower_raw").map(TowerKey::mode),
            Some(Mode::Raw)
        );
        assert_eq!(TowerKey::from_binding("transcribe"), None);
    }

    #[test]
    fn a_tap_that_started_keeps_recording() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        assert_eq!(k.press(TowerKey::Dictate, t0), Plan::Toggle(Mode::Clean));
        k.toggled(TowerKey::Dictate, Some("s_1".into()));
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(120), T, never_ended),
            Plan::Nothing
        );
    }

    #[test]
    fn a_hold_that_started_stops_on_release() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        k.press(TowerKey::Raw, t0);
        k.toggled(TowerKey::Raw, Some("s_1".into()));
        assert_eq!(
            k.release(TowerKey::Raw, t0 + ms(300), T, never_ended),
            Plan::Stop {
                mode: Mode::Raw,
                session: "s_1".into()
            }
        );
    }

    #[test]
    fn a_press_that_stopped_ignores_its_release() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        k.press(TowerKey::Dictate, t0);
        k.toggled(TowerKey::Dictate, None); // "stopped": text was pasted
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(5_000), T, never_ended),
            Plan::Nothing
        );
    }

    #[test]
    fn auto_repeat_downs_are_ignored() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        assert_eq!(k.press(TowerKey::Dictate, t0), Plan::Toggle(Mode::Clean));
        k.toggled(TowerKey::Dictate, Some("s_1".into()));
        for i in 1..20 {
            assert_eq!(k.press(TowerKey::Dictate, t0 + ms(30 * i)), Plan::Nothing);
        }
        // The hold is measured from the first down, not the last repeat.
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(700), T, never_ended),
            Plan::Stop {
                mode: Mode::Clean,
                session: "s_1".into()
            }
        );
    }

    #[test]
    fn a_stray_release_does_nothing() {
        let mut k = Keys::default();
        assert_eq!(
            k.release(TowerKey::Dictate, Instant::now(), T, never_ended),
            Plan::Nothing
        );
    }

    #[test]
    fn a_stale_release_for_an_ended_session_does_not_stop_the_next_one() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        k.press(TowerKey::Dictate, t0);
        k.toggled(TowerKey::Dictate, Some("s_1".into()));
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(60_000), T, |s| s == "s_1"),
            Plan::Nothing
        );
    }

    #[test]
    fn keys_are_independent() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        k.press(TowerKey::Dictate, t0);
        k.toggled(TowerKey::Dictate, Some("s_1".into()));
        // Raw tapped while dictate is held: its own toggle (the server stops
        // s_1 in raw mode and returns the text).
        assert_eq!(
            k.press(TowerKey::Raw, t0 + ms(100)),
            Plan::Toggle(Mode::Raw)
        );
        k.toggled(TowerKey::Raw, None);
        assert_eq!(
            k.release(TowerKey::Raw, t0 + ms(150), T, never_ended),
            Plan::Nothing
        );
        // Dictate's release now names a session the server has ended.
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(900), T, |s| s == "s_1"),
            Plan::Nothing
        );
    }

    #[test]
    fn a_failed_toggle_ignores_the_release() {
        let mut k = Keys::default();
        let t0 = Instant::now();
        k.press(TowerKey::Dictate, t0);
        // Network error / busy / debounced: nothing started.
        k.toggled(TowerKey::Dictate, None);
        assert_eq!(
            k.release(TowerKey::Dictate, t0 + ms(2_000), T, never_ended),
            Plan::Nothing
        );
        // And the next press toggles again.
        assert_eq!(
            k.press(TowerKey::Dictate, t0 + ms(3_000)),
            Plan::Toggle(Mode::Clean)
        );
    }
}
