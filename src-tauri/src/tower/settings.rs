//! Tower settings, kept in their own store (`tower_settings.json`) so the fork
//! never touches upstream's `AppSettings` schema and the token is never part of
//! the settings dump upstream logs at startup.
//!
//! v1 keeps the token in this plain store file in the app data directory; see
//! TOWER.md for the trade-off.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use tauri_plugin_store::StoreExt;

pub const DEFAULT_URL: &str = "http://192.168.1.182:8790";
const STORE_PATH: &str = "tower_settings.json";
const STORE_KEY: &str = "tower";

/// Where dictation audio is captured and transcribed.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "lowercase")]
pub enum TowerBackend {
    /// Stock Handy: local microphone and local models.
    Local,
    /// The tower service owns the mic and the session; this app only sends
    /// hotkey intents and pastes the returned text.
    #[default]
    Tower,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(default)]
pub struct TowerSettings {
    pub backend: TowerBackend,
    pub url: String,
    pub token: String,
}

impl Default for TowerSettings {
    fn default() -> Self {
        Self {
            backend: TowerBackend::default(),
            url: DEFAULT_URL.to_string(),
            token: String::new(),
        }
    }
}

// Hand-written so the token can never reach a log line through `{:?}`.
impl std::fmt::Debug for TowerSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TowerSettings")
            .field("backend", &self.backend)
            .field("url", &self.url)
            .field("token_set", &!self.token.is_empty())
            .finish()
    }
}

/// What the frontend sees. The token itself never leaves the backend.
#[derive(Serialize, Clone, Debug, Type)]
pub struct TowerSettingsView {
    pub backend: TowerBackend,
    pub url: String,
    pub has_token: bool,
    pub default_url: String,
    /// Whether the `/events` stream is currently connected.
    pub connected: bool,
}

impl TowerSettings {
    pub fn view(&self, connected: bool) -> TowerSettingsView {
        TowerSettingsView {
            backend: self.backend,
            url: self.url.clone(),
            has_token: !self.token.is_empty(),
            default_url: DEFAULT_URL.to_string(),
            connected,
        }
    }
}

pub fn load(app: &AppHandle) -> TowerSettings {
    let Ok(store) = app.store(crate::portable::store_path(STORE_PATH)) else {
        log::warn!("Tower settings store unavailable; using defaults");
        return TowerSettings::default();
    };
    match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value(value).unwrap_or_else(|e| {
            log::warn!("Stored tower settings are invalid ({e}); using defaults");
            TowerSettings::default()
        }),
        None => {
            let defaults = TowerSettings::default();
            save(app, &defaults);
            defaults
        }
    }
}

pub fn save(app: &AppHandle, settings: &TowerSettings) {
    match app.store(crate::portable::store_path(STORE_PATH)) {
        Ok(store) => match serde_json::to_value(settings) {
            Ok(value) => store.set(STORE_KEY, value),
            Err(e) => log::error!("Failed to serialize tower settings: {e}"),
        },
        Err(e) => log::error!("Failed to open the tower settings store: {e}"),
    }
}

/// Trims and checks a base URL: `http(s)://host[:port]`, no trailing slash.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let url = raw.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| "The URL must start with http:// or https://".to_string())?;
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return Err("The URL needs a host, e.g. http://192.168.1.182:8790".to_string());
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_the_tower_backend_at_home() {
        let s = TowerSettings::default();
        assert_eq!(s.backend, TowerBackend::Tower);
        assert_eq!(s.url, DEFAULT_URL);
        assert!(s.token.is_empty());
    }

    #[test]
    fn a_partial_store_keeps_its_fields() {
        let s: TowerSettings =
            serde_json::from_value(serde_json::json!({ "backend": "local" })).unwrap();
        assert_eq!(s.backend, TowerBackend::Local);
        assert_eq!(s.url, DEFAULT_URL);
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let s = TowerSettings {
            token: "super-secret-value".into(),
            ..Default::default()
        };
        let printed = format!("{s:?}");
        assert!(!printed.contains("super-secret-value"));
        assert!(printed.contains("token_set: true"));
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(
            normalize_url(" http://192.168.1.182:8790/ ").unwrap(),
            "http://192.168.1.182:8790"
        );
        assert_eq!(
            normalize_url("https://tower.example").unwrap(),
            "https://tower.example"
        );
        assert!(normalize_url("192.168.1.182:8790").is_err());
        assert!(normalize_url("http://").is_err());
    }
}
