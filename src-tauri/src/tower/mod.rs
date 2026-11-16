//! handy-tower: the "tower" dictation backend (see TOWER.md).
//!
//! One USB mic is plugged into the tower server, which owns the single
//! recording session. In tower mode this app never opens a local audio device
//! or loads a model: hotkeys become HTTP calls, `GET /events` drives the
//! overlay and the Esc binding, and whichever machine calls stop pastes the
//! returned text through Handy's own paste path.
//!
//! Upstream files only call the small hooks at the top of this module, so a
//! rebase on upstream touches as little as possible.

pub mod client;
pub mod events;
pub mod hotkeys;
mod overlay;
pub mod settings;

use client::{Mode, Outcome, Stopped, TowerClient, TowerMessage};
use events::{ServerState, ServerView, Snapshot};
use hotkeys::{Input, TowerKey};
use settings::{TowerBackend, TowerSettingsView};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

use crate::notices::{self, Notice, NoticeCode, NoticeSource};
use crate::settings::KeyboardImplementation;
use crate::tray::{set_tray_state, TrayIconState};

// ============================================================================
// Hooks called from upstream code
// ============================================================================

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether the tower backend is selected. Cheap; safe from any thread.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// The fork never writes transcripts or recordings, in either backend.
pub fn persistence_disabled() -> bool {
    true
}

/// Upstream's self-updater would replace this fork with stock Handy.
pub fn updater_disabled() -> bool {
    true
}

/// Which bindings the shortcut backends register right now.
pub fn binding_enabled(id: &str) -> bool {
    match id {
        hotkeys::DICTATE_BINDING | hotkeys::RAW_BINDING => is_active(),
        "transcribe" | "transcribe_with_post_process" => !is_active(),
        _ => true,
    }
}

/// Shortcut events. Returns `true` when the tower handled (or swallowed) it.
pub fn handle_shortcut(app: &AppHandle, binding_id: &str, is_pressed: bool) -> bool {
    if let Some(key) = TowerKey::from_binding(binding_id) {
        if is_active() {
            send(
                app,
                Input::Key {
                    key,
                    pressed: is_pressed,
                    at: Instant::now(),
                },
            );
        }
        return true;
    }
    if !is_active() {
        return false;
    }
    match binding_id {
        "cancel" => {
            if is_pressed {
                cancel_session(app);
            }
            true
        }
        // Not registered in tower mode; never start local capture.
        "transcribe" | "transcribe_with_post_process" => true,
        _ => false,
    }
}

/// CLI `--toggle-transcription` / SIGUSR2. Returns `true` when handled.
pub fn handle_external_toggle(app: &AppHandle) -> bool {
    if !is_active() {
        return false;
    }
    send(app, Input::ExternalToggle);
    true
}

/// Esc, the overlay's ✕, the tray's Cancel and `--cancel`. Returns `true`
/// when handled.
pub fn cancel(app: &AppHandle) -> bool {
    if !is_active() {
        return false;
    }
    cancel_session(app);
    true
}

/// Before the managers exist: read the backend and make sure the local mic
/// can't be opened by an always-on setting.
pub fn prepare(app: &AppHandle) {
    let s = settings::load(app);
    ACTIVE.store(s.backend == TowerBackend::Tower, Ordering::SeqCst);
    app.manage(Runtime::default());
    if is_active() {
        log::info!("Dictation backend: tower ({})", s.url);
        disable_local_capture_settings(app);
    } else {
        log::info!("Dictation backend: local");
    }
}

/// After the overlay exists: connect.
pub fn start(app: &AppHandle) {
    if is_active() {
        start_runtime(app);
    }
}

/// Before shortcuts register: tower mode uses the RegisterHotKey (Windows) /
/// Carbon (macOS) backend. See TOWER.md, "Hotkeys".
pub fn enforce_keyboard_implementation(app: &AppHandle) {
    if !is_active() {
        return;
    }
    let mut s = crate::settings::get_settings(app);
    if s.keyboard_implementation == KeyboardImplementation::HandyKeys {
        log::info!("Tower backend: switching shortcuts from handy-keys to the tauri backend");
        s.keyboard_implementation = KeyboardImplementation::Tauri;
        crate::settings::write_settings(app, s);
    }
}

