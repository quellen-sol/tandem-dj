//! Music library: folders, Serato crates, the "Shared with me" folder, and
//! "All Music" (all of them together).
//!
//! Tags and content hashes are cached on disk (keyed by path, size and
//! modified time), so rescans are fast and a track is only hashed once.
//!
//! Beatgrids a DJ has edited are kept separately (`beatgrids.json`), keyed by
//! content hash, so they're never overwritten by analysis and follow the
//! track wherever it's loaded from.

use crate::analysis::{self, BeatGrid};
use crate::audio::TrackAudio;
use crate::config::{LibrarySort, SortColumn};
use crate::model::TrackId;
use crate::serato::{self, SeratoCrate};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    /// Detected key, see [`crate::keys`].
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
    /// Which version of BPM detection produced `grid`.
    #[serde(default)]
    grid_version: u32,
    #[serde(default)]
    key: Option<i8>,
    #[serde(default)]
    key_done: bool,
    /// Which version of key detection produced `key`.
    #[serde(default)]
    key_version: u32,
}

/// Bump when key detection improves: cached keys from older versions are redone.
const KEY_VERSION: u32 = 3;
/// Bump when BPM detection improves: cached grids from older versions are
/// redone (grids the DJ edited are kept).
const GRID_VERSION: u32 = 2;

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
    /// Every library folder, "Shared with me" and every Serato crate, with
    /// each file listed once.
    All,
}

#[derive(Default)]
pub struct TrackList {
    pub tracks: Vec<LibTrack>,
    pub scanning: bool,
    /// Asked to scan again mid-scan (its sources changed): scan again after.
    rescan: bool,
    pub error: Option<String>,
    /// Bumped whenever a track's details change (BPM, key, names), so a
    /// sorted view knows to re-sort.
    pub rev: u64,
}

/// The sortable columns of one row in the track list.
pub struct SortRow<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub bpm: Option<f64>,
    pub key: Option<i8>,
    /// Seconds; 0 = unknown.
    pub duration: f64,
}

