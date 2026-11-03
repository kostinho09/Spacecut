//! Listening to a Space (or a file) inside the app.
//!
//! A Space replay is a list of ~3 s AAC segments. Playback fetches them from
//! the one under the playhead onward, a few at a time (see `fetch`), strips
//! their ID3 headers and pipes the raw AAC into ffmpeg, in order, which hands
//! back PCM for the speaker. Feeding the
//! segments ourselves keeps ffmpeg away from the stream timestamps, so the
//! MPEG-TS wrap that breaks seeking in long Spaces never reaches it, and the
//! playhead sits on the same #EXTINF timeline the cuts use.
//!
//! A Space still on air grows as it plays: its live list, which holds only
//! the last few pieces, names the new ones every few seconds, and any it
//! skipped (say, while a request hung) come from a fresh replay.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use serde::Serialize;

use crate::ffmpeg;
use crate::fetch;
use crate::hls::{self, Segment};
use crate::util::quiet;

/// Seconds of decoded audio kept ready ahead of the speaker.
const AHEAD_SECS: f64 = 4.0;

/// How far behind the newest piece "live" plays, so a piece that is slow to
/// come (10 s at worst, as measured) doesn't break the sound.
pub const LIVE_BACK: f64 = 10.0;

/// How often a live Space's list is asked for its newest pieces: while it
/// plays, and while paused. The list holds only ~9 s, so never less often.
const LIVE_EVERY: Duration = Duration::from_millis(2500);
const LIVE_EVERY_PAUSED: Duration = Duration::from_secs(6);

/// A fresh replay for pieces the live list skipped, at most this often.
const TOP_UP_EVERY: Duration = Duration::from_secs(15);

/// With no new piece for this long, a fresh replay tells whether the Space is over.
const LIVE_QUIET: Duration = Duration::from_secs(60);

/// What the decoder thread and the audio callback share.
#[derive(Default)]
struct Shared {
    buf: Mutex<VecDeque<f32>>,
    room: Condvar,
    /// A sound effect being previewed, mixed over whatever plays.
    fx: Mutex<VecDeque<f32>>,
    playing: AtomicBool,
    ended: AtomicBool,
    /// The decoder of the current session has delivered everything.
    eof: AtomicBool,
    /// Frames played since the current session started.
    frames: AtomicU64,
    /// Bumped on every seek or load; older decoders notice and stop.
    gen: AtomicU64,
    rate: AtomicU32,
    channels: AtomicU32,
    broken: AtomicBool,
}

enum Source {
    Hls(Arc<Vec<Segment>>),
    Live(Arc<Live>),
    Direct(String),
}

/// A Space on air: its pieces so far, added to as new ones come out.
#[derive(Default)]
struct Live {
    state: Mutex<OnAir>,
    grew: Condvar,
    /// Another source was loaded: stop following this one.
    stop: AtomicBool,
}

#[derive(Default)]
struct OnAir {
    segments: Vec<Segment>,
    ended: bool,
}

impl Live {
    fn duration(&self) -> f64 {
        self.state.lock().unwrap().segments.last().map_or(0.0, |s| s.start + s.dur)
    }

    fn ended(&self) -> bool {
        self.state.lock().unwrap().ended
    }

    fn end(&self) {
        self.state.lock().unwrap().ended = true;
        self.grew.notify_all();
    }

    /// Adds the pieces the live list names after the last one held.
    fn add(&self, pieces: &[hls::LivePiece]) -> News {
        let mut st = self.state.lock().unwrap();
        let Some(mut have) = st.segments.last().and_then(|s| hls::piece_number(&s.url)) else { return News::Skipped };
        let mut news = News::None;
        let held = have;
        for piece in pieces.iter().filter(|p| p.seq > held) {
            // Pieces it no longer names: only a replay has those
            if piece.seq != have + 1 {
                news = News::Skipped;
                break;
            }
            let last = &st.segments[st.segments.len() - 1];
            let next = Segment { start: last.start + last.dur, dur: piece.dur, url: beside(&last.url, &piece.name) };
            st.segments.push(next);
            have = piece.seq;
            news = News::Added;
        }
        drop(st);
        self.grew.notify_all();
        news
    }

    /// Takes the pieces past the last one held from a fresh replay of the
    /// Space; true if there were any.
    fn top_up(&self, url: &str, given: Option<&str>) -> bool {
        let Ok(list) = hls::resolve(url, given) else { return false };
        let mut st = self.state.lock().unwrap();
        let have = st.segments.last().and_then(|s| hls::piece_number(&s.url));
        let Some(from) = list.segments.iter().position(|s| hls::piece_number(&s.url) > have) else { return false };
        // On the timeline the pieces already held set
        let mut t = st.segments.last().map_or(0.0, |s| s.start + s.dur);
        for seg in &list.segments[from..] {
            st.segments.push(Segment { start: t, dur: seg.dur, url: seg.url.clone() });
            t += seg.dur;
        }
        drop(st);
        self.grew.notify_all();
        true
    }
}

