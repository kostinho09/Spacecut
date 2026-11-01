//! The cut queue: each row becomes an .m4a, then optionally everything is
//! joined into one file.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::ffmpeg::{self, Slot};
use crate::hls;
use crate::util::{clean_name, fmt_hms, parse_hms, path_str};

macro_rules! args {
    ($($a:expr),* $(,)?) => { vec![$(String::from($a)),*] };
}

const VIDEO_EXTS: [&str; 11] = ["mp4", "mkv", "mov", "avi", "webm", "m4v", "flv", "wmv", "mpg", "mpeg", "3gp"];
const AUDIO_EXTS: [&str; 8] = ["mp3", "wav", "flac", "ogg", "aiff", "aif", "wma", "opus"];
const AAC_44K: [&str; 8] = ["-c:a", "aac", "-ar", "44100", "-ac", "2", "-b:a", "192k"];
const HLS_PROTOCOLS: [&str; 2] = ["-protocol_whitelist", "file,http,https,tcp,tls,crypto"];

/// Cuts that run longer than this get a "still cutting" note.
const SLOW_AFTER: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
pub struct Row {
    pub id: u64,
    pub url: String,
    pub full: bool,
    pub start: String,
    pub end: String,
    pub name: String,
    /// Seconds, when the window knows it; used to show steady progress.
    #[serde(default)]
    pub length: f64,
    /// The Space's dynamic link, if it came from one: where to find its
    /// pieces should the replay not give them out.
    #[serde(default)]
    pub live: String,
}

#[derive(Deserialize, Default)]
pub struct Jingle {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub gap: f64,
}

#[derive(Deserialize)]
pub struct Job {
    pub rows: Vec<Row>,
    pub output_folder: String,
    pub merge: bool,
    pub merge_name: String,
    pub keep_clips: bool,
    /// Keyed by position: j_before, j_0 … j_N between clips, j_after.
    pub jingles: HashMap<String, Jingle>,
}

#[derive(Default)]
pub struct Control {
    pub running: AtomicBool,
    pub cancel: AtomicBool,
    pub child: Slot,
}

impl Control {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

/// Where progress goes: the window, or a test.
type Emit<'a> = &'a dyn Fn(&str, Value);

pub fn start(app: AppHandle, ffmpeg: Option<PathBuf>, ctl: Arc<Control>, job: Job) -> Result<(), String> {
    if ctl.running.swap(true, Ordering::SeqCst) {
        return Err("A cut is already running".into());
    }
    ctl.cancel.store(false, Ordering::SeqCst);
    thread::spawn(move || {
        let emit = |name: &str, payload: Value| {
            let _ = app.emit(name, payload);
        };
        run_job(&emit, ffmpeg, &ctl, &job);
        ctl.running.store(false, Ordering::SeqCst);
        emit("job-done", json!({ "cancelled": ctl.cancel.load(Ordering::SeqCst) }));
    });
    Ok(())
}

pub(crate) fn run_job(emit: Emit, ffmpeg: Option<PathBuf>, ctl: &Control, job: &Job) {
    let rows: Vec<&Row> = job.rows.iter().filter(|r| !r.url.trim().is_empty()).collect();
    let Some(first) = rows.first() else { return };
    let Some(ffmpeg) = ffmpeg else {
        emit("job-status", json!({ "id": first.id, "status": "FFmpeg Not Found" }));
        return;
    };
    let out = PathBuf::from(&job.output_folder);
    let _ = fs::create_dir_all(&out);
    // Each clip fills the progress bar as much as it is long; merging takes
    // roughly another third of the time the cuts took.
    let lens: Vec<f64> = rows.iter().map(|r| length_of(&ffmpeg, r)).collect();
    let cuts: f64 = lens.iter().map(|l| l.max(1.0)).sum();
    let meter = Meter { total: cuts + if job.merge { cuts * 0.3 } else { 0.0 }, ..Meter::default() };
    let worker = Worker { emit, ffmpeg, out, ctl, warned: Cell::new(false), meter };

    let mut clips = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        if worker.cancelled() {
            break;
        }
        worker.warned.set(false);
        worker.begin(lens[n].max(1.0));
        if let Some(path) = worker.cut_row(n, row, lens[n]) {
            worker.status(row.id, "Done");
            clips.push((path, lens[n]));
        }
        worker.finish_item();
    }

