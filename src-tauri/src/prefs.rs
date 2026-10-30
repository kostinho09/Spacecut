//! Same files the Python app used, so the output folder and the sound
//! effects carry over.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::home_dir;

fn prefs_path() -> PathBuf {
    home_dir().join(".spacecutter_prefs.json")
}

fn effects_path() -> PathBuf {
    home_dir().join(".spacecutter_effects.json")
}

pub fn load() -> Map<String, Value> {
    fs::read_to_string(prefs_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn set(key: &str, value: Value) {
    let mut prefs = load();
    prefs.insert(key.to_string(), value);
    if let Ok(s) = serde_json::to_string(&prefs) {
        let _ = fs::write(prefs_path(), s);
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Effect {
    pub name: String,
    pub path: String,
}

pub fn load_effects() -> Vec<Effect> {
    fs::read_to_string(effects_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_effects(effects: &[Effect]) {
    if let Ok(s) = serde_json::to_string(effects) {
        let _ = fs::write(effects_path(), s);
    }
}
