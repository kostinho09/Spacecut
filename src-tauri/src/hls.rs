use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;

pub(crate) fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0")
            // Kept open for the next pieces, one for each fetched at a time
            .max_idle_connections_per_host(crate::fetch::PARALLEL + 2)
            .build()
    })
}

/// What was fetched, counted for tests that compare versions.
#[cfg(test)]
pub mod net {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    pub static LIVE: AtomicU64 = AtomicU64::new(0);
    pub static LISTS: AtomicU64 = AtomicU64::new(0);
    pub static LIST_PIECES: AtomicU64 = AtomicU64::new(0);
    pub static PIECES: AtomicU64 = AtomicU64::new(0);
    pub static PIECE_BYTES: AtomicU64 = AtomicU64::new(0);
    pub static OTHER: AtomicU64 = AtomicU64::new(0);
    pub fn add(c: &AtomicU64, n: u64) {
        c.fetch_add(n, Relaxed);
    }
    pub fn report() -> String {
        format!(
            "NET live={} lists={} list_pieces={} pieces={} piece_bytes={} other={}",
            LIVE.load(Relaxed), LISTS.load(Relaxed), LIST_PIECES.load(Relaxed), PIECES.load(Relaxed), PIECE_BYTES.load(Relaxed), OTHER.load(Relaxed)
        )
    }
}

pub fn get_text(url: &str) -> Result<String, String> {
    let text = agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    #[cfg(test)]
    if url.contains("dynamic_playlist") {
        net::add(&net::LIVE, 1);
    } else if url.contains("master_playlist") {
        net::add(&net::OTHER, 1);
    } else {
        net::add(&net::LISTS, 1);
        net::add(&net::LIST_PIECES, text.matches("#EXTINF").count() as u64);
    }
    Ok(text)
}

pub fn is_m3u8(url: &str) -> bool {
    url.split('?').next().unwrap_or("").contains(".m3u8")
}

fn join(base: &str, rel: &str) -> String {
    url::Url::parse(base)
        .and_then(|b| b.join(rel))
        .map(|u| u.to_string())
        .unwrap_or_else(|_| rel.to_string())
}

#[derive(Clone)]
pub struct Segment {
    pub start: f64,
    pub dur: f64,
    pub url: String,
}

#[derive(Clone)]
pub struct Playlist {
    pub segments: Vec<Segment>,
    pub duration: f64,
    /// When the Space was recorded, if the playlist says.
    pub started: Option<String>,
    /// The media playlist it was read from.
    pub url: String,
    /// Its pieces are taken from elsewhere than the list says (see `resolve`).
    pub moved: bool,
    /// The recording is over: the list won't grow any more.
    pub ended: bool,
    /// When the last piece ends, in seconds since 1970, if the list says:
    /// how far behind live its end is, without asking anything.
    pub ends_at: Option<f64>,
}

/// A #EXT-X-PROGRAM-DATE-TIME, in seconds since 1970.
fn clock(line: &str) -> Option<f64> {
    let t = chrono::DateTime::parse_from_rfc3339(line.trim()).ok()?;
    Some(t.timestamp_millis() as f64 / 1000.0)
}

/// When the last piece of a media playlist ends: the last date it gives,
/// plus the pieces after it.
fn ends_at(text: &str) -> Option<f64> {
    let mut at = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            at = clock(rest);
        } else if let (Some(rest), Some(t)) = (line.strip_prefix("#EXTINF:"), at.as_mut()) {
            *t += rest.split(',').next().and_then(|d| d.trim().parse::<f64>().ok()).unwrap_or(0.0);
        }
    }
    at
}

/// Segments of a media playlist, each placed on the timeline the cut
/// timestamps use: the running sum of #EXTINF durations.
pub fn parse_media(text: &str, base: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut t = 0.0;
    let mut dur: Option<f64> = None;
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            dur = rest.split(',').next().and_then(|d| d.trim().parse().ok());
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(d) = dur.take() {
                segments.push(Segment { start: t, dur: d, url: join(base, line) });
                t += d;
            }
        }
    }
    segments
}

/// A Space's playlist, following a master playlist to its first stream.
pub fn load_playlist(url: &str) -> Result<Playlist, String> {
    let mut url = url.trim().to_string();
    for _ in 0..3 {
        let text = get_text(&url).map_err(|e| format!("Could not load the playlist ({e})"))?;
        if !text.contains("#EXTM3U") {
            return Err("This link is not a Space replay playlist".into());
        }
        if text.contains("#EXT-X-STREAM-INF") {
            let variant = text
                .lines()
                .map(str::trim)
                .skip_while(|l| !l.starts_with("#EXT-X-STREAM-INF"))
                .skip(1)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .ok_or("The playlist lists no streams")?;
            url = join(&url, variant);
            continue;
        }
        let segments = parse_media(&text, &url);
        let Some(last) = segments.last() else {
            return Err("The playlist has no audio in it".into());
        };
        let duration = last.start + last.dur;
        let started = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("#EXT-X-PROGRAM-DATE-TIME:"))
            .map(|s| s.trim().to_string());
        let ended = text.contains("#EXT-X-ENDLIST");
        return Ok(Playlist { segments, duration, started, url, moved: false, ended, ends_at: ends_at(&text) });
    }
    Err("The playlist points to too many other playlists".into())
}

