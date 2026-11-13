//! Music library: folders, Serato crates and the "Shared with me" folder.
//!
//! Tags and content hashes are cached on disk (keyed by path, size and
//! modified time), so rescans are fast and a track is only hashed once.
//!
//! Beatgrids a DJ has edited are kept separately (`beatgrids.json`), keyed by
//! content hash, so they're never overwritten by analysis and follow the
//! track wherever it's loaded from.

use crate::analysis::{self, BeatGrid};
use crate::audio::TrackAudio;
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
    /// BPM from Tandem's own analysis (or the DJ's edited grid), once known.
    #[serde(default)]
    pub grid_bpm: Option<f64>,
    /// Detected (or tagged) key, see [`crate::keys`].
    #[serde(default)]
    pub key: Option<i8>,
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
    /// Analysis result (`analyzed` is true even when no steady beat was found).
    /// Always the detected grid; edits live in [`Library::user_grids`].
    #[serde(default)]
    grid: Option<BeatGrid>,
    #[serde(default)]
    analyzed: bool,
    #[serde(default)]
    key: Option<i8>,
    #[serde(default)]
    key_done: bool,
    /// Which version of key detection produced `key`.
    #[serde(default)]
    key_version: u32,
}

/// Bump when key detection improves: cached keys from older versions are redone.
const KEY_VERSION: u32 = 2;

/// "Left - Right" parts of a file name, if it has that shape.
fn name_split(path: &Path) -> Option<(String, String)> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let (a, b) = stem.split_once(" - ")?;
    Some((a.trim().to_string(), b.trim().to_string()))
}

fn tidy(s: &str) -> String {
    s.replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Untagged files take their names from "A - B" file names. Libraries
/// differ on which side is the artist; the artist side repeats across files
/// (one artist, many tracks), the title side mostly doesn't. Decide per
/// list and tidy underscores into spaces.
fn fix_file_names(tracks: &mut [LibTrack]) -> bool {
    let derived: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            name_split(&t.path)
                .map(|(a, b)| t.artist == a && t.title == b)
                .unwrap_or(false)
        })
        .map(|(i, _)| i)
        .collect();
    if derived.is_empty() {
        return false;
    }
    let mut left: HashMap<String, usize> = HashMap::new();
    let mut right: HashMap<String, usize> = HashMap::new();
    for &i in &derived {
        *left.entry(tracks[i].artist.to_lowercase()).or_default() += 1;
        *right.entry(tracks[i].title.to_lowercase()).or_default() += 1;
    }
    let repeats = |m: &HashMap<String, usize>| m.values().filter(|&&c| c > 1).sum::<usize>();
    let swap = derived.len() >= 6 && repeats(&right) > repeats(&left) * 2;
    for &i in &derived {
        let t = &mut tracks[i];
        let (a, b) = (tidy(&t.artist), tidy(&t.title));
        if swap {
            t.title = a;
            t.artist = b;
        } else {
            t.artist = a;
            t.title = b;
        }
    }
    swap
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
    /// Background BPM analysis: (list key, done, total).
    pub analyzing: Option<(String, usize, usize)>,
    /// Folders whose untagged files are named "Title - Artist".
    title_first_dirs: std::collections::HashSet<PathBuf>,
    /// Beatgrids edited by hand, by content hash.
    user_grids: HashMap<TrackId, BeatGrid>,
    grids_path: PathBuf,
    grids_dirty: bool,
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
        let grids_path = cache_path.with_file_name("beatgrids.json");
        let user_grids = match std::fs::read(&grids_path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::error!("Couldn't read {}: {e}", grids_path.display());
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        };
        Self {
            folders: Vec::new(),
            crates: Vec::new(),
            shared_dir,
            lists: HashMap::new(),
            cache,
            cache_path,
            cache_dirty: false,
            size_index: HashMap::new(),
            analyzing: None,
            title_first_dirs: Default::default(),
            user_grids,
            grids_path,
            grids_dirty: false,
        }
    }

    /// Write edited beatgrids to disk, if any changed.
    pub fn save_grids(&mut self) {
        if !self.grids_dirty {
            return;
        }
        self.grids_dirty = false;
        let res = serde_json::to_vec(&self.user_grids)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                let tmp = self.grids_path.with_extension("tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, &self.grids_path)
            });
        if let Err(e) = res {
            log::error!("Couldn't save edited beatgrids: {e}");
        }
    }

    /// The grid a track shows: the DJ's edit if there is one, else analysis.
    fn shown_grid(&self, e: &CacheEntry) -> Option<BeatGrid> {
        e.id.and_then(|id| self.user_grids.get(&id).copied())
            .or(e.grid)
    }

    fn shown_bpm(&self, e: &CacheEntry) -> Option<f64> {
        self.shown_grid(e).map(|g| g.main_bpm(e.duration))
    }

    pub fn save_cache(&mut self) {
        self.save_grids();
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
                        grid: None,
                        analyzed: false,
                        key: None,
                        key_done: false,
                        key_version: 0,
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
            grid_bpm: None,
            key: None,
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
            let mut shown_bpm = hit.as_ref().and_then(|e| lib.lock().shown_bpm(e));
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
                        grid: None,
                        analyzed: false,
                        key: None,
                        key_done: false,
                        key_version: 0,
                    };
                    let mut l = lib.lock();
                    l.cache
                        .insert(path.to_string_lossy().to_string(), e.clone());
                    l.cache_dirty = true;
                    shown_bpm = None;
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
                grid_bpm: shown_bpm,
                key: e.key,
            });
            if i % 500 == 499 {
                let mut l = lib.lock();
                l.lists.entry(key.clone()).or_default().tracks = out.clone();
            }
        }
        let swapped = fix_file_names(&mut out);
        let mut l = lib.lock();
        if swapped {
            if let SourceKind::Folder(dir) = &kind {
                l.title_first_dirs.insert(dir.clone());
            }
        }
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

