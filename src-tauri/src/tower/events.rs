//! `GET /events`: the server's session state, pushed over Server-Sent Events.
//! It never carries text. Both machines stay connected; the stream drives the
//! overlay, the tray icon and the Esc cancel binding.

use super::client::{map_reqwest_error, ClientError, TowerClient};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// The server pings every 15 s; silence for this long means a dead link.
const IDLE_TIMEOUT: Duration = Duration::from_secs(40);
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerState {
    Idle,
    Recording,
    Finishing,
    Held,
    #[serde(other)]
    Unknown,
}

/// One `state` event (or `GET /state`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub state: ServerState,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub max_ms: u64,
    #[serde(default)]
    pub reason: Option<String>,
}

impl Snapshot {
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            ServerState::Recording | ServerState::Finishing | ServerState::Held
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser (the subset the tower uses: `event:`, `data:`,
/// `:` comments, blank-line dispatch). Bytes may split anywhere.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent {
                        event: if self.event.is_empty() {
                            "message".to_string()
                        } else {
                            std::mem::take(&mut self.event)
                        },
                        data: self.data.join("\n"),
                    });
                }
                self.event.clear();
                self.data.clear();
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / ping
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_ref(), ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

/// What this client knows about the server's session.
#[derive(Default)]
pub struct ServerView {
    pub connected: bool,
    pub current: Option<Snapshot>,
    /// When `current` arrived, to tick the elapsed time locally.
    pub received_at: Option<Instant>,
    /// Sessions seen and then replaced: a late push-to-talk release for one of
    /// these must not stop whatever is running now.
    ended: VecDeque<String>,
    /// What this client did and the stream hasn't confirmed yet (it lags, or
    /// it is down). Any stream event replaces it.
    local: Option<(Snapshot, Instant)>,
}

const ENDED_MEMORY: usize = 16;

impl ServerView {
    pub fn apply(&mut self, snap: Snapshot) {
        if let Some(prev) = self.current.as_ref().and_then(|c| c.session.clone()) {
            let still_running = snap.session.as_deref() == Some(prev.as_str()) && snap.is_active();
            if !still_running {
                self.mark_ended(prev);
            }
        }
        self.current = Some(snap);
        self.received_at = Some(Instant::now());
        self.local = None;
    }

    /// Records this client's own transition until the stream confirms it.
    pub fn set_local(&mut self, snap: Option<Snapshot>) {
        self.local = snap.map(|s| (s, Instant::now()));
    }

    /// The state to show: our own unconfirmed transition first, else the
    /// stream's (only while connected; a dead stream shows nothing).
    pub fn display(&self) -> Option<(&Snapshot, Instant)> {
        if let Some((s, at)) = &self.local {
            return Some((s, *at));
        }
        if !self.connected {
            return None;
        }
        match (&self.current, self.received_at) {
            (Some(s), Some(at)) => Some((s, at)),
            _ => None,
        }
    }

    fn mark_ended(&mut self, session: String) {
        if !self.ended.contains(&session) {
            if self.ended.len() == ENDED_MEMORY {
                self.ended.pop_front();
            }
            self.ended.push_back(session);
        }
    }

    pub fn session_ended(&self, session: &str) -> bool {
        self.ended.iter().any(|s| s == session)
    }

    /// Whether the stream has already told us about `session` (running or
    /// ended), so an optimistic local update would only be stale.
    pub fn knows(&self, session: &str) -> bool {
        self.session_ended(session)
            || self
                .current
                .as_ref()
                .is_some_and(|c| c.session.as_deref() == Some(session))
    }

    pub fn set_disconnected(&mut self) {
        self.connected = false;
        self.local = None;
        // Whatever was running may end while we can't see it; the first event
        // after reconnecting is the current state, and `apply` marks the old
        // session ended then.
    }

    /// Whether the server is (as far as we know) in a session, so Esc should
    /// be grabbed.
    pub fn session_active(&self) -> bool {
        self.display().is_some_and(|(s, _)| s.is_active())
    }
}

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(BACKOFF_MAX)
}

enum StreamEnd {
    /// Connected, then lost the stream.
    Lost(String),
    /// Never connected.
    Failed(ClientError),
}