    if job.merge && !clips.is_empty() && !worker.cancelled() {
        emit("job-note", json!({ "text": "Merging…" }));
        worker.begin(cuts * 0.3);
        worker.merge(&clips, job);
        worker.finish_item();
        if !job.keep_clips {
            for (clip, _) in &clips {
                let _ = fs::remove_file(clip);
            }
        }
    }
}

fn live_of(row: &Row) -> Option<&str> {
    Some(row.live.trim()).filter(|l| !l.is_empty())
}

/// How long a row's clip is: its span, or the whole recording for "Full".
fn length_of(ffmpeg: &Path, row: &Row) -> f64 {
    if row.length > 0.0 {
        return row.length;
    }
    if !row.full {
        return match (parse_hms(&row.start), parse_hms(&row.end)) {
            (Some(a), Some(b)) if b > a => (b - a) as f64,
            _ => 0.0,
        };
    }
    let url = row.url.trim();
    if hls::is_m3u8(url) {
        hls::resolve(url, live_of(row)).map(|p| p.duration).unwrap_or(0.0)
    } else {
        ffmpeg::probe_duration(ffmpeg, url).unwrap_or(0.0)
    }
}

/// Turns ffmpeg's progress into one steady 0–100 for the whole job.
#[derive(Default)]
struct Meter {
    total: f64,
    /// Work finished, in the same units as `total`.
    done: Cell<f64>,
    /// How much of `total` the item under way is worth.
    weight: Cell<f64>,
    /// The share of the item the running command covers, and its length in seconds.
    step: Cell<(f64, f64, f64)>,
    shown: Cell<f64>,
}

/// What most likely went wrong, in a few plain words, from ffmpeg's messages.
fn explain(stderr: &str, out: &Path) -> &'static str {
    let e = stderr.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| e.contains(w));
    if has(&["no space left on device"]) {
        "The disk is full"
    } else if has(&["permission denied", "operation not permitted", "read-only file system"]) || !out.is_dir() {
        "Space Cutter can't save in the output folder"
    } else if has(&["403 forbidden", "404 not found", "410 gone", "http error 403", "http error 404", "server returned 4"]) {
        "The link has expired or no longer works"
    } else if has(&[
        "failed to resolve", "could not resolve", "nodename nor servname", "network is unreachable",
        "connection refused", "connection timed out", "timed out", "connection reset", "broken pipe",
    ]) {
        "No internet connection, or the server didn't answer"
    } else if has(&["no such file or directory"]) {
        "The file isn't there any more"
    } else if has(&["invalid data found", "does not contain any stream", "matches no streams", "output file is empty", "no audio"]) {
        "There's no audio in this that Space Cutter can read"
    } else if has(&["server returned 5"]) {
        "The server had a problem; try again later"
    } else {
        "Something went wrong while cutting it; try again"
    }
}

/// `dir/base.ext`, or `base (2).ext` and so on when that is taken, so a cut
/// never replaces an earlier one.
fn unique(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{base}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{base} ({n}).{ext}"))).find(|p| !p.exists()).unwrap()
}

struct Worker<'a> {
    emit: Emit<'a>,
    ffmpeg: PathBuf,
    out: PathBuf,
    ctl: &'a Control,
    warned: Cell<bool>,
    meter: Meter,
}