fn temp_path(ext: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("spacecutter_{}_{n}.{ext}", std::process::id()))
}

/// Spaces whose timestamps start near the MPEG-TS limit wrap around
/// mid-stream, so seeking past that point yields an empty file. This writes a
/// local playlist with only the segments covering `s0..e0` and returns it with
/// the offset of `s0` into its first segment. None if no segment covers it.
pub fn trim_playlist(segments: &[Segment], s0: f64, e0: f64) -> Option<(PathBuf, f64)> {
    let picked: Vec<&Segment> = segments.iter().filter(|s| s.start + s.dur > s0 && s.start < e0).collect();
    let first = picked.first()?;
    let longest = picked.iter().map(|s| s.dur).fold(0.0, f64::max);
    let target = ((longest + 1.0) as u64).max(1);
    let mut body = format!(
        "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-VERSION:3\n#EXT-X-MEDIA-SEQUENCE:0\n"
    );
    for s in &picked {
        body.push_str(&format!("#EXTINF:{},\n{}\n", s.dur, s.url));
    }
    body.push_str("#EXT-X-ENDLIST\n");
    let path = temp_path("m3u8");
    std::fs::write(&path, body).ok()?;
    Some((path, (s0 - first.start).max(0.0)))
}

/// X's other servers for the same files. The one in a link sometimes refuses
/// a Space the others still give out (prod-fastly-eu-central-1 did).
const OTHER_HOSTS: [&str; 2] = ["prod-fastly-a.video.pscp.tv", "prod-fastly-p.video.pscp.tv"];

/// A server asked last even for its own links: for its Spaces of 27 Oct to
/// 1 Nov 2026 it refused the master and the replay (400) and took 5–10 s to
/// fail on pdxa (503), while the other servers gave out both.
const LAST_HOST: &str = "prod-fastly-eu-central-1.video.pscp.tv";

/// A link, then the same on X's other servers; one on `LAST_HOST` goes last.
fn on_hosts(url: &str) -> Vec<String> {
    let mut out = vec![url.to_string()];
    if let Ok(parsed) = url::Url::parse(url) {
        if parsed.host_str().is_some_and(|h| h.ends_with(".video.pscp.tv")) {
            for host in OTHER_HOSTS {
                let mut other = parsed.clone();
                if other.set_host(Some(host)).is_ok() && !out.contains(&other.to_string()) {
                    out.push(other.to_string());
                }
            }
            if parsed.host_str() == Some(LAST_HOST) {
                out.rotate_left(1);
            }
        }
    }
    out
}

/// The two forms of a Space's replay link, from its dynamic link and the
/// playlist name its master gives (`playlist_<number>`), in the order tried:
/// first where X keeps replays now (pdxa), then beside the dynamic playlist.
fn replay_forms(dynamic: &str, name: &str) -> (String, String) {
    let pdxa = match (dynamic.find("/Transcoding/"), dynamic.find("/non_transcode/")) {
        (Some(t), Some(n)) => format!("{}/a{}/{REPLAYS}/audio-space/{name}.m3u8?type=replay", &dynamic[..t], &dynamic[t..n]),
        _ => String::new(),
    };
    let beside = dynamic.replace("dynamic_playlist.m3u8", &format!("{name}.m3u8")).replace("type=live", "type=replay");
    (pdxa, beside)
}

/// The Replay URL of a Space, from the dynamic URL it had while live: with the
/// playlist name from the master, first the pdxa form, then the one beside the
/// dynamic playlist, whichever opens (list and audio) first. Tried on the
/// link's server, then on X's other servers; if nothing opens, the pdxa form.
pub fn live_to_replay(dynamic: &str) -> Result<String, String> {
    let dynamic = dynamic.trim();
    if live_base(dynamic).is_none() || !dynamic.contains("/non_transcode/") {
        return Err("this is not a Space's dynamic link".into());
    }
    // Only the list is asked for here; whether its audio plays is the
    // player's to find out, which falls back to the live pieces if not
    let opens_there = |url: &String| !url.is_empty() && load_playlist(url).is_ok_and(|list| !list.segments.is_empty());
    let mut first = None;
    for dynamic in on_hosts(dynamic) {
        let master = dynamic.replace("/dynamic_playlist.m3u8?type=live", "/master_playlist.m3u8");
        #[cfg(test)]
        net::add(&net::OTHER, 1);
        let name = agent()
            .get(&master)
            .timeout(Duration::from_secs(10))
            .call()
            .ok()
            .and_then(|r| r.into_string().ok())
            .and_then(|text| Regex::new(r"playlist_\d+").ok()?.find(&text).map(|m| m.as_str().to_string()));
        let (pdxa, beside) = replay_forms(&dynamic, name.as_deref().unwrap_or("playlist_0"));
        first.get_or_insert(pdxa.clone());
        if opens_there(&pdxa) {
            return Ok(pdxa);
        }
        if name.is_some() && opens_there(&beside) {
            return Ok(beside);
        }
    }
    first.filter(|f| !f.is_empty()).ok_or_else(|| "this is not a Space's dynamic link".into())
}

