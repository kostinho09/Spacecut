use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use regex::Regex;

use crate::util::quiet;

pub const EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// The process a job is waiting on, so a cancel (or quitting) can stop it.
pub type Slot = Arc<Mutex<Option<Child>>>;

pub struct Output {
    pub ok: bool,
    pub stderr: String,
}

/// The bundled ffmpeg; in a development build, also the copy in the project
/// folder. Never one from the PATH (such as Homebrew's), so a build that is
/// missing its own ffmpeg says so instead of quietly working on this Mac only.
pub fn locate(resource_dir: Option<PathBuf>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = resource_dir {
        candidates.push(dir.join(EXE));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(EXE));
            candidates.push(dir.join("../Resources").join(EXE));
        }
    }
    if cfg!(debug_assertions) {
        let os = if cfg!(windows) { "windows" } else { "mac" };
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../ffmpeg").join(os).join(EXE));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Runs a command to the end, stopping it if `cancel` is raised. `tick` is
/// called a few times a second while it runs, with how far ffmpeg has got
/// (seconds of output written, from its "time=" reports).
pub fn run(cmd: Command, cancel: &AtomicBool, slot: &Slot, tick: impl FnMut(Option<f64>)) -> Output {
    execute(cmd, cancel, slot, tick, None)
}

/// As `run`, with `feed` writing what ffmpeg reads (`-i pipe:0`), on a thread
/// of its own. The flag it is given is raised once ffmpeg has finished, so it
/// can stop waiting; what it returns comes back with the output.
pub fn run_fed<T: Send + 'static>(
    cmd: Command,
    cancel: &AtomicBool,
    slot: &Slot,
    tick: impl FnMut(Option<f64>),
    feed: impl FnOnce(ChildStdin, Arc<AtomicBool>) -> T + Send + 'static,
) -> (Output, Option<T>) {
    let finished = Arc::new(AtomicBool::new(false));
    let (told, (tx, rx)) = (finished.clone(), mpsc::channel());
    let out = execute(cmd, cancel, slot, tick, Some(Box::new(move |stdin| {
        let _ = tx.send(feed(stdin, told));
    })));
    finished.store(true, Ordering::SeqCst);
    (out, rx.recv().ok())
}

type Feed = Box<dyn FnOnce(ChildStdin) + Send>;

fn execute(mut cmd: Command, cancel: &AtomicBool, slot: &Slot, mut tick: impl FnMut(Option<f64>), feed: Option<Feed>) -> Output {
    cmd.stdin(if feed.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::null()).stderr(Stdio::piped());
    quiet(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Output { ok: false, stderr: e.to_string() },
    };
    if let (Some(feed), Some(stdin)) = (feed, child.stdin.take()) {
        thread::spawn(move || feed(stdin));
    }
    let reached = Arc::new(Mutex::new(None));
    let stderr = child.stderr.take().map(|e| read_reports(e, reached.clone()));
    *slot.lock().unwrap() = Some(child);

    let status = loop {
        {
            let mut guard = slot.lock().unwrap();
            let Some(child) = guard.as_mut() else { break None };
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    *guard = None;
                    break Some(status);
                }
                Ok(None) => {}
                Err(_) => {
                    *guard = None;
                    break None;
                }
            }
        }
        tick(*reached.lock().unwrap());
        thread::sleep(Duration::from_millis(150));
    };

    let stderr = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    let ok = status.is_some_and(|s| s.success()) && !cancel.load(Ordering::SeqCst);
    Output { ok, stderr }
}

/// Collects ffmpeg's messages, noting the latest "time=" of its progress lines.
fn read_reports<R: Read + Send + 'static>(mut r: R, reached: Arc<Mutex<Option<f64>>>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            all.extend_from_slice(&buf[..n]);
            // Progress lines end in \r; look at the tail for the newest one
            let tail = String::from_utf8_lossy(&all[all.len().saturating_sub(600)..]).into_owned();
            if let Some(t) = tail.rsplit("time=").next().filter(|_| tail.contains("time=")).and_then(parse_clock) {
                *reached.lock().unwrap() = Some(t);
            }
        }
        String::from_utf8_lossy(&all).into_owned()
    })
}

/// "01:02:03.45 bitrate=…" → seconds (None for "N/A" or a negative start).
fn parse_clock(s: &str) -> Option<f64> {
    let clock = s.split_whitespace().next()?;
    let mut secs = 0.0;
    for part in clock.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    (secs >= 0.0).then_some(secs)
}

/// `-i` and the input, which may be a link from the internet: then ffmpeg
/// may follow it, and what it names (a playlist's pieces), only to the
/// internet, never to this computer's files. A local file is left as it was.
pub fn input(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    if input.starts_with("http://") || input.starts_with("https://") {
        args.extend(["-protocol_whitelist".to_string(), "http,https,tcp,tls,crypto".to_string()]);
    }
    args.extend(["-i".to_string(), input.to_string()]);
    args
}

/// What `ffmpeg -i` prints about a file, without decoding it.
fn describe(ffmpeg: &Path, input: &str) -> String {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").args(self::input(input)).stdin(Stdio::null()).stdout(Stdio::null());
    quiet(&mut cmd);
    cmd.output().map(|o| String::from_utf8_lossy(&o.stderr).into_owned()).unwrap_or_default()
}

/// Sample rate, channel count and bitrate of a file's audio, so everything a
/// merge joins can be encoded to match the first clip.
pub fn probe_audio(ffmpeg: &Path, path: &Path) -> (u32, u32, String) {
    let info = describe(ffmpeg, &path.to_string_lossy());
    let Some(line) = info.lines().find(|l| l.contains("Audio:")) else {
        return (44100, 2, "192k".into());
    };
    let rate = Regex::new(r"(\d+) Hz").unwrap().captures(line).and_then(|c| c[1].parse().ok()).unwrap_or(44100);
    let mono = Regex::new(r"Hz, ([^,]+)").unwrap().captures(line).is_some_and(|c| c[1].trim() == "mono");
    let bitrate = Regex::new(r"(\d+) kb/s")
        .unwrap()
        .captures(line)
        .and_then(|c| c[1].parse::<u32>().ok())
        .map(|k| format!("{}k", k.clamp(64, 320)))
        .unwrap_or_else(|| "192k".into());
    (rate, if mono { 1 } else { 2 }, bitrate)
}

/// Length of a file in seconds, when ffmpeg can tell.
pub fn probe_duration(ffmpeg: &Path, input: &str) -> Option<f64> {
    let info = describe(ffmpeg, input);
    let caps = Regex::new(r"Duration: (\d+):(\d+):(\d+(?:\.\d+)?)").unwrap().captures(&info)?;
    let h: f64 = caps[1].parse().ok()?;
    let m: f64 = caps[2].parse().ok()?;
    let s: f64 = caps[3].parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_from_the_internet_leads_only_to_the_internet() {
        let remote = input("https://prod-fastly-a.video.pscp.tv/a/playlist_1.m3u8?type=replay");
        assert_eq!(remote[..2], ["-protocol_whitelist", "http,https,tcp,tls,crypto"]);
        assert!(!remote[1].contains("file"));
        assert_eq!(remote[2..], ["-i", "https://prod-fastly-a.video.pscp.tv/a/playlist_1.m3u8?type=replay"]);
        assert_eq!(input("/Users/Shared/clip.mp4"), ["-i", "/Users/Shared/clip.mp4"]);
        assert_eq!(input(r"C:\clips\clip.mp4"), ["-i", r"C:\clips\clip.mp4"]);
    }
}