/// Refuses handy-keys while the tower backend is active.
pub fn check_keyboard_implementation(implementation: &str) -> Result<(), String> {
    if is_active() && implementation == "handy_keys" {
        return Err(
            "The tower backend needs the tauri shortcut backend (RegisterHotKey/Carbon); \
             handy-keys' low-level hook would also fire for keys Deskflow sends to the other \
             machine. See TOWER.md."
                .to_string(),
        );
    }
    Ok(())
}

// ============================================================================
// Runtime
// ============================================================================

#[derive(Default)]
struct Ui {
    /// The message on screen and the generation that put it there.
    message: Option<(TowerMessage, u64)>,
    generation: u64,
    /// Whether the tower put the overlay up (so idle may take it down).
    shown: bool,
}

#[derive(Default)]
struct Runtime {
    client: Mutex<Option<Arc<TowerClient>>>,
    view: Mutex<ServerView>,
    ui: Mutex<Ui>,
    keys: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Input>>>,
    actor: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    sse: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    wake: Arc<tokio::sync::Notify>,
    cancel_grabbed: AtomicBool,
    tray: AtomicU8,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn runtime(app: &AppHandle) -> Option<tauri::State<'_, Runtime>> {
    app.try_state::<Runtime>()
}

fn send(app: &AppHandle, input: Input) {
    let Some(rt) = runtime(app) else { return };
    if !lock(&rt.view).connected {
        // Don't wait out the backoff: the user wants the tower now.
        rt.wake.notify_one();
    }
    let keys = lock(&rt.keys);
    if let Some(tx) = keys.as_ref() {
        let _ = tx.send(input);
    }
}

fn start_runtime(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    let s = settings::load(app);
    let client = match TowerClient::new(&s.url, &s.token) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            log::error!("Tower client unavailable: {e}");
            return;
        }
    };
    *lock(&rt.client) = Some(Arc::clone(&client));

    {
        let mut keys = lock(&rt.keys);
        if keys.is_none() {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            *keys = Some(tx);
            *lock(&rt.actor) = Some(tauri::async_runtime::spawn(hotkeys::run(app.clone(), rx)));
        }
    }

    if let Some(old) = lock(&rt.sse).take() {
        old.abort();
    }
    on_disconnected(app);
    if client.has_token() {
        let handle =
            tauri::async_runtime::spawn(events::run(app.clone(), client, Arc::clone(&rt.wake)));
        *lock(&rt.sse) = Some(handle);
    } else {
        log::warn!("Tower backend has no token yet; set it in Settings > Tower");
    }
}

fn stop_runtime(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    if let Some(old) = lock(&rt.sse).take() {
        old.abort();
    }
    lock(&rt.keys).take();
    if let Some(actor) = lock(&rt.actor).take() {
        actor.abort();
    }
    lock(&rt.client).take();
    *lock(&rt.view) = ServerView::default();
    lock(&rt.ui).message = None;
    render(app);
    sync_session_side_effects(app);
    emit_status(app, false);
}

/// The client for a user-initiated request, or `None` after telling the user
/// why there isn't one.
fn client_for_request(app: &AppHandle) -> Option<Arc<TowerClient>> {
    let rt = runtime(app)?;
    let client = lock(&rt.client).clone();
    let client = client?;
    if !client.has_token() {
        show_message(app, TowerMessage::NoToken);
        return None;
    }
    Some(client)
}

fn session_ended(app: &AppHandle, session: &str) -> bool {
    let Some(rt) = runtime(app) else {
        return false;
    };
    let ended = lock(&rt.view).session_ended(session);
    ended
}

fn cancel_session(app: &AppHandle) {
    let Some(client) = client_for_request(app) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match client.cancel().await {
            Ok(cancelled) => {
                log::info!("Tower session cancel requested (cancelled: {cancelled})");
                if let Some(rt) = runtime(&app) {
                    // The stream confirms with an idle event; without one,
                    // forget our own unconfirmed state now.
                    lock(&rt.view).set_local(None);
                }
                render(&app);
                sync_session_side_effects(&app);
            }
            Err(e) => {
                log::warn!("Tower cancel failed: {e}");
                if let Some(m) = client::error_message(&e) {
                    show_message(&app, m);
                }
            }
        }
    });
}

