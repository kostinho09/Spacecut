//! Asks GitHub, at most once a day, whether a newer Spacecut is out. It is
//! the same request as opening the Releases page: nothing about the user is
//! sent. In between checks it answers from what the last one found. A newer
//! version shows as a small tag at the top of the window, and nothing more.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::prefs;

const REPO: &str = "kostinho09/SpaceCutter";
const DAY: u64 = 24 * 60 * 60;

#[derive(Serialize, Debug, PartialEq)]
pub struct Update {
    /// The release's tag, such as "v2.1.0".
    pub version: String,
    pub url: String,
}

/// "v2.1.0" (or "2.1", "2.1.0-beta") as numbers to compare.
fn numbers(v: &str) -> Option<[u64; 3]> {
    let core: String = v.trim().trim_start_matches(['v', 'V']).chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let mut out = [0; 3];
    for (i, part) in core.split('.').take(3).enumerate() {
        out[i] = part.parse().ok()?;
    }
    Some(out)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The tag of the newest published release, or None without an answer.
fn latest_release() -> Option<String> {
    let body = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", concat!("Spacecut/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let release: Value = serde_json::from_str(&body).ok()?;
    let tag = release.get("tag_name")?.as_str()?;
    // Only a plain tag goes into the link the app opens
    tag.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c)).then(|| tag.to_string())
}

fn release_url(tag: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/{tag}")
}

/// What to tell the user, from the settings: a release newer than `current`
/// that they have not hidden.
fn decide(prefs: &Map<String, Value>, current: &str) -> Option<Update> {
    let latest = prefs.get("update_latest")?.as_str()?;
    let dismissed = prefs.get("update_dismissed").and_then(Value::as_str);
    (numbers(latest)? > numbers(current)? && dismissed != Some(latest)).then(|| Update { version: latest.to_string(), url: release_url(latest) })
}

/// What "Check for Updates…" in the menu found.
#[derive(Debug, PartialEq)]
pub enum Found {
    Newer(Update),
    UpToDate,
    /// GitHub didn't answer (no internet, say).
    NoAnswer,
}

/// Asks GitHub now, whatever the last check found or the user hid, and
/// keeps the answer for the window's own daily check.
pub fn check_now(current: &str) -> Found {
    let latest = latest_release();
    if let Some(tag) = &latest {
        prefs::set("update_latest", json!(tag));
        prefs::set("update_checked", json!(now()));
    }
    compare(latest, current)
}

fn compare(latest: Option<String>, current: &str) -> Found {
    let Some(tag) = latest else { return Found::NoAnswer };
    match (numbers(&tag), numbers(current)) {
        (Some(theirs), Some(ours)) if theirs > ours => Found::Newer(Update { url: release_url(&tag), version: tag }),
        _ => Found::UpToDate,
    }
}

/// A newer version to point the user to, if there is one.
pub fn check(current: &str) -> Option<Update> {
    let last = prefs::load().get("update_checked").and_then(Value::as_u64);
    if last.is_none_or(|t| now().saturating_sub(t) >= DAY) {
        // Without an answer (no internet, say) it tries again next time
        if let Some(tag) = latest_release() {
            prefs::set("update_latest", json!(tag));
            prefs::set("update_checked", json!(now()));
        }
    }
    decide(&prefs::load(), current)
}

/// The user hid the tag: say nothing more about this version.
pub fn dismiss(version: &str) {
    prefs::set("update_dismissed", json!(version));
}

/// Opens a release page in the default browser; only this repository's.
pub fn open(url: &str) -> Result<(), String> {
    if !url.starts_with(&format!("https://github.com/{REPO}/releases/")) {
        return Err("Not a Spacecut release".into());
    }
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        std::process::Command::new("explorer")
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url);
    crate::util::quiet(&mut cmd);
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(latest: &str, dismissed: Option<&str>) -> Map<String, Value> {
        let mut p = Map::new();
        p.insert("update_latest".into(), json!(latest));
        if let Some(d) = dismissed {
            p.insert("update_dismissed".into(), json!(d));
        }
        p
    }

    #[test]
    fn compares_versions() {
        assert_eq!(numbers("v2.1.0"), Some([2, 1, 0]));
        assert_eq!(numbers("2.1"), Some([2, 1, 0]));
        assert_eq!(numbers("v2.10.0-beta"), Some([2, 10, 0]));
        assert!(numbers("v2.10.0") > numbers("v2.9.3"));
        assert_eq!(numbers("latest"), None);
    }

    #[test]
    fn tells_only_of_newer_versions_not_hidden() {
        let newer = decide(&prefs("v2.1.0", None), "2.0.0").expect("2.1.0 is newer");
        assert_eq!(newer.url, "https://github.com/kostinho09/SpaceCutter/releases/tag/v2.1.0");
        assert_eq!(decide(&prefs("v1.1.1", None), "2.0.0"), None, "older");
        assert_eq!(decide(&prefs("v2.0.0", None), "2.0.0"), None, "the same");
        assert_eq!(decide(&prefs("v2.1.0", Some("v2.1.0")), "2.0.0"), None, "hidden");
        assert!(decide(&prefs("v2.2.0", Some("v2.1.0")), "2.0.0").is_some(), "a later one shows again");
        assert_eq!(decide(&Map::new(), "2.0.0"), None, "never checked");
    }

    #[test]
    fn the_menu_tells_what_it_found_even_if_hidden() {
        let newer = Update { version: "v2.1.0".into(), url: "https://github.com/kostinho09/SpaceCutter/releases/tag/v2.1.0".into() };
        assert_eq!(compare(Some("v2.1.0".into()), "2.0.0"), Found::Newer(newer));
        assert_eq!(compare(Some("v1.1.1".into()), "2.0.0"), Found::UpToDate, "the newest release is older");
        assert_eq!(compare(Some("v2.0.0".into()), "2.0.0"), Found::UpToDate);
        assert_eq!(compare(None, "2.0.0"), Found::NoAnswer);
    }

    /// Needs the network.
    #[test]
    #[ignore]
    fn asks_github() {
        let tag = latest_release().expect("an answer from GitHub");
        println!("latest release: {tag}");
        assert!(numbers(&tag).is_some());
    }

    #[test]
    fn opens_only_release_pages() {
        assert!(open("https://example.com/").is_err());
        assert!(open("https://github.com/someone/else/releases/tag/v1").is_err());
    }
}