/// The folder where the live stream of a Space kept its pieces: the
/// dynamic link's own.
pub fn live_base(dynamic: &str) -> Option<String> {
    let dynamic = dynamic.trim();
    dynamic.find("dynamic_playlist.m3u8").map(|i| dynamic[..i].to_string())
}

/// The dynamic link a Space's other links (replay, master) stand beside.
fn dynamic_of(url: &str) -> Option<String> {
    let url = url.trim();
    if !url.contains("/non_transcode/") || !url.contains("/audio-space/") || !is_m3u8(url) {
        return None;
    }
    let folder = &url[..url.split('?').next()?.rfind('/')? + 1];
    Some(format!("{folder}dynamic_playlist.m3u8?type=live"))
}

/// The replay as Spaces used to have it: named in the master playlist,
/// beside the dynamic one. Asks the server.
fn replay_beside_live(dynamic: &str) -> Option<String> {
    on_hosts(dynamic).into_iter().find_map(|dynamic| {
        let master = dynamic.replace("/dynamic_playlist.m3u8?type=live", "/master_playlist.m3u8");
        #[cfg(test)]
        net::add(&net::OTHER, 1);
        let text = agent().get(&master).timeout(Duration::from_secs(10)).call().ok()?.into_string().ok()?;
        let name = text.split("audio-space/").nth(1)?.lines().next()?.trim().to_string();
        Some(dynamic.replace("dynamic_playlist.m3u8", &name).replace("type=live", "type=replay"))
    })
}

/// Whether the server gives out a piece.
fn opens(segment: &Segment) -> bool {
    #[cfg(test)]
    net::add(&net::OTHER, 1);
    agent().get(&segment.url).timeout(Duration::from_secs(10)).call().is_ok()
}

/// The same pieces, taken from where the live stream kept them.
fn at_live(mut list: Playlist, base: &str) -> Playlist {
    for seg in &mut list.segments {
        let name = seg.url.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("");
        seg.url = format!("{base}{name}?type=live");
    }
    list.moved = true;
    list
}

/// Where the live stream of a Space kept its pieces before 31 Oct 2026.
const OLD_LIVE: &str = "periscope-replay-direct";

/// Where X keeps Space replays now, under each Space's own key; any playlist
/// number opens the whole list there.
const REPLAYS: &str = "non_transcode/pdxa/periscope-replays-public";

/// The replay where X keeps them, under a number not asked for before, so a
/// cache along the way can't hand back an older list of a Space still live.
fn archive_of(dynamic: &str) -> Option<String> {
    let key_end = dynamic.find("/non_transcode/")?;
    let number = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    Some(format!("{}/{REPLAYS}/audio-space/playlist_{number}.m3u8?type=replay", &dynamic[..key_end]))
}

/// A Space's playlist, ready to play and cut. Knowing the Space (from its
/// dynamic link `live`, or from any link of it), each on the link's server
/// and then on X's other servers:
/// 1. where X keeps replays now (`REPLAYS`, pdxa),
/// 2. the pieces where the live stream kept them (atla, say; not for
///    Spaces in the old place, see `search`),
/// and only if both fail, the link as given, then the replay as Spaces used
/// to have it, beside the dynamic playlist.
pub fn resolve(url: &str, live: Option<&str>) -> Result<Playlist, String> {
    if let Some(list) = recall(url, live) {
        return Ok(list);
    }
    let list = find(url, live)?;
    remember(url, live, &list);
    Ok(list)
}

/// Where a Space's audio was found, so the player (on every load) and each
/// clip of a cut don't look again. Kept for good, a Space on air too: its
/// list is asked for again only when something needs what came out since
/// (see `resolve_fresh`).
fn found() -> &'static Mutex<HashMap<String, Playlist>> {
    static FOUND: OnceLock<Mutex<HashMap<String, Playlist>>> = OnceLock::new();
    FOUND.get_or_init(Default::default)
}

fn key(url: &str, live: Option<&str>) -> String {
    format!("{}|{}", url.trim(), live.unwrap_or("").trim())
}

