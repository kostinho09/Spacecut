use std::path::{Path, PathBuf};
use std::process::Command;

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Keeps a console window from flashing up for every ffmpeg run on Windows.
pub fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// "HH:MM:SS" (also "MM:SS" or plain seconds) to seconds.
pub fn parse_hms(t: &str) -> Option<u64> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0u64;
    for p in parts {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        total = total * 60 + p.parse::<u64>().ok()?;
    }
    Some(total)
}

pub fn fmt_hms(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// A file name the user typed, made safe to write.
pub fn clean_name(name: &str) -> String {
    let forbidden: &[char] = if cfg!(windows) {
        &['/', '\\', ':', '*', '?', '"', '<', '>', '|']
    } else {
        &['/', '\\', ':']
    };
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if forbidden.contains(&c) || c.is_control() { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cfg!(windows) && reserved_on_windows(&cleaned) {
        format!("{cleaned}_")
    } else {
        cleaned
    }
}

/// Names Windows keeps for devices, whatever follows a dot: a file called
/// "CON" or "nul.m4a" can't be made (or vanishes, for NUL).
fn reserved_on_windows(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_timestamps() {
        assert_eq!(parse_hms("01:30:45"), Some(5445));
        assert_eq!(parse_hms("2:35:00"), Some(9300));
        assert_eq!(parse_hms("90"), Some(90));
        assert_eq!(parse_hms("HH:MM:SS"), None);
        assert_eq!(parse_hms(""), None);
        assert_eq!(fmt_hms(9300), "02:35:00");
    }

    #[test]
    fn cleans_file_names() {
        assert_eq!(clean_name("  part 1/2: intro. "), "part 1-2- intro");
    }

    #[test]
    fn knows_the_names_windows_keeps() {
        for name in ["CON", "con", "Nul", "nul.part", "PRN", "aux", "COM1", "lpt9", "CONIN$"] {
            assert!(reserved_on_windows(name), "{name}");
        }
        for name in ["CONTENT", "console", "Null", "COM0", "COM10", "LPT", "my nul", "Space 1"] {
            assert!(!reserved_on_windows(name), "{name}");
        }
    }
}