/// Title and artist for a file as the library shows them (tags, or the
/// tidied file name in the order its folder uses).
pub fn display_names(lib: &Arc<Mutex<Library>>, path: &Path) -> (String, String) {
    let (title, artist, _, _) = read_tags(path);
    let Some((a, b)) = name_split(path) else {
        return (title, artist);
    };
    if artist != a || title != b {
        return (title, artist); // came from real tags
    }
    let title_first = lib
        .lock()
        .title_first_dirs
        .iter()
        .any(|d| path.starts_with(d));
    if title_first {
        (tidy(&a), tidy(&b))
    } else {
        (tidy(&b), tidy(&a))
    }
}

/// The grid analysis found for a file, if it has been analysed (and hasn't changed).
pub fn detected_grid(lib: &Arc<Mutex<Library>>, path: &Path) -> Option<BeatGrid> {
    let (size, mtime) = stat(path)?;
    lib.lock().cached(path, size, mtime).and_then(|e| e.grid)
}

/// The grid to use for a file: the DJ's edit, else what analysis found.
pub fn cached_grid(lib: &Arc<Mutex<Library>>, path: &Path) -> Option<BeatGrid> {
    let (size, mtime) = stat(path)?;
    let l = lib.lock();
    l.cached(path, size, mtime).and_then(|e| l.shown_grid(e))
}

/// The DJ's edited beatgrid for a track, if any.
pub fn user_grid(lib: &Arc<Mutex<Library>>, id: TrackId) -> Option<BeatGrid> {
    lib.lock().user_grids.get(&id).copied()
}

/// Remember a beatgrid a DJ set for a track: kept if `grid` is an edit,
/// forgotten otherwise (back to what analysis found). `path`, if this
/// machine has the file, updates the BPM the library lists show.
pub fn set_user_grid(
    lib: &Arc<Mutex<Library>>,
    id: TrackId,
    path: Option<&Path>,
    grid: Option<BeatGrid>,
) {
    let mut l = lib.lock();
    let changed = match grid.filter(|g| g.edited) {
        Some(g) => l.user_grids.insert(id, g) != Some(g),
        None => l.user_grids.remove(&id).is_some(),
    };
    if !changed {
        return;
    }
    l.grids_dirty = true;
    let Some(path) = path else { return };
    let shown = stat(path)
        .and_then(|(size, mtime)| l.cached(path, size, mtime).cloned())
        .and_then(|e| l.shown_bpm(&e));
    for list in l.lists.values_mut() {
        for t in list.tracks.iter_mut().filter(|t| t.path == path) {
            t.grid_bpm = shown;
        }
    }
}

