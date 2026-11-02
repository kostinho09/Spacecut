//! A Space's pieces, fetched several at a time and handed on in order.
//!
//! Reading a Space's list itself, ffmpeg asks the server for one ~3 s piece,
//! waits for it, then asks for the next, so a long cut spends most of its
//! time waiting on the server. Here up to `PARALLEL` pieces are asked for at
//! once and passed on strictly in the list's order, each without its ID3
//! header; one that arrives before its turn waits in memory.
//!
//! Now and then, mostly in a live Space, the server takes 20–30 s over one
//! piece, whichever connection asks; meanwhile the other requests go on with
//! the pieces after it, as far ahead as the caller allows. A dropped
//! connection is tried again, and a server that says it is busy gets fewer
//! requests at a time; the pieces carry on from where they were. A piece the
//! server hasn't got is left out, as ffmpeg leaves out one it can't open.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::hls;

/// Pieces asked for at once, about as many as a browser opens to one server.
pub const PARALLEL: usize = 6;
/// The fewest asked for at once after the server pushes back.
const FEWEST: usize = 2;
/// How far past the piece being handed on a cut fetches (about 40 KB a
/// piece, so some 20 MB): enough to keep going through a slow piece.
pub const AHEAD_CUT: usize = 512;
/// How far ahead the player fetches, about 50 s of sound.
pub const AHEAD_PLAY: usize = 16;
/// The waits between tries of a piece, in ms: about half a minute in all for
/// a busy server or a dropped connection, the first two for anything else.
const WAITS: [u64; 6] = [500, 1000, 2000, 4000, 8000, 15000];
/// Missing pieces left out before the list is taken to be wrong.
const MAX_LEFT_OUT: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// The server says it is busy: 429, 5xx.
    Busy,
    /// The connection dropped, or no answer came in time.
    Dropped,
    /// The server hasn't got it: 404, 410.
    Missing,
    Other,
}

/// Why a piece didn't come.
pub struct Refusal {
    text: String,
    kind: Kind,
}

type Get = dyn Fn(&str) -> Result<Vec<u8>, Refusal> + Send + Sync;
type Stop = dyn Fn() -> bool + Send + Sync;

struct Run {
    urls: Vec<String>,
    ahead: usize,
    get: Box<Get>,
    stop: Box<Stop>,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    /// The next piece to ask for, and the next to hand on.
    next: usize,
    turn: usize,
    /// Pieces that came before their turn (empty: left out).
    early: BTreeMap<usize, Vec<u8>>,
    asking: usize,
    /// Requests allowed at once.
    limit: usize,
    left_out: usize,
    failed: Option<String>,
    /// Nothing more is wanted.
    over: bool,
}

/// Fetches `urls` up to `PARALLEL` at a time, at most `ahead` past the one
/// being handed on, and gives each piece to `take` with its place in the
/// list, in order and without its ID3 header. It ends early when `stop` says
/// so or `take` returns false (both fine), and with an error when a piece
/// can't be had even after waiting, or too many are missing.
pub fn in_order(
    urls: Vec<String>,
    ahead: usize,
    stop: impl Fn() -> bool + Send + Sync + 'static,
    take: impl FnMut(usize, &[u8]) -> bool,
) -> Result<(), String> {
    run(urls, ahead, get, stop, take)
}

fn get(url: &str) -> Result<Vec<u8>, Refusal> {
    let refused = |text: String, kind| Refusal { text, kind };
    let resp = hls::agent().get(url).call().map_err(|e| match &e {
        ureq::Error::Status(code, _) => refused(
            e.to_string(),
            match code {
                429 | 500..=599 => Kind::Busy,
                404 | 410 => Kind::Missing,
                _ => Kind::Other,
            },
        ),
        ureq::Error::Transport(_) => refused(e.to_string(), Kind::Dropped),
    })?;
    let mut bytes = Vec::new();
    resp.into_reader().take(64 << 20).read_to_end(&mut bytes).map_err(|e| refused(e.to_string(), Kind::Dropped))?;
    if bytes.is_empty() {
        return Err(refused(format!("{url}: empty"), Kind::Other));
    }
    Ok(bytes)
}

