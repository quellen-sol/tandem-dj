//! Music library: folders, the DJ's own crates, Serato crates, the "Shared
//! with me" folder, and "All Music" (all of them together).
//!
//! Tags and content hashes are cached on disk (keyed by path, size and
//! modified time), so rescans are fast and a track is only hashed once.
//!
//! Beatgrids a DJ has edited are kept separately (`beatgrids.json`), keyed by
//! content hash, so they're never overwritten by analysis and follow the
//! track wherever it's loaded from. Crates the DJ makes in Tandem are kept
//! in `crates.json` beside them.

use crate::analysis::{self, BeatGrid};
use crate::audio::TrackAudio;
use crate::config::{LibrarySort, SortColumn};
use crate::model::TrackId;
use crate::protocol::TrackInfo;
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
    /// One of the DJ's own crates, by id.
    Mine(u64),
    Shared,
    /// Every library folder, "Shared with me", every crate of the DJ's and
    /// every Serato crate, with each file listed once.
    All,
}

/// A crate the DJ made in Tandem: a named list of tracks on this computer,
/// in the order they were added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MyCrate {
    /// Never changes, so a rename keeps everything else about it.
    pub id: u64,
    pub name: String,
    pub tracks: Vec<PathBuf>,
}

/// What a [`resync`] found, once every list it rescanned is done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Resynced {
    /// Tracks in none of those lists before that are in one now.
    pub added: usize,
    /// Tracks that were in one of them and are in none now (deleted, moved
    /// away, or taken out of a Serato crate).
    pub removed: usize,
}