fn is_analyzed(lib: &Arc<Mutex<Library>>, path: &Path) -> bool {
    let Some((size, mtime)) = stat(path) else {
        return true;
    };
    lib.lock()
        .cached(path, size, mtime)
        .map(|e| e.analyzed)
        .unwrap_or(false)
}

/// Remember an analysis result for a file and show it in every list.
pub fn record_grid(lib: &Arc<Mutex<Library>>, path: &Path, grid: Option<BeatGrid>) {
    let Some((size, mtime)) = stat(path) else {
        return;
    };
    let mut l = lib.lock();
    let key = path.to_string_lossy().to_string();
    if let Some(e) = l
        .cache
        .get_mut(&key)
        .filter(|e| e.size == size && e.mtime == mtime)
    {
        e.grid = grid;
        e.analyzed = true;
        l.cache_dirty = true;
    }
    let shown = l
        .cached(path, size, mtime)
        .cloned()
        .and_then(|e| l.shown_bpm(&e))
        .or(grid.map(|g| g.bpm_at(0.0)));
    for list in l.lists.values_mut() {
        for t in list.tracks.iter_mut().filter(|t| t.path == path) {
            t.grid_bpm = shown;
        }
    }
}

/// A key from the file's tags (TKEY etc.), if it has one.
pub fn read_key_tag(path: &Path) -> Option<i8> {
    use lofty::prelude::*;
    let tf = lofty::read_from_path(path).ok()?;
    let tag = tf.primary_tag().or_else(|| tf.first_tag())?;
    tag.get_string(&ItemKey::InitialKey)
        .and_then(crate::keys::parse)
}

fn key_known(lib: &Arc<Mutex<Library>>, path: &Path) -> Option<Option<i8>> {
    let (size, mtime) = stat(path)?;
    lib.lock()
        .cached(path, size, mtime)
        .filter(|e| e.key_done && e.key_version == KEY_VERSION)
        .map(|e| e.key)
}

fn record_key(lib: &Arc<Mutex<Library>>, path: &Path, key: Option<i8>) {
    let Some((size, mtime)) = stat(path) else {
        return;
    };
    let mut l = lib.lock();
    let k = path.to_string_lossy().to_string();
    if let Some(e) = l
        .cache
        .get_mut(&k)
        .filter(|e| e.size == size && e.mtime == mtime)
    {
        e.key = key;
        e.key_done = true;
        e.key_version = KEY_VERSION;
        l.cache_dirty = true;
    }
    for list in l.lists.values_mut() {
        for t in list.tracks.iter_mut().filter(|t| t.path == path) {
            t.key = key;
        }
    }
}

/// Beatgrid and key for a decoded track: from the cache, or worked out now
/// (and cached). A beatgrid the DJ edited (looked up by `audio.id`) wins
/// over analysis, and a key tag in the file wins over detection.
pub fn analysis_for(
    lib: &Arc<Mutex<Library>>,
    path: &Path,
    audio: &TrackAudio,
    tag_bpm: Option<f32>,
) -> (Option<BeatGrid>, Option<i8>) {
    let detected = if is_analyzed(lib, path) {
        detected_grid(lib, path)
    } else {
        let g = analysis::analyze(audio, tag_bpm.map(|b| b as f64));
        record_grid(lib, path, g);
        g
    };
    let grid = user_grid(lib, audio.id).or(detected);
    let key = match key_known(lib, path) {
        Some(k) => k,
        None => {
            let k = read_key_tag(path).or_else(|| crate::keys::detect(audio));
            record_key(lib, path, k);
            k
        }
    };
    (grid, key)
}

/// Cached key for a file, if known.
pub fn cached_key(lib: &Arc<Mutex<Library>>, path: &Path) -> Option<i8> {
    key_known(lib, path).flatten()
}

