use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: Option<u64>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub path: Option<PathBuf>,
}

pub fn parse_tracks(value: &serde_json::Value) -> Vec<Track> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|song| {
            let id = song["id"].as_u64()?;
            let artists = song.get("ar").or_else(|| song.get("artists"));
            let artist = artists
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v["name"].as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                })
                .unwrap_or_default();
            Some(Track {
                id: Some(id),
                title: song["name"].as_str().unwrap_or("未命名歌曲").into(),
                artist,
                album: song
                    .get("al")
                    .or_else(|| song.get("album"))
                    .and_then(|a| a["name"].as_str())
                    .unwrap_or("")
                    .into(),
                duration_ms: song
                    .get("dt")
                    .or_else(|| song.get("duration"))
                    .and_then(|d| d.as_u64())
                    .unwrap_or(0),
                path: None,
            })
        })
        .collect()
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Prefer an exact platform ID. Text matching is intentionally conservative: ambiguous
/// versions must stream from the platform rather than silently play the wrong recording.
pub fn local_match<'a>(remote: &Track, local: &'a [Track]) -> Option<&'a Track> {
    if let Some(id) = remote.id {
        if let Some(found) = local
            .iter()
            .find(|s| s.id == Some(id) && s.path.as_ref().is_some_and(|p| p.is_file()))
        {
            return Some(found);
        }
    }
    let title = normalized(&remote.title);
    let artist = normalized(&remote.artist);
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    let mut matches = local.iter().filter(|s| {
        s.path.as_ref().is_some_and(|p| p.is_file())
            && (s.id.is_none() || remote.id.is_none())
            && normalized(&s.title) == title
            && normalized(&s.artist) == artist
            && (remote.album.is_empty()
                || s.album.is_empty()
                || normalized(&remote.album) == normalized(&s.album))
            && (remote.duration_ms == 0
                || s.duration_ms == 0
                || remote.duration_ms.abs_diff(s.duration_ms) <= 2000)
    });
    let first = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(first)
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    pub tracks: Vec<Track>,
    pub index: Option<usize>,
    shuffle: bool,
    bag: Vec<usize>,
    history: Vec<usize>,
    rng: u64,
}

#[derive(Debug, Default)]
pub struct SearchPager {
    pub query: String,
    pub epoch: u64,
    pub loaded: usize,
    pub total: usize,
    pub in_flight: bool,
}
impl SearchPager {
    pub fn begin(&mut self, epoch: u64, query: String) -> (String, usize) {
        *self = Self {
            query: query.clone(),
            epoch,
            in_flight: true,
            ..Self::default()
        };
        (query, 0)
    }
    pub fn next_page(&mut self) -> Option<(String, usize)> {
        if !self.has_more() {
            return None;
        }
        self.in_flight = true;
        Some((self.query.clone(), self.loaded))
    }
    pub fn accept(&mut self, epoch: u64, offset: usize, count: usize, total: usize) -> bool {
        if epoch != self.epoch || offset != self.loaded || !self.in_flight {
            return false;
        }
        self.loaded += count;
        self.total = if count == 0 { self.loaded } else { total };
        self.in_flight = false;
        true
    }
    pub fn has_more(&self) -> bool {
        !self.query.is_empty() && !self.in_flight && self.loaded < self.total
    }
}

#[derive(Debug, Default)]
pub struct PlaybackIntent {
    pub epoch: u64,
    pub loaded: Option<u64>,
    pub pending: bool,
    pub playing: bool,
}
impl PlaybackIntent {
    pub fn begin(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.pending = true;
        self.playing = true;
    }
    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }
    pub fn ready(&mut self, epoch: u64) -> Option<bool> {
        if epoch != self.epoch || !self.pending {
            return None;
        }
        self.pending = false;
        self.loaded = Some(epoch);
        Some(self.playing)
    }
    pub fn ended(&mut self, epoch: u64) -> bool {
        if self.pending || self.loaded != Some(epoch) || epoch != self.epoch {
            return false;
        }
        self.loaded = None;
        true
    }
}
impl Queue {
    pub fn replace(&mut self, tracks: Vec<Track>, index: usize) {
        self.index = if index < tracks.len() {
            Some(index)
        } else {
            None
        };
        self.tracks = tracks;
        self.bag.clear();
        self.history.clear();
    }
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }
    /// Switching modes keeps the current track. The bag is rebuilt lazily, so an index
    /// can never survive a queue replacement or a mode change.
    pub fn set_shuffle(&mut self, enabled: bool) {
        self.shuffle = enabled;
        self.bag.clear();
        self.history.clear();
        if enabled {
            self.rng = seed();
        }
    }
    pub fn advance(&mut self, delta: i32) -> Option<Track> {
        if !self.shuffle {
            let index = self.index? as i64 + delta as i64;
            if index < 0 || index >= self.tracks.len() as i64 {
                return None;
            }
            self.index = Some(index as usize);
            return self.tracks.get(index as usize).cloned();
        }
        if delta > 0 {
            // One full pass over the shuffled bag plays every track once before any repeat.
            if self.tracks.len() < 2 {
                return None;
            }
            if self.bag.is_empty() {
                self.refill();
            }
            let next = self.bag.pop()?;
            if let Some(current) = self.index {
                self.history.push(current);
            }
            self.index = Some(next);
            return self.tracks.get(next).cloned();
        }
        if delta < 0 {
            let previous = self.history.pop()?;
            self.index = Some(previous);
            return self.tracks.get(previous).cloned();
        }
        None
    }
    fn refill(&mut self) {
        let mut pool = (0..self.tracks.len())
            .filter(|&i| Some(i) != self.index)
            .collect::<Vec<_>>();
        for i in (1..pool.len()).rev() {
            let j = (self.next_random() % (i as u64 + 1)) as usize;
            pool.swap(i, j);
        }
        self.bag = pool;
    }
    fn next_random(&mut self) -> u64 {
        if self.rng == 0 {
            self.rng = seed();
        }
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    (nanos ^ ((std::process::id() as u64) << 32)) | 1
}