/// A [`resync`] under way.
struct Resyncing {
    /// The lists it's waiting for.
    waiting: HashSet<String>,
    /// Every list it rescans.
    keys: Vec<String>,
    /// The files those lists had before.
    before: HashSet<String>,
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
    /// The DJ's own crates, in the order they were made.
    pub my_crates: Vec<MyCrate>,
    crates_path: PathBuf,
    resyncing: Option<Resyncing>,
    /// The last resync, once it finished, for the UI to report.
    pub resynced: Option<Resynced>,
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

/// The list key of one of the DJ's own crates.
pub fn my_crate_key(id: u64) -> String {
    format!("mine:{id}")
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
        let crates_path = cache_path.with_file_name("crates.json");
        let my_crates = match std::fs::read(&crates_path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                log::error!("Couldn't read {}: {e}", crates_path.display());
                Vec::new()
            }),
            Err(_) => Vec::new(),
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
            my_crates,
            crates_path,
            resyncing: None,
            resynced: None,
        }
    }

    /// Write the DJ's crates to disk (after every change: it's small).
    fn save_crates(&self) {
        let res = serde_json::to_vec_pretty(&self.my_crates)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                let tmp = self.crates_path.with_extension("tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, &self.crates_path)
            });
        if let Err(e) = res {
            log::error!("Couldn't save your crates: {e}");
        }
    }

    pub fn my_crate(&self, id: u64) -> Option<&MyCrate> {
        self.my_crates.iter().find(|c| c.id == id)
    }

    /// Every file the lists with these keys hold.
    fn files_in(&self, keys: &[String]) -> HashSet<String> {
        keys.iter()
            .filter_map(|k| self.lists.get(k))
            .flat_map(|l| l.tracks.iter().map(|t| same_file(&t.path)))
            .collect()
    }

    /// A list finished scanning: if a resync was waiting for it and it was
    /// the last, work out what changed.
    fn resync_scanned(&mut self, key: &str) {
        let Some(r) = self.resyncing.as_mut() else {
            return;
        };
        r.waiting.remove(key);
        if !r.waiting.is_empty() {
            return;
        }
        let r = self.resyncing.take().expect("checked above");
        let after = self.files_in(&r.keys);
        let done = Resynced {
            added: after.difference(&r.before).count(),
            removed: r.before.difference(&after).count(),
        };
        log::info!(
            "Resynced the library: {} new tracks, {} gone",
            done.added,
            done.removed
        );
        self.resynced = Some(done);
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
    // Asked again meanwhile: it goes round again, still scanning, so the
    // list never looks finished with what it held before.
    std::thread::spawn(move || loop {
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
        entry.error = error;
        let again = std::mem::take(&mut entry.rescan);
        entry.scanning = again;
        if !again {
            l.resync_scanned(&key);
        }
        l.save_cache();
        if !again {
            break;
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

/// Look again at everything on disk: every folder, Shared with me, which
/// Serato crates there are and what's in them, and the DJ's crates, then
/// All Music. Lists stay on screen while they're rescanned. When it's done,
/// `resynced` says how many tracks came and went.
pub fn resync(lib: &Arc<Mutex<Library>>, music_dir: Option<&Path>) {
    let mut jobs: Vec<(String, SourceKind)> = Vec::new();
    {
        let mut l = lib.lock();
        l.refresh_crates(music_dir);
        for f in &l.folders {
            jobs.push((folder_key(f), SourceKind::Folder(f.clone())));
        }
        jobs.push((KEY_SHARED.into(), SourceKind::Shared));
        // Crates only if they've been listed: they're read when picked.
        for c in &l.crates {
            if l.lists.contains_key(&crate_key(c)) {
                jobs.push((crate_key(c), SourceKind::Crate(c.clone())));
            }
        }
        for c in &l.my_crates {
            if l.lists.contains_key(&my_crate_key(c.id)) {
                jobs.push((my_crate_key(c.id), SourceKind::Mine(c.id)));
            }
        }
        let keys: Vec<String> = jobs.iter().map(|(k, _)| k.clone()).collect();
        let before = l.files_in(&keys);
        l.resynced = None;
        l.resyncing = Some(Resyncing {
            waiting: keys.iter().cloned().collect(),
            keys,
            before,
        });
    }
    log::info!("Resyncing the library ({} lists)", jobs.len());
    for (key, kind) in jobs {
        scan(lib, key, kind);
    }
    refresh_all(lib);
}

/// Is a [`resync`] still going?
pub fn resyncing(lib: &Arc<Mutex<Library>>) -> bool {
    lib.lock().resyncing.is_some()
}

/// Make a new, empty crate. Returns its id.
pub fn new_crate(lib: &Arc<Mutex<Library>>, name: &str) -> u64 {
    let mut l = lib.lock();
    let mut id = fastrand::u64(1..);
    while l.my_crate(id).is_some() {
        id = fastrand::u64(1..);
    }
    l.my_crates.push(MyCrate {
        id,
        name: crate_name(name),
        tracks: Vec::new(),
    });
    l.save_crates();
    l.lists.insert(my_crate_key(id), TrackList::default());
    id
}

/// A crate's name as typed, tidied: one line, not empty, not too long.
fn crate_name(name: &str) -> String {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name: String = name.chars().take(60).collect();
    if name.is_empty() {
        "New crate".into()
    } else {
        name
    }
}

pub fn rename_crate(lib: &Arc<Mutex<Library>>, id: u64, name: &str) {
    let mut l = lib.lock();
    if let Some(c) = l.my_crates.iter_mut().find(|c| c.id == id) {
        c.name = crate_name(name);
        l.save_crates();
    }
}

/// Delete a crate (its files stay where they are).
pub fn delete_crate(lib: &Arc<Mutex<Library>>, id: u64) {
    {
        let mut l = lib.lock();
        l.my_crates.retain(|c| c.id != id);
        l.lists.remove(&my_crate_key(id));
        l.save_crates();
    }
    refresh_all(lib);
}

/// Add tracks to the end of a crate, leaving out any it already has.
/// Returns how many went in. The new ones are hashed in the background,
/// so sharing the crate later doesn't wait for it.
pub fn add_to_crate(lib: &Arc<Mutex<Library>>, id: u64, paths: &[PathBuf]) -> usize {
    let added: Vec<PathBuf> = {
        let mut l = lib.lock();
        let Some(c) = l.my_crates.iter_mut().find(|c| c.id == id) else {
            return 0;
        };
        let mut have: HashSet<String> = c.tracks.iter().map(|p| same_file(p)).collect();
        let added: Vec<PathBuf> = paths
            .iter()
            .filter(|p| have.insert(same_file(p)))
            .cloned()
            .collect();
        c.tracks.extend(added.iter().cloned());
        if !added.is_empty() {
            l.save_crates();
        }
        added
    };
    if added.is_empty() {
        return 0;
    }
    scan(lib, my_crate_key(id), SourceKind::Mine(id));
    refresh_all(lib);
    let n = added.len();
    let lib = lib.clone();
    std::thread::spawn(move || {
        for p in added {
            let _ = hash_path(&lib, &p);
        }
    });
    n
}

/// Take tracks out of a crate (the files stay where they are).
pub fn remove_from_crate(lib: &Arc<Mutex<Library>>, id: u64, paths: &[PathBuf]) {
    {
        let mut l = lib.lock();
        let Some(c) = l.my_crates.iter_mut().find(|c| c.id == id) else {
            return;
        };
        let gone: HashSet<String> = paths.iter().map(|p| same_file(p)).collect();
        let before = c.tracks.len();
        c.tracks.retain(|p| !gone.contains(&same_file(p)));
        if c.tracks.len() == before {
            return;
        }
        l.save_crates();
    }
    scan(lib, my_crate_key(id), SourceKind::Mine(id));
    refresh_all(lib);
}

/// What the room is told about a library track (shared live in a crate):
/// its content hash, worked out now if it isn't known yet, with the names
/// the library shows and the grid and key it has. `None` if the file can't
/// be read.
pub fn room_info(lib: &Arc<Mutex<Library>>, t: &LibTrack) -> Option<TrackInfo> {
    let id = hash_path(lib, &t.path).ok()?;
    Some(TrackInfo {
        id,
        title: t.title.clone(),
        artist: t.artist.clone(),
        bpm: t.bpm,
        duration: t.duration,
        size: t.size,
        file_name: t
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        grid: cached_grid(lib, &t.path),
        key: t.key.or_else(|| cached_key(lib, &t.path)),
    })
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
        SourceKind::Mine(id) => {
            let paths = lib
                .lock()
                .my_crate(*id)
                .map(|c| c.tracks.clone())
                .unwrap_or_default();
            let (here, gone): (Vec<PathBuf>, Vec<PathBuf>) =
                paths.into_iter().partition(|p| p.exists());
            let error = match gone.len() {
                0 => None,
                1 => Some("1 track in this crate is missing: moved or deleted?".to_string()),
                n => Some(format!(
                    "{n} tracks in this crate are missing: moved or deleted?"
                )),
            };
            (vec![(None, here)], error)
        }
        // Each part is grouped as its own list is, so names come out the
        // same here as there. Folders come first, so a track that's also in
        // a crate is listed from its folder.
        SourceKind::All => {
            let (folders, shared, crates, mine) = {
                let l = lib.lock();
                let mine: Vec<PathBuf> = l
                    .my_crates
                    .iter()
                    .flat_map(|c| c.tracks.iter().cloned())
                    .collect();
                (
                    l.folders.clone(),
                    l.shared_dir.clone(),
                    l.crates.clone(),
                    mine,
                )
            };
            let mut groups: Vec<FileGroup> = folders
                .into_iter()
                .map(|dir| {
                    let files = walk(&dir);
                    (Some(dir), files)
                })
                .collect();
            groups.push((None, walk(&shared)));
            groups.push((None, mine.into_iter().filter(|p| p.exists()).collect()));
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

/// A path as compared to list each file once: Windows paths ignore case,
/// and take either slash.
fn same_file(path: &Path) -> String {
    let s = path.to_string_lossy();
    if cfg!(windows) {
        s.to_lowercase().replace('/', "\\")
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

/// The DJ's own files of a track, best first, to keep in a crate. Unlike
/// [`find_by_hash`], which any copy will do for, this never returns one in
/// `temp` (the session copies, gone when Tandem closes), and gives every
/// other file with exactly this content: those in folders and crates first,
/// then Shared with me, then ones no longer in the library. `loaded` is the
/// file this booth plays it from: if it's the DJ's own it counts whatever
/// became of it since (retagged, or on a drive that's gone), as it would
/// have dragged from the library. Reads files, so not for the UI thread.
pub fn kept_files(
    lib: &Arc<Mutex<Library>>,
    id: TrackId,
    size: Option<u64>,
    loaded: Option<PathBuf>,
    temp: &Path,
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = {
        let l = lib.lock();
        let known = (l.cache.iter())
            .filter(|(_, e)| e.id == Some(id))
            .map(|(p, _)| PathBuf::from(p));
        let same_size = size.and_then(|s| l.size_index.get(&s));
        known
            .chain(same_size.into_iter().flatten().cloned())
            .collect()
    };
    files.sort();
    files.retain(|p| {
        stat(p).is_some_and(|(s, _)| size.is_none_or(|size| s == size))
            && hash_path(lib, p).ok() == Some(id)
    });
    files.extend(loaded.clone());
    files.retain(|p| !is_in(p, temp));
    let l = lib.lock();
    files.sort_by_key(|p| {
        let listed = || {
            l.lists
                .values()
                .any(|li| li.tracks.iter().any(|t| t.path == *p))
        };
        let rank = if p.starts_with(&l.shared_dir) {
            1
        } else if l.folders.iter().any(|f| p.starts_with(f))
            || l.my_crates.iter().any(|c| c.tracks.contains(p))
            || listed()
        {
            0
        } else {
            2
        };
        (rank, loaded.as_ref() != Some(p))
    });
    let mut seen = HashSet::new();
    files.retain(|p| seen.insert(same_file(p)));
    files
}

/// Is this file somewhere in `dir`, whichever way its path is written?
fn is_in(path: &Path, dir: &Path) -> bool {
    let real = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    path.starts_with(dir) || real(path).starts_with(real(dir))
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

    /// A track's files to keep: never the session copy, even when a music
    /// folder holds the session cache; the DJ's folders before Shared with
    /// me, and that before a file that's in none of their lists.
    #[test]
    fn kept_files_are_the_djs_own() {
        let dir = std::env::temp_dir().join(format!("tandem-kept-test-{}", fastrand::u64(..)));
        let file = |f: &str, hz: f64| {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            let signal = crate::audio::test_signal(44_100, 0.2, hz);
            crate::audio::write_wav(&p, 44_100, &signal).unwrap();
            p
        };
        let temp = dir.join("data/session-cache");
        let copy = file("data/session-cache/abc-B - Song.wav", 330.0);
        let id = hash_file(&copy).unwrap();
        let size = std::fs::metadata(&copy).unwrap().len();
        let lib = Arc::new(Mutex::new(Library::new(
            dir.join("shared"),
            dir.join("cache.json"),
        )));
        let kept =
            |loaded: Option<&PathBuf>| kept_files(&lib, id, Some(size), loaded.cloned(), &temp);
        assert!(kept(Some(&copy)).is_empty(), "nothing but the session copy");

        // The session cache inside a music folder: found there, not kept.
        let scan_dir = |d: &Path| {
            lib.lock().folders.push(d.to_path_buf());
            scan(&lib, folder_key(d), SourceKind::Folder(d.to_path_buf()));
            titles(&lib, &folder_key(d));
        };
        scan_dir(&dir.join("data"));
        assert_eq!(find_by_hash(&lib, id, size), Some(copy.clone()));
        assert!(kept(Some(&copy)).is_empty(), "the session copy was kept");
        // However its path is written.
        let odd = dir.join("data/../data/session-cache/abc-B - Song.wav");
        assert!(kept(Some(&odd)).is_empty(), "the session copy was kept");

        // A file Tandem has played that's in none of the DJ's lists, then
        // the same song in Shared with me, then in a folder of theirs: the
        // last one listed wasn't hashed until it's looked for.
        let loose = file("loose/B - Song.wav", 330.0);
        hash_path(&lib, &loose).unwrap();
        assert_eq!(kept(Some(&copy)), std::slice::from_ref(&loose));
        let gift = file("shared/Bob/Set/B - Song.wav", 330.0);
        scan(&lib, KEY_SHARED.into(), SourceKind::Shared);
        titles(&lib, KEY_SHARED);
        assert_eq!(kept(Some(&copy)), [gift.clone(), loose.clone()]);
        let mine = file("music/B - Song.wav", 330.0);
        file("music/C - Other.wav", 440.0);
        scan_dir(&dir.join("music"));
        let all = [mine.clone(), gift.clone(), loose.clone()];
        assert_eq!(kept(Some(&copy)), all);
        assert_eq!(kept(None), all);
        // Of two as good, the one on the deck.
        let twin = file("music/sub/B - Song (2).wav", 330.0);
        scan(
            &lib,
            folder_key(&dir.join("music")),
            SourceKind::Folder(dir.join("music")),
        );
        titles(&lib, &folder_key(&dir.join("music")));
        assert_eq!(kept(Some(&twin))[0], twin);
        assert_eq!(kept(Some(&mine))[0], mine);

        // Their file on the deck, retagged or gone since it was loaded, is
        // still theirs.
        for p in [&twin, &gift, &loose] {
            std::fs::remove_file(p).unwrap();
        }
        std::fs::write(&mine, b"retagged").unwrap();
        assert_eq!(kept(Some(&mine)), std::slice::from_ref(&mine));
        std::fs::remove_file(&mine).unwrap();
        assert_eq!(kept(Some(&mine)), std::slice::from_ref(&mine));
        assert!(kept(Some(&copy)).is_empty());
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

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..1000 {
            if done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    fn titles(lib: &Arc<Mutex<Library>>, key: &str) -> Vec<String> {
        wait_for("the scan", || {
            lib.lock().lists.get(key).is_some_and(|l| !l.scanning)
        });
        lib.lock().lists[key]
            .tracks
            .iter()
            .map(|t| t.title.clone())
            .collect()
    }

    #[test]
    fn crates_keep_their_tracks_in_order_across_restarts() {
        let dir = std::env::temp_dir().join(format!("tandem-crates-test-{}", fastrand::u64(..)));
        let signal = crate::audio::test_signal(44_100, 0.2, 330.0);
        let file = |f: &str| {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            crate::audio::write_wav(&p, 44_100, &signal).unwrap();
            p
        };
        let (a, b, c) = (
            file("music/A - One.wav"),
            file("music/B - Two.wav"),
            file("elsewhere/C - Three.wav"),
        );
        let open = || {
            Arc::new(Mutex::new(Library::new(
                dir.join("shared"),
                dir.join("cache.json"),
            )))
        };
        let lib = open();
        let id = new_crate(&lib, "  Friday \n warm-up ");
        assert_eq!(lib.lock().my_crate(id).unwrap().name, "Friday warm-up");
        assert_eq!(add_to_crate(&lib, id, &[b.clone(), a.clone()]), 2);
        // Already in it: left out, the rest go on the end.
        assert_eq!(add_to_crate(&lib, id, &[a.clone(), c.clone()]), 1);
        let key = my_crate_key(id);
        assert_eq!(titles(&lib, &key), ["Two", "One", "Three"]);
        rename_crate(&lib, id, "Gig");

        // Kept across restarts.
        let lib = open();
        let kept = lib.lock().my_crate(id).cloned().unwrap();
        assert_eq!(kept.name, "Gig");
        assert_eq!(kept.tracks, [b.clone(), a.clone(), c.clone()]);

        // A file that's gone is left out of the list, and the list says so.
        std::fs::remove_file(&a).unwrap();
        scan(&lib, key.clone(), SourceKind::Mine(id));
        assert_eq!(titles(&lib, &key), ["Two", "Three"]);
        let error = lib.lock().lists[&key].error.clone().unwrap();
        assert!(error.contains("1 track"), "{error}");

        // All Music has crate tracks that aren't in a folder.
        lib.lock().folders = vec![dir.join("music")];
        scan(&lib, KEY_ALL.into(), SourceKind::All);
        assert_eq!(titles(&lib, KEY_ALL), ["Two", "Three"]);

        remove_from_crate(&lib, id, std::slice::from_ref(&b));
        assert_eq!(titles(&lib, &key), ["Three"]);
        delete_crate(&lib, id);
        assert!(open().lock().my_crates.is_empty());
        assert!(!lib.lock().lists.contains_key(&key));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resync_finds_new_and_removed_files() {
        let dir = std::env::temp_dir().join(format!("tandem-resync-test-{}", fastrand::u64(..)));
        let signal = crate::audio::test_signal(44_100, 0.2, 330.0);
        let file = |f: &str| {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            crate::audio::write_wav(&p, 44_100, &signal).unwrap();
            p
        };
        let (_, old) = (file("a/A - One.wav"), file("b/B - Two.wav"));
        let lib = Arc::new(Mutex::new(Library::new(
            dir.join("shared"),
            dir.join("cache.json"),
        )));
        let folders = vec![dir.join("a"), dir.join("b")];
        lib.lock().folders = folders.clone();
        for f in &folders {
            scan(&lib, folder_key(f), SourceKind::Folder(f.clone()));
            titles(&lib, &folder_key(f));
        }
        // A song appears in one folder, one goes from the other, and one
        // arrives in a folder within a folder.
        file("a/new/C - Three.wav");
        file("a/D - Four.wav");
        std::fs::remove_file(&old).unwrap();
        resync(&lib, None);
        // Under way, unless the scans have finished already.
        assert!(resyncing(&lib) || lib.lock().resynced.is_some());
        wait_for("the resync", || !resyncing(&lib));
        assert_eq!(
            lib.lock().resynced,
            Some(Resynced {
                added: 2,
                removed: 1
            })
        );
        assert_eq!(
            titles(&lib, &folder_key(&folders[0])),
            ["One", "Four", "Three"]
        );
        assert!(titles(&lib, &folder_key(&folders[1])).is_empty());
        // Nothing changed since: nothing to report.
        resync(&lib, None);
        wait_for("the second resync", || !resyncing(&lib));
        assert_eq!(lib.lock().resynced, Some(Resynced::default()));
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