fn remember(url: &str, live: Option<&str>, list: &Playlist) {
    found().lock().unwrap().insert(key(url, live), list.clone());
}

fn recall(url: &str, live: Option<&str>) -> Option<Playlist> {
    found().lock().unwrap().get(&key(url, live)).cloned()
}

/// The replay of a Space known to be on air, asked for at once where X keeps
/// replays: one request, without `resolve`'s checks (whether it is on air,
/// whether its pieces open), which are known by then.
pub fn replay_now(dynamic: &str) -> Result<Playlist, String> {
    let mut error = None;
    for candidate in archive_of(dynamic).map(|a| on_hosts(&a)).unwrap_or_default() {
        match load_playlist(&candidate) {
            Ok(mut list) if !list.segments.is_empty() => {
                list.ended = false;
                return Ok(list);
            }
            Ok(_) => {}
            Err(e) => {
                error.get_or_insert(e);
            }
        }
    }
    Err(error.unwrap_or_else(|| "The server gave no replay".into()))
}

/// The playlist as known; for a Space that was on air when it was asked for
/// and has ended since (a look at its small live list), as it ended.
pub fn resolve_known(url: &str, live: Option<&str>) -> Result<Playlist, String> {
    let list = resolve(url, live)?;
    if list.ended || !dynamic_for(url, live).is_some_and(|d| matches!(check_live(&d), LiveCheck::Over)) {
        return Ok(list);
    }
    resolve_fresh(url, live)
}

/// The whole of a Space as it is now: for one on air, all that is out by
/// this moment (a fresh replay), not only what was known.
pub fn resolve_whole(url: &str, live: Option<&str>) -> Result<Playlist, String> {
    let list = resolve(url, live)?;
    if list.ended {
        return Ok(list);
    }
    resolve_fresh(url, live)
}

/// What a Space on air grew to in the player: kept for when it is opened
/// again, so its bar doesn't fall back to what it was first.
pub fn keep_grown(url: &str, live: Option<&str>, segments: &[Segment], ends_at: Option<f64>, ended: bool) {
    let mut found = found().lock().unwrap();
    let Some(list) = found.get_mut(&key(url, live)) else { return };
    if segments.len() >= list.segments.len() {
        list.segments = segments.to_vec();
        list.duration = segments.last().map_or(0.0, |s| s.start + s.dur);
        list.ends_at = ends_at.or(list.ends_at);
        list.ended |= ended;
    }
}

/// How many times a list was asked for again (tests count them).
#[cfg(test)]
pub static FRESH: AtomicU64 = AtomicU64::new(0);

/// Pieces a Space on air took on in the player from its live list, right
/// after those known: kept too, so a clip marked in them needs no replay.
pub fn keep_added(url: &str, live: Option<&str>, added: &[Segment], ends_at: Option<f64>) {
    let mut found = found().lock().unwrap();
    let Some(list) = found.get_mut(&key(url, live)) else { return };
    for seg in added {
        let last = list.segments.last().and_then(|s| piece_number(&s.url));
        if last.zip(piece_number(&seg.url)).is_some_and(|(a, b)| b == a + 1) {
            list.segments.push(Segment { start: list.duration, dur: seg.dur, url: seg.url.clone() });
            list.duration += seg.dur;
        }
    }
    list.ends_at = ends_at.or(list.ends_at);
}

/// A Space's playlist as it is now, asked for again even if known: for a
/// Space on air, when its newest part is wanted.
pub fn resolve_fresh(url: &str, live: Option<&str>) -> Result<Playlist, String> {
    #[cfg(test)]
    FRESH.fetch_add(1, Ordering::Relaxed);
    let list = find(url, live)?;
    remember(url, live, &list);
    Ok(list)
}

fn find(url: &str, live: Option<&str>) -> Result<Playlist, String> {
    // Any link of a Space (a replay, say) tells its key and folder
    let Some(dynamic) = dynamic_for(url, live) else {
        return load_playlist(url);
    };
    // The replay of a Space still live says it has ended all the same
    let on_air = matches!(check_live(&dynamic), LiveCheck::On(_));
    let mut list = search(url, &dynamic, on_air)?;
    if on_air {
        list.ended = false;
    }
    Ok(list)
}

/// The dynamic link of a Space: the one given, or the one beside its other links.
pub fn dynamic_for(url: &str, live: Option<&str>) -> Option<String> {
    live.filter(|d| live_base(d).is_some()).map(str::to_string).or_else(|| dynamic_of(url))
}

/// A piece the live list names: its number, length and file name.
pub struct LivePiece {
    pub seq: u64,
    pub dur: f64,
    pub name: String,
    /// When it starts, in seconds since 1970.
    pub at: Option<f64>,
}

pub enum LiveCheck {
    /// On air, with the latest pieces.
    On(Vec<LivePiece>),
    Over,
    /// No answer, or a link that can't tell.
    Unknown,
}

