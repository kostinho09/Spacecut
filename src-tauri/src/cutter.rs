//! The cut queue: each row becomes an .m4a, then optionally everything is
//! joined into one file.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::fetch;
use crate::ffmpeg::{self, Slot};
use crate::hls;
use crate::util::{clean_name, fmt_hms, parse_hms, path_str};

macro_rules! args {
    ($($a:expr),* $(,)?) => { vec![$(String::from($a)),*] };
}

// The local files the app takes, by kind: the file pickers offer these and
// dropping takes them (MEDIA_EXTS in ui/app.js is the same list)
/// Video: the sound copied out as it is, or made AAC if it can't be
pub(crate) const VIDEO_EXTS: [&str; 11] = ["mp4", "mkv", "mov", "avi", "webm", "m4v", "flv", "wmv", "mpg", "mpeg", "3gp"];
/// Sound in other formats: made AAC
pub(crate) const AUDIO_EXTS: [&str; 9] = ["mp3", "wav", "flac", "ogg", "aiff", "aif", "wma", "opus", "dts"];
/// Already AAC (or MPEG-TS with AAC, as a Space's pieces): copied as it is
pub(crate) const AAC_EXTS: [&str; 3] = ["m4a", "aac", "ts"];
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

/// Why a clip can't be cut from a Space when it starts where the Space has
/// ended (or, still live, hasn't got to yet); None when it starts within it.
fn past_the_end(list: &hls::Playlist, start: u64) -> Option<String> {
    let end = list.segments.last().map(|s| s.start + s.dur)?;
    if (start as f64) < end {
        return None;
    }
    let at = fmt_hms(end as u64);
    Some(if list.ended { format!("starts after the Space ends ({at})") } else { format!("starts after where the live Space has got to ({at})") })
}