// ----------------------------------------------------------------------------
// Event-stream callbacks (events.rs)
// ----------------------------------------------------------------------------

fn on_connected(app: &AppHandle) {
    if let Some(rt) = runtime(app) {
        lock(&rt.view).connected = true;
    }
    emit_status(app, true);
}

fn on_disconnected(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    let was = {
        let mut view = lock(&rt.view);
        let was = view.connected;
        view.set_disconnected();
        was
    };
    render(app);
    sync_session_side_effects(app);
    if was {
        emit_status(app, false);
    }
}

fn on_server_state(app: &AppHandle, snap: Snapshot) {
    let Some(rt) = runtime(app) else { return };
    log::debug!(
        "Tower state: {:?} ({})",
        snap.state,
        snap.reason.as_deref().unwrap_or("-")
    );
    // A session we were showing failed on the server with nobody waiting on
    // a stop (e.g. transcribing an auto-stopped take): say so on both screens.
    let failed = snap.state == ServerState::Idle && snap.reason.as_deref() == Some("error");
    let was_active = {
        let mut view = lock(&rt.view);
        let was_active = view.current.as_ref().is_some_and(Snapshot::is_active);
        view.apply(snap);
        was_active
    };
    if failed && was_active {
        show_message(app, TowerMessage::ServerError);
    } else {
        render(app);
    }
    sync_session_side_effects(app);
}

// ----------------------------------------------------------------------------
// Hotkey-actor callbacks (hotkeys.rs)
// ----------------------------------------------------------------------------

fn on_started(app: &AppHandle, session: &str, mode: Mode) {
    let Some(rt) = runtime(app) else { return };
    {
        let mut view = lock(&rt.view);
        // Show it now unless the stream already told us (it usually has).
        if !view.knows(session) {
            view.set_local(Some(Snapshot {
                state: ServerState::Recording,
                session: Some(session.to_string()),
                mode: Some(mode.as_str().to_string()),
                elapsed_ms: 0,
                max_ms: 0,
                reason: Some("started".into()),
            }));
        }
    }
    lock(&rt.ui).message = None;
    render(app);
    sync_session_side_effects(app);
}

fn on_stopping(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    {
        let mut view = lock(&rt.view);
        if !view.connected {
            view.set_local(Some(Snapshot {
                state: ServerState::Finishing,
                session: None,
                mode: None,
                elapsed_ms: 0,
                max_ms: 0,
                reason: Some("stopping".into()),
            }));
        }
    }
    render(app);
}

/// Handles the result of a stop (or a toggle that stopped).
fn deliver(app: &AppHandle, result: Result<Stopped, client::ClientError>) {
    if let Some(rt) = runtime(app) {
        let mut view = lock(&rt.view);
        if !view.connected {
            view.set_local(None);
        }
    }
    match client::classify(result) {
        Outcome::Paste {
            text,
            cleanup_failed,
        } => paste(app, text, cleanup_failed),
        Outcome::Message(m) => show_message(app, m),
        Outcome::Quiet => render(app),
    }
    sync_session_side_effects(app);
}

/// Pastes through Handy's own path (clipboard snapshot/restore, history-
/// exclusion formats), off the async runtime: paste sleeps between steps.
fn paste(app: &AppHandle, text: String, cleanup_failed: bool) {
    render(app);
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("tower-paste".into())
        .spawn(move || match crate::utils::paste(text, app.clone()) {
            Ok(crate::utils::PasteOutcome::Pasted) => {
                notices::clear(&app, NoticeSource::Dictation);
                if cleanup_failed {
                    show_message(&app, TowerMessage::CleanupFailed);
                }
            }
            Ok(crate::utils::PasteOutcome::Blocked) => notices::raise(
                &app,
                Notice::new(NoticeCode::PasteBlockedElevated, NoticeSource::Dictation),
            ),
            Err(e) => {
                log::error!("Failed to paste the tower transcript: {e}");
                notices::raise(
                    &app,
                    Notice::new(NoticeCode::PasteFailed, NoticeSource::Dictation),
                );
            }
        });
    if let Err(e) = spawned {
        log::error!("Failed to start the paste thread: {e}");
    }
}