/// The number in a piece's name: `chunk_<time>_<number>_a.aac`.
pub fn piece_number(url: &str) -> Option<u64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"_(\d+)_[a-z]\.aac").unwrap());
    re.captures(url.split('?').next()?.rsplit('/').next()?)?.get(1)?.as_str().parse().ok()
}

/// Whether a Space is on air, from its live list, which holds only its last
/// few pieces (a replay asked for meanwhile ends the same way a finished one does).
pub fn check_live(dynamic: &str) -> LiveCheck {
    // Spaces in the old place (until 30 Oct 2026) are long over, and their
    // server (eu-central) can take seconds to answer at all
    if dynamic.contains(REPLAYS) || dynamic.contains(OLD_LIVE) || live_base(dynamic).is_none() {
        return LiveCheck::Unknown;
    }
    #[cfg(test)]
    net::add(&net::LIVE, 1);
    let text = match agent().get(dynamic).timeout(Duration::from_secs(5)).call() {
        Ok(r) => r.into_string().unwrap_or_default(),
        Err(ureq::Error::Status(404 | 410, _)) => return LiveCheck::Over,
        Err(_) => return LiveCheck::Unknown,
    };
    if !text.contains("#EXTM3U") {
        return LiveCheck::Unknown;
    }
    if text.contains("#EXT-X-ENDLIST") {
        return LiveCheck::Over;
    }
    let mut pieces = Vec::new();
    let (mut dur, mut at) = (None, None);
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("#EXT-X-PROGRAM-DATE-TIME:") {
            at = clock(rest);
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            dur = rest.split(',').next().and_then(|d| d.trim().parse().ok());
        } else if !line.is_empty() && !line.starts_with('#') {
            if let (Some(d), Some(seq)) = (dur.take(), piece_number(line)) {
                pieces.push(LivePiece { seq, dur: d, name: line.split('?').next().unwrap_or(line).to_string(), at: at.take() });
            }
        }
    }
    LiveCheck::On(pieces)
}

fn search(url: &str, dynamic: &str, on_air: bool) -> Result<Playlist, String> {
    let plays = |list: &Playlist| list.segments.first().is_some_and(opens);
    let mut error = None;
    // A list of the pieces, wherever their audio turns out to be
    let mut names = None;
    // A pdxa link as given is tried as it is, before asking again under a new
    // number; not while live, when it holds only the Space up to when it was made
    let as_given = (url.contains(REPLAYS) && !on_air).then(|| url.to_string());
    for candidate in as_given.into_iter().chain(archive_of(dynamic).map(|a| on_hosts(&a)).unwrap_or_default()) {
        match load_playlist(&candidate) {
            Ok(mut list) if plays(&list) => {
                list.moved = candidate != url;
                return Ok(list);
            }
            Ok(list) => {
                names.get_or_insert(list);
            }
            Err(e) => {
                error.get_or_insert(e);
            }
        }
    }
    let given = on_hosts(url).into_iter().find_map(|candidate| match load_playlist(&candidate) {
        Ok(list) => Some((candidate, list)),
        Err(e) => {
            error.get_or_insert(e);
            None
        }
    });
    // Spaces in the old place (until 30 Oct 2026) kept their live pieces in
    // the replay's own folder: asked for as live there, a piece opens only
    // while a cache holds it (one played or cut a moment ago), the others
    // answer 404, and that 404 is then handed out for the replay too
    let live_apart = !dynamic.contains(OLD_LIVE);
    if let Some(list) = names.as_ref().or(given.as_ref().map(|(_, list)| list)).filter(|_| live_apart) {
        for base in on_hosts(dynamic).iter().filter_map(|d| live_base(d)) {
            // The first live pieces may be gone already; one in the middle tells
            let moved = at_live(list.clone(), &base);
            if moved.segments.get(moved.segments.len() / 2).is_some_and(opens) {
                return Ok(moved);
            }
        }
    }
    if let Some((candidate, mut list)) = given {
        if plays(&list) {
            list.moved = candidate != url;
            return Ok(list);
        }
    }
    let error = error.unwrap_or_else(|| "The server no longer gives out this Space's audio".into());
    let old = replay_beside_live(dynamic).ok_or(error.clone())?;
    let mut list = load_playlist(&old).map_err(|_| error)?;
    list.moved = list.url != url;
    Ok(list)
}

/// The raw AAC of a Space segment: each one opens with ID3 tags (stream
/// timestamps, metadata) that would otherwise land mid-stream when segments
/// are joined.
pub fn strip_id3(mut b: &[u8]) -> &[u8] {
    while b.len() >= 10 && &b[..3] == b"ID3" {
        let size = (b[6] as usize & 0x7f) << 21
            | (b[7] as usize & 0x7f) << 14
            | (b[8] as usize & 0x7f) << 7
            | (b[9] as usize & 0x7f);
        let footer = if b[5] & 0x10 != 0 { 10 } else { 0 };
        let total = 10 + size + footer;
        if total > b.len() {
            return &[];
        }
        b = &b[total..];
    }
    b
}