fn run(
    urls: Vec<String>,
    ahead: usize,
    get: impl Fn(&str) -> Result<Vec<u8>, Refusal> + Send + Sync + 'static,
    stop: impl Fn() -> bool + Send + Sync + 'static,
    mut take: impl FnMut(usize, &[u8]) -> bool,
) -> Result<(), String> {
    let workers = PARALLEL.min(urls.len());
    let state = State { next: 0, turn: 0, early: BTreeMap::new(), asking: 0, limit: PARALLEL, left_out: 0, failed: None, over: false };
    let run = Arc::new(Run { urls, ahead: ahead.max(1), get: Box::new(get), stop: Box::new(stop), state: Mutex::new(state), changed: Condvar::new() });
    // Fetchers are left to finish on their own: one may still be waiting on
    // the server after the caller is done, and nobody needs to wait for it.
    // One starts at once and the rest when the first piece is in, so pieces
    // dropped straight away (a quick skip in the player) cost one request.
    let fetcher = || {
        let run = run.clone();
        thread::spawn(move || run.work());
    };
    fetcher();
    let result = run.hand_on(&mut take, || (1..workers).for_each(|_| fetcher()));
    run.state.lock().unwrap().over = true;
    run.changed.notify_all();
    result
}

impl Run {
    /// Gives the pieces on in order, each as soon as it is there; `more`
    /// once the first is in.
    fn hand_on(&self, take: &mut impl FnMut(usize, &[u8]) -> bool, more: impl FnOnce()) -> Result<(), String> {
        let mut more = Some(more);
        for i in 0..self.urls.len() {
            if (self.stop)() {
                return Ok(());
            }
            let piece = {
                let mut s = self.state.lock().unwrap();
                loop {
                    if let Some(piece) = s.early.remove(&i) {
                        s.turn = i + 1;
                        break piece;
                    }
                    if let Some(e) = &s.failed {
                        return Err(e.clone());
                    }
                    if (self.stop)() {
                        return Ok(());
                    }
                    s = self.changed.wait_timeout(s, Duration::from_millis(100)).unwrap().0;
                }
            };
            // A place ahead is free again
            self.changed.notify_all();
            if let Some(more) = more.take() {
                more();
            }
            if !piece.is_empty() && !take(i, hls::strip_id3(&piece)) {
                return Ok(());
            }
        }
        Ok(())
    }

    /// One fetcher: takes the next piece due, whenever there is room for it.
    fn work(&self) {
        loop {
            let i = {
                let mut s = self.state.lock().unwrap();
                loop {
                    if s.over || s.failed.is_some() || s.next >= self.urls.len() || (self.stop)() {
                        return;
                    }
                    if s.asking < s.limit && s.next < s.turn + self.ahead {
                        break;
                    }
                    s = self.changed.wait_timeout(s, Duration::from_millis(100)).unwrap().0;
                }
                s.asking += 1;
                s.next += 1;
                s.next - 1
            };
            let got = self.fetch(i);
            let mut s = self.state.lock().unwrap();
            s.asking -= 1;
            match got {
                Ok(piece) => {
                    s.early.insert(i, piece);
                }
                Err(r) if r.kind == Kind::Missing && s.left_out < MAX_LEFT_OUT => {
                    s.left_out += 1;
                    s.early.insert(i, Vec::new());
                }
                Err(r) => {
                    s.failed.get_or_insert(r.text);
                }
            }
            drop(s);
            self.changed.notify_all();
        }
    }

    /// A piece, tried again after each of `WAITS` while the server is busy or
    /// the connection drops, and twice more for anything else. When the
    /// server says it is busy, it gets one request fewer at a time from then
    /// on; a slow or dropped connection isn't the server asking for that.
    fn fetch(&self, i: usize) -> Result<Vec<u8>, Refusal> {
        let mut attempt = 0;
        loop {
            let refusal = match (self.get)(&self.urls[i]) {
                Ok(piece) => return Ok(piece),
                Err(r) => r,
            };
            if refusal.kind == Kind::Busy {
                let mut s = self.state.lock().unwrap();
                s.limit = s.limit.saturating_sub(1).max(FEWEST);
            }
            let patience = if matches!(refusal.kind, Kind::Busy | Kind::Dropped) { WAITS.len() } else { 2 };
            if attempt == patience || !self.pause(WAITS[attempt]) {
                return Err(refusal);
            }
            attempt += 1;
        }
    }