#[derive(PartialEq)]
enum News {
    None,
    Added,
    Skipped,
}

/// A piece named by the live list, where the pieces held so far are kept.
fn beside(last: &str, name: &str) -> String {
    let (path, query) = last.split_once('?').map_or((last, None), |(p, q)| (p, Some(q)));
    let folder = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let name = name.split('?').next().unwrap_or(name);
    match query {
        Some(q) => format!("{folder}{name}?{q}"),
        None => format!("{folder}{name}"),
    }
}

/// Follows a Space on air until it is over or another source is loaded.
fn follow(live: Arc<Live>, shared: Arc<Shared>, dynamic: String, url: String, given: Option<String>) {
    let mut news = Instant::now();
    let mut topped = Instant::now() - TOP_UP_EVERY;
    loop {
        let every = if shared.playing.load(Relaxed) { LIVE_EVERY } else { LIVE_EVERY_PAUSED };
        let st = live.state.lock().unwrap();
        let (st, _) = live.grew.wait_timeout_while(st, every, |_| !live.stop.load(Relaxed)).unwrap();
        drop(st);
        if live.stop.load(Relaxed) {
            return;
        }
        match hls::check_live(&dynamic) {
            hls::LiveCheck::Over => {
                live.top_up(&url, given.as_deref());
                live.end();
                return;
            }
            hls::LiveCheck::On(pieces) => {
                let grew = match live.add(&pieces) {
                    News::Added => true,
                    News::Skipped if topped.elapsed() >= TOP_UP_EVERY => {
                        topped = Instant::now();
                        live.top_up(&url, given.as_deref())
                    }
                    _ => false,
                };
                if grew {
                    news = Instant::now();
                } else if news.elapsed() > LIVE_QUIET {
                    if !live.top_up(&url, given.as_deref()) {
                        live.end();
                        return;
                    }
                    news = Instant::now();
                }
            }
            hls::LiveCheck::Unknown => {}
        }
    }
}

type ChildSlot = Arc<Mutex<Option<Child>>>;

#[derive(Default)]
struct Inner {
    source: Option<Arc<Source>>,
    url: String,
    duration: f64,
    /// Where the current session started, in seconds.
    base: f64,
    /// Playback speed: 1 is normal, 0.5 half, 2 double.
    speed: f64,
    session: bool,
    child: Option<ChildSlot>,
    error: Arc<Mutex<Option<String>>>,
    device: Option<String>,
    /// Wakes the audio thread; dropping it ends the thread and its stream.
    wake: Option<mpsc::Sender<()>>,
}

pub struct Player {
    shared: Arc<Shared>,
    inner: Mutex<Inner>,
    ffmpeg: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct LoadInfo {
    duration: f64,
    started: Option<String>,
    /// A Space still on air.
    live: bool,
}

#[derive(Serialize)]
pub struct State {
    loaded: bool,
    url: String,
    duration: f64,
    pos: f64,
    playing: bool,
    buffering: bool,
    ended: bool,
    error: Option<String>,
    fx: bool,
    live: bool,
}

impl Player {
    pub fn new(ffmpeg: Option<PathBuf>) -> Self {
        let inner = Inner { speed: 1.0, ..Inner::default() };
        Self { shared: Arc::new(Shared::default()), inner: Mutex::new(inner), ffmpeg }
    }

    /// Opens a Space or a file; `live` is a Space's dynamic link, if known.
    pub fn load(&self, url: &str, live: Option<&str>) -> Result<LoadInfo, String> {
        let url = url.trim();
        if url.is_empty() {
            return Err("Paste a Replay URL or choose a file first".into());
        }
        let (source, duration, started) = if hls::is_m3u8(url) {
            let list = hls::resolve(url, live)?;
            match hls::dynamic_for(url, live).filter(|_| !list.ended) {
                Some(dynamic) => {
                    let on_air = Arc::new(Live { state: Mutex::new(OnAir { segments: list.segments, ended: false }), ..Live::default() });
                    let (follower, shared, url, given) = (on_air.clone(), self.shared.clone(), url.to_string(), live.map(str::to_string));
                    thread::spawn(move || follow(follower, shared, dynamic, url, given));
                    (Source::Live(on_air), list.duration, list.started)
                }
                None => (Source::Hls(Arc::new(list.segments)), list.duration, list.started),
            }
        } else {
            let web = url.starts_with("http://") || url.starts_with("https://");
            if !web && !Path::new(url).exists() {
                return Err("File not found".into());
            }
            let ff = self.ffmpeg.as_deref().ok_or("FFmpeg not found")?;
            (Source::Direct(url.to_string()), ffmpeg::probe_duration(ff, url).unwrap_or(0.0), None)
        };
        let live = matches!(source, Source::Live(_));
        let mut inner = self.inner.lock().unwrap();
        self.shared.playing.store(false, Relaxed);
        self.stop_session(&mut inner);
        let_go(&inner);
        inner.source = Some(Arc::new(source));
        inner.url = url.to_string();
        inner.duration = duration;
        inner.base = 0.0;
        Ok(LoadInfo { duration, started, live })
    }