/// Whether a Space is cut from its pieces fetched several at a time; tests
/// turn it off to compare with ffmpeg reading the list itself.
fn in_parallel() -> bool {
    #[cfg(test)]
    let on = tests::PARALLEL.with(Cell::get);
    #[cfg(not(test))]
    let on = true;
    on
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
        // A whole Space on air: all that is out when Cut is pressed
        hls::resolve_whole(url, live_of(row)).map(|p| p.duration).unwrap_or(0.0)
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
        "Spacecut can't save in the output folder"
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
        "There's no audio in this that Spacecut can read"
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

    /// What to do as ffmpeg reports how far it has got; with `slow`, the row
    /// is also flagged once it has run for a while.
    fn ticks(&self, slow: Option<(u64, Instant)>) -> impl FnMut(Option<f64>) + '_ {
        move |reached| {
            if let Some(t) = reached {
                self.reached(t);
            }
            if let Some((id, since)) = slow {
                if !self.warned.get() && since.elapsed() > SLOW_AFTER {
                    self.warned.set(true);
                    (self.emit)("job-slow", json!({ "id": id }));
                }
            }
        }
    }

    /// Runs a command; with `slow`, flags the row once it has run for a while.
    fn exec(&self, cmd: Command, slow: Option<(u64, Instant)>) -> ffmpeg::Output {
        ffmpeg::run(cmd, &self.ctl.cancel, &self.ctl.child, self.ticks(slow))
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
        // The whole of a Space on air: all that is out when Cut is pressed
        let list_of = |url, live| if span.is_none() { hls::resolve_whole(url, live) } else { hls::resolve(url, live) };
        let mut space = (!local && hls::is_m3u8(url)).then(|| list_of(url, live_of(row)).ok()).flatten();
        // The list already known covers a clip within it; one of a Space on air
        // that ends past it needs the list as it is now, and may need to wait
        // a moment for its last seconds
        if let Some((_, e0)) = span {
            for tries in 0..6 {
                let short = space.as_ref().is_some_and(|l| !l.ended && l.duration < e0 as f64);
                if !short || self.cancelled() {
                    break;
                }
                if tries > 0 {
                    thread::sleep(Duration::from_secs(2));
                }
                if let Ok(list) = hls::resolve_fresh(url, live_of(row)) {
                    space = Some(list);
                }
            }
        }
        // ffmpeg would give the start of the Space instead
        if let (Some(list), Some((s0, _))) = (&space, span) {
            if let Some(reason) = past_the_end(list, s0) {
                (self.emit)("job-status", json!({ "id": id, "status": "Past the End", "reason": reason }));
                return None;
            }
        }
        // Its pieces fetched several at a time (see `fetch`); should that not
        // work out, ffmpeg reads the list itself, as it always did
        if let Some(list) = space.as_ref().filter(|_| in_parallel()) {
            match self.fetched(id, list, span, &ts, len) {
                Some(true) => return self.remux(id, &ts_file, m4a_file, len),
                Some(false) => {
                    let _ = fs::remove_file(&ts_file);
                }
                None => {
                    let _ = fs::remove_file(&ts_file);
                    return None;
                }
            }
        }
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
                        None => [ffmpeg::input(url), args!["-c", "copy", "-f", "mpegts", "-y", &ts]].concat(),
                    }
                }
                _ => [ffmpeg::input(url), args!["-c", "copy", "-f", "mpegts", "-y", &ts]].concat(),
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
                        None => [args!["-ss", fmt_hms(s0)], ffmpeg::input(url), args!["-t", dur, "-c", "copy", "-f", "mpegts", "-y", &ts]].concat(),
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
                (None, _) => a.extend(ffmpeg::input(url)),
                (Some((s0, e0)), Some((list, offset))) => {
                    a.extend(HLS_PROTOCOLS.iter().map(|s| s.to_string()));
                    a.extend(args!["-i", path_str(list), "-ss", format!("{offset:.3}"), "-t", (e0 - s0).to_string()]);
                }
                (Some((s0, e0)), None) => {
                    a.extend(args!["-ss", fmt_hms(s0)]);
                    a.extend(ffmpeg::input(url));
                    a.extend(args!["-t", (e0 - s0).to_string()]);
                }
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
        self.remux(id, &ts_file, m4a_file, len)
    }

    /// A Space's clip copied into `ts` from its pieces, fetched several at a
    /// time and fed to ffmpeg in order: true once done, false when it should
    /// be cut the old way (a piece couldn't be had, or ffmpeg failed on them),
    /// None if cancelled.
    fn fetched(&self, id: u64, list: &hls::Playlist, span: Option<(u64, u64)>, ts: &str, len: f64) -> Option<bool> {
        let (s0, e0) = span.map_or((0.0, list.duration), |(a, b)| (a as f64, b as f64));
        let pieces: Vec<&hls::Segment> = list.segments.iter().filter(|s| s.start + s.dur > s0 && s.start < e0).collect();
        // Only AAC pieces, as Spaces have, run on into one another when fed
        let aac = pieces.iter().all(|s| s.url.split('?').next().unwrap_or("").ends_with(".aac"));
        let Some(first) = pieces.first().filter(|_| aac) else {
            return Some(false);
        };
        let mut a = args!["-f", "aac", "-i", "pipe:0"];
        if let Some((s0, e0)) = span {
            a.extend(args!["-ss", format!("{:.3}", (s0 as f64 - first.start).max(0.0)), "-t", (e0 - s0).to_string()]);
        }
        a.extend(args!["-c", "copy", "-f", "mpegts", "-y", ts]);
        let mut cmd = Command::new(&self.ffmpeg);
        cmd.args(&a);
        let urls: Vec<String> = pieces.iter().map(|s| s.url.clone()).collect();
        // The last piece only up to the clip's end: where the list's timing
        // has a gap (a piece listed longer than its sound), the sound would
        // otherwise run on past the end before ffmpeg's -t stops it
        let last = pieces.len() - 1;
        let until = span.map(|(_, e0)| e0 as f64 - pieces[last].start);
        self.status(id, "Cutting");
        self.step(0.0, 0.92, len);
        let ticks = self.ticks(Some((id, Instant::now())));
        let (out, fed) = ffmpeg::run_fed(cmd, &self.ctl.cancel, &self.ctl.child, ticks, move |mut stdin, finished| {
            fetch::in_order(urls, fetch::AHEAD_CUT, move || finished.load(Ordering::SeqCst), |i, piece| {
                let piece = match until {
                    Some(secs) if i == last => hls::adts_until(piece, secs),
                    _ => piece,
                };
                stdin.write_all(piece).is_ok()
            })
        });
        if self.cancelled() {
            return None;
        }
        Some(out.ok && matches!(fed, Some(Ok(()))))
    }

    /// The copied .ts into the .m4a that is kept.
    fn remux(&self, id: u64, ts_file: &Path, m4a_file: PathBuf, len: f64) -> Option<PathBuf> {
        self.status(id, "Converting");
        self.step(0.92, 1.0, len);
        let out = self.ff(args!["-i", path_str(ts_file), "-c", "copy", "-y", path_str(&m4a_file)], None);
        if out.ok {
            let _ = fs::remove_file(ts_file);
            return Some(m4a_file);
        }
        self.fail(id, "Convert Error", &out.stderr);
        None
    }

    /// Joins the clips, with sound effects or silence between them, in one
    /// go: every part is decoded and they are put end to end sample by
    /// sample (ffmpeg's concat filter), then encoded once to match the first
    /// clip, so the effects adapt to the main audio rather than the other way
    /// around. (Parts joined as separate MPEG-TS files lost or overlapped
    /// sound at every join, since such files under-report their length, and
    /// one too short to read, a 0.5 s silence say, ended the join there.)
    fn merge(&self, clips: &[(PathBuf, f64)], job: &Job) {
        let (rate, channels, bitrate) = ffmpeg::probe_audio(&self.ffmpeg, &clips[0].0);
        let layout = if channels == 1 { "mono" } else { "stereo" };
        // A gap set on one separator also applies wherever none was set.
        let default_gap = job.jingles.values().map(|j| j.gap).fold(0.0, f64::max);
        // What goes in a separator: a sound effect, or seconds of silence
        let insert = |key: &str, fallback: f64| -> Option<(Option<String>, f64)> {
            let jingle = job.jingles.get(key);
            let effect = jingle.map(|j| j.path.as_str()).unwrap_or("");
            let gap = jingle.map(|j| j.gap).unwrap_or(fallback);
            if !effect.is_empty() && Path::new(effect).exists() {
                Some((Some(effect.to_string()), ffmpeg::probe_duration(&self.ffmpeg, effect).unwrap_or(1.0)))
            } else if gap > 0.0 {
                Some((None, gap))
            } else {
                None
            }
        };
        let mut parts = Vec::new();
        parts.extend(insert("j_before", 0.0));
        for (i, (clip, len)) in clips.iter().enumerate() {
            parts.push((Some(path_str(clip)), *len));
            if i + 1 < clips.len() {
                parts.extend(insert(&format!("j_{i}"), default_gap));
            }
        }
        parts.extend(insert("j_after", 0.0));
        if self.cancelled() {
            return;
        }

        let mut a = Vec::new();
        let mut graph = String::new();
        for (n, (file, secs)) in parts.iter().enumerate() {
            match file {
                Some(path) => a.extend(args!["-i", path]),
                None => a.extend(args!["-f", "lavfi", "-t", format!("{secs:.3}"), "-i", format!("anullsrc=r={rate}:cl={layout}")]),
            }
            graph.push_str(&format!("[{n}:a:0]aresample={rate},aformat=sample_fmts=fltp:sample_rates={rate}:channel_layouts={layout}[p{n}];"));
        }
        graph.extend((0..parts.len()).map(|n| format!("[p{n}]")));
        graph.push_str(&format!("concat=n={}:v=0:a=1[out]", parts.len()));

        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let name = clean_name(&job.merge_name);
        let name = if name.is_empty() { format!("merged_{stamp}") } else { name };
        let merged = unique(&self.out, &name, "m4a");
        a.extend(args!["-filter_complex", graph, "-map", "[out]", "-c:a", "aac", "-ar", rate.to_string(), "-ac", channels.to_string(), "-b:a", bitrate]);
        a.extend(args!["-y", path_str(&merged)]);
        let total = parts.iter().map(|p| p.1).sum::<f64>().max(1.0);
        self.step(0.0, 1.0, total);
        let out = self.ff(a, None);
        if !out.ok && !self.cancelled() {
            let _ = fs::remove_file(&merged);
            (self.emit)("job-note", json!({ "text": format!("Could not merge the clips: {}", explain(&out.stderr, &self.out)) }));
        }
    }
}

