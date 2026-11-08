//! The settings and the sound effects, in Spacecut's own files. The first
//! time, they start as a copy of the files the older SpaceCutter (1.x)
//! kept, so the output folder and the effects carry over; those files are
//! left as they are for 1.x.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::util::home_dir;

/// One of Spacecut's files in the home folder, started from SpaceCutter's
/// if there is one and Spacecut's isn't there yet.
fn own_file(name: &str) -> PathBuf {
    let path = home_dir().join(format!(".spacecut_{name}.json"));
    if !path.exists() {
        let old = home_dir().join(format!(".spacecutter_{name}.json"));
        if old.exists() {
            let _ = fs::copy(&old, &path);
        }
    }
    path
}

fn prefs_path() -> PathBuf {
    own_file("prefs")
}

fn effects_path() -> PathBuf {
    own_file("effects")
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
