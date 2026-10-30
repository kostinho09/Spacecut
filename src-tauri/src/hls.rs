use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent("Mozilla/5.0")
            .build()
    })
}

pub fn get_text(url: &str) -> Result<String, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
}

pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent().get(url).call().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    resp.into_reader().take(64 << 20).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
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

pub struct Playlist {
    pub segments: Vec<Segment>,
    pub duration: f64,
    /// When the Space was recorded, if the playlist says.
    pub started: Option<String>,
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
        return Ok(Playlist { segments, duration, started });
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
/// the offset of `s0` into its first segment. None if `url` isn't a plain
/// media playlist.
pub fn trim_playlist(url: &str, s0: f64, e0: f64) -> Option<(PathBuf, f64)> {
    if !is_m3u8(url) {
        return None;
    }
    let text = get_text(url).ok()?;
    if !text.contains("#EXTM3U") || text.contains("#EXT-X-STREAM-INF") {
        return None;
    }
    let segments = parse_media(&text, url);
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

/// The Replay URL of a Space, from the dynamic URL it had while live.
pub fn live_to_replay(dynamic: &str) -> Result<String, String> {
    let dynamic = dynamic.trim();
    let master = dynamic.replace("/dynamic_playlist.m3u8?type=live", "/master_playlist.m3u8");
    let text = agent()
        .get(&master)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let data = text
        .split("audio-space/")
        .nth(1)
        .ok_or("The server did not answer with a playlist")?
        .replace(['\n', '\r'], "");
    Ok(dynamic.replace("dynamic_playlist.m3u8", &data).replace("type=live", "type=replay"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