    pub fn play(&self) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.source.is_none() {
            return Err("Nothing is loaded".into());
        }
        let format = self.format();
        let mut pos = self.position(&inner);
        self.ensure_audio(&mut inner)?;
        let duration = duration_of(&inner);
        let ended = self.shared.ended.load(Relaxed) || (duration > 0.0 && pos >= duration - 0.5);
        if ended {
            // Caught up with a Space on air: back on live; otherwise from the start
            pos = if on_air(&inner) { (duration - LIVE_BACK).max(0.0) } else { 0.0 };
        }
        let failed = inner.error.lock().unwrap().is_some();
        if !inner.session || ended || failed || format != self.format() {
            self.restart(&mut inner, pos);
        }
        self.shared.playing.store(true, Relaxed);
        Self::wake(&inner);
        Ok(())
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Relaxed);
    }

    pub fn seek(&self, t: f64) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.source.is_none() {
            return Err("Nothing is loaded".into());
        }
        let duration = duration_of(&inner);
        let end = if duration > 0.0 { duration } else { f64::MAX };
        self.ensure_audio(&mut inner)?;
        // Decoding starts right away, so playing after a paused seek is instant
        self.restart(&mut inner, t.clamp(0.0, end));
        Self::wake(&inner);
        Ok(())
    }

    /// Changes the playback speed, carrying on from the same spot.
    pub fn set_speed(&self, speed: f64) -> Result<(), String> {
        let speed = speed.clamp(0.5, 2.0);
        let mut inner = self.inner.lock().unwrap();
        if (speed - inner.speed).abs() < 1e-6 {
            return Ok(());
        }
        let pos = self.position(&inner);
        inner.speed = speed;
        if inner.session {
            self.ensure_audio(&mut inner)?;
            self.restart(&mut inner, pos);
            Self::wake(&inner);
        }
        Ok(())
    }

    pub fn state(&self) -> State {
        let inner = self.inner.lock().unwrap();
        let sh = &self.shared;
        let playing = sh.playing.load(Relaxed);
        let buffering = playing && !sh.eof.load(Relaxed) && sh.buf.lock().map(|b| b.is_empty()).unwrap_or(false);
        let error = inner.error.lock().unwrap().clone();
        State {
            loaded: inner.source.is_some(),
            url: inner.url.clone(),
            duration: duration_of(&inner),
            pos: self.position(&inner),
            playing,
            buffering,
            ended: sh.ended.load(Relaxed),
            error,
            fx: sh.fx.lock().map(|f| !f.is_empty()).unwrap_or(false),
            live: on_air(&inner),
        }
    }

    /// Previews a sound effect over whatever is playing (first minute at most).
    pub fn fx_play(&self, path: &str) -> Result<(), String> {
        let ff = self.ffmpeg.as_deref().ok_or("FFmpeg not found")?;
        let (rate, channels) = {
            let mut inner = self.inner.lock().unwrap();
            self.ensure_audio(&mut inner)?;
            self.format()
        };
        let mut cmd = Command::new(ff);
        cmd.args(["-hide_banner", "-loglevel", "error", "-i", path, "-t", "60", "-vn", "-f", "f32le"])
            .args(["-ac", &channels.to_string(), "-ar", &rate.to_string(), "pipe:1"])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        quiet(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        let samples: VecDeque<f32> = out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        if samples.is_empty() {
            return Err("Could not play this sound".into());
        }
        *self.shared.fx.lock().unwrap() = samples;
        Self::wake(&self.inner.lock().unwrap());
        Ok(())
    }

    pub fn fx_stop(&self) {
        self.shared.fx.lock().unwrap().clear();
    }

    pub fn shutdown(&self) {
        let mut inner = self.inner.lock().unwrap();
        self.shared.playing.store(false, Relaxed);
        self.stop_session(&mut inner);
        let_go(&inner);
        inner.wake = None;
    }

    fn format(&self) -> (u32, u32) {
        (self.shared.rate.load(Relaxed), self.shared.channels.load(Relaxed))
    }

    fn position(&self, inner: &Inner) -> f64 {
        let rate = self.shared.rate.load(Relaxed).max(1) as f64;
        // Each second played covers `speed` seconds of the recording
        let pos = inner.base + self.shared.frames.load(Relaxed) as f64 / rate * inner.speed;
        let duration = duration_of(inner);
        if duration > 0.0 {
            pos.min(duration)
        } else {
            pos
        }
    }

    fn wake(inner: &Inner) {
        if let Some(w) = &inner.wake {
            let _ = w.send(());
        }
    }

    /// Opens the output stream, again if the default device changed (say,
    /// headphones plugged in) or the old stream broke.
    fn ensure_audio(&self, inner: &mut Inner) -> Result<(), String> {
        let device = cpal::default_host().default_output_device().and_then(|d| d.name().ok());
        if inner.wake.is_some() && !self.shared.broken.load(Relaxed) && device == inner.device {
            return Ok(());
        }
        inner.wake = None;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (wake_tx, wake_rx) = mpsc::channel();
        let shared = self.shared.clone();
        thread::spawn(move || audio_thread(shared, ready_tx, wake_rx));
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("The audio output stopped".into()),
        }
        self.shared.broken.store(false, Relaxed);
        inner.wake = Some(wake_tx);
        inner.device = device;
        Ok(())
    }

    fn stop_session(&self, inner: &mut Inner) {
        let sh = &self.shared;
        sh.gen.fetch_add(1, Relaxed);
        kill(inner.child.take());
        sh.buf.lock().unwrap().clear();
        sh.room.notify_all();
        sh.frames.store(0, Relaxed);
        sh.eof.store(false, Relaxed);
        sh.ended.store(false, Relaxed);
        *inner.error.lock().unwrap() = None;
        inner.session = false;
    }

    /// Starts decoding from `at`, dropping whatever was buffered.
    fn restart(&self, inner: &mut Inner, at: f64) {
        self.stop_session(inner);
        inner.base = at;
        let (Some(source), Some(ffmpeg)) = (inner.source.clone(), self.ffmpeg.clone()) else {
            *inner.error.lock().unwrap() = Some("FFmpeg not found".into());
            self.shared.eof.store(true, Relaxed);
            return;
        };
        let gen = self.shared.gen.load(Relaxed);
        let slot = ChildSlot::default();
        inner.child = Some(slot.clone());
        inner.session = true;
        let (shared, error) = (self.shared.clone(), inner.error.clone());
        let (rate, channels) = self.format();
        let speed = inner.speed;
        thread::spawn(move || {
            let result = decode(&ffmpeg, &source, at, speed, rate, channels, gen, &shared, &slot);
            if shared.gen.load(Relaxed) == gen {
                if let Err(e) = result {
                    *error.lock().unwrap() = Some(e);
                }
                shared.eof.store(true, Relaxed);
            }
        });
    }
}

