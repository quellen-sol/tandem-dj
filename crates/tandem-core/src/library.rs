//! Music library: folders, Serato crates and the "Shared with me" folder.
//!
//! Tags and content hashes are cached on disk (keyed by path, size and
//! modified time), so rescans are fast and a track is only hashed once.

use crate::model::TrackId;
use crate::serato::{self, SeratoCrate};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

pub const AUDIO_EXTS: &[&str] = &[
    "mp3", "m4a", "aac", "mp4", "flac", "wav", "aif", "aiff", "ogg", "alac",
];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibTrack {
    pub path: PathBuf,
    pub size: u64,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f32>,
    pub duration: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    title: String,
    artist: String,
    bpm: Option<f32>,
    duration: f64,
    id: Option<TrackId>,
}

/// Title/artist/BPM/duration from tags, falling back to "Artist - Title" file names.
pub fn read_tags(path: &Path) -> (String, String, Option<f32>, f64) {
    use lofty::prelude::*;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let (mut artist, mut title) = match stem.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), stem.clone()),
    };
    let mut bpm = None;
    let mut duration = 0.0;
    if let Ok(tf) = lofty::read_from_path(path) {
        duration = tf.properties().duration().as_secs_f64();
        if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
            if let Some(t) = tag.title() {
                if !t.trim().is_empty() {
                    title = t.trim().to_string();
                }
            }
            if let Some(a) = tag.artist() {
                if !a.trim().is_empty() {
                    artist = a.trim().to_string();
                }
            }
            bpm = tag
                .get_string(&ItemKey::Bpm)
                .or_else(|| tag.get_string(&ItemKey::IntegerBpm))
                .and_then(|s| s.trim().replace(',', ".").parse::<f32>().ok())
                .filter(|b| *b > 20.0 && *b < 400.0);
        }
    }
    (title, artist, bpm, duration)
}

pub fn hash_file(path: &Path) -> std::io::Result<TrackId> {
    let mut f = std::fs::File::open(path)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(TrackId(*h.finalize().as_bytes()))
}

fn stat(path: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some((m.len(), mtime))
}

#[derive(Clone, Debug)]
pub enum SourceKind {
    Folder(PathBuf),
    Crate(SeratoCrate),
    Shared,
}

#[derive(Default)]
pub struct TrackList {
    pub tracks: Vec<LibTrack>,
    pub scanning: bool,
    pub error: Option<String>,
}

pub struct Library {
    pub folders: Vec<PathBuf>,
    pub crates: Vec<SeratoCrate>,
    pub shared_dir: PathBuf,
    pub lists: HashMap<String, TrackList>,
    cache: HashMap<String, CacheEntry>,
    cache_path: PathBuf,
    cache_dirty: bool,
    size_index: HashMap<u64, Vec<PathBuf>>,
}

pub const KEY_SHARED: &str = "shared";

pub fn folder_key(p: &Path) -> String {
    format!("folder:{}", p.display())
}

pub fn crate_key(c: &SeratoCrate) -> String {
    format!("crate:{}", c.file.display())
}

impl Library {
    pub fn new(shared_dir: PathBuf, cache_path: PathBuf) -> Self {
        let cache = std::fs::read(&cache_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            folders: Vec::new(),
            crates: Vec::new(),
            shared_dir,
            lists: HashMap::new(),
            cache,
            cache_path,
            cache_dirty: false,
            size_index: HashMap::new(),
        }
    }

    pub fn save_cache(&mut self) {
        if !self.cache_dirty {
            return;
        }
        if let Ok(bytes) = serde_json::to_vec(&self.cache) {
            let tmp = self.cache_path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.cache_path);
            }
        }
        self.cache_dirty = false;
    }

    fn cached(&self, path: &Path, size: u64, mtime: u64) -> Option<&CacheEntry> {
        self.cache
            .get(&path.to_string_lossy().to_string())
            .filter(|e| e.size == size && e.mtime == mtime)
    }

    fn index(&mut self, tracks: &[LibTrack]) {
        for t in tracks {
            let v = self.size_index.entry(t.size).or_default();
            if !v.contains(&t.path) {
                v.push(t.path.clone());
            }
        }
    }

    pub fn record_hash(&mut self, path: &Path, id: TrackId) {
        let Some((size, mtime)) = stat(path) else {
            return;
        };
        let key = path.to_string_lossy().to_string();
        match self.cache.get_mut(&key) {
            Some(e) if e.size == size && e.mtime == mtime => e.id = Some(id),
            _ => {
                let (title, artist, bpm, duration) = read_tags(path);
                self.cache.insert(
                    key,
                    CacheEntry {
                        size,
                        mtime,
                        title,
                        artist,
                        bpm,
                        duration,
                        id: Some(id),
                    },
                );
            }
        }
        self.index(&[LibTrack {
            path: path.to_path_buf(),
            size,
            title: String::new(),
            artist: String::new(),
            bpm: None,
            duration: 0.0,
        }]);
        self.cache_dirty = true;
    }

    /// Re-discover Serato crates on this machine.
    pub fn refresh_crates(&mut self, music_dir: Option<&Path>) {
        self.crates = serato::discover(music_dir);
    }
}