impl Worker<'_> {
    fn begin(&self, weight: f64) {
        self.meter.weight.set(weight);
        self.meter.step.set((0.0, 1.0, 0.0));
    }

    /// The next command covers `lo..hi` of the current item and writes `len` seconds.
    fn step(&self, lo: f64, hi: f64, len: f64) {
        self.meter.step.set((lo, hi, len));
    }

    fn finish_item(&self) {
        let m = &self.meter;
        m.done.set(m.done.get() + m.weight.get());
        m.weight.set(0.0);
        self.show(m.done.get());
    }

    fn reached(&self, t: f64) {
        let m = &self.meter;
        let (lo, hi, len) = m.step.get();
        if len > 0.0 {
            self.show(m.done.get() + m.weight.get() * (lo + (hi - lo) * (t / len).clamp(0.0, 1.0)));
        }
    }

    /// Sends the progress on, only ever forward and in small steps.
    fn show(&self, at: f64) {
        let m = &self.meter;
        let pct = if m.total > 0.0 { (at / m.total * 100.0).clamp(0.0, 100.0) } else { 0.0 };
        if pct >= m.shown.get() + 0.25 || (pct >= 100.0 && m.shown.get() < 100.0) {
            m.shown.set(pct);
            (self.emit)("job-progress", json!({ "value": (pct * 10.0).round() / 10.0 }));
        }
    }

    fn status(&self, id: u64, status: &str) {
        (self.emit)("job-status", json!({ "id": id, "status": status }));
    }

    fn cancelled(&self) -> bool {
        self.ctl.cancel.load(Ordering::SeqCst)
    }

    /// Reports a failed row with the likely reason.
    fn fail(&self, id: u64, status: &str, stderr: &str) {
        if self.cancelled() {
            return;
        }
        let reason = explain(stderr, &self.out);
        (self.emit)("job-status", json!({ "id": id, "status": status, "reason": reason }));
    }

    /// Runs a command; with `slow`, flags the row once it has run for a while.
    fn exec(&self, cmd: Command, slow: Option<(u64, Instant)>) -> ffmpeg::Output {
        ffmpeg::run(cmd, &self.ctl.cancel, &self.ctl.child, |reached| {
            if let Some(t) = reached {
                self.reached(t);
            }
            if let Some((id, since)) = slow {
                if !self.warned.get() && since.elapsed() > SLOW_AFTER {
                    self.warned.set(true);
                    (self.emit)("job-slow", json!({ "id": id }));
                }
            }
        })
    }

    fn ff(&self, args: Vec<String>, slow: Option<(u64, Instant)>) -> ffmpeg::Output {
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(&args);
        self.exec(cmd, slow)
    }

    /// Cuts one row; the output path, or None after reporting what went wrong.
    fn cut_row(&self, n: usize, row: &Row, len: f64) -> Option<PathBuf> {
        let url = row.url.trim();
        let id = row.id;
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let custom = clean_name(&row.name);
        let base = if custom.is_empty() { format!("cut_{}_{stamp}", n + 1) } else { custom };

        let span = if row.full {
            None
        } else {
            match (parse_hms(&row.start), parse_hms(&row.end)) {
                (Some(a), Some(b)) if b > a => Some((a, b)),
                _ => {
                    self.status(id, "Invalid Time");
                    return None;
                }
            }
        };

        let m4a_file = unique(&self.out, &base, "m4a");
        let ts_file = m4a_file.with_extension("ts");
        let (ts, m4a) = (path_str(&ts_file), path_str(&m4a_file));

        let local = Path::new(url).exists();
        let ext = Path::new(url)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let is_video = local && VIDEO_EXTS.contains(&ext.as_str());
        let is_audio = local && AUDIO_EXTS.contains(&ext.as_str());

        if is_video {
            // Copy the audio track as is; re-encode only if it won't fit an .m4a
            self.status(id, "Cutting");
            let mut stderr = String::new();
            for encode in [&["-acodec", "copy"][..], &AAC_44K[..]] {
                let mut a = Vec::new();
                if let Some((s0, _)) = span {
                    a.extend(args!["-ss", fmt_hms(s0)]);
                }
                a.extend(args!["-i", url]);
                if let Some((s0, e0)) = span {
                    a.extend(args!["-t", (e0 - s0).to_string()]);
                }
                a.push("-vn".into());
                a.extend(encode.iter().map(|s| s.to_string()));
                if span.is_some() {
                    a.extend(args!["-avoid_negative_ts", "1"]);
                }
                a.extend(args!["-y", &m4a]);
                self.step(0.0, 1.0, len);
                let out = self.ff(a, None);
                if out.ok {
                    return Some(m4a_file);
                }
                if self.cancelled() {
                    return None;
                }
                stderr = out.stderr;
            }
            self.fail(id, "Cut Error", &stderr);
            return None;
        }

        if is_audio {
            // MP3, WAV, FLAC and the like: straight to AAC
            let mut a = Vec::new();
            if let Some((s0, e0)) = span {
                a.extend(args!["-ss", fmt_hms(s0), "-i", url, "-t", (e0 - s0).to_string(), "-vn"]);
                a.extend(AAC_44K.iter().map(|s| s.to_string()));
                a.extend(args!["-avoid_negative_ts", "1"]);
            } else {
                a.extend(args!["-i", url, "-vn"]);
                a.extend(AAC_44K.iter().map(|s| s.to_string()));
            }
            a.extend(args!["-y", &m4a]);
            self.status(id, "Cutting");
            self.step(0.0, 1.0, len);
            let out = self.ff(a, None);
            if out.ok {
                return Some(m4a_file);
            }
            self.fail(id, "Cut Error", &out.stderr);
            return None;
        }

        // A Space (or a local .m4a/.ts/.aac): copy through an MPEG-TS file
        let space = (!local && hls::is_m3u8(url)).then(|| hls::resolve(url, live_of(row)).ok()).flatten();
        let mut trim: Option<(PathBuf, f64)> = None;
        let cut = match span {
            // Pieces from elsewhere than the list says need a list of their own
            None => match &space {
                Some(list) if list.moved => {
                    trim = hls::trim_playlist(&list.segments, 0.0, list.duration);
                    match &trim {
                        Some((own, _)) => {
                            let mut a: Vec<String> = HLS_PROTOCOLS.iter().map(|s| s.to_string()).collect();
                            a.extend(args!["-i", path_str(own), "-c", "copy", "-f", "mpegts", "-y", &ts]);
                            a
                        }
                        None => args!["-i", url, "-c", "copy", "-f", "mpegts", "-y", &ts],
                    }
                }
                _ => args!["-i", url, "-c", "copy", "-f", "mpegts", "-y", &ts],
            },
            Some((s0, e0)) => {
                let dur = (e0 - s0).to_string();
                if local {
                    args!["-ss", fmt_hms(s0), "-i", url, "-t", dur, "-c", "copy", "-avoid_negative_ts", "1", "-f", "mpegts", "-y", &ts]
                } else {
                    trim = space.as_ref().and_then(|list| hls::trim_playlist(&list.segments, s0 as f64, e0 as f64));
                    match &trim {
                        Some((list, offset)) => {
                            let mut a: Vec<String> = HLS_PROTOCOLS.iter().map(|s| s.to_string()).collect();
                            a.extend(args!["-i", path_str(list), "-ss", format!("{offset:.3}"), "-t", dur, "-c", "copy", "-f", "mpegts", "-y", &ts]);
                            a
                        }
                        None => args!["-ss", fmt_hms(s0), "-i", url, "-t", dur, "-c", "copy", "-f", "mpegts", "-y", &ts],
                    }
                }
            }
        };
        let drop_trim = |trim: &Option<(PathBuf, f64)>| {
            if let Some((list, _)) = trim {
                let _ = fs::remove_file(list);
            }
        };

        self.status(id, "Cutting");
        self.step(0.0, 0.92, len);
        let copied = self.ff(cut, Some((id, Instant::now())));
        if self.cancelled() {
            drop_trim(&trim);
            let _ = fs::remove_file(&ts_file);
            return None;
        }
        if !copied.ok {
            // Re-encode straight to .m4a (handles MP3 behind the wrong extension and the like)
            self.status(id, "Converting");
            let mut a = Vec::new();
            match (span, &trim) {
                (None, _) => a.extend(args!["-i", url]),
                (Some((s0, e0)), Some((list, offset))) => {
                    a.extend(HLS_PROTOCOLS.iter().map(|s| s.to_string()));
                    a.extend(args!["-i", path_str(list), "-ss", format!("{offset:.3}"), "-t", (e0 - s0).to_string()]);
                }
                (Some((s0, e0)), None) => a.extend(args!["-ss", fmt_hms(s0), "-i", url, "-t", (e0 - s0).to_string()]),
            }
            a.extend(AAC_44K.iter().map(|s| s.to_string()));
            a.extend(args!["-y", &m4a]);
            self.step(0.0, 0.92, len);
            let out = self.ff(a, Some((id, Instant::now())));
            drop_trim(&trim);
            let _ = fs::remove_file(&ts_file);
            if out.ok {
                return Some(m4a_file);
            }
            self.fail(id, "Cut Error", &format!("{}\n{}", copied.stderr, out.stderr));
            return None;
        }
        drop_trim(&trim);

        self.status(id, "Converting");
        self.step(0.92, 1.0, len);
        let out = self.ff(args!["-i", &ts, "-c", "copy", "-y", &m4a], None);
        if out.ok {
            let _ = fs::remove_file(&ts_file);
            return Some(m4a_file);
        }
        self.fail(id, "Convert Error", &out.stderr);
        None
    }

    /// Joins the clips, with sound effects or silence between them. Everything
    /// is re-encoded to match the first clip, so the effects adapt to the
    /// main audio rather than the other way around.
    fn merge(&self, clips: &[(PathBuf, f64)], job: &Job) {
        static SILENCE: AtomicU64 = AtomicU64::new(0);
        let first = path_str(&clips[0].0);
        let (rate, channels, bitrate) = ffmpeg::probe_audio(&self.ffmpeg, &clips[0].0);
        // Progress: the first half while the parts are converted, the second while they are joined
        let total = clips.iter().map(|c| c.1).sum::<f64>().max(1.0);
        let converted = Cell::new(0.0);
        let advance = |len: f64| {
            let lo = converted.get() / (2.0 * total);
            converted.set(converted.get() + len);
            self.step(lo, (converted.get() / (2.0 * total)).min(0.5), len);
        };
        let encode = args!["-c:a", "aac", "-ar", rate.to_string(), "-ac", channels.to_string(), "-b:a", bitrate];
        let usable = |p: &Path, ok: bool| (ok && fs::metadata(p).is_ok_and(|m| m.len() > 512)).then(|| p.to_path_buf());

        let to_ts = |input: &str, len: f64| -> Option<PathBuf> {
            advance(len);
            let ts = PathBuf::from(format!("{input}_conv.ts"));
            let mut a = args!["-i", input, "-vn"];
            a.extend(encode.iter().cloned());
            a.extend(args!["-f", "mpegts", "-y", path_str(&ts)]);
            usable(&ts, self.ff(a, None).ok)
        };
        // Silence is the first clip muted, which avoids lavfi filters a
        // trimmed-down ffmpeg may lack.
        let silence = |secs: f64| -> Option<PathBuf> {
            let n = SILENCE.fetch_add(1, Ordering::Relaxed);
            let ms = chrono::Local::now().timestamp_millis();
            let path = self.out.join(format!("_silence_{ms}_{n}.ts"));
            let mut a = args!["-i", &first, "-t", secs.to_string(), "-vn", "-af", "volume=0"];
            a.extend(encode.iter().cloned());
            a.extend(args!["-f", "mpegts", "-y", path_str(&path)]);
            usable(&path, self.ff(a, None).ok)
        };
        // A gap set on one separator also applies wherever none was set.
        let default_gap = job.jingles.values().map(|j| j.gap).fold(0.0, f64::max);
        let insert = |key: &str, fallback: f64| -> Option<PathBuf> {
            let jingle = job.jingles.get(key);
            let effect = jingle.map(|j| j.path.as_str()).unwrap_or("");
            let gap = jingle.map(|j| j.gap).unwrap_or(fallback);
            if !effect.is_empty() && Path::new(effect).exists() {
                to_ts(effect, ffmpeg::probe_duration(&self.ffmpeg, effect).unwrap_or(1.0))
            } else if gap > 0.0 {
                silence(gap)
            } else {
                None
            }
        };

        let mut parts = Vec::new();
        parts.extend(insert("j_before", 0.0));
        for (i, (clip, len)) in clips.iter().enumerate() {
            parts.extend(to_ts(&path_str(clip), *len));
            if i + 1 < clips.len() {
                parts.extend(insert(&format!("j_{i}"), default_gap));
            }
        }
        parts.extend(insert("j_after", 0.0));
        if parts.is_empty() || self.cancelled() {
            return;
        }

        let list = self.out.join("_merge_list.txt");
        let body: String = parts
            .iter()
            .map(|p| format!("file '{}'\n", path_str(p).replace('\'', "'\\''")))
            .collect();
        if fs::write(&list, body).is_ok() {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let name = clean_name(&job.merge_name);
            let name = if name.is_empty() { format!("merged_{stamp}") } else { name };
            let merged = unique(&self.out, &name, "m4a");
            // Re-encoding the join guarantees a clean end, even after a trailing effect
            let mut a = args!["-f", "concat", "-safe", "0", "-i", path_str(&list)];
            a.extend(encode.iter().cloned());
            a.extend(args!["-y", path_str(&merged)]);
            self.step(0.5, 1.0, total);
            self.ff(a, None);
        }
        for p in &parts {
            let name = path_str(p);
            if name.contains("_conv.ts") || name.contains("_silence_") {
                let _ = fs::remove_file(p);
            }
        }
        let _ = fs::remove_file(&list);
    }
}

