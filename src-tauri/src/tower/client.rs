//! HTTP client for the tower dictation API (`docs/API.md` in desk-tools).
//!
//! Privacy: transcript text only ever lives in [`Stopped::text`]. None of the
//! types here derive `Debug` over text, and nothing in this module logs it.

use serde::Deserialize;
use std::time::Duration;

/// Sent as `X-Dictation-Client` on every call; the server debounces on it.
pub const CLIENT_ID: &str = if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(target_os = "macos") {
    "mac"
} else {
    "linux"
};

/// The tower is on the LAN: anything slower than this to connect is down.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// A stop (or a toggle that stops) blocks until the text is ready (API.md).
pub const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const SHORT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ollama cleanup on the server.
    Clean,
    /// Parakeet output as is.
    Raw,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Clean => "clean",
            Mode::Raw => "raw",
        }
    }
}

/// A transcript handed back by a stop or a toggle that stopped.
pub struct Stopped {
    pub text: String,
    pub cleanup_failed: bool,
}

impl std::fmt::Debug for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stopped")
            .field("chars", &self.text.chars().count())
            .field("cleanup_failed", &self.cleanup_failed)
            .finish()
    }
}

#[derive(Debug)]
pub enum Toggled {
    Started { session: String },
    Stopped(Stopped),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// The server answered with an error body (`{"error":..,"message":..}`).
    Api { status: u16, code: String },
    /// Could not reach the server (refused, unreachable, DNS, reset).
    Network(String),
    /// Connected, but no answer in time.
    Timeout,
    /// The server answered with something this client can't read.
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Api { status, code } => write!(f, "HTTP {status} {code}"),
            ClientError::Network(e) => write!(f, "network error: {e}"),
            ClientError::Timeout => write!(f, "timed out"),
            ClientError::Protocol(e) => write!(f, "unexpected response: {e}"),
        }
    }
}

impl ClientError {
    pub fn is_api(&self, status: u16, code: &str) -> bool {
        matches!(self, ClientError::Api { status: s, code: c } if *s == status && c == code)
    }
}

/// The JSON body of `started` / `stopped` responses. Text stays private: see
/// [`Stopped`].
#[derive(Deserialize, Default)]
#[serde(default)]
struct ActionBody {
    action: String,
    session: Option<String>,
    text: Option<String>,
    cleanup: Option<String>,
}