#[cfg(test)]
mod tests {
    //! Needs the network and a replay that has not expired:
    //! SPACE_URL='https://…/playlist_….m3u8?type=replay' cargo test -- --ignored --nocapture

    use super::*;
    use std::sync::Mutex;

    thread_local! {
        /// See `in_parallel`.
        pub(super) static PARALLEL: Cell<bool> = const { Cell::new(true) };
    }

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

    /// What can be dropped on the window is what the file picker offers.
    #[test]
    fn dropping_takes_what_the_picker_offers() {
        let js = include_str!("../../ui/app.js");
        let list = js.split("const MEDIA_EXTS = [").nth(1).unwrap().split(']').next().unwrap();
        let mut dropped: Vec<&str> = list.split(',').map(|s| s.trim().trim_matches('\'')).filter(|s| !s.is_empty()).collect();
        let mut picked: Vec<&str> = AAC_EXTS.iter().chain(&AUDIO_EXTS).chain(&VIDEO_EXTS).copied().collect();
        dropped.sort_unstable();
        picked.sort_unstable();
        assert_eq!(dropped, picked);
    }

    /// Merges short tone clips with a 0.5 s silence and effects at the start,
    /// between and at the end: every part is there, to the sample (a short
    /// part once ended the join, and joins lost or added sound).
    #[test]
    fn merges_every_part_whole() {
        let ff = ffmpeg::locate(None).unwrap();
        let dir = std::env::temp_dir().join(format!("spacecutter_parts_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Two tones and a burst of noise, written here as 48 kHz stereo WAV
        // (the app's ffmpeg has no sound generators); the tones made m4a by it
        let wav = |name: &str, secs: f64, sample: &dyn Fn(usize) -> f64| {
            let frames = (secs * 48000.0).round() as usize;
            let size = (frames * 4) as u32;
            let mut data = Vec::with_capacity(44 + frames * 4);
            data.extend_from_slice(b"RIFF");
            data.extend_from_slice(&(36 + size).to_le_bytes());
            data.extend_from_slice(b"WAVEfmt ");
            for field in [16u32, 1 | 2 << 16, 48000, 48000 * 4, 4 | 16 << 16] {
                data.extend_from_slice(&field.to_le_bytes());
            }
            data.extend_from_slice(b"data");
            data.extend_from_slice(&size.to_le_bytes());
            for i in 0..frames {
                let s = ((sample(i) * 32767.0) as i16).to_le_bytes();
                data.extend_from_slice(&s);
                data.extend_from_slice(&s);
            }
            let path = dir.join(name);
            fs::write(&path, data).unwrap();
            path
        };
        let tone = |name: &str, hz: f64, secs: f64| {
            let source = wav(&format!("{name}.wav"), secs, &|i| (i as f64 * hz * std::f64::consts::TAU / 48000.0).sin() * 0.5);
            let path = dir.join(name);
            let ok = Command::new(&ff).args(["-v", "error", "-i"]).arg(&source).args(["-ac", "2", "-y"]).arg(&path).status().unwrap().success();
            assert!(ok, "made {name}");
            path_str(&path)
        };
        let a = tone("a.m4a", 1000.0, 2.0);
        let b = tone("b.m4a", 2000.0, 1.5);
        let noise = std::cell::Cell::new(0x2545_f491_u32);
        let fx = path_str(&wav("fx.wav", 0.4, &|_| {
            let x = noise.get().wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            noise.set(x);
            (x as f64 / u32::MAX as f64 * 2.0 - 1.0) * 0.3
        }));
        let clip = |id, url: &str| {
            let mut r = row(id, url, "", "", &format!("c{id}"));
            r.full = true;
            r
        };
        let mut jingles = HashMap::new();
        jingles.insert("j_before".to_string(), Jingle { path: fx.clone(), gap: 0.0 });
        jingles.insert("j_0".to_string(), Jingle { path: String::new(), gap: 0.5 });
        jingles.insert("j_after".to_string(), Jingle { path: fx.clone(), gap: 0.0 });
        let out = dir.join("out");
        let job = Job { rows: vec![clip(1, &a), clip(2, &b)], output_folder: path_str(&out), merge: true, merge_name: "merged".into(), keep_clips: true, jingles };
        run(&job);
        let parts = length(&out.join("c1.m4a")) + 0.5 + length(&out.join("c2.m4a")) + 2.0 * length(Path::new(&fx));
        let merged = length(&out.join("merged.m4a"));
        println!("parts {parts:.3} s, merged {merged:.3} s");
        assert!((merged - parts).abs() < 0.05, "every part is in the merged file: {merged:.3} s of {parts:.3} s");
        let _ = fs::remove_dir_all(&dir);
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

    /// A fingerprint of each AAC frame of a file's sound, in order.
    fn frames_of(path: &Path) -> Vec<String> {
        let out = Command::new(ffmpeg::locate(None).unwrap())
            .args(["-v", "error", "-i", &path_str(path), "-map", "0:a", "-c", "copy", "-f", "framemd5", "-"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.rsplit(',').next().map(|md5| md5.trim().to_string()))
            .collect()
    }

    /// The same clip cut both ways, timed: ffmpeg reading the list itself,
    /// one piece after another, and the pieces fetched several at a time.
    /// Both must hold the same AAC frames, bit for bit, but for a frame or
    /// two where they start and end. Where the Space has a gap (a piece
    /// listed longer than its sound, the host's connection having dropped),
    /// ffmpeg keeps it as a pause, so its clip is that much longer; the fed
    /// pieces run on without it, as the player plays them:
    /// SPACE_URL='https://…' CLIP='01:00:00-01:20:00' cargo test -- --ignored --nocapture cuts_the_same
    #[test]
    #[ignore]
    fn cuts_the_same_with_the_pieces_fetched_several_at_a_time() {
        let url = std::env::var("SPACE_URL").expect("set SPACE_URL");
        let clip = std::env::var("CLIP").unwrap_or_else(|_| "01:00:00-01:20:00".into());
        let (start, end) = clip.split_once('-').unwrap();
        let dir = std::env::temp_dir().join(format!("spacecutter_same_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        hls::resolve(&url, None).unwrap();
        // The second cut finds its pieces in the server's cache; FIRST=several for the other order
        let mut ways = [(false, "one-by-one"), (true, "several")];
        if std::env::var("FIRST").is_ok_and(|f| f == "several") {
            ways.reverse();
        }
        for (parallel, name) in ways {
            PARALLEL.with(|p| p.set(parallel));
            let t = Instant::now();
            let events = run(&Job { rows: vec![row(1, &url, start, end, name)], output_folder: path_str(&dir), merge: false, merge_name: String::new(), keep_clips: true, jingles: HashMap::new() });
            println!("{name}: {:.1} s", t.elapsed().as_secs_f64());
            assert!(events.iter().any(|e| e.contains("Done")), "{events:#?}");
        }
        PARALLEL.with(|p| p.set(true));
        let (a, b) = (dir.join("one-by-one.m4a"), dir.join("several.m4a"));
        let (la, lb) = (length(&a), length(&b));
        let (fa, fb) = (frames_of(&a), frames_of(&b));
        if std::env::var("KEEP").is_ok() {
            println!("kept in {}", dir.display());
        } else {
            let _ = fs::remove_dir_all(&dir);
        }
        // Where the second starts in the first, or the first in the second
        let shift = (0..3)
            .find_map(|k| (fa.get(k) == fb.first()).then_some(k as i64))
            .or_else(|| (1..3).find_map(|k| (fb.get(k) == fa.first()).then_some(-(k as i64))))
            .expect("the starts differ");
        let (sa, sb) = (shift.max(0) as usize, (-shift).max(0) as usize);
        let n = (fa.len() - sa).min(fb.len() - sb);
        let same = (0..n).take_while(|&i| fa[sa + i] == fb[sb + i]).count();
        println!("lengths: {la:.3} s and {lb:.3} s; frames: {} and {}, the same for {same} of {n} (one starting {shift} on)", fa.len(), fb.len());
        assert_eq!(same, n, "the frames differ from frame {same} on");
        assert!(fa.len().abs_diff(fb.len()) <= 4, "one has more frames than the other");
        assert!(lb <= la + 0.05, "the fed pieces came out longer");
    }

    /// A whole Space cut from its dynamic link, as the app does it, timed;
    /// WAY=one-by-one for ffmpeg reading the list itself:
    /// DYNAMIC_URL='https://…' WAY=several cargo test -- --ignored --nocapture times_a_whole
    #[test]
    #[ignore]
    fn times_a_whole_space() {
        let dynamic = std::env::var("DYNAMIC_URL").expect("set DYNAMIC_URL");
        let parallel = std::env::var("WAY").map_or(true, |w| w != "one-by-one");
        PARALLEL.with(|p| p.set(parallel));
        let replay = hls::live_to_replay(&dynamic).unwrap();
        let dir = std::env::temp_dir().join(format!("spacecutter_whole_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut whole = row(1, &replay, "", "", "whole");
        whole.full = true;
        whole.live = dynamic;
        let t = Instant::now();
        let events = run(&Job { rows: vec![whole], output_folder: path_str(&dir), merge: false, merge_name: String::new(), keep_clips: true, jingles: HashMap::new() });
        let took = t.elapsed().as_secs_f64();
        let got = length(&dir.join("whole.m4a"));
        let _ = fs::remove_dir_all(&dir);
        PARALLEL.with(|p| p.set(true));
        println!("{}: {:.1} min for {:.1} min of sound", if parallel { "several at a time" } else { "one by one" }, took / 60.0, got / 60.0);
        assert!(events.iter().any(|e| e.contains("Done")), "{events:#?}");
    }

    /// A job as the window sends it, read from the JSON file in JOB, run and
    /// timed; what it reports is printed with the seconds since the start:
    /// JOB=job.json cargo test -- --ignored --nocapture runs_a_job_from
    #[test]
    #[ignore]
    fn runs_a_job_from_a_file() {
        let job: Job = serde_json::from_str(&fs::read_to_string(std::env::var("JOB").expect("set JOB")).unwrap()).unwrap();
        let t = Instant::now();
        let emit = |name: &str, payload: Value| {
            if name != "job-progress" {
                println!("{:8.1} s  {name} {payload}", t.elapsed().as_secs_f64());
            }
        };
        run_job(&emit, ffmpeg::locate(None), &Control::default(), &job);
        println!("{:8.1} s  finished", t.elapsed().as_secs_f64());
    }

    /// A cancel stops a cut at once, fetching included, and leaves nothing behind.
    #[test]
    #[ignore]
    fn a_cancel_stops_the_fetching_at_once() {
        let url = std::env::var("SPACE_URL").expect("set SPACE_URL");
        let dir = std::env::temp_dir().join(format!("spacecutter_cancel_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        hls::resolve(&url, None).unwrap();
        let mut whole = row(1, &url, "", "", "whole");
        whole.full = true;
        let job = Job { rows: vec![whole], output_folder: path_str(&dir), merge: false, merge_name: String::new(), keep_clips: true, jingles: HashMap::new() };
        let ctl = Arc::new(Control::default());
        let stopper = {
            let ctl = ctl.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(5));
                ctl.stop();
                Instant::now()
            })
        };
        run_job(&|_: &str, _: Value| {}, ffmpeg::locate(None), &ctl, &job);
        let took = stopper.join().unwrap().elapsed();
        let left: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        let _ = fs::remove_dir_all(&dir);
        println!("stopped {:.2} s after the cancel; left behind: {left:?}", took.as_secs_f64());
        assert!(took < Duration::from_secs(2));
        assert!(left.is_empty());
    }

    /// A piece the server hasn't got is left out and the clip still comes
    /// out, about a piece shorter; with too many gone it is cut the old way.
    #[test]
    #[ignore]
    fn leaves_out_a_missing_piece() {
        let url = std::env::var("SPACE_URL").expect("set SPACE_URL");
        let dir = std::env::temp_dir().join(format!("spacecutter_gone_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut list = hls::resolve(&url, None).unwrap();
        let ctl = Control::default();
        let worker = Worker { emit: &|_: &str, _: Value| {}, ffmpeg: ffmpeg::locate(None).unwrap(), out: dir.clone(), ctl: &ctl, warned: Cell::new(false), meter: Meter::default() };
        let k = list.segments.iter().position(|s| s.start > 630.0).unwrap();
        list.segments[k].url = list.segments[k].url.replace("chunk_", "chunk_gone_");
        let ts = dir.join("one-gone.ts");
        let one = worker.fetched(1, &list, Some((600, 660)), &path_str(&ts), 60.0);
        let got = length(&ts);
        for s in list.segments.iter_mut().skip(k + 2).take(6) {
            s.url = s.url.replace("chunk_", "chunk_gone_");
        }
        let many = worker.fetched(2, &list, Some((600, 660)), &path_str(&dir.join("many-gone.ts")), 60.0);
        let _ = fs::remove_dir_all(&dir);
        println!("one piece gone: {one:?}, {got:.2} s; seven gone: {many:?}");
        assert_eq!(one, Some(true));
        assert!(got > 56.0 && got < 60.5, "{got} s");
        assert_eq!(many, Some(false));
    }

    #[test]
    fn refuses_a_clip_that_starts_after_the_space() {
        let list = |ended| hls::Playlist {
            segments: (0..3).map(|i| hls::Segment { start: i as f64 * 3.0, dur: 3.0, url: String::new() }).collect(),
            duration: 9.0,
            started: None,
            url: String::new(),
            moved: false,
            ended,
            ends_at: None,
        };
        assert_eq!(past_the_end(&list(true), 8), None, "it starts in the last piece");
        assert_eq!(past_the_end(&list(true), 9).as_deref(), Some("starts after the Space ends (00:00:09)"));
        assert_eq!(past_the_end(&list(false), 60).as_deref(), Some("starts after where the live Space has got to (00:00:09)"));
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