/// The ADTS frames of a piece of AAC that start within its first `secs`, so
/// a clip's last piece stops where the clip does. The whole piece if it
/// doesn't read as ADTS.
pub fn adts_until(b: &[u8], secs: f64) -> &[u8] {
    const RATES: [f64; 13] = [96000.0, 88200.0, 64000.0, 48000.0, 44100.0, 32000.0, 24000.0, 22050.0, 16000.0, 12000.0, 11025.0, 8000.0, 7350.0];
    let (mut pos, mut t) = (0, 0.0);
    while t < secs && pos < b.len() {
        let h = &b[pos..];
        // Newer Spaces carry ID3 tags between their frames too
        if h.len() >= 10 && &h[..3] == b"ID3" {
            let size = (h[6] as usize & 0x7f) << 21 | (h[7] as usize & 0x7f) << 14 | (h[8] as usize & 0x7f) << 7 | (h[9] as usize & 0x7f);
            pos += 10 + size + if h[5] & 0x10 != 0 { 10 } else { 0 };
            continue;
        }
        if h.len() < 7 || h[0] != 0xff || h[1] & 0xf0 != 0xf0 {
            return b;
        }
        let Some(rate) = RATES.get(((h[2] >> 2) & 0xf) as usize) else { return b };
        let len = (h[3] as usize & 0x3) << 11 | (h[4] as usize) << 3 | (h[5] as usize) >> 5;
        if len < 7 || len > h.len() {
            return b;
        }
        pos += len;
        t += ((h[6] & 0x3) as f64 + 1.0) * 1024.0 / rate;
    }
    &b[..pos.min(b.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` frames of AAC as a Space has them: ADTS, 48 kHz, stereo, with an
    /// ID3 tag between some of them, as newer Spaces have.
    fn adts(n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..n {
            if i % 3 == 1 {
                out.extend(b"ID3\x04\x00\x00\x00\x00\x00\x1a");
                out.extend([0x54; 0x1a]);
            }
            let len = 7 + 20 + i;
            out.extend([0xff, 0xf1, 0x4c, 0x80 | (len >> 11) as u8, (len >> 3) as u8, ((len & 7) << 5) as u8 | 0x1f, 0xfc]);
            out.extend(std::iter::repeat_n(i as u8, len - 7));
        }
        out
    }

    #[test]
    fn keeps_the_frames_that_start_in_time() {
        let piece = adts(10);
        let frame = 1024.0 / 48000.0;
        let frames = |b: &[u8]| (0..10).filter(|&i| b.ends_with(&[i as u8; 20])).max().map_or(0, |i| i + 1);
        assert_eq!(frames(adts_until(&piece, 2.5 * frame)), 3, "the frame under the end is kept");
        assert_eq!(frames(adts_until(&piece, 0.5 * frame)), 1);
        assert_eq!(frames(adts_until(&piece, 7.5 * frame)), 8);
        assert_eq!(adts_until(&piece, 1.0), &piece[..], "all of it");
        assert_eq!(adts_until(&piece, 0.0), &[] as &[u8]);
        assert_eq!(adts_until(b"not adts at all", 0.01), b"not adts at all");
    }

    /// How long each step takes on a real Space:
    /// DYNAMIC_URL='https://…/dynamic_playlist.m3u8?type=live' cargo test -- --ignored --nocapture times_
    #[test]
    #[ignore]
    fn times_the_steps() {
        let dynamic = std::env::var("DYNAMIC_URL").expect("set DYNAMIC_URL");
        let t = std::time::Instant::now();
        let replay = live_to_replay(&dynamic).unwrap();
        println!("convert: {:.2} s, to {replay}", t.elapsed().as_secs_f64());
        let t = std::time::Instant::now();
        let list = resolve(&replay, Some(&dynamic)).unwrap();
        println!("player load right after: {:.3} s, {} pieces", t.elapsed().as_secs_f64(), list.segments.len());
        let t = std::time::Instant::now();
        resolve(&replay, Some(&dynamic)).unwrap();
        println!("each later load or clip: {:.3} s", t.elapsed().as_secs_f64());
        let t = std::time::Instant::now();
        let fresh = find(&replay, Some(&dynamic)).unwrap();
        println!("looking it up afresh (pdxa link used as given): {:.2} s, from {}", t.elapsed().as_secs_f64(), if fresh.url == replay { "the link itself" } else { "elsewhere" });
        let t = std::time::Instant::now();
        let plain = load_playlist(&replay).unwrap();
        println!("just the list: {:.2} s, {} pieces", t.elapsed().as_secs_f64(), plain.segments.len());
        let t = std::time::Instant::now();
        for seg in list.segments.iter().take(5) {
            assert!(opens(seg));
        }
        println!("5 pieces one by one: {:.2} s", t.elapsed().as_secs_f64());
    }

    /// The user's own example: these two links, in this order.
    /// Needs a Space that was on air and is over now: OVER_URL='https://…/dynamic_playlist.m3u8?type=live'.
    #[test]
    #[ignore]
    fn knows_a_space_is_over() {
        let dynamic = std::env::var("OVER_URL").expect("set OVER_URL to the dynamic link of a Space that has ended");
        assert!(matches!(check_live(&dynamic), LiveCheck::Over));
        let list = resolve(&live_to_replay(&dynamic).unwrap(), Some(&dynamic)).unwrap();
        println!("{} pieces, {:.0} s, ended: {}", list.segments.len(), list.duration, list.ended);
        assert!(list.ended);
    }

    #[test]
    fn knows_when_the_last_piece_ends() {
        let text = "#EXTM3U\n#EXT-X-PROGRAM-DATE-TIME:2026-11-03T05:36:21.572Z\n#EXTINF:3.0,\nchunk_1_0_b.aac\n#EXT-X-PROGRAM-DATE-TIME:2026-11-03T05:36:24.500Z\n#EXTINF:2.5,\nchunk_1_1_b.aac\n";
        let end = ends_at(text).unwrap();
        assert!((end - (clock("2026-11-03T05:36:24.500Z").unwrap() + 2.5)).abs() < 1e-6);
        assert_eq!(ends_at("#EXTM3U\n#EXTINF:3.0,\nchunk_1_0_b.aac\n"), None);
    }

    #[test]
    fn reads_the_number_of_a_piece() {
        assert_eq!(piece_number("https://h/a/chunk_1793688216796384610_2571_b.aac?type=live"), Some(2571));
        assert_eq!(piece_number("chunk_1793680508649668229_0_b.aac"), Some(0));
        assert_eq!(piece_number("https://h/a/playlist_1665.m3u8"), None);
    }

    #[test]
    fn makes_both_replay_forms_exactly() {
        let dynamic = "https://prod-fastly-a.video.pscp.tv/Transcoding/v1/hls/EXAMPLE_SPACE_KEY/non_transcode/atla/x-media-live-stream-atla/audio-space/dynamic_playlist.m3u8?type=live";
        let (pdxa, beside) = replay_forms(dynamic, "playlist_16653265225496066892");
        assert_eq!(pdxa, "https://prod-fastly-a.video.pscp.tv/a/Transcoding/v1/hls/EXAMPLE_SPACE_KEY/non_transcode/pdxa/periscope-replays-public/audio-space/playlist_16653265225496066892.m3u8?type=replay");
        assert_eq!(beside, "https://prod-fastly-a.video.pscp.tv/Transcoding/v1/hls/EXAMPLE_SPACE_KEY/non_transcode/atla/x-media-live-stream-atla/audio-space/playlist_16653265225496066892.m3u8?type=replay");
    }

    #[test]
    fn knows_where_a_spaces_replay_and_pieces_are() {
        let base = "https://prod-fastly-a.video.pscp.tv/Transcoding/v1/hls/KEY/non_transcode/atla/x-media-live-stream-atla/audio-space";
        let dynamic = format!("{base}/dynamic_playlist.m3u8?type=live");
        assert_eq!(live_base(&dynamic).unwrap(), format!("{base}/"));
        assert_eq!(dynamic_of(&format!("{base}/playlist_1665.m3u8?type=replay")).unwrap(), dynamic);
        assert_eq!(dynamic_of("https://example.com/a.m3u8"), None);
        let hosts = on_hosts(&dynamic);
        assert_eq!(hosts.len(), 2, "prod-fastly-a is already the link's own: {hosts:?}");
        assert!(hosts[1].starts_with("https://prod-fastly-p.video.pscp.tv/Transcoding/"));
        assert_eq!(on_hosts("https://example.com/a.m3u8").len(), 1);
        let archive = archive_of(&dynamic).unwrap();
        assert!(archive.starts_with("https://prod-fastly-a.video.pscp.tv/Transcoding/v1/hls/KEY/non_transcode/pdxa/periscope-replays-public/audio-space/playlist_"));
        assert!(archive.ends_with(".m3u8?type=replay") && archive != archive_of(&dynamic).unwrap());
    }

    /// prod-fastly-eu-central-1 refused its own Spaces (VthB, oZn48, y5nElS)
    /// and was slow to: its links are tried on the other servers first.
    #[test]
    fn asks_eu_central_last() {
        let dynamic = "https://prod-fastly-eu-central-1.video.pscp.tv/Transcoding/v1/hls/KEY/non_transcode/eu-central-1/periscope-replay-direct-prod-eu-central-1-public/audio-space/dynamic_playlist.m3u8?type=live";
        let hosts = on_hosts(dynamic);
        let names: Vec<&str> = hosts.iter().map(|u| u.split('/').nth(2).unwrap()).collect();
        assert_eq!(names, ["prod-fastly-a.video.pscp.tv", "prod-fastly-p.video.pscp.tv", "prod-fastly-eu-central-1.video.pscp.tv"]);
        assert!(hosts.iter().all(|u| u.ends_with("/eu-central-1/periscope-replay-direct-prod-eu-central-1-public/audio-space/dynamic_playlist.m3u8?type=live")));
    }

    #[test]
    fn takes_pieces_from_the_live_folder() {
        let list = Playlist {
            segments: vec![Segment { start: 0.0, dur: 3.0, url: "https://h/a/x/transcode/pdxa/r/JWT/audio-space/chunk_1_0_b.aac".into() }],
            duration: 3.0,
            started: None,
            url: String::new(),
            moved: false,
            ended: true,
            ends_at: None,
        };
        let moved = at_live(list, "https://h/x/non_transcode/atla/live/audio-space/");
        assert_eq!(moved.segments[0].url, "https://h/x/non_transcode/atla/live/audio-space/chunk_1_0_b.aac?type=live");
        assert!(moved.moved);
    }

    /// Needs the network and a Space that is live or recently ended:
    /// DYNAMIC_URL='https://…/dynamic_playlist.m3u8?type=live' cargo test -- --ignored resolves
    #[test]
    #[ignore]
    fn resolves_the_replay_and_falls_back_to_the_live_pieces() {
        let dynamic = std::env::var("DYNAMIC_URL").expect("set DYNAMIC_URL");
        let replay = live_to_replay(&dynamic).unwrap();
        let list = resolve(&replay, Some(&dynamic)).unwrap();
        println!("replay: {} pieces, {:.0} s, from {}", list.segments.len(), list.duration, list.segments[0].url);
        assert!(list.segments.len() > 10 && list.segments[0].url.contains("/pdxa/periscope-replays-public/"), "pdxa comes first");
        // A replay link alone, even one where no replay is, finds the same
        let beside = dynamic.replace("dynamic_playlist.m3u8", "playlist_16653273897914402342.m3u8").replace("type=live", "type=replay");
        let list = resolve(&beside, None).unwrap();
        println!("old-style replay link alone: {} pieces from {}", list.segments.len(), list.segments[0].url);
        assert!(list.segments[0].url.contains("/pdxa/periscope-replays-public/"));
        // The master's own path lists the pieces but refuses them: the live ones stand in
        let master = dynamic.replace("/dynamic_playlist.m3u8?type=live", "/master_playlist.m3u8");
        let text = get_text(&master).unwrap();
        let refused = join(&master, text.lines().find(|l| l.starts_with('/')).unwrap().trim());
        let list = resolve(&refused, Some(&dynamic)).unwrap();
        println!("refused replay -> {} pieces from {}", list.segments.len(), list.segments[list.segments.len() / 2].url);
        assert!(list.moved && list.segments[0].url.contains("/pdxa/periscope-replays-public/"), "pdxa comes first");
        // With pdxa out of reach too (a key it doesn't know), the live pieces stand in
        let pieces = at_live(load_playlist(&refused).unwrap(), &live_base(&dynamic).unwrap());
        assert!(pieces.segments.get(pieces.segments.len() / 2).is_some_and(opens), "a live piece opens");

    }

    #[test]
    fn strips_the_id3_tags_of_a_space_segment() {
        // As in a real segment: a PRIV tag (63 bytes), a TXXX tag (153 bytes), then ADTS audio
        let mut seg = b"ID3\x04\x00\x00\x00\x00\x00\x3f".to_vec();
        seg.extend([0u8; 63]);
        seg.extend(b"ID3\x04\x00\x00\x00\x00\x01\x19");
        seg.extend([0u8; 153]);
        seg.extend([0xff, 0xf1, 0x50]);
        assert_eq!(strip_id3(&seg), &[0xff, 0xf1, 0x50]);
    }

    #[test]
    fn places_segments_on_the_extinf_timeline() {
        let text = "#EXTM3U\n#EXT-X-PROGRAM-DATE-TIME:2026-10-26T23:50:02.606Z\n#EXTINF:3.029,\nchunk_0_a.aac\n\
                    #EXT-X-PROGRAM-DATE-TIME:2026-10-26T23:50:06.624Z\n#EXTINF:3.051,\nchunk_1_a.aac\n#EXT-X-ENDLIST\n";
        let segs = parse_media(text, "https://cdn.example/audio-space/playlist_1.m3u8?type=replay");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].url, "https://cdn.example/audio-space/chunk_1_a.aac");
        assert!((segs[1].start - 3.029).abs() < 1e-9);
    }
}
