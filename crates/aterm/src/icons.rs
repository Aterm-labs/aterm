//! Per-session and per-project emoji icons.
//!
//! Native-only (the extension keeps these in its `globalState`, which isn't
//! shared), so they live in their own `~/.config/aterm/icons.json` and never
//! touch the metadata files that ARE shared with the extension/sidecar.
//!
//! A global, lazily-loaded store mirroring `settings.rs`, so any UI site can
//! read or set an icon without threading state through the panel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(default)]
struct Icons {
    /// `provider:id` -> emoji.
    sessions: HashMap<String, String>,
    /// absolute project path -> emoji.
    projects: HashMap<String, String>,
}

static STORE: LazyLock<RwLock<Icons>> = LazyLock::new(|| RwLock::new(load_from_disk()));

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".config/aterm/icons.json")
}

fn load_from_disk() -> Icons {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save(icons: &Icons) {
    let p = path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(icons) {
        let _ = std::fs::write(p, json);
    }
}

/// The emoji for a session `provider:id`, if set.
pub fn session(key: &str) -> Option<String> {
    STORE.read().unwrap().sessions.get(key).cloned()
}

/// Set (or clear, when empty) the emoji for a session.
pub fn set_session(key: &str, emoji: &str) {
    let mut s = STORE.write().unwrap();
    let e = emoji.trim();
    if e.is_empty() {
        s.sessions.remove(key);
    } else {
        s.sessions.insert(key.to_string(), e.to_string());
    }
    save(&s);
}

/// The emoji for a project path, if set.
pub fn project(path: &str) -> Option<String> {
    STORE.read().unwrap().projects.get(path).cloned()
}

/// Set (or clear, when empty) the emoji for a project path.
pub fn set_project(path: &str, emoji: &str) {
    let mut s = STORE.write().unwrap();
    let e = emoji.trim();
    if e.is_empty() {
        s.projects.remove(path);
    } else {
        s.projects.insert(path.to_string(), e.to_string());
    }
    save(&s);
}