/// Order `rows` (indices into a list) by `sort`. Text sorts case-blind and
/// keys sort in Camelot order (1A, 1B, 2A…). Rows with an empty cell (no
/// BPM yet, no key, unknown length) go last either way, and rows that tie
/// keep their order from `rows`.
pub fn sort_rows<'a>(rows: &mut [usize], sort: LibrarySort, row: impl Fn(usize) -> SortRow<'a>) {
    #[derive(PartialEq, PartialOrd)]
    enum Cell {
        Text(String),
        Num(f64),
    }
    let cell = |r: SortRow| {
        match sort.column {
            SortColumn::Title => Some(Cell::Text(r.title.trim().to_lowercase())),
            SortColumn::Artist => Some(Cell::Text(r.artist.trim().to_lowercase())),
            SortColumn::Bpm => r.bpm.filter(|b| *b > 0.0).map(Cell::Num),
            SortColumn::Key => r
                .key
                .filter(|k| (0..24).contains(k))
                .map(|k| Cell::Num(crate::keys::camelot_rank(k) as f64)),
            SortColumn::Time => Some(r.duration).filter(|d| *d > 0.0).map(Cell::Num),
        }
        .filter(|c| !matches!(c, Cell::Text(t) if t.is_empty()))
    };
    let mut keyed: Vec<(usize, Option<Cell>)> = rows.iter().map(|&i| (i, cell(row(i)))).collect();
    keyed.sort_by(|(_, a), (_, b)| match (a, b) {
        (Some(a), Some(b)) => {
            let o = a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
            if sort.descending {
                o.reverse()
            } else {
                o
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    for (slot, (i, _)) in rows.iter_mut().zip(keyed) {
        *slot = i;
    }
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
    /// Background BPM analysis, while one runs.
    pub analyzing: Option<Analyzing>,
    /// Folders whose untagged files are named "Title - Artist".
    title_first_dirs: HashSet<PathBuf>,
    /// Beatgrids edited by hand, by content hash.
    user_grids: HashMap<TrackId, BeatGrid>,
    grids_path: PathBuf,
    grids_dirty: bool,
}

/// A background BPM analysis ([`analyze_list`]) in progress.
#[derive(Clone, Debug)]
pub struct Analyzing {
    /// The list it was started on.
    pub key: String,
    /// Tracks finished so far, of `total`.
    pub done: usize,
    pub total: usize,
    /// Cancelled ([`cancel_analysis`]): the workers stop after the tracks
    /// they're on, and `analyzing` clears when they have.
    pub stopping: bool,
}

pub const KEY_SHARED: &str = "shared";
pub const KEY_ALL: &str = "all";

pub fn folder_key(p: &Path) -> String {
    format!("folder:{}", p.display())
}

pub fn crate_key(c: &SeratoCrate) -> String {
    format!("crate:{}", c.file.display())
}

impl Library {
    pub fn new(shared_dir: PathBuf, cache_path: PathBuf) -> Self {
        let mut cache: HashMap<String, CacheEntry> = std::fs::read(&cache_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        // Grids from an older BPM detection are analysed again.
        for e in cache
            .values_mut()
            .filter(|e| e.grid_version != GRID_VERSION)
        {
            e.grid = None;
            e.analyzed = false;
        }
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
                        grid_version: 0,
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

/// Scan one source in the background, filling `lists[key]`. A first scan
/// fills the list in as it goes; a rescan keeps the old list on screen until
/// it's done. Asking while that source is scanning scans it again after.
pub fn scan(lib: &Arc<Mutex<Library>>, key: String, kind: SourceKind) {
    {
        let mut l = lib.lock();
        let entry = l.lists.entry(key.clone()).or_default();
        if entry.scanning {
            entry.rescan = true;
            return;
        }
        entry.scanning = true;
        entry.error = None;
    }
    let lib = lib.clone();
    std::thread::spawn(move || {
        let (groups, error) = source_files(&lib, &kind);
        let mut out: Vec<LibTrack> = Vec::new();
        let mut seen = HashSet::new();
        let mut title_first = Vec::new();
        for (dir, files) in groups {
            let start = out.len();
            for path in files {
                if !seen.insert(same_file(&path)) {
                    continue;
                }
                let Some(t) = list_track(&lib, path) else {
                    continue;
                };
                out.push(t);
                if out.len().is_multiple_of(500) {
                    let mut l = lib.lock();
                    let entry = l.lists.entry(key.clone()).or_default();
                    if out.len() > entry.tracks.len() {
                        entry.tracks = out.clone();
                        entry.rev += 1;
                    }
                }
            }
            if fix_file_names(&mut out[start..]) {
                title_first.extend(dir);
            }
        }
        let mut l = lib.lock();
        l.title_first_dirs.extend(title_first);
        l.index(&out);
        let entry = l.lists.entry(key.clone()).or_default();
        entry.tracks = out;
        entry.rev += 1;
        entry.scanning = false;
        entry.error = error;
        let again = std::mem::take(&mut entry.rescan);
        l.save_cache();
        drop(l);
        if again {
            scan(&lib, key, kind);
        }
    });
}

/// Rescan "All Music" after its sources change (a folder removed, crates
/// rescanned, a share received), if it has been listed. Choosing it in the
/// library always rescans it.
pub fn refresh_all(lib: &Arc<Mutex<Library>>) {
    if lib.lock().lists.contains_key(KEY_ALL) {
        scan(lib, KEY_ALL.into(), SourceKind::All);
    }
}

/// One group of files from a source, which decides on its own whether its
/// untagged files are named "Title - Artist" (see [`fix_file_names`]), with
/// the folder to remember that for.
type FileGroup = (Option<PathBuf>, Vec<PathBuf>);

/// The files a source lists, and an error to show for it.
fn source_files(lib: &Arc<Mutex<Library>>, kind: &SourceKind) -> (Vec<FileGroup>, Option<String>) {
    match kind {
        SourceKind::Folder(dir) => (vec![(Some(dir.clone()), walk(dir))], None),
        SourceKind::Shared => {
            let dir = lib.lock().shared_dir.clone();
            (vec![(None, walk(&dir))], None)
        }
        SourceKind::Crate(c) => match serato::crate_tracks(c) {
            Ok(paths) => (
                vec![(None, paths.into_iter().filter(|p| p.exists()).collect())],
                None,
            ),
            Err(e) => (Vec::new(), Some(format!("couldn't read crate: {e}"))),
        },
        // Each part is grouped as its own list is, so names come out the
        // same here as there. Folders come first, so a track that's also in
        // a crate is listed from its folder.
        SourceKind::All => {
            let (folders, shared, crates) = {
                let l = lib.lock();
                (l.folders.clone(), l.shared_dir.clone(), l.crates.clone())
            };
            let mut groups: Vec<FileGroup> = folders
                .into_iter()
                .map(|dir| {
                    let files = walk(&dir);
                    (Some(dir), files)
                })
                .collect();
            groups.push((None, walk(&shared)));
            for c in &crates {
                match serato::crate_tracks(c) {
                    Ok(paths) => groups.push((None, paths)),
                    Err(e) => log::warn!("All Music: couldn't read crate {}: {e}", c.name),
                }
            }
            (groups, None)
        }
    }
}

/// A path as compared to list each file once: Windows paths ignore case.
fn same_file(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    }
}

/// A file as a list shows it: from the cache, or its tags (then cached).
/// `None` if the file is missing.
fn list_track(lib: &Arc<Mutex<Library>>, path: PathBuf) -> Option<LibTrack> {
    let (size, mtime) = stat(&path)?;
    let hit = lib.lock().cached(&path, size, mtime).cloned();
    let mut shown_bpm = hit.as_ref().and_then(|e| lib.lock().shown_bpm(e));
    let e = match hit {
        Some(e) => e,
        None => {
            let (title, artist, bpm, duration) = read_tags(&path);
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
                grid_version: 0,
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
    Some(LibTrack {
        path,
        size,
        title: e.title,
        artist: e.artist,
        bpm: e.bpm,
        duration: e.duration,
        grid_bpm: shown_bpm,
        key: e.key,
    })
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
            list.rev += 1;
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
        e.grid_version = GRID_VERSION;
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
            list.rev += 1;
        }
    }
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
            list.rev += 1;
        }
    }
}

/// Beatgrid and key for a decoded track: from the cache, or worked out now
/// from the audio (and cached). A beatgrid the DJ edited (looked up by
/// `audio.id`) wins over analysis. BPM and key tags in the file are not
/// used: both come from the audio.
pub fn analysis_for(
    lib: &Arc<Mutex<Library>>,
    path: &Path,
    audio: &TrackAudio,
) -> (Option<BeatGrid>, Option<i8>) {
    let detected = if is_analyzed(lib, path) {
        detected_grid(lib, path)
    } else {
        let g = analysis::analyze(audio);
        record_grid(lib, path, g);
        g
    };
    let grid = user_grid(lib, audio.id).or(detected);
    let key = match key_known(lib, path) {
        Some(k) => k,
        None => {
            let k = crate::keys::detect(audio);
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
    lib.lock().analyzing = Some(Analyzing {
        key: key.to_string(),
        done: 0,
        total,
        stopping: false,
    });
    let queue = Arc::new(Mutex::new(todo));
    let workers = Arc::new(std::sync::atomic::AtomicUsize::new(2));
    for _ in 0..2 {
        let (lib, queue, workers) = (lib.clone(), queue.clone(), workers.clone());
        std::thread::spawn(move || {
            loop {
                if analysis_stopping(&lib) {
                    break;
                }
                let Some(path) = queue.lock().pop() else {
                    break;
                };
                if !is_analyzed(&lib, &path) || key_known(&lib, &path).is_none() {
                    // Make sure the file has a cache entry to hold the result.
                    let _ = hash_path(&lib, &path);
                    match crate::audio::decode_file(&path, TrackId([0; 32])) {
                        // Cancelled while decoding: leave this one for next time.
                        Ok(_) if analysis_stopping(&lib) => break,
                        Ok(a) => {
                            let _ = analysis_for(&lib, &path, &a);
                        }
                        Err(_) => {
                            record_grid(&lib, &path, None);
                            record_key(&lib, &path, None);
                        }
                    }
                }
                if let Some(a) = lib.lock().analyzing.as_mut() {
                    a.done += 1;
                }
            }
            if workers.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                let mut l = lib.lock();
                match l.analyzing.take() {
                    Some(a) if a.stopping => {
                        log::info!(
                            "BPM analysis cancelled after {} of {} tracks",
                            a.done,
                            a.total
                        )
                    }
                    _ => log::info!("BPM analysis finished"),
                }
                l.save_cache();
            }
        });
    }
}

/// Stop the background BPM analysis, if one is running. Tracks already done
/// keep their results; the rest are left for the next [`analyze_list`].
pub fn cancel_analysis(lib: &Arc<Mutex<Library>>) {
    if let Some(a) = lib.lock().analyzing.as_mut() {
        a.stopping = true;
    }
}

fn analysis_stopping(lib: &Arc<Mutex<Library>>) -> bool {
    lib.lock().analyzing.as_ref().is_none_or(|a| a.stopping)
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
    fn sorting_rows() {
        // (title, artist, bpm, key, duration)
        type Fields = (&'static str, &'static str, Option<f64>, Option<i8>, f64);
        let rows: [Fields; 5] = [
            ("beta", "Zed", Some(128.0), Some(9 + 12), 300.0), // 8A
            ("Alpha", "", None, None, 0.0),
            ("gamma", "amy", Some(122.5), Some(0), 240.0), // 8B
            ("alpha", "Bo", Some(174.0), Some(8 + 12), 410.0), // 1A
            ("Delta", "bo", Some(122.5), Some(7), 200.0),  // 9B
        ];
        let field = |i: usize| {
            let r = rows[i];
            SortRow {
                title: r.0,
                artist: r.1,
                bpm: r.2,
                key: r.3,
                duration: r.4,
            }
        };
        let order = |column, descending| {
            let mut v: Vec<usize> = (0..rows.len()).collect();
            sort_rows(&mut v, LibrarySort { column, descending }, field);
            v
        };
        // Case-blind; ties keep their order.
        assert_eq!(order(SortColumn::Title, false), [1, 3, 0, 4, 2]);
        assert_eq!(order(SortColumn::Title, true), [2, 4, 0, 1, 3]);
        // Empty cells last, both ways.
        assert_eq!(order(SortColumn::Artist, false), [2, 3, 4, 0, 1]);
        assert_eq!(order(SortColumn::Artist, true), [0, 3, 4, 2, 1]);
        assert_eq!(order(SortColumn::Bpm, false), [2, 4, 0, 3, 1]);
        assert_eq!(order(SortColumn::Bpm, true), [3, 0, 2, 4, 1]);
        // Camelot order: 1A, 8A, 8B, 9B.
        assert_eq!(order(SortColumn::Key, false), [3, 0, 2, 4, 1]);
        assert_eq!(order(SortColumn::Time, false), [4, 2, 0, 3, 1]);
        assert_eq!(order(SortColumn::Time, true), [3, 0, 2, 4, 1]);
        // Sorting a filtered subset only reorders that subset.
        let mut v = vec![4, 0, 2];
        sort_rows(
            &mut v,
            LibrarySort {
                column: SortColumn::Bpm,
                descending: false,
            },
            field,
        );
        assert_eq!(v, [4, 2, 0]);
    }

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

    #[test]
    fn cancelled_analysis_stops_early_and_carries_on_later() {
        let dir = std::env::temp_dir().join(format!("tandem-cancel-test-{}", fastrand::u64(..)));
        std::fs::create_dir_all(&dir).unwrap();
        let signal = crate::audio::test_signal(44_100, 8.0, 330.0);
        let n = 12;
        for i in 0..n {
            let p = dir.join(format!("Artist - Song {i}.wav"));
            crate::audio::write_wav(&p, 44_100, &signal).unwrap();
        }
        let lib = Arc::new(Mutex::new(Library::new(
            dir.join("shared"),
            dir.join("cache.json"),
        )));
        let key = folder_key(&dir);
        let wait_for = |done: &dyn Fn(&Library) -> bool| {
            for _ in 0..6000 {
                if done(&lib.lock()) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("timed out");
        };
        scan(&lib, key.clone(), SourceKind::Folder(dir.clone()));
        wait_for(&|l| !l.lists[&key].scanning);
        let paths: Vec<PathBuf> = lib.lock().lists[&key]
            .tracks
            .iter()
            .map(|t| t.path.clone())
            .collect();
        assert_eq!(paths.len(), n);
        let analysed = || {
            paths
                .iter()
                .filter(|p| is_analyzed(&lib, p) && key_known(&lib, p).is_some())
                .count()
        };

        analyze_list(&lib, &key);
        cancel_analysis(&lib);
        assert!(lib.lock().analyzing.as_ref().is_some_and(|a| a.stopping));
        // Starting again while it stops does nothing.
        analyze_list(&lib, &key);
        assert!(lib.lock().analyzing.as_ref().is_some_and(|a| a.stopping));
        wait_for(&|l| l.analyzing.is_none());
        // Only the tracks the two workers were on when it was cancelled.
        let first = analysed();
        assert!(first <= 2, "{first} tracks analysed after cancelling");

        // Analyze BPM again carries on with the rest.
        analyze_list(&lib, &key);
        assert_eq!(
            lib.lock().analyzing.as_ref().map(|a| a.total),
            Some(n - first)
        );
        wait_for(&|l| l.analyzing.is_none());
        assert_eq!(analysed(), n);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn all_music_lists_every_source_once() {
        let dir = std::env::temp_dir().join(format!("tandem-all-test-{}", fastrand::u64(..)));
        let signal = crate::audio::test_signal(44_100, 0.2, 330.0);
        for f in [
            "a/Artist - Song.wav",
            "a/sub/Other - Tune.wav",
            "shared/Friend - Gift.wav",
            "loose/Solo - One.wav",
        ] {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            crate::audio::write_wav(&p, 44_100, &signal).unwrap();
        }
        // A Serato crate holding a track that's also in a folder, one that
        // isn't, and one that's gone.
        let field = |tag: &[u8; 4], data: &[u8]| {
            let mut v = tag.to_vec();
            v.extend((data.len() as u32).to_be_bytes());
            v.extend(data);
            v
        };
        let mut crate_file = Vec::new();
        for p in ["a/Artist - Song.wav", "loose/Solo - One.wav", "gone.wav"] {
            let utf16: Vec<u8> = p.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
            crate_file.extend(field(b"otrk", &field(b"ptrk", &utf16)));
        }
        std::fs::write(dir.join("Gig.crate"), crate_file).unwrap();

        let lib = Arc::new(Mutex::new(Library::new(
            dir.join("shared"),
            dir.join("cache.json"),
        )));
        {
            let mut l = lib.lock();
            // The second folder is inside the first: its track is listed once.
            l.folders = vec![dir.join("a"), dir.join("a").join("sub")];
            l.crates = vec![SeratoCrate {
                name: "Gig".into(),
                file: dir.join("Gig.crate"),
                root: dir.clone(),
            }];
        }
        let titles = || {
            for _ in 0..500 {
                if !lib.lock().lists[KEY_ALL].scanning {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let l = lib.lock();
            assert!(!l.lists[KEY_ALL].scanning, "scan didn't finish");
            l.lists[KEY_ALL]
                .tracks
                .iter()
                .map(|t| t.title.clone())
                .collect::<Vec<_>>()
        };
        refresh_all(&lib);
        assert!(!lib.lock().lists.contains_key(KEY_ALL), "only once listed");
        scan(&lib, KEY_ALL.into(), SourceKind::All);
        // Folders first, then Shared with me, then crate tracks not in a folder.
        assert_eq!(titles(), ["Song", "Tune", "Gift", "One"]);

        // Remove the outer folder: its track still comes from the crate.
        lib.lock().folders = vec![dir.join("a").join("sub")];
        refresh_all(&lib);
        assert_eq!(titles(), ["Tune", "Gift", "Song", "One"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