/// Runs until aborted: connect, read events, reconnect with backoff.
pub async fn run(app: AppHandle, client: Arc<TowerClient>, wake: Arc<tokio::sync::Notify>) {
    let mut backoff = BACKOFF_MIN;
    let mut quiet_failures = 0u32;
    loop {
        let end = stream_once(&app, &client).await;
        super::on_disconnected(&app);
        match end {
            StreamEnd::Lost(why) => {
                log::info!("Tower event stream lost ({why}); reconnecting");
                backoff = BACKOFF_MIN;
                quiet_failures = 0;
            }
            StreamEnd::Failed(e) => {
                // Log the first few, then every 30th, so a powered-off tower
                // doesn't fill the log.
                if quiet_failures < 3 || quiet_failures.is_multiple_of(30) {
                    log::warn!("Tower event stream: {e}; retrying in {backoff:?}");
                }
                quiet_failures += 1;
                if e.is_api(401, "unauthorized") {
                    // A wrong token won't fix itself; retry slowly.
                    backoff = BACKOFF_MAX;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = wake.notified() => {}
        }
        backoff = next_backoff(backoff);
    }
}

async fn stream_once(app: &AppHandle, client: &TowerClient) -> StreamEnd {
    let resp = match client.events_request().send().await {
        Ok(r) => r,
        Err(e) => return StreamEnd::Failed(map_reqwest_error(e)),
    };
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let code = if status == 401 {
            "unauthorized".to_string()
        } else {
            format!("http_{status}")
        };
        return StreamEnd::Failed(ClientError::Api { status, code });
    }
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::default();
    let mut announced = false;
    loop {
        let chunk = match tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await {
            Err(_) => return StreamEnd::Lost("no data or ping within 40 s".into()),
            Ok(None) => return StreamEnd::Lost("closed by server".into()),
            Ok(Some(Err(e))) => return StreamEnd::Lost(map_reqwest_error(e).to_string()),
            Ok(Some(Ok(chunk))) => chunk,
        };
        if !announced {
            announced = true;
            log::info!("Tower event stream connected");
            super::on_connected(app);
        }
        for ev in parser.feed(&chunk) {
            if ev.event != "state" {
                continue;
            }
            match serde_json::from_str::<Snapshot>(&ev.data) {
                Ok(snap) => super::on_server_state(app, snap),
                Err(e) => log::warn!("Ignoring an unreadable tower state event: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(state: ServerState, session: Option<&str>) -> Snapshot {
        Snapshot {
            state,
            session: session.map(str::to_string),
            mode: None,
            elapsed_ms: 0,
            max_ms: 600_000,
            reason: None,
        }
    }

    #[test]
    fn parses_state_events_and_skips_pings() {
        let mut p = SseParser::default();
        let evs = p.feed(
            b": ping\n\nevent: state\ndata: {\"state\":\"held\",\"session\":\"s_1\",\"reason\":\"auto_stop_silence\",\"elapsed_ms\":51000}\n\n",
        );
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event, "state");
        let s: Snapshot = serde_json::from_str(&evs[0].data).unwrap();
        assert_eq!(s.state, ServerState::Held);
        assert_eq!(s.session.as_deref(), Some("s_1"));
        assert_eq!(s.reason.as_deref(), Some("auto_stop_silence"));
        assert_eq!(s.elapsed_ms, 51_000);
    }

    #[test]
    fn events_split_across_chunks_and_crlf() {
        let mut p = SseParser::default();
        let raw = b"event: state\r\ndata: {\"state\":\"recording\",\"elapsed_ms\":5}\r\n\r\n";
        let mut evs = Vec::new();
        for b in raw.chunks(3) {
            evs.extend(p.feed(b));
        }
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "{\"state\":\"recording\",\"elapsed_ms\":5}");
    }

    #[test]
    fn multi_line_data_and_default_event_name() {
        let mut p = SseParser::default();
        let evs = p.feed(b"data: a\ndata: b\n\n");
        assert_eq!(
            evs,
            vec![SseEvent {
                event: "message".into(),
                data: "a\nb".into()
            }]
        );
    }

    #[test]
    fn unknown_states_do_not_break_parsing() {
        let s: Snapshot = serde_json::from_str(r#"{"state":"warming","elapsed_ms":0}"#).unwrap();
        assert_eq!(s.state, ServerState::Unknown);
        assert!(!s.is_active());
    }

    #[test]
    fn a_session_that_goes_idle_is_remembered_as_ended() {
        let mut v = ServerView::default();
        v.apply(snap(ServerState::Recording, Some("s_1")));
        assert!(!v.session_ended("s_1"));
        assert!(v.knows("s_1"));
        v.apply(snap(ServerState::Held, Some("s_1")));
        assert!(!v.session_ended("s_1"));
        v.apply(snap(ServerState::Idle, None));
        assert!(v.session_ended("s_1"));
        assert!(v.knows("s_1"));
        assert!(!v.knows("s_2"));
    }

    #[test]
    fn a_replaced_session_is_ended() {
        let mut v = ServerView::default();
        v.apply(snap(ServerState::Recording, Some("s_1")));
        // Reconnected after a gap: someone else's session is now running.
        v.apply(snap(ServerState::Recording, Some("s_2")));
        assert!(v.session_ended("s_1"));
        assert!(!v.session_ended("s_2"));
    }

    #[test]
    fn ended_memory_is_bounded() {
        let mut v = ServerView::default();
        for i in 0..40 {
            v.apply(snap(ServerState::Recording, Some(&format!("s_{i}"))));
            v.apply(snap(ServerState::Idle, None));
        }
        assert!(v.session_ended("s_39"));
        assert!(!v.session_ended("s_0"));
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let mut b = BACKOFF_MIN;
        let mut seen = vec![b];
        for _ in 0..8 {
            b = next_backoff(b);
            seen.push(b);
        }
        assert_eq!(seen[1], Duration::from_secs(1));
        assert_eq!(*seen.last().unwrap(), BACKOFF_MAX);
    }
}