/// Scan one source in the background, filling `lists[key]`.
pub fn scan(lib: &Arc<Mutex<Library>>, key: String, kind: SourceKind) {
    {
        let mut l = lib.lock();
        let entry = l.lists.entry(key.clone()).or_default();
        if entry.scanning {
            return;
        }
        entry.scanning = true;
        entry.error = None;
    }
    let lib = lib.clone();
    std::thread::spawn(move || {
        let (files, error) = match &kind {
            SourceKind::Folder(dir) => (walk(dir), None),
            SourceKind::Shared => {
                let dir = lib.lock().shared_dir.clone();
                (walk(&dir), None)
            }
            SourceKind::Crate(c) => match serato::crate_tracks(c) {
                Ok(paths) => (paths.into_iter().filter(|p| p.exists()).collect(), None),
                Err(e) => (Vec::new(), Some(format!("couldn't read crate: {e}"))),
            },
        };
        let mut out = Vec::with_capacity(files.len());
        for (i, path) in files.iter().enumerate() {
            let Some((size, mtime)) = stat(path) else {
                continue;
            };
            let hit = lib.lock().cached(path, size, mtime).cloned();
            let e = match hit {
                Some(e) => e,
                None => {
                    let (title, artist, bpm, duration) = read_tags(path);
                    let e = CacheEntry {
                        size,
                        mtime,
                        title,
                        artist,
                        bpm,
                        duration,
                        id: None,
                    };
                    let mut l = lib.lock();
                    l.cache
                        .insert(path.to_string_lossy().to_string(), e.clone());
                    l.cache_dirty = true;
                    e
                }
            };
            out.push(LibTrack {
                path: path.clone(),
                size,
                title: e.title,
                artist: e.artist,
                bpm: e.bpm,
                duration: e.duration,
            });
            if i % 500 == 499 {
                let mut l = lib.lock();
                l.lists.entry(key.clone()).or_default().tracks = out.clone();
            }
        }
        let mut l = lib.lock();
        l.index(&out);
        let entry = l.lists.entry(key).or_default();
        entry.tracks = out;
        entry.scanning = false;
        entry.error = error;
        l.save_cache();
    });
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && name != "_Serato_" {
                    stack.push(p);
                }
            } else if is_audio(&p) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Content hash of a file, using the cache when the file hasn't changed.
pub fn hash_path(lib: &Arc<Mutex<Library>>, path: &Path) -> std::io::Result<TrackId> {
    let (size, mtime) = stat(path).ok_or_else(|| std::io::Error::other("file not found"))?;
    if let Some(id) = lib.lock().cached(path, size, mtime).and_then(|e| e.id) {
        return Ok(id);
    }
    let id = hash_file(path)?;
    lib.lock().record_hash(path, id);
    Ok(id)
}

/// Find a local copy of a track by content hash. Checks known hashes first,
/// then hashes library files with exactly the same size (usually 0-2 files).
pub fn find_by_hash(lib: &Arc<Mutex<Library>>, id: TrackId, size: u64) -> Option<PathBuf> {
    let (known, candidates) = {
        let l = lib.lock();
        let known: Vec<PathBuf> = l
            .cache
            .iter()
            .filter(|(_, e)| e.id == Some(id))
            .map(|(p, _)| PathBuf::from(p))
            .collect();
        (known, l.size_index.get(&size).cloned().unwrap_or_default())
    };
    for p in known {
        if stat(&p).map(|(s, _)| s == size).unwrap_or(false) && hash_path(lib, &p).ok() == Some(id)
        {
            return Some(p);
        }
    }
    candidates
        .into_iter()
        .find(|p| hash_path(lib, p).ok() == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tracks_by_hash_via_size_index() {
        let dir = std::env::temp_dir().join(format!("tandem-lib-test-{}", fastrand::u64(..)));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let wav = dir.join("sub").join("Artist - Song.wav");
        crate::audio::write_wav(&wav, 44_100, &crate::audio::test_signal(44_100, 0.5, 330.0))
            .unwrap();
        let lib = Arc::new(Mutex::new(Library::new(
            dir.join("shared"),
            dir.join("cache.json"),
        )));
        scan(&lib, folder_key(&dir), SourceKind::Folder(dir.clone()));
        for _ in 0..200 {
            if !lib.lock().lists[&folder_key(&dir)].scanning {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let t = lib.lock().lists[&folder_key(&dir)].tracks[0].clone();
        assert_eq!(t.title, "Song");
        assert_eq!(t.artist, "Artist");
        let id = hash_file(&wav).unwrap();
        assert_eq!(find_by_hash(&lib, id, t.size), Some(wav.clone()));
        assert_eq!(find_by_hash(&lib, TrackId([7; 32]), t.size), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