/// How long the loaded recording is: so far, for a Space on air.
fn duration_of(inner: &Inner) -> f64 {
    match inner.source.as_deref() {
        Some(Source::Live(live)) => live.duration(),
        _ => inner.duration,
    }
}

fn on_air(inner: &Inner) -> bool {
    matches!(inner.source.as_deref(), Some(Source::Live(live)) if !live.ended())
}

/// Stops following the Space on air loaded until now, if it was one.
fn let_go(inner: &Inner) {
    if let Some(Source::Live(live)) = inner.source.as_deref() {
        live.stop.store(true, Relaxed);
        live.grew.notify_all();
    }
}

fn kill(slot: Option<ChildSlot>) {
    if let Some(slot) = slot {
        if let Some(mut child) = slot.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Owns the output stream, running it only while something plays so the Mac
/// is free to sleep when the app sits idle.
fn audio_thread(shared: Arc<Shared>, ready: mpsc::Sender<Result<(), String>>, wake: mpsc::Receiver<()>) {
    let stream = match build_stream(&shared) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut running = false;
    loop {
        if let Err(mpsc::RecvTimeoutError::Disconnected) = wake.recv_timeout(Duration::from_millis(250)) {
            break;
        }
        let wanted = shared.playing.load(Relaxed) || shared.fx.lock().map(|f| !f.is_empty()).unwrap_or(false);
        if wanted != running {
            if wanted {
                let _ = stream.play();
            } else {
                let _ = stream.pause();
            }
            running = wanted;
        }
    }
}

fn build_stream(shared: &Arc<Shared>) -> Result<cpal::Stream, String> {
    let device = cpal::default_host().default_output_device().ok_or("No audio output found")?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    shared.rate.store(config.sample_rate().0, Relaxed);
    shared.channels.store(config.channels() as u32, Relaxed);
    let stream_config: cpal::StreamConfig = config.clone().into();
    match config.sample_format() {
        cpal::SampleFormat::F32 => open::<f32>(&device, &stream_config, shared),
        cpal::SampleFormat::I16 => open::<i16>(&device, &stream_config, shared),
        cpal::SampleFormat::U16 => open::<u16>(&device, &stream_config, shared),
        cpal::SampleFormat::I32 => open::<i32>(&device, &stream_config, shared),
        other => Err(format!("Unsupported audio format {other:?}")),
    }
}

fn open<T>(device: &cpal::Device, config: &cpal::StreamConfig, shared: &Arc<Shared>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let (sh, broken) = (shared.clone(), shared.clone());
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| fill(data, channels, &sh),
            move |_| broken.broken.store(true, Relaxed),
            None,
        )
        .map_err(|e| e.to_string())
}

fn fill<T>(data: &mut [T], channels: usize, sh: &Shared)
where
    T: SizedSample + FromSample<f32>,
    f32: FromSample<T>,
{
    let mut n = 0;
    if sh.playing.load(Relaxed) {
        if let Ok(mut buf) = sh.buf.try_lock() {
            n = data.len().min(buf.len());
            n -= n % channels;
            for (d, s) in data.iter_mut().zip(buf.drain(..n)) {
                *d = T::from_sample(s);
            }
            let drained = buf.is_empty();
            drop(buf);
            if n > 0 {
                sh.frames.fetch_add((n / channels) as u64, Relaxed);
                sh.room.notify_one();
            }
            if drained && n < data.len() && sh.eof.load(Relaxed) {
                sh.playing.store(false, Relaxed);
                sh.ended.store(true, Relaxed);
            }
        }
    }
    for d in &mut data[n..] {
        *d = T::EQUILIBRIUM;
    }
    if let Ok(mut fx) = sh.fx.try_lock() {
        let m = data.len().min(fx.len());
        for (d, s) in data.iter_mut().zip(fx.drain(..m)) {
            *d = T::from_sample((f32::from_sample(*d) + s).clamp(-1.0, 1.0));
        }
    }
}

/// The pieces a session feeds ffmpeg: a fixed list, or one still growing.
enum Feed {
    Fixed(Arc<Vec<Segment>>, usize),
    Live(Arc<Live>, usize),
}

/// One playback session: ffmpeg decoding from `at` onward into the shared
/// buffer, at `speed` (pitch kept by ffmpeg's atempo filter).
#[allow(clippy::too_many_arguments)]
fn decode(
    ffmpeg: &Path,
    source: &Source,
    at: f64,
    speed: f64,
    rate: u32,
    channels: u32,
    gen: u64,
    sh: &Arc<Shared>,
    slot: &ChildSlot,
) -> Result<(), String> {
    let live = || sh.gen.load(Relaxed) == gen;
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    let mut skip = 0.0;
    let mut feed = None;
    match source {
        Source::Hls(segments) => {
            let first = segments
                .iter()
                .position(|s| s.start + s.dur > at)
                .ok_or("This is the end of the recording")?;
            skip = (at - segments[first].start).max(0.0);
            cmd.args(["-f", "aac", "-i", "pipe:0"]).stdin(Stdio::piped());
            feed = Some(Feed::Fixed(segments.clone(), first));
        }
        Source::Live(live) => {
            let st = live.state.lock().unwrap();
            let first = st
                .segments
                .iter()
                .position(|s| s.start + s.dur > at)
                .or_else(|| st.segments.len().checked_sub(1))
                .ok_or("This is the end of the recording")?;
            skip = (at - st.segments[first].start).max(0.0);
            drop(st);
            cmd.args(["-f", "aac", "-i", "pipe:0"]).stdin(Stdio::piped());
            feed = Some(Feed::Live(live.clone(), first));
        }
        Source::Direct(input) => {
            let from = format!("{at:.3}");
            cmd.args(["-ss", from.as_str(), "-i", input.as_str(), "-vn"]).stdin(Stdio::null());
        }
    }
    if (speed - 1.0).abs() > 1e-6 {
        cmd.args(["-af", &format!("atempo={speed}")]);
    }
    let (ac, ar) = (channels.to_string(), rate.to_string());
    cmd.args(["-f", "f32le", "-ac", ac.as_str(), "-ar", ar.as_str(), "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    quiet(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| format!("Could not start ffmpeg: {e}"))?;
    let stdin = child.stdin.take();
    let mut stdout = child.stdout.take().ok_or("ffmpeg gave no output")?;
    let stderr = child.stderr.take();
    *slot.lock().unwrap() = Some(child);
    if !live() {
        kill(Some(slot.clone()));
        return Ok(());
    }

    let errors = stderr.map(|mut e| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = e.read_to_string(&mut s);
            s
        })
    });

    let feeder = match (feed, stdin) {
        (Some(Feed::Fixed(segments, first)), Some(mut pipe)) => {
            let sh = sh.clone();
            Some(thread::spawn(move || -> Result<(), String> {
                // A few pieces ahead are fetched at once, so a slow one doesn't hold up the sound
                let urls = segments[first..].iter().map(|s| s.url.clone()).collect();
                fetch::in_order(urls, fetch::AHEAD_PLAY, move || sh.gen.load(Relaxed) != gen, |_, piece| pipe.write_all(piece).is_ok())
                    .map_err(|e| format!("Could not load the audio ({e})"))
            }))
        }
        (Some(Feed::Live(live, first)), Some(mut pipe)) => {
            let sh = sh.clone();
            Some(thread::spawn(move || -> Result<(), String> {
                let mut next = first;
                loop {
                    // The pieces out so far, then on as new ones come
                    let urls: Vec<String> = {
                        let mut st = live.state.lock().unwrap();
                        while st.segments.len() <= next && !st.ended && sh.gen.load(Relaxed) == gen && !live.stop.load(Relaxed) {
                            st = live.grew.wait_timeout(st, Duration::from_millis(300)).unwrap().0;
                        }
                        if st.segments.len() <= next || sh.gen.load(Relaxed) != gen {
                            return Ok(());
                        }
                        st.segments[next..].iter().map(|s| s.url.clone()).collect()
                    };
                    next += urls.len();
                    let mut open = true;
                    let stop = { let sh = sh.clone(); move || sh.gen.load(Relaxed) != gen };
                    fetch::in_order(urls, fetch::AHEAD_PLAY, stop, |_, piece| {
                        open = pipe.write_all(piece).is_ok();
                        open
                    })
                    .map_err(|e| format!("Could not load the audio ({e})"))?;
                    if !open {
                        return Ok(());
                    }
                }
            }))
        }
        _ => None,
    };

    let per_frame = channels as usize;
    // The skip is into the recording; sped up, it takes less output to cover
    let mut to_skip = (skip / speed * rate as f64) as usize * per_frame;
    let cap = (AHEAD_SECS * rate as f64) as usize * per_frame;
    let mut raw = vec![0u8; 32 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut decoded = false;
    while live() {
        let n = match stdout.read(&mut raw) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        pending.extend_from_slice(&raw[..n]);
        let whole = pending.len() - pending.len() % 4;
        let mut samples: Vec<f32> = pending[..whole]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        pending.drain(..whole);
        if to_skip > 0 {
            let k = to_skip.min(samples.len());
            samples.drain(..k);
            to_skip -= k;
        }
        if samples.is_empty() {
            continue;
        }
        decoded = true;
        let mut buf = sh.buf.lock().unwrap();
        while buf.len() >= cap && live() {
            buf = sh.room.wait_timeout(buf, Duration::from_millis(50)).unwrap().0;
        }
        if !live() {
            break;
        }
        buf.extend(samples);
    }

    let child = slot.lock().unwrap().take();
    let status = child.and_then(|mut c| {
        if !live() {
            let _ = c.kill();
        }
        c.wait().ok()
    });
    drop(stdout);
    let fed = feeder.map(|f| f.join().unwrap_or(Ok(()))).unwrap_or(Ok(()));
    let log = errors.and_then(|e| e.join().ok()).unwrap_or_default();
    if !live() {
        return Ok(());
    }
    fed?;
    if !decoded && !status.is_some_and(|s| s.success()) {
        let last = log.lines().last().unwrap_or("").trim();
        return Err(if last.is_empty() { "Could not play this source".into() } else { last.to_string() });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! These need the network and a replay that has not expired:
    //! SPACE_URL='https://…/playlist_….m3u8?type=replay' cargo test -- --ignored --nocapture

    use super::*;
    use std::time::Instant;

    const RATE: u32 = 48000;

    fn piece(seq: u64) -> hls::LivePiece {
        hls::LivePiece { seq, dur: 3.0, name: format!("chunk_17936_{seq}_b.aac") }
    }

    #[test]
    fn a_space_on_air_grows_by_the_pieces_its_live_list_names() {
        let base = "https://h/hls/K/non_transcode/pdxa/periscope-replays-public/audio-space/";
        let segments = (0..3).map(|i| Segment { start: i as f64 * 3.0, dur: 3.0, url: format!("{base}chunk_17936_{i}_b.aac") }).collect();
        let live = Live { state: Mutex::new(OnAir { segments, ended: false }), ..Live::default() };
        assert!(live.add(&[piece(1), piece(2)]) == News::None, "nothing new");
        assert!(live.add(&[piece(2), piece(3), piece(4)]) == News::Added);
        assert_eq!(live.duration(), 15.0);
        // Its own name, where the others are; not the live folder's
        assert_eq!(live.state.lock().unwrap().segments[4].url, format!("{base}chunk_17936_4_b.aac"));
        assert!(live.add(&[piece(7), piece(8)]) == News::Skipped, "5 and 6 only a replay has");
        assert_eq!(live.duration(), 15.0);
        assert_eq!(beside("https://h/live/chunk_1_9_b.aac?type=live", "chunk_1_10_b.aac?type=live"), "https://h/live/chunk_1_10_b.aac?type=live");
    }

    /// Needs a Space on air: LIVE_URL='https://…/dynamic_playlist.m3u8?type=live'.
    /// Opens it, plays 25 s from live (more than it had when opened), and checks it grew.
    #[test]
    #[ignore]
    fn follows_a_space_on_air() {
        let dynamic = std::env::var("LIVE_URL").expect("set LIVE_URL to a live Space's dynamic link");
        let replay = hls::live_to_replay(&dynamic).unwrap();
        let player = Player::new(ffmpeg::locate(None));
        let info = player.load(&replay, Some(&dynamic)).unwrap();
        assert!(info.live, "the Space is on air");
        let Some(source) = player.inner.lock().unwrap().source.clone() else { panic!("nothing loaded") };
        let Source::Live(live) = &*source else { panic!("not followed") };
        let opened = live.duration();
        println!("opened at {opened:.1} s");
        let shared = Arc::new(Shared::default());
        let worker = {
            let (shared, source) = (shared.clone(), source.clone());
            let ffmpeg = ffmpeg::locate(None).unwrap();
            thread::spawn(move || decode(&ffmpeg, &source, opened - LIVE_BACK, 1.0, RATE, 2, 0, &shared, &ChildSlot::default()))
        };
        let want = 25 * RATE as usize * 2;
        let started = Instant::now();
        let mut got = 0;
        let mut silent_run = 0usize;
        let mut worst = 0usize;
        while got < want {
            let chunk: Vec<f32> = shared.buf.lock().unwrap().drain(..).collect();
            shared.room.notify_all();
            got += chunk.len();
            assert!(started.elapsed() < Duration::from_secs(90), "the sound stopped coming at {:.1} s", got as f64 / 2.0 / RATE as f64);
            thread::sleep(Duration::from_millis(20));
            // Keep to real time, as a speaker would
            let due = started.elapsed().as_secs_f64() * RATE as f64 * 2.0;
            if (got as f64) > due + 4.0 * RATE as f64 * 2.0 {
                thread::sleep(Duration::from_millis(200));
            }
            silent_run = if chunk.is_empty() { silent_run + 1 } else { 0 };
            worst = worst.max(silent_run);
        }
        let grown = live.duration() - opened;
        println!("played 25 s in {:.1} s; the Space grew {grown:.1} s; longest wait {} ms", started.elapsed().as_secs_f64(), worst * 20);
        shared.gen.fetch_add(1, Relaxed);
        shared.room.notify_all();
        worker.join().unwrap().unwrap();
        assert!(grown > 15.0, "it kept following");
        player.shutdown();
    }

    fn space_url() -> String {
        std::env::var("SPACE_URL").expect("set SPACE_URL to a Space replay URL")
    }

    /// `secs` of what the player plays from `at`, mixed down to mono.
    fn play_from(url: &str, at: f64, secs: f64) -> Vec<f32> {
        let ffmpeg = ffmpeg::locate(None).expect("ffmpeg");
        let list = hls::load_playlist(url).unwrap();
        let source = Source::Hls(Arc::new(list.segments));
        let shared = Arc::new(Shared::default());
        let worker = {
            let shared = shared.clone();
            thread::spawn(move || decode(&ffmpeg, &source, at, 1.0, RATE, 2, 0, &shared, &ChildSlot::default()))
        };
        let want = (secs * RATE as f64) as usize * 2;
        let started = Instant::now();
        let mut out = Vec::new();
        while out.len() < want {
            out.extend(shared.buf.lock().unwrap().drain(..));
            shared.room.notify_all();
            assert!(!worker.is_finished() || !shared.buf.lock().unwrap().is_empty() || out.len() >= want, "decoding stopped early");
            assert!(started.elapsed() < Duration::from_secs(60), "too slow");
            thread::sleep(Duration::from_millis(10));
        }
        shared.gen.fetch_add(1, Relaxed);
        shared.room.notify_all();
        worker.join().unwrap().unwrap();
        out.truncate(want);
        out.chunks_exact(2).map(|c| (c[0] + c[1]) / 2.0).collect()
    }

    fn decode_file(path: &Path, secs: f64) -> Vec<f32> {
        let out = Command::new(ffmpeg::locate(None).unwrap())
            .args(["-v", "error", "-i", &path.to_string_lossy(), "-t", &secs.to_string()])
            .args(["-f", "f32le", "-ac", "1", "-ar", "48000", "pipe:1"])
            .output()
            .unwrap();
        out.stdout.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Loudness every 10 ms.
    fn envelope(x: &[f32]) -> Vec<f32> {
        x.chunks(480).map(rms).collect()
    }

    /// The shift of `b` against `a`, in 10 ms steps, where the two match best.
    fn best_lag(a: &[f32], b: &[f32], max: i64) -> (i64, f32) {
        let mut best = (0, f32::MIN);
        for lag in -max..=max {
            let pairs: Vec<(f32, f32)> = (0..a.len() as i64)
                .filter_map(|i| {
                    let j = i + lag;
                    (j >= 0 && (j as usize) < b.len()).then(|| (a[i as usize], b[j as usize]))
                })
                .collect();
            let n = pairs.len() as f32;
            let (ma, mb) = pairs.iter().fold((0.0, 0.0), |(x, y), (p, q)| (x + p / n, y + q / n));
            let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
            for (p, q) in &pairs {
                cov += (p - ma) * (q - mb);
                va += (p - ma) * (p - ma);
                vb += (q - mb) * (q - mb);
            }
            let r = cov / (va.sqrt() * vb.sqrt()).max(1e-12);
            if r > best.1 {
                best = (lag, r);
            }
        }
        best
    }

    /// Seconds of output the player makes from `at` to the end of a file, at `speed`.
    fn output_secs(path: &Path, at: f64, speed: f64) -> f64 {
        let ffmpeg = ffmpeg::locate(None).expect("ffmpeg");
        let source = Source::Direct(path.to_string_lossy().into_owned());
        let shared = Arc::new(Shared::default());
        let worker = {
            let shared = shared.clone();
            thread::spawn(move || decode(&ffmpeg, &source, at, speed, RATE, 2, 0, &shared, &ChildSlot::default()))
        };
        let mut samples = 0;
        loop {
            let done = worker.is_finished();
            samples += shared.buf.lock().unwrap().drain(..).count();
            shared.room.notify_all();
            if done {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        worker.join().unwrap().unwrap();
        samples as f64 / 2.0 / RATE as f64
    }

    #[test]
    fn plays_faster_and_slower() {
        // Ten seconds of a tone, written as a plain WAV file
        let path = std::env::temp_dir().join(format!("spacecutter_speed_{}.wav", std::process::id()));
        let n = 10 * RATE;
        let mut wav = Vec::new();
        for (tag, value) in [("RIFF", 36 + n * 2), ("WAVE", 0), ("fmt ", 16)] {
            wav.extend_from_slice(tag.as_bytes());
            if tag != "WAVE" {
                wav.extend_from_slice(&value.to_le_bytes());
            }
        }
        wav.extend_from_slice(&[1, 0, 1, 0]);
        wav.extend_from_slice(&RATE.to_le_bytes());
        wav.extend_from_slice(&(RATE * 2).to_le_bytes());
        wav.extend_from_slice(&[2, 0, 16, 0]);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(n * 2).to_le_bytes());
        for i in 0..n {
            let v = ((i as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 8000.0) as i16;
            wav.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();

        let cases = [(0.0, 1.0, 10.0), (0.0, 2.0, 5.0), (0.0, 0.5, 20.0), (4.0, 1.5, 4.0)];
        let got: Vec<f64> = cases.iter().map(|&(at, speed, _)| output_secs(&path, at, speed)).collect();
        let _ = std::fs::remove_file(&path);
        for (&(at, speed, want), got) in cases.iter().zip(&got) {
            assert!((got - want).abs() < 0.1, "from {at} s at {speed}×: {got:.2} s, expected {want} s");
        }
    }

    #[test]
    #[ignore]
    fn plays_past_the_timestamp_wrap() {
        let audio = play_from(&space_url(), 9300.0, 3.0);
        println!("rms at 02:35:00 = {:.4}", rms(&audio));
        assert!(rms(&audio) > 0.002, "the player gave silence");
    }

    #[test]
    #[ignore]
    fn player_and_cuts_share_a_timeline() {
        let url = space_url();
        let dir = std::env::temp_dir().join(format!("spacecutter_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let job = crate::cutter::Job {
            rows: vec![crate::cutter::Row {
                id: 1,
                url: url.clone(),
                full: false,
                start: "02:35:00".into(),
                end: "02:35:20".into(),
                name: "reference".into(),
                length: 0.0,
                live: String::new(),
            }],
            output_folder: dir.to_string_lossy().into_owned(),
            merge: false,
            merge_name: String::new(),
            keep_clips: true,
            jingles: Default::default(),
        };
        let events = Mutex::new(Vec::new());
        let emit = |name: &str, payload: serde_json::Value| events.lock().unwrap().push(format!("{name} {payload}"));
        crate::cutter::run_job(&emit, ffmpeg::locate(None), &crate::cutter::Control::default(), &job);
        println!("{:#?}", events.lock().unwrap());
        let clip = dir.join("reference.m4a");
        let length = ffmpeg::probe_duration(&ffmpeg::locate(None).unwrap(), &clip.to_string_lossy()).unwrap();
        println!("cut length = {length:.2} s");
        assert!((length - 20.0).abs() < 0.5, "the cut is {length} s long");

        let cut = envelope(&decode_file(&clip, 8.0));
        let played = envelope(&play_from(&url, 9300.0, 8.0));
        let (lag, r) = best_lag(&cut, &played, 100);
        println!("player vs cut: {} ms apart, match {r:.3}", lag * 10);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(r > 0.6, "the two do not sound alike (r = {r})");
        assert!(lag.abs() <= 10, "the player is {} ms off the cut", lag * 10);
    }
}