// ----------------------------------------------------------------------------
// Overlay, tray, Esc
// ----------------------------------------------------------------------------

fn show_message(app: &AppHandle, message: TowerMessage) {
    let Some(rt) = runtime(app) else { return };
    let generation = {
        let mut ui = lock(&rt.ui);
        ui.generation += 1;
        ui.message = Some((message, ui.generation));
        ui.generation
    };
    render(app);
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(overlay::MESSAGE_HOLD);
        let Some(rt) = runtime(&app) else { return };
        {
            let mut ui = lock(&rt.ui);
            if !matches!(ui.message, Some((_, g)) if g == generation) {
                return; // a newer message took over
            }
            ui.message = None;
        }
        render(&app);
    });
}

fn render(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    let view = lock(&rt.view);
    let mut ui = lock(&rt.ui);
    let payload = overlay::decide(&view, ui.message.map(|(m, _)| m), Instant::now());
    drop(view);
    overlay::apply(app, payload, &mut ui.shown);
}

/// Esc grab and tray icon follow the session.
fn sync_session_side_effects(app: &AppHandle) {
    let Some(rt) = runtime(app) else { return };
    let (active, tray) = {
        let view = lock(&rt.view);
        let state = view.display().map(|(s, _)| s.state);
        let tray = match state {
            Some(ServerState::Recording | ServerState::Held) => TrayIconState::Recording,
            Some(ServerState::Finishing) => TrayIconState::Transcribing,
            _ => TrayIconState::Idle,
        };
        (view.session_active() && is_active(), tray)
    };

    if rt.cancel_grabbed.swap(active, Ordering::SeqCst) != active {
        if active {
            crate::shortcut::register_cancel_shortcut(app);
        } else {
            crate::shortcut::unregister_cancel_shortcut(app);
        }
    }

    let code = match tray {
        TrayIconState::Idle => 0,
        TrayIconState::Recording => 1,
        TrayIconState::Transcribing => 2,
    };
    if rt.tray.swap(code, Ordering::SeqCst) != code {
        set_tray_state(app, tray);
    }
}

fn emit_status(app: &AppHandle, connected: bool) {
    let _ = app.emit("tower-status", connected);
}

fn disable_local_capture_settings(app: &AppHandle) {
    let mut s = crate::settings::get_settings(app);
    if s.always_on_microphone {
        log::info!("Tower backend: turning off the always-on local microphone");
        s.always_on_microphone = false;
        crate::settings::write_settings(app, s);
    }
}

// ============================================================================
// Commands
// ============================================================================

fn connected(app: &AppHandle) -> bool {
    let Some(rt) = runtime(app) else {
        return false;
    };
    let connected = lock(&rt.view).connected;
    connected
}

fn view(app: &AppHandle) -> TowerSettingsView {
    settings::load(app).view(connected(app))
}

#[tauri::command]
#[specta::specta]
pub fn get_tower_settings(app: AppHandle) -> TowerSettingsView {
    view(&app)
}

/// Saves the URL and, when given, the token (`""` clears it), then reconnects.
#[tauri::command]
#[specta::specta]
pub fn set_tower_connection(
    app: AppHandle,
    url: String,
    token: Option<String>,
) -> Result<TowerSettingsView, String> {
    let url = settings::normalize_url(&url)?;
    let mut s = settings::load(&app);
    s.url = url;
    if let Some(token) = token {
        s.token = token.trim().to_string();
    }
    settings::save(&app, &s);
    log::info!("Tower connection settings saved ({:?})", s);
    if is_active() {
        start_runtime(&app);
    }
    Ok(view(&app))
}