/// Analyse every not-yet-analysed track in a list, in the background.
pub fn analyze_list(lib: &Arc<Mutex<Library>>, key: &str) {
    let paths: Vec<PathBuf> = {
        let l = lib.lock();
        if l.analyzing.is_some() {
            return;
        }
        l.lists
            .get(key)
            .map(|li| li.tracks.iter().map(|t| t.path.clone()).collect())
            .unwrap_or_default()
    };
    let todo: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| !is_analyzed(lib, p) || key_known(lib, p).is_none())
        .collect();
    if todo.is_empty() {
        return;
    }
    let total = todo.len();
    log::info!("Analysing BPM for {total} tracks…");
    lib.lock().analyzing = Some((key.to_string(), 0, total));
    let queue = Arc::new(Mutex::new(todo));
    let workers = Arc::new(std::sync::atomic::AtomicUsize::new(2));
    for _ in 0..2 {
        let (lib, queue, workers) = (lib.clone(), queue.clone(), workers.clone());
        std::thread::spawn(move || {
            loop {
                let Some(path) = queue.lock().pop() else {
                    break;
                };
                if !is_analyzed(&lib, &path) || key_known(&lib, &path).is_none() {
                    // Make sure the file has a cache entry to hold the result.
                    let _ = hash_path(&lib, &path);
                    let hint = stat(&path)
                        .and_then(|(s, m)| lib.lock().cached(&path, s, m).and_then(|e| e.bpm));
                    match crate::audio::decode_file(&path, TrackId([0; 32])) {
                        Ok(a) => {
                            let _ = analysis_for(&lib, &path, &a, hint);
                        }
                        Err(_) => {
                            record_grid(&lib, &path, None);
                            record_key(&lib, &path, None);
                        }
                    }
                }
                if let Some(a) = lib.lock().analyzing.as_mut() {
                    a.1 += 1;
                }
            }
            if workers.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                let mut l = lib.lock();
                l.analyzing = None;
                l.save_cache();
                log::info!("BPM analysis finished");
            }
        });
    }
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
    fn detects_title_first_file_names() {
        let names = [
            "Air_Maxes - KETTAMA_Shady_Nasty_Fred_again..",
            "Me_heavy - Fred_again..",
            "Bleu_better_with_time - Fred_again..",
            "Already - Rochelle_Jordan",
            "All_Along - Rochelle_Jordan",
            "Around - Rochelle_Jordan",
            "21_Years - Skepta_Fred_again..",
            "Back_2_Back - Skepta_Fred_again..",
        ];
        let mut tracks: Vec<LibTrack> = names
            .iter()
            .map(|n| {
                let (a, b) = n.split_once(" - ").unwrap();
                LibTrack {
                    path: PathBuf::from(format!("/m/{n}.wav")),
                    size: 1,
                    title: b.to_string(),
                    artist: a.to_string(),
                    bpm: None,
                    duration: 0.0,
                    grid_bpm: None,
                    key: None,
                }
            })
            .collect();
        assert!(fix_file_names(&mut tracks));
        assert_eq!(tracks[3].title, "Already");
        assert_eq!(tracks[3].artist, "Rochelle Jordan");
        assert_eq!(tracks[0].title, "Air Maxes");
    }

    #[test]
    fn edited_grids_are_saved_and_reset_forgets_them() {
        let dir = std::env::temp_dir().join(format!("tandem-grid-test-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let open = || {
            Arc::new(Mutex::new(Library::new(
                dir.join("shared"),
                dir.join("library-cache.json"),
            )))
        };
        let id = TrackId([3; 32]);
        let edited = BeatGrid::new(0.2, 124.0).unwrap().set_marker(30.0).unwrap();
        let lib = open();
        set_user_grid(&lib, id, None, Some(edited));
        lib.lock().save_cache();
        assert_eq!(user_grid(&open(), id), Some(edited), "kept across restarts");
        // Resetting sends the detected (unedited) grid: the edit is forgotten.
        set_user_grid(&lib, id, None, BeatGrid::new(0.2, 124.0));
        lib.lock().save_cache();
        assert_eq!(user_grid(&open(), id), None);
        let _ = std::fs::remove_dir_all(dir);
    }

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