    /// Waits `ms`, unless the pieces stop being wanted meanwhile (then false).
    fn pause(&self, ms: u64) -> bool {
        let until = Instant::now() + Duration::from_millis(ms);
        loop {
            if (self.stop)() || self.state.lock().unwrap().over {
                return false;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            thread::sleep(left.min(Duration::from_millis(50)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};

    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| i.to_string()).collect()
    }

    /// A made-up server: each piece is its own number, after a short wait
    /// that differs from piece to piece, so they come back out of order.
    fn slow(url: &str) -> Result<Vec<u8>, Refusal> {
        let i: u64 = url.parse().unwrap();
        thread::sleep(Duration::from_millis(1 + (i * 7919) % 23));
        Ok(i.to_le_bytes().to_vec())
    }

    fn number(piece: &[u8]) -> usize {
        u64::from_le_bytes(piece.try_into().unwrap()) as usize
    }

    fn refusal(text: &str, kind: Kind) -> Refusal {
        Refusal { text: text.into(), kind }
    }

    #[test]
    fn hands_the_pieces_on_in_order_a_few_at_a_time() {
        let (now, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (n, m) = (now.clone(), most.clone());
        let counted = move |url: &str| {
            m.fetch_max(n.fetch_add(1, SeqCst) + 1, SeqCst);
            let got = slow(url);
            n.fetch_sub(1, SeqCst);
            got
        };
        let mut seen = Vec::new();
        let started = Instant::now();
        run(urls(300), AHEAD_PLAY, counted, || false, |i, piece| {
            assert_eq!(i, number(piece), "each piece comes with its place");
            seen.push(i);
            true
        })
        .unwrap();
        assert_eq!(seen, (0..300).collect::<Vec<_>>());
        assert_eq!(most.load(SeqCst), PARALLEL, "never more than {PARALLEL} at once, and that many in use");
        // One at a time, these would take about 3.6 s
        assert!(started.elapsed() < Duration::from_millis(1500), "{:?}", started.elapsed());
    }

    #[test]
    fn fetches_only_a_little_ahead_of_what_is_taken() {
        let fetched = Arc::new(AtomicUsize::new(0));
        let f = fetched.clone();
        let mut furthest = 0;
        let get = move |url: &str| {
            f.fetch_max(url.parse::<usize>().unwrap() + 1, SeqCst);
            slow(url)
        };
        run(urls(100), AHEAD_PLAY, get, || false, |_, piece| {
            // Taken slowly, as when ffmpeg or the speaker is the slower one
            thread::sleep(Duration::from_millis(5));
            furthest = furthest.max(fetched.load(SeqCst) - number(piece));
            true
        })
        .unwrap();
        assert!(furthest <= AHEAD_PLAY + 1, "fetched {furthest} pieces ahead");
    }

    #[test]
    fn stops_when_told_or_when_nothing_more_is_taken() {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let mut taken = 0;
        run(urls(1000), AHEAD_PLAY, slow, move || s.load(SeqCst), |_, _| {
            taken += 1;
            if taken == 10 {
                stop.store(true, SeqCst);
            }
            true
        })
        .unwrap();
        assert!(taken <= 10 + AHEAD_PLAY, "took {taken}");

        let mut taken = 0;
        run(urls(1000), AHEAD_PLAY, slow, || false, |_, _| {
            taken += 1;
            taken < 5
        })
        .unwrap();
        assert_eq!(taken, 5);
    }

    #[test]
    fn asks_for_one_piece_only_until_the_first_is_in() {
        let (calls, stop) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (c, s) = (calls.clone(), stop.clone());
        let get = move |url: &str| {
            c.fetch_add(1, SeqCst);
            // Dropped while the first piece is on its way, as by a quick skip in the player
            s.store(true, SeqCst);
            slow(url)
        };
        let mut taken = 0;
        run(urls(100), AHEAD_PLAY, get, move || stop.load(SeqCst), |_, _| {
            taken += 1;
            true
        })
        .unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(SeqCst), 1, "one request in all");
        assert!(taken <= 1);
    }

    #[test]
    fn waits_out_a_busy_server_and_carries_on() {
        let tries = Arc::new(AtomicUsize::new(0));
        let t = tries.clone();
        let get = move |url: &str| {
            // Piece 30 fails three times, as during a short drop of the connection
            if url == "30" && t.fetch_add(1, SeqCst) < 3 {
                return Err(refusal("connection reset", Kind::Dropped));
            }
            slow(url)
        };
        let mut seen = Vec::new();
        run(urls(60), AHEAD_PLAY, get, || false, |_, piece| {
            seen.push(number(piece));
            true
        })
        .unwrap();
        assert_eq!(seen, (0..60).collect::<Vec<_>>(), "nothing lost, nothing out of order");
        assert_eq!(tries.load(SeqCst), 4);
    }

    #[test]
    fn the_rest_go_on_while_the_server_takes_long_over_one_piece() {
        let fetched = Arc::new(AtomicUsize::new(0));
        let f = fetched.clone();
        let get = move |url: &str| {
            // Piece 10 takes 3 s, as the server sometimes does
            if url == "10" {
                thread::sleep(Duration::from_secs(3));
            }
            f.fetch_add(1, SeqCst);
            slow(url)
        };
        let mut ready_by_then = 0;
        let started = Instant::now();
        run(urls(400), AHEAD_CUT, get, || false, |i, _| {
            if i == 10 {
                ready_by_then = fetched.load(SeqCst);
            }
            true
        })
        .unwrap();
        assert!(ready_by_then > 300, "only {ready_by_then} pieces fetched while piece 10 was on its way");
        assert!(started.elapsed() < Duration::from_millis(4000), "{:?}", started.elapsed());
    }

    #[test]
    fn a_slow_or_dropped_connection_does_not_cut_the_requests() {
        let (tries, now, most_after) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (t, n, m) = (tries.clone(), now.clone(), most_after.clone());
        let get = move |url: &str| {
            let at_once = n.fetch_add(1, SeqCst) + 1;
            let i: usize = url.parse().unwrap();
            // The first try of pieces 1 to 8 times out
            let got = if (1..=8).contains(&i) && t.fetch_add(1, SeqCst) < 8 {
                Err(refusal("timed out reading response", Kind::Dropped))
            } else {
                if i > 60 {
                    m.fetch_max(at_once, SeqCst);
                }
                slow(url)
            };
            n.fetch_sub(1, SeqCst);
            got
        };
        run(urls(200), AHEAD_PLAY, get, || false, |_, _| true).unwrap();
        assert_eq!(most_after.load(SeqCst), PARALLEL, "still {PARALLEL} at once");
    }

    #[test]
    fn leaves_out_a_missing_piece_as_ffmpeg_does() {
        let get = |url: &str| if url == "20" { Err(refusal("404 Not Found", Kind::Missing)) } else { slow(url) };
        let mut seen = Vec::new();
        run(urls(40), AHEAD_PLAY, get, || false, |_, piece| {
            seen.push(number(piece));
            true
        })
        .unwrap();
        assert_eq!(seen, (0..40).filter(|&i| i != 20).collect::<Vec<_>>());
    }

    #[test]
    fn too_many_missing_pieces_or_a_refused_one_end_it() {
        let get = |url: &str| if url.parse::<usize>().unwrap() % 7 == 3 { Err(refusal("404 Not Found", Kind::Missing)) } else { slow(url) };
        let result = run(urls(100), AHEAD_PLAY, get, || false, |_, _| true);
        assert_eq!(result.err().as_deref(), Some("404 Not Found"));

        let tries = Arc::new(AtomicUsize::new(0));
        let t = tries.clone();
        let get = move |url: &str| {
            if url == "20" {
                t.fetch_add(1, SeqCst);
                return Err(refusal("403 Forbidden", Kind::Other));
            }
            slow(url)
        };
        let mut taken = Vec::new();
        let result = run(urls(40), AHEAD_PLAY, get, || false, |_, piece| {
            taken.push(number(piece));
            true
        });
        assert_eq!(result.err().as_deref(), Some("403 Forbidden"));
        assert_eq!(tries.load(SeqCst), 3);
        assert!(taken.iter().all(|&i| i < 20), "nothing past the refused piece is handed on");
    }

    #[test]
    fn asks_for_fewer_at_once_when_the_server_pushes_back() {
        let (served, now, most_after) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (c, n, m) = (served.clone(), now.clone(), most_after.clone());
        let refused = Mutex::new(std::collections::HashSet::new());
        let get = move |url: &str| {
            let i: usize = url.parse().unwrap();
            let at_once = n.fetch_add(1, SeqCst) + 1;
            // Pieces 1 to 8 are refused once each: "too busy, slow down"
            let got = if (1..=8).contains(&i) && refused.lock().unwrap().insert(i) {
                Err(refusal("503 Service Unavailable", Kind::Busy))
            } else {
                if c.fetch_add(1, SeqCst) > 40 {
                    m.fetch_max(at_once, SeqCst);
                }
                slow(url)
            };
            n.fetch_sub(1, SeqCst);
            got
        };
        let mut seen = Vec::new();
        run(urls(120), AHEAD_PLAY, get, || false, |_, piece| {
            seen.push(number(piece));
            true
        })
        .unwrap();
        assert_eq!(seen, (0..120).collect::<Vec<_>>(), "every piece still comes, in order");
        assert!(most_after.load(SeqCst) <= FEWEST, "{} at once after the push back", most_after.load(SeqCst));
    }
}