impl ActionBody {
    fn into_stopped(self) -> Stopped {
        Stopped {
            cleanup_failed: self.cleanup.as_deref() == Some("failed"),
            text: self.text.unwrap_or_default(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ErrorBody {
    error: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CancelBody {
    cancelled: bool,
}

/// `/health`, flattened for the settings screen.
#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct TowerHealth {
    pub ok: bool,
    pub asr_loaded: bool,
    pub mic_present: bool,
    pub llm_reachable: bool,
}

#[derive(Clone)]
pub struct TowerClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl TowerClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // The tower is a LAN address. A system or corporate proxy (the work
            // Mac runs GlobalProtect) must never see these requests.
            .no_proxy()
            .build()
            .map_err(|e| format!("Failed to build the HTTP client: {e}"))?;
        Ok(Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    pub fn has_token(&self) -> bool {
        !self.token.is_empty()
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.token)
            .header("X-Dictation-Client", CLIENT_ID)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, ClientError> {
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let code = serde_json::from_str::<ErrorBody>(&body)
            .ok()
            .map(|b| b.error)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        Err(ClientError::Api {
            status: status.as_u16(),
            code,
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<T, ClientError> {
        let resp = self.send(req).await?;
        let bytes = resp.bytes().await.map_err(map_reqwest_error)?;
        serde_json::from_slice(&bytes).map_err(|e| ClientError::Protocol(e.to_string()))
    }

    /// `POST /record/toggle`: starts when idle, stops (and returns the text)
    /// when recording or held.
    pub async fn toggle(&self, mode: Mode) -> Result<Toggled, ClientError> {
        let req = self
            .request(reqwest::Method::POST, "/record/toggle")
            .query(&[("mode", mode.as_str())])
            .timeout(STOP_TIMEOUT);
        let body: ActionBody = self.json(req).await?;
        match body.action.as_str() {
            "started" => Ok(Toggled::Started {
                session: body.session.unwrap_or_default(),
            }),
            "stopped" => Ok(Toggled::Stopped(body.into_stopped())),
            other => Err(ClientError::Protocol(format!("unknown action '{other}'"))),
        }
    }

    /// `POST /record/start` (push-to-talk press). Returns the session id.
    /// The hotkeys always toggle (the session may belong to the other
    /// machine); kept for the live tests and scripts.
    #[allow(dead_code)]
    pub async fn start(&self, mode: Mode) -> Result<String, ClientError> {
        let req = self
            .request(reqwest::Method::POST, "/record/start")
            .query(&[("mode", mode.as_str())])
            .timeout(SHORT_TIMEOUT);
        let body: ActionBody = self.json(req).await?;
        Ok(body.session.unwrap_or_default())
    }

    /// `POST /record/stop`: blocks until the text is ready.
    pub async fn stop(&self, mode: Mode) -> Result<Stopped, ClientError> {
        let req = self
            .request(reqwest::Method::POST, "/record/stop")
            .query(&[("mode", mode.as_str())])
            .timeout(STOP_TIMEOUT);
        let body: ActionBody = self.json(req).await?;
        Ok(body.into_stopped())
    }

    /// `POST /record/cancel`: idempotent. Returns whether something was
    /// discarded.
    pub async fn cancel(&self) -> Result<bool, ClientError> {
        let req = self
            .request(reqwest::Method::POST, "/record/cancel")
            .timeout(SHORT_TIMEOUT);
        let body: CancelBody = self.json(req).await?;
        Ok(body.cancelled)
    }

    /// `GET /state`.
    #[allow(dead_code)]
    pub async fn state(&self) -> Result<super::events::Snapshot, ClientError> {
        let req = self
            .request(reqwest::Method::GET, "/state")
            .timeout(SHORT_TIMEOUT);
        self.json(req).await
    }

    /// `GET /health`. A 503 still carries the body (`ok:false`).
    pub async fn health(&self) -> Result<TowerHealth, ClientError> {
        let req = self
            .request(reqwest::Method::GET, "/health")
            .timeout(SHORT_TIMEOUT);
        let resp = req.send().await.map_err(map_reqwest_error)?;
        let status = resp.status().as_u16();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ClientError::Protocol(e.to_string()))?;
        if status == 401 {
            return Err(ClientError::Api {
                status,
                code: "unauthorized".into(),
            });
        }
        let flag = |a: &str, b: &str| body[a][b].as_bool().unwrap_or(false);
        Ok(TowerHealth {
            ok: body["ok"].as_bool().unwrap_or(false),
            asr_loaded: flag("asr", "loaded"),
            mic_present: flag("mic", "present"),
            llm_reachable: flag("llm", "reachable"),
        })
    }

    /// `GET /events` request, without a total timeout (the stream is
    /// long-lived; the reader enforces an idle timeout instead).
    pub fn events_request(&self) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, "/events")
            .header("Accept", "text/event-stream")
    }
}

pub fn map_reqwest_error(e: reqwest::Error) -> ClientError {
    if e.is_timeout() {
        ClientError::Timeout
    } else if e.is_decode() {
        ClientError::Protocol(e.to_string())
    } else {
        // reqwest's Display names the URL (host and path, never headers), so
        // it is safe to keep for logs.
        ClientError::Network(e.to_string())
    }
}

/// Short, content-free messages the overlay shows (mapped to text in
/// `src/overlay/TowerOverlay.tsx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TowerMessage {
    Busy,
    NoSpeech,
    CleanupFailed,
    Unreachable,
    Unauthorized,
    NoToken,
    MicUnavailable,
    Timeout,
    ServerError,
}

impl TowerMessage {
    pub fn is_error(self) -> bool {
        !matches!(
            self,
            TowerMessage::Busy | TowerMessage::NoSpeech | TowerMessage::CleanupFailed
        )
    }
}

/// What to do with the result of a stop (or a toggle that stopped).
#[derive(PartialEq, Eq)]
pub enum Outcome {
    /// Paste this text; `cleanup_failed` adds a small notice afterwards.
    Paste { text: String, cleanup_failed: bool },
    /// Show a message, paste nothing.
    Message(TowerMessage),
    /// Nothing to paste and nothing worth saying.
    Quiet,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Paste {
                text,
                cleanup_failed,
            } => write!(
                f,
                "Paste({} chars, cleanup_failed={cleanup_failed})",
                text.chars().count()
            ),
            Outcome::Message(m) => write!(f, "Message({m:?})"),
            Outcome::Quiet => write!(f, "Quiet"),
        }
    }
}

/// Maps an error to what the user sees (API.md "Errors"). `None` = silent.
pub fn error_message(e: &ClientError) -> Option<TowerMessage> {
    match e {
        // The other machine's hotkey fired too (Deskflow); the server kept the
        // first one. Nothing to say.
        ClientError::Api { status: 409, code } if code == "debounced" => None,
        // Esc during finishing: paste nothing.
        ClientError::Api { status: 409, code } if code == "cancelled" => None,
        // A push-to-talk release after the session already ended elsewhere.
        ClientError::Api { status: 409, code } if code == "not_recording" => None,
        ClientError::Api { status: 409, code } if code == "busy" => Some(TowerMessage::Busy),
        ClientError::Api { status: 401, .. } => Some(TowerMessage::Unauthorized),
        ClientError::Api { status: 503, code } if code == "mic_unavailable" => {
            Some(TowerMessage::MicUnavailable)
        }
        ClientError::Api { .. } | ClientError::Protocol(_) => Some(TowerMessage::ServerError),
        ClientError::Network(_) => Some(TowerMessage::Unreachable),
        ClientError::Timeout => Some(TowerMessage::Timeout),
    }
}

pub fn classify(result: Result<Stopped, ClientError>) -> Outcome {
    match result {
        Ok(stopped) if stopped.text.trim().is_empty() => Outcome::Message(TowerMessage::NoSpeech),
        Ok(stopped) => Outcome::Paste {
            text: stopped.text,
            cleanup_failed: stopped.cleanup_failed,
        },
        Err(e) => error_message(&e).map_or(Outcome::Quiet, Outcome::Message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16, code: &str) -> ClientError {
        ClientError::Api {
            status,
            code: code.into(),
        }
    }

    fn stopped(text: &str, cleanup_failed: bool) -> Result<Stopped, ClientError> {
        Ok(Stopped {
            text: text.into(),
            cleanup_failed,
        })
    }

    #[test]
    fn client_id_names_the_os() {
        assert!(["windows", "mac", "linux"].contains(&CLIENT_ID));
    }

    #[test]
    fn text_is_pasted_and_failed_cleanup_is_flagged() {
        assert_eq!(
            classify(stopped("Hello there.", false)),
            Outcome::Paste {
                text: "Hello there.".into(),
                cleanup_failed: false
            }
        );
        assert_eq!(
            classify(stopped("hello there", true)),
            Outcome::Paste {
                text: "hello there".into(),
                cleanup_failed: true
            }
        );
    }

    #[test]
    fn empty_text_is_no_speech() {
        for t in ["", "  ", "\n"] {
            assert_eq!(
                classify(stopped(t, false)),
                Outcome::Message(TowerMessage::NoSpeech)
            );
        }
    }

    #[test]
    fn conflicts_map_to_the_contract() {
        assert_eq!(classify(Err(api(409, "debounced"))), Outcome::Quiet);
        assert_eq!(classify(Err(api(409, "cancelled"))), Outcome::Quiet);
        assert_eq!(classify(Err(api(409, "not_recording"))), Outcome::Quiet);
        assert_eq!(
            classify(Err(api(409, "busy"))),
            Outcome::Message(TowerMessage::Busy)
        );
    }

    #[test]
    fn failures_map_to_overlay_errors() {
        assert_eq!(
            classify(Err(api(401, "unauthorized"))),
            Outcome::Message(TowerMessage::Unauthorized)
        );
        assert_eq!(
            classify(Err(api(503, "mic_unavailable"))),
            Outcome::Message(TowerMessage::MicUnavailable)
        );
        assert_eq!(
            classify(Err(api(500, "asr_failed"))),
            Outcome::Message(TowerMessage::ServerError)
        );
        assert_eq!(
            classify(Err(ClientError::Network("refused".into()))),
            Outcome::Message(TowerMessage::Unreachable)
        );
        assert_eq!(
            classify(Err(ClientError::Timeout)),
            Outcome::Message(TowerMessage::Timeout)
        );
    }

    #[test]
    fn debug_output_never_contains_text() {
        let s = Stopped {
            text: "private words".into(),
            cleanup_failed: false,
        };
        assert!(!format!("{s:?}").contains("private"));
        let o = classify(Ok(s));
        assert!(!format!("{o:?}").contains("private"));
    }

    #[test]
    fn action_bodies_parse() {
        let b: ActionBody =
            serde_json::from_str(r#"{"action":"started","session":"s_7f3a","state":"recording"}"#)
                .unwrap();
        assert_eq!(b.action, "started");
        assert_eq!(b.session.as_deref(), Some("s_7f3a"));
        let b: ActionBody = serde_json::from_str(
            r#"{"action":"stopped","session":"s_7f3a","state":"idle","text":"Hi.","mode":"clean","cleanup":"failed","timings":{"audio_ms":1}}"#,
        )
        .unwrap();
        let s = b.into_stopped();
        assert_eq!(s.text, "Hi.");
        assert!(s.cleanup_failed);
    }

    #[test]
    fn unreachable_server_is_a_network_error() {
        // Port 9 (discard) on localhost is closed on any sane test box.
        let client = TowerClient::new("http://127.0.0.1:9", "t").unwrap();
        let err = tauri::async_runtime::block_on(client.cancel()).unwrap_err();
        assert!(
            matches!(err, ClientError::Network(_) | ClientError::Timeout),
            "{err:?}"
        );
        assert_eq!(error_message(&err).map(|m| m.is_error()), Some(true));
    }
}

/// Opt-in tests against a live tower service. They run only when
/// `HANDY_TOWER_IT_URL` and `HANDY_TOWER_IT_TOKEN` are set, e.g.:
///
/// ```sh
/// set -a; . /home/tower/dictation/.env; set +a
/// HANDY_TOWER_IT_URL=http://127.0.0.1:8790 HANDY_TOWER_IT_TOKEN="$DICTATION_TOKEN" \
///   cargo test --lib tower::client::live -- --nocapture --test-threads=1
/// ```
///
/// They drive the real session, so nothing else should be dictating. Text is
/// never printed, only its length.
#[cfg(test)]
mod live {
    use super::*;
    use crate::tower::events::{ServerState, Snapshot, SseParser};
    use futures_util::StreamExt;
    use std::sync::{Arc, Mutex};

    fn live_client() -> Option<TowerClient> {
        let url = std::env::var("HANDY_TOWER_IT_URL").ok()?;
        let token = std::env::var("HANDY_TOWER_IT_TOKEN").ok()?;
        Some(TowerClient::new(&url, &token).expect("client"))
    }

    /// Collects `state` events from `/events` in the background.
    fn collect_events(
        client: &TowerClient,
    ) -> (
        Arc<Mutex<Vec<Snapshot>>>,
        tauri::async_runtime::JoinHandle<()>,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let req = client.events_request();
        let handle = tauri::async_runtime::spawn(async move {
            let resp = req.send().await.expect("events connect");
            assert!(
                resp.status().is_success(),
                "events status {}",
                resp.status()
            );
            let mut stream = resp.bytes_stream();
            let mut parser = SseParser::default();
            while let Some(Ok(chunk)) = stream.next().await {
                for ev in parser.feed(&chunk) {
                    if ev.event == "state" {
                        if let Ok(s) = serde_json::from_str::<Snapshot>(&ev.data) {
                            sink.lock().unwrap().push(s);
                        }
                    }
                }
            }
        });
        (seen, handle)
    }

    fn wait_for(seen: &Arc<Mutex<Vec<Snapshot>>>, pred: impl Fn(&Snapshot) -> bool) -> bool {
        for _ in 0..50 {
            if seen.lock().unwrap().iter().any(&pred) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn rt<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }

    #[test]
    fn live_toggle_cancel_and_events() {
        let Some(client) = live_client() else {
            eprintln!("skipped: HANDY_TOWER_IT_URL / HANDY_TOWER_IT_TOKEN not set");
            return;
        };

        // Start from idle whatever the previous run left behind.
        rt(client.cancel()).expect("cancel works");

        let health = rt(client.health()).expect("health");
        eprintln!("health: {health:?}");
        assert!(health.asr_loaded, "ASR must be loaded for this test");

        let (seen, events) = collect_events(&client);
        assert!(
            wait_for(&seen, |s| s.state == ServerState::Idle),
            "the first event is the current (idle) state"
        );

        // Toggle start -> speak (the test mic plays a WAV) -> toggle stop.
        let session = match rt(client.toggle(Mode::Clean)).expect("toggle start") {
            Toggled::Started { session } => session,
            Toggled::Stopped(_) => panic!("expected started"),
        };
        assert!(!session.is_empty());
        assert!(
            wait_for(&seen, |s| s.state == ServerState::Recording
                && s.session.as_deref() == Some(&session)),
            "SSE reports recording for our session"
        );
        std::thread::sleep(Duration::from_secs(6));
        let stopped = match rt(client.toggle(Mode::Clean)).expect("toggle stop") {
            Toggled::Stopped(s) => s,
            Toggled::Started { .. } => panic!("expected stopped"),
        };
        eprintln!(
            "stopped: {} chars, cleanup_failed={}",
            stopped.text.chars().count(),
            stopped.cleanup_failed
        );
        assert!(
            !stopped.text.trim().is_empty(),
            "the test mic produced speech"
        );
        assert!(
            wait_for(&seen, |s| s.state == ServerState::Finishing),
            "SSE reports finishing"
        );
        assert!(
            wait_for(&seen, |s| s.state == ServerState::Idle
                && s.reason.as_deref() == Some("stopped")),
            "SSE reports idle/stopped"
        );

        // Raw push-to-talk path: start, stop.
        let session = rt(client.start(Mode::Raw)).expect("start raw");
        assert!(!session.is_empty());
        std::thread::sleep(Duration::from_secs(3));
        let raw = rt(client.stop(Mode::Raw)).expect("stop raw");
        eprintln!("raw: {} chars", raw.text.chars().count());
        assert!(!raw.cleanup_failed);

        // Cancel discards a recording; a stop afterwards finds nothing.
        match rt(client.toggle(Mode::Clean)).expect("toggle start 2") {
            Toggled::Started { .. } => {}
            Toggled::Stopped(_) => panic!("expected started"),
        }
        std::thread::sleep(Duration::from_secs(1));
        assert!(rt(client.cancel()).expect("cancel"), "cancel discarded it");
        assert!(
            wait_for(&seen, |s| s.state == ServerState::Idle
                && s.reason.as_deref() == Some("cancelled")),
            "SSE reports cancelled"
        );
        assert_eq!(
            error_message(&rt(client.stop(Mode::Clean)).unwrap_err()),
            None,
            "stop after cancel is a silent not_recording"
        );
        assert!(!rt(client.cancel()).expect("cancel again"), "idempotent");
        assert_eq!(rt(client.state()).expect("state").state, ServerState::Idle);

        events.abort();
    }

    #[test]
    fn live_wrong_token_is_unauthorized() {
        let Some(client) = live_client() else {
            eprintln!("skipped: HANDY_TOWER_IT_URL / HANDY_TOWER_IT_TOKEN not set");
            return;
        };
        let bad = TowerClient::new(&client.base, "definitely-wrong").unwrap();
        let err = tauri::async_runtime::block_on(bad.toggle(Mode::Clean)).unwrap_err();
        assert!(err.is_api(401, "unauthorized"), "{err:?}");
        assert_eq!(error_message(&err), Some(TowerMessage::Unauthorized));
    }
}