#[tauri::command]
#[specta::specta]
pub fn set_tower_backend(
    app: AppHandle,
    backend: TowerBackend,
) -> Result<TowerSettingsView, String> {
    let mut s = settings::load(&app);
    let to_tower = backend == TowerBackend::Tower;
    if s.backend == backend && is_active() == to_tower {
        return Ok(view(&app));
    }
    log::info!("Switching dictation backend to {:?}", backend);

    if to_tower {
        // End any local take before the local pipeline goes dark.
        crate::utils::cancel_current_operation(&app);
    } else {
        stop_runtime(&app);
    }

    let shortcuts_ready = app
        .try_state::<crate::commands::ShortcutsInitialized>()
        .is_some();
    let swapped = [
        "transcribe",
        "transcribe_with_post_process",
        hotkeys::DICTATE_BINDING,
        hotkeys::RAW_BINDING,
    ];
    let bindings = crate::settings::get_bindings(&app);
    if shortcuts_ready {
        for id in swapped {
            if let Some(b) = bindings.get(id) {
                if binding_enabled(id) {
                    let _ = crate::shortcut::unregister_shortcut(&app, b.clone());
                }
            }
        }
    }

    ACTIVE.store(to_tower, Ordering::SeqCst);
    s.backend = backend;
    settings::save(&app, &s);

    if to_tower {
        disable_local_capture_settings(&app);
        // Close a mic left open by always-on mode.
        let rm = app.state::<Arc<crate::managers::audio::AudioRecordingManager>>();
        if let Err(e) = rm.update_mode(crate::managers::audio::MicrophoneMode::OnDemand) {
            log::warn!("Could not close the local microphone: {e}");
        }
    }

    if shortcuts_ready {
        let implementation = crate::settings::get_settings(&app).keyboard_implementation;
        if to_tower && implementation == KeyboardImplementation::HandyKeys {
            // Re-registers every enabled binding with the tauri backend.
            crate::shortcut::change_keyboard_implementation_setting(app.clone(), "tauri".into())?;
        } else {
            let post_process = crate::settings::get_settings(&app).post_process_enabled;
            for id in swapped {
                if id == "transcribe_with_post_process" && !post_process {
                    continue;
                }
                if let Some(b) = bindings.get(id) {
                    if binding_enabled(id) {
                        if let Err(e) = crate::shortcut::register_shortcut(&app, b.clone()) {
                            log::warn!("Could not register '{id}': {e}");
                        }
                    }
                }
            }
        }
    }

    if to_tower {
        start_runtime(&app);
    }
    Ok(view(&app))
}

/// `GET /health` with the saved settings, for the settings screen.
#[tauri::command]
#[specta::specta]
pub async fn tower_check_connection(app: AppHandle) -> Result<client::TowerHealth, String> {
    let s = settings::load(&app);
    if s.token.is_empty() {
        return Err("No token saved yet.".into());
    }
    let client = TowerClient::new(&s.url, &s.token)?;
    client.health().await.map_err(|e| match e {
        client::ClientError::Api { status: 401, .. } => "The tower rejected the token.".into(),
        client::ClientError::Network(_) | client::ClientError::Timeout => {
            format!("Can't reach the tower at {}.", s.url)
        }
        other => format!("The tower answered unexpectedly ({other})."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_follow_the_backend() {
        ACTIVE.store(false, Ordering::SeqCst);
        assert!(binding_enabled("transcribe"));
        assert!(!binding_enabled(hotkeys::DICTATE_BINDING));
        assert!(binding_enabled("cancel"));
        ACTIVE.store(true, Ordering::SeqCst);
        assert!(!binding_enabled("transcribe"));
        assert!(!binding_enabled("transcribe_with_post_process"));
        assert!(binding_enabled(hotkeys::DICTATE_BINDING));
        assert!(binding_enabled(hotkeys::RAW_BINDING));
        assert!(check_keyboard_implementation("handy_keys").is_err());
        assert!(check_keyboard_implementation("tauri").is_ok());
        ACTIVE.store(false, Ordering::SeqCst);
        assert!(check_keyboard_implementation("handy_keys").is_ok());
    }

    #[test]
    fn nothing_persists_and_the_updater_is_off() {
        assert!(persistence_disabled());
        assert!(updater_disabled());
    }
}