#[cfg(test)]
mod tests {
    //! Needs the network and a replay that has not expired:
    //! SPACE_URL='https://…/playlist_….m3u8?type=replay' cargo test -- --ignored --nocapture

    use super::*;
    use std::sync::Mutex;

    fn row(id: u64, url: &str, start: &str, end: &str, name: &str) -> Row {
        Row { id, url: url.into(), full: false, start: start.into(), end: end.into(), name: name.into(), length: 0.0, live: String::new() }
    }

    fn run(job: &Job) -> Vec<String> {
        let events = Mutex::new(Vec::new());
        let emit = |name: &str, payload: Value| events.lock().unwrap().push(format!("{name} {payload}"));
        run_job(&emit, ffmpeg::locate(None), &Control::default(), job);
        events.into_inner().unwrap()
    }

    fn length(path: &Path) -> f64 {
        ffmpeg::probe_duration(&ffmpeg::locate(None).unwrap(), &path_str(path)).unwrap_or(0.0)
    }

    #[test]
    #[ignore]
    fn merges_clips_with_a_gap_and_cuts_local_files() {
        let url = std::env::var("SPACE_URL").expect("set SPACE_URL");
        let dir = std::env::temp_dir().join(format!("spacecutter_merge_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let out = path_str(&dir);

        let mut jingles = HashMap::new();
        jingles.insert("j_0".to_string(), Jingle { path: String::new(), gap: 1.0 });
        let events = run(&Job {
            rows: vec![row(1, &url, "02:35:00", "02:35:12", "a"), row(2, &url, "02:40:00", "02:40:08", "b")],
            output_folder: out.clone(),
            merge: true,
            merge_name: "joined".into(),
            keep_clips: true,
            jingles,
        });
        println!("{events:#?}");
        let (a, b, joined) = (length(&dir.join("a.m4a")), length(&dir.join("b.m4a")), length(&dir.join("joined.m4a")));
        println!("a = {a:.2} s, b = {b:.2} s, joined = {joined:.2} s");
        assert!((a - 12.0).abs() < 0.5 && (b - 8.0).abs() < 0.5);
        assert!((joined - 21.0).abs() < 0.7, "12 s + 1 s gap + 8 s should make 21 s");
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        println!("files: {leftovers:?}");
        assert_eq!(leftovers.len(), 3, "temporary files were left behind");

        // A local file from the queue: 00:00:02 → 00:00:07 of the first clip
        let local = dir.join("a.m4a");
        let events = run(&Job {
            rows: vec![row(3, &path_str(&local), "00:00:02", "00:00:07", "local")],
            output_folder: out,
            merge: false,
            merge_name: String::new(),
            keep_clips: true,
            jingles: HashMap::new(),
        });
        println!("{events:#?}");
        let cut = length(&dir.join("local.m4a"));
        println!("local cut = {cut:.2} s");
        let _ = fs::remove_dir_all(&dir);
        assert!((cut - 5.0).abs() < 0.5);
    }

    /// What the app does with a Space's dynamic link: convert it, then cut
    /// 20 seconds, with the link kept for finding the pieces:
    /// DYNAMIC_URL='https://…/dynamic_playlist.m3u8?type=live' cargo test -- --ignored cuts_a_space_from
    #[test]
    #[ignore]
    fn cuts_a_space_from_its_dynamic_link() {
        let dynamic = std::env::var("DYNAMIC_URL").expect("set DYNAMIC_URL");
        let replay = hls::live_to_replay(&dynamic).unwrap();
        println!("converted: {replay}");
        let dir = std::env::temp_dir().join(format!("spacecutter_dyn_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut clip = row(1, &replay, "00:10:00", "00:10:20", "from-dynamic");
        clip.live = dynamic;
        let events = run(&Job { rows: vec![clip], output_folder: path_str(&dir), merge: false, merge_name: String::new(), keep_clips: true, jingles: HashMap::new() });
        println!("{events:#?}");
        let got = length(&dir.join("from-dynamic.m4a"));
        println!("cut: {got:.2} s (want 20)");
        let _ = fs::remove_dir_all(&dir);
        assert!((got - 20.0).abs() < 0.5);
    }

    /// A replay whose list opens but whose pieces the server refuses (the
    /// master's own path, with its key) is still cut, from where else the
    /// pieces are (see `hls::resolve`):
    /// DYNAMIC_URL='https://…/dynamic_playlist.m3u8?type=live' cargo test -- --ignored cuts_when
    #[test]
    #[ignore]
    fn cuts_when_the_given_replay_refuses_its_pieces() {
        let dynamic = std::env::var("DYNAMIC_URL").expect("set DYNAMIC_URL");
        let master = dynamic.replace("/dynamic_playlist.m3u8?type=live", "/master_playlist.m3u8");
        let text = hls::get_text(&master).unwrap();
        let refused = url::Url::parse(&master).unwrap().join(text.lines().find(|l| l.starts_with('/')).unwrap().trim()).unwrap().to_string();
        let dir = std::env::temp_dir().join(format!("spacecutter_live_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut clip = row(1, &refused, "00:10:00", "00:10:20", "elsewhere");
        clip.live = dynamic.clone();
        let mut whole = row(2, &refused, "", "", "whole-elsewhere");
        whole.live = dynamic;
        whole.full = std::env::var("WITH_FULL").is_ok();
        let mut rows = vec![clip];
        if whole.full {
            rows.push(whole);
        }
        let events = run(&Job { rows, output_folder: path_str(&dir), merge: false, merge_name: String::new(), keep_clips: true, jingles: HashMap::new() });
        println!("{events:#?}");
        let got = length(&dir.join("elsewhere.m4a"));
        println!("cut from elsewhere: {got:.2} s (want 20)");
        if dir.join("whole-elsewhere.m4a").exists() {
            println!("whole Space from elsewhere: {:.1} s", length(&dir.join("whole-elsewhere.m4a")));
        }
        let _ = fs::remove_dir_all(&dir);
        assert!((got - 20.0).abs() < 0.5);
    }

    /// This Space's timestamps wrap at about 00:52:03; a clip across that point
    /// must come out whole, and so must the whole Space.
    #[test]
    #[ignore]
    fn cuts_across_the_timestamp_wrap() {
        let url = std::env::var("SPACE_URL").expect("set SPACE_URL");
        let dir = std::env::temp_dir().join(format!("spacecutter_wrap_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut full = row(2, &url, "", "", "full");
        full.full = std::env::var("WITH_FULL").is_ok();
        let mut rows = vec![row(1, &url, "00:51:30", "00:52:40", "across")];
        if full.full {
            rows.push(full);
        }
        let events = run(&Job {
            rows,
            output_folder: path_str(&dir),
            merge: false,
            merge_name: String::new(),
            keep_clips: true,
            jingles: HashMap::new(),
        });
        println!("{events:#?}");
        let across = length(&dir.join("across.m4a"));
        println!("across the wrap: {across:.2} s (want 70)");
        if std::env::var("WITH_FULL").is_ok() {
            println!("whole Space: {:.1} s (want about 11742)", length(&dir.join("full.m4a")));
        }
        let _ = fs::remove_dir_all(&dir);
        assert!((across - 70.0).abs() < 0.6);
    }

    /// What a user is told when a clip can't be cut. Needs the network.
    #[test]
    #[ignore]
    fn explains_failures() {
        let reasons = |url: &str, folder: &str| -> Vec<String> {
            run(&Job {
                rows: vec![row(1, url, "00:00:10", "00:00:20", "")],
                output_folder: folder.into(),
                merge: false,
                merge_name: String::new(),
                keep_clips: true,
                jingles: HashMap::new(),
            })
            .into_iter()
            .filter(|e| e.contains("reason"))
            .collect()
        };
        let tmp = path_str(&std::env::temp_dir());
        let gone = "https://prod-fastly-eu-central-1.video.pscp.tv/Transcoding/v1/hls/AAAA/non_transcode/eu-central-1/periscope-replay-direct-prod-eu-central-1-public/audio-space/playlist_1.m3u8?type=replay";
        println!("expired link:  {:?}", reasons(gone, &tmp));
        println!("missing file:  {:?}", reasons("/Users/Shared/no-such-clip.m4a", &tmp));
        println!("no internet:   {:?}", reasons("https://no-such-host.invalid/a/playlist.m3u8", &tmp));
        println!("locked folder: {:?}", reasons(gone, "/System/Library/spacecutter-test"));
    }

    #[test]
    fn rejects_bad_timestamps() {
        let events = run(&Job {
            rows: vec![row(7, "/no/such/file.m4a", "00:10:00", "00:05:00", "")],
            output_folder: path_str(&std::env::temp_dir()),
            merge: false,
            merge_name: String::new(),
            keep_clips: true,
            jingles: HashMap::new(),
        });
        assert!(events.iter().any(|e| e.contains("Invalid Time")), "{events:?}");
    }
}
