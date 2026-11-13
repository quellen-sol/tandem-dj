//! Application state and the per-frame loop. Drawing lives in `ui_*.rs`.

use crate::midi::Midi;
use crate::output::{self, AudioOutputs, DeviceInfo};
use eframe::egui;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_core::config::{LibrarySort, Paths, Settings};
use tandem_core::library::{self, LibTrack, Library, SortRow, SourceKind};
use tandem_core::mapping::MidiOut;
use tandem_core::model::{Action, TrackId, DECKS};
use tandem_core::protocol::TrackInfo;
use tandem_core::render::LocalParams;
use tandem_core::serato::SeratoCrate;
use tandem_core::session::{Session, SessionHandle, SessionView};
use tandem_core::time::SharedClock;

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Room,
    Shared,
    Folder(PathBuf),
    Crate(SeratoCrate),
}

impl Source {
    pub fn key(&self) -> String {
        match self {
            Source::Room => "room".into(),
            Source::Shared => library::KEY_SHARED.into(),
            Source::Folder(p) => library::folder_key(p),
            Source::Crate(c) => library::crate_key(c),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Source::Room => "Tracks in this room".into(),
            Source::Shared => "Shared with me".into(),
            Source::Folder(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string()),
            Source::Crate(c) => format!("Serato crate: {}", c.name),
        }
    }

    pub fn kind(&self) -> Option<SourceKind> {
        match self {
            Source::Room => None,
            Source::Shared => Some(SourceKind::Shared),
            Source::Folder(p) => Some(SourceKind::Folder(p.clone())),
            Source::Crate(c) => Some(SourceKind::Crate(c.clone())),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Item {
    File(PathBuf),
    Known(TrackId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Audio,
    Controller,
    General,
}

/// What the visible rows were last worked out from.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterKey {
    source: String,
    query: String,
    len: usize,
    /// Changes when tracks' details (BPM, key, names) change.
    rev: u64,
    sort: Option<LibrarySort>,
}

/// BPM shown for a library track: analysed (or edited) first, then tags.
pub fn lib_bpm(t: &LibTrack) -> Option<f64> {
    t.grid_bpm.or(t.bpm.map(|b| b as f64))
}

/// BPM shown for a track in this room.
pub fn room_bpm(t: &TrackInfo) -> Option<f64> {
    t.grid
        .map(|g| g.main_bpm(t.duration))
        .or(t.bpm.map(|b| b as f64))
}

fn lib_row(t: &LibTrack) -> SortRow<'_> {
    SortRow {
        title: &t.title,
        artist: &t.artist,
        bpm: lib_bpm(t),
        key: t.key,
        duration: t.duration,
    }
}

fn room_row(t: &TrackInfo) -> SortRow<'_> {
    SortRow {
        title: &t.title,
        artist: &t.artist,
        bpm: room_bpm(t),
        key: t.key,
        duration: t.duration,
    }
}

/// Changes when any room track's BPM or key does (the room list is short).
fn room_rev(tracks: &[TrackInfo]) -> u64 {
    tracks.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, t| {
        let bpm = room_bpm(t).unwrap_or(-1.0).to_bits();
        let key = t.key.unwrap_or(-1) as u64;
        (h ^ bpm ^ key.rotate_left(17)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub struct ShareDraft {
    pub title: String,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub pledge: bool,
}

pub struct UiState {
    pub source: Source,
    pub search: String,
    pub sel_index: usize,
    pub scroll_to_sel: bool,
    pub filtered: Vec<usize>,
    pub filter_key: Option<FilterKey>,
    pub show_settings: bool,
    pub settings_tab: SettingsTab,
    pub show_terms: bool,
    pub host_open: bool,
    pub join_open: bool,
    pub room_code: String,
    pub join_code: String,
    pub joining: bool,
    pub join_result: Arc<Mutex<Option<Result<(), String>>>>,
    pub share_draft: Option<ShareDraft>,
    pub incoming_checked: bool,
    pub held: HashMap<String, bool>,
    pub toast: Option<(String, Instant)>,
    pub name_draft: String,
    pub terms_checked: bool,
    /// Beatgrid editing open on this deck.
    pub grid_edit: [bool; DECKS],
}

/// Developer options: `--screenshot <file.bmp>` saves the window and exits;
/// `--demo` loads the first two library tracks and starts playing (deck B
/// synced, key-matched and on a 4-beat loop).
pub struct DevOpts {
    pub screenshot: Option<PathBuf>,
    pub shot_frame: u64,
    pub demo: bool,
    pub host_port: Option<u16>,
    pub join: Option<String>,
    pub search: Option<String>,
    pub shared: bool,
    pub demo_started: bool,
    pub frame: u64,
}

pub struct TandemApp {
    pub(crate) dev: DevOpts,
    pub(crate) paths: Paths,
    pub(crate) settings: Settings,
    pub(crate) settings_dirty: Option<Instant>,
    pub(crate) session: SessionHandle,
    pub(crate) library: Arc<Mutex<Library>>,
    pub(crate) audio: AudioOutputs,
    pub(crate) midi: Midi,
    pub(crate) devices: Vec<DeviceInfo>,
    pub(crate) midi_ports: Vec<String>,
    pub(crate) ui: UiState,
    /// Actions gathered while drawing, applied at the end of the frame.
    pub(crate) pending: Vec<Action>,
    /// Local (this booth only) settings edited this frame.
    pub(crate) local_edit: LocalParams,
}

impl TandemApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Paths, dev: DevOpts) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let _ = paths.ensure();
        paths.clear_session_cache();
        let settings = Settings::load(&paths.settings_file);

        let library = Arc::new(Mutex::new(Library::new(
            paths.shared_dir.clone(),
            paths.library_cache.clone(),
        )));
        {
            let mut l = library.lock();
            l.folders = settings.library_folders.clone();
            l.refresh_crates(paths.music_dir.as_deref());
        }
        for f in &settings.library_folders {
            library::scan(
                &library,
                library::folder_key(f),
                SourceKind::Folder(f.clone()),
            );
        }
        library::scan(&library, library::KEY_SHARED.into(), SourceKind::Shared);

        let session = Session::create(
            settings.name.clone(),
            paths.clone(),
            library.clone(),
            Arc::new(SharedClock::new()),
        );
        {
            let mut s = session.lock();
            s.max_upload_bytes_s = settings.max_upload_mb_s as f64 * 1024.0 * 1024.0;
            s.bcast_delay_us = settings.audio.broadcast_delay_ms as i64 * 1000;
            s.set_local(settings.local);
        }
        let (snap, clock) = {
            let s = session.lock();
            (s.snapshot_handle(), s.clock())
        };
        let audio = output::start(&settings.audio, snap, clock);

        let mut midi = Midi::new(settings.tempo_range);
        if let Some(port) = settings.midi_device.clone() {
            if let Err(e) = midi.connect(&port, session.clone(), &paths.mappings_dir) {
                log::warn!("MIDI: {e:#}");
                midi.error = Some(format!("{e:#}"));
            }
        }

        let source = settings
            .library_folders
            .first()
            .map(|f| Source::Folder(f.clone()))
            .unwrap_or(Source::Shared);
        let ui = UiState {
            source,
            search: String::new(),
            sel_index: 0,
            scroll_to_sel: false,
            filtered: Vec::new(),
            filter_key: None,
            show_settings: false,
            settings_tab: SettingsTab::Audio,
            show_terms: false,
            host_open: false,
            join_open: false,
            room_code: tandem_core::net::random_room_code(),
            join_code: String::new(),
            joining: false,
            join_result: Arc::new(Mutex::new(None)),
            share_draft: None,
            incoming_checked: false,
            held: HashMap::new(),
            toast: None,
            name_draft: settings.name.clone(),
            terms_checked: false,
            grid_edit: [false; DECKS],
        };
        let local_edit = settings.local;
        Self {
            dev,
            paths,
            settings,
            settings_dirty: None,
            session,
            library,
            audio,
            midi,
            devices: output::list_devices(),
            midi_ports: crate::midi::ports(),
            ui,
            pending: Vec::new(),
            local_edit,
        }
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.settings_dirty.get_or_insert_with(Instant::now);
    }

    pub(crate) fn toast(&mut self, msg: impl Into<String>) {
        self.ui.toast = Some((msg.into(), Instant::now()));
    }

    pub(crate) fn restart_audio(&mut self) {
        self.audio = AudioOutputs::none(); // close devices before reopening them
        let (snap, clock) = {
            let mut s = self.session.lock();
            s.bcast_delay_us = self.settings.audio.broadcast_delay_ms as i64 * 1000;
            (s.snapshot_handle(), s.clock())
        };
        self.audio = output::start(&self.settings.audio, snap, clock);
        self.mark_dirty();
    }

    /// Press/release edges for a button that must be held (CUE, pitch bend).
    pub(crate) fn hold(&mut self, key: String, down: bool) -> (bool, bool) {
        let prev = self.ui.held.insert(key, down).unwrap_or(false);
        (down && !prev, !down && prev)
    }

    pub(crate) fn set_source(&mut self, source: Source) {
        if let Some(kind) = source.kind() {
            let key = source.key();
            let needs_scan =
                !self.library.lock().lists.contains_key(&key) || matches!(source, Source::Crate(_));
            if needs_scan {
                library::scan(&self.library, key, kind);
            }
        }
        self.ui.source = source;
        self.ui.sel_index = 0;
    }

    pub(crate) fn add_folder(&mut self, dir: PathBuf) {
        if !self.settings.library_folders.contains(&dir) {
            self.settings.library_folders.push(dir.clone());
            self.library.lock().folders = self.settings.library_folders.clone();
            self.mark_dirty();
        }
        library::scan(
            &self.library,
            library::folder_key(&dir),
            SourceKind::Folder(dir.clone()),
        );
        self.set_source(Source::Folder(dir));
    }

    pub(crate) fn remove_folder(&mut self, dir: &PathBuf) {
        self.settings.library_folders.retain(|f| f != dir);
        self.library.lock().folders = self.settings.library_folders.clone();
        if self.ui.source == Source::Folder(dir.clone()) {
            self.set_source(Source::Shared);
        }
        self.mark_dirty();
    }

    /// Click on a column heading: sort by it, then the other way round,
    /// then back to the list's own order.
    pub(crate) fn sort_by(&mut self, column: tandem_core::config::SortColumn) {
        self.settings.library_sort = match self.settings.library_sort {
            Some(s) if s.column == column && !s.descending => Some(LibrarySort {
                column,
                descending: true,
            }),
            Some(s) if s.column == column => None,
            _ => Some(LibrarySort {
                column,
                descending: false,
            }),
        };
        self.mark_dirty();
    }

    /// Recompute which rows match the search, in sorted order, when the
    /// list, the query, the sort or the tracks' details change.
    fn refresh_filter(&mut self, view: &SessionView) {
        let q = self.ui.search.trim().to_lowercase();
        let src = self.ui.source.key();
        let sort = self.settings.library_sort;
        let lib = self.library.clone();
        let lib = lib.lock();
        let (len, rev) = match &self.ui.source {
            Source::Room => (view.room_tracks.len(), room_rev(&view.room_tracks)),
            _ => lib
                .lists
                .get(&src)
                .map(|l| (l.tracks.len(), l.rev))
                .unwrap_or((0, 0)),
        };
        let key = FilterKey {
            source: src.clone(),
            query: q.clone(),
            len,
            rev,
            sort,
        };
        if self.ui.filter_key.as_ref() == Some(&key) {
            return;
        }
        // The track under the selection, to keep it selected.
        let selected = self.ui.filtered.get(self.ui.sel_index).copied();
        let matches = |t: &str, a: &str| {
            let (t, a) = (t.to_lowercase(), a.to_lowercase());
            // Words must all match; "|" separates alternatives ("house | garage").
            q.split('|').any(|alt| {
                alt.split_whitespace()
                    .all(|w| t.contains(w) || a.contains(w))
            })
        };
        self.ui.filtered = match &self.ui.source {
            Source::Room => view
                .room_tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| matches(&t.title, &t.artist))
                .map(|(i, _)| i)
                .collect(),
            _ => lib
                .lists
                .get(&src)
                .map(|l| {
                    l.tracks
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| matches(&t.title, &t.artist))
                        .map(|(i, _)| i)
                        .collect()
                })
                .unwrap_or_default(),
        };
        if let Some(sort) = sort {
            match &self.ui.source {
                Source::Room => library::sort_rows(&mut self.ui.filtered, sort, |i| {
                    room_row(&view.room_tracks[i])
                }),
                _ => {
                    if let Some(l) = lib.lists.get(&src) {
                        library::sort_rows(&mut self.ui.filtered, sort, |i| lib_row(&l.tracks[i]));
                    }
                }
            }
        }
        let prev = self.ui.filter_key.replace(key.clone());
        let same_list = prev
            .as_ref()
            .is_some_and(|p| p.source == key.source && p.query == key.query);
        if !same_list {
            self.ui.sel_index = 0;
        } else if let Some(row) =
            selected.and_then(|s| self.ui.filtered.iter().position(|&i| i == s))
        {
            self.ui.sel_index = row;
            // Re-sorted by hand: bring the selected track into view.
            if prev.is_some_and(|p| p.sort != key.sort) {
                self.ui.scroll_to_sel = true;
            }
        }
        if self.ui.sel_index >= self.ui.filtered.len() {
            self.ui.sel_index = self.ui.filtered.len().saturating_sub(1);
        }
    }

    pub(crate) fn item_at(&self, row: usize, view: &SessionView) -> Option<Item> {
        let idx = *self.ui.filtered.get(row)?;
        match &self.ui.source {
            Source::Room => view.room_tracks.get(idx).map(|t| Item::Known(t.id)),
            src => {
                let lib = self.library.lock();
                lib.lists
                    .get(&src.key())
                    .and_then(|l| l.tracks.get(idx))
                    .map(|t| Item::File(t.path.clone()))
            }
        }
    }

    pub(crate) fn load(&mut self, deck: usize, item: Item, _view: &SessionView) {
        // Loading onto a playing deck is fine: the new track starts on the beat.
        let mut s = self.session.lock();
        match item {
            Item::File(p) => s.load_path(deck, p),
            Item::Known(id) => s.load_known(deck, id),
        }
    }

    /// First deck that isn't playing.
    /// The deck to load into on double-click: a stopped one, otherwise the
    /// one that's quieter in the mix.
    pub(crate) fn free_deck(&self, view: &SessionView) -> Option<usize> {
        (0..DECKS)
            .find(|d| !view.decks[*d].state.playing)
            .or_else(|| {
                let gain = |d: usize| {
                    tandem_core::render::fader_gain(view.room.decks[d].volume)
                        * tandem_core::render::xfader_gain(d, view.room.crossfader)
                };
                (0..DECKS).min_by(|a, b| gain(*a).partial_cmp(&gain(*b)).unwrap())
            })
    }

    fn handle_midi_queue(&mut self, view: &SessionView) {
        let outs: Vec<MidiOut> = std::mem::take(&mut *self.midi.ui_queue.lock());
        for o in outs {
            match o {
                MidiOut::Browse(delta) => {
                    let n = self.ui.filtered.len();
                    if n > 0 {
                        self.ui.sel_index = (self.ui.sel_index as i64 + delta as i64)
                            .clamp(0, n as i64 - 1)
                            as usize;
                        self.ui.scroll_to_sel = true;
                    }
                }
                MidiOut::LoadSelected(deck) => {
                    if let Some(item) = self.item_at(self.ui.sel_index, view) {
                        self.load(deck as usize, item, view);
                    }
                }
                MidiOut::Learned(b) => {
                    self.midi.save_mapping(&self.paths.mappings_dir);
                    self.toast(format!(
                        "Mapped {} to {}",
                        b.trigger.label(),
                        b.target.label()
                    ));
                }
                _ => {}
            }
        }
    }

    fn end_frame(&mut self, view: &SessionView) {
        let actions = std::mem::take(&mut self.pending);
        if !actions.is_empty() || self.local_edit != view.local {
            let mut s = self.session.lock();
            for a in actions {
                s.act(a);
            }
            if self.local_edit != view.local {
                s.set_local(self.local_edit);
            }
        }
        if self.local_edit != self.settings.local {
            self.settings.local = self.local_edit;
            self.mark_dirty();
        }
        if let Some(t) = self.settings_dirty {
            if t.elapsed() > Duration::from_millis(800) {
                self.settings.save(&self.paths.settings_file);
                self.settings_dirty = None;
            }
        }
    }
}

impl eframe::App for TandemApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));
        if self.settings.terms_accepted < tandem_core::terms::TERMS_VERSION
            || self.settings.name.trim().is_empty()
        {
            self.terms_gate(ctx);
            return;
        }
        let view = self.session.lock().view();
        self.local_edit = view.local;
        self.handle_midi_queue(&view);
        self.refresh_filter(&view);

        self.top_bar(ctx, &view);
        self.status_bar(ctx);
        self.library_panel(ctx, &view);
        egui::CentralPanel::default().show(ctx, |ui| self.decks_area(ui, &view));
        self.windows(ctx, &view);
        self.end_frame(&view);
        self.dev_hooks(ctx, &view);
    }
}

impl TandemApp {
    fn dev_hooks(&mut self, ctx: &egui::Context, view: &SessionView) {
        self.dev.frame += 1;
        let f = self.dev.frame;
        if f == 2 {
            if let Some(q) = self.dev.search.clone() {
                self.ui.search = q;
            }
        }
        if self.dev.demo {
            if f == 120 {
                for d in 0..DECKS {
                    if let Some(item) = self.item_at(d, view) {
                        self.load(d, item, view);
                    }
                }
            }
            let loaded = view
                .decks
                .iter()
                .all(|d| d.state.track.is_some() && d.audio.is_some());
            if f >= 200 && loaded && !self.dev.demo_started {
                self.dev.demo_started = true;
                let mut s = self.session.lock();
                s.act(Action::Seek { deck: 0, pos: 60.0 });
                s.act(Action::Play { deck: 0 });
                s.act(Action::HotCue { deck: 0, slot: 0 });
                s.act(Action::Seek {
                    deck: 1,
                    pos: 61.37,
                });
                s.act(Action::Sync { deck: 1, on: true });
                s.act(Action::Play { deck: 1 });
                let room = s.latest();
                if let (k, Some(t)) = (room.decks[1].key, room.decks[0].sounding_key()) {
                    if k >= 0 {
                        s.act(Action::KeyShift {
                            deck: 1,
                            semitones: tandem_core::keys::match_shift(k, t),
                        });
                    }
                }
                s.act(Action::Crossfader { v: 0.25 });
                s.act(Action::BeatLoop {
                    deck: 1,
                    beats: 4.0,
                });
                drop(s);
                self.dev.frame = self.dev.shot_frame.saturating_sub(600);
            }
        }
        if let (Some(port), 30) = (self.dev.host_port, f) {
            if let Err(e) = tandem_core::net::host(&self.session, port, "DEVTEST") {
                log::error!("dev host: {e:#}");
            }
        }
        if let (Some(addr), 30) = (self.dev.join.clone(), f) {
            let h = self.session.clone();
            std::thread::spawn(move || {
                if let Err(e) = tandem_core::net::join(&h, &addr, "DEVTEST") {
                    log::error!("dev join: {e:#}");
                }
            });
        }
        if self.dev.demo
            && self.dev.host_port.is_some()
            && !self.dev.shared
            && f > 200
            && view.link.peer.is_some()
        {
            self.dev.shared = true;
            let paths: Vec<PathBuf> = {
                let lib = self.library.lock();
                lib.lists
                    .get(&self.ui.source.key())
                    .map(|l| l.tracks.iter().map(|t| t.path.clone()).collect())
                    .unwrap_or_default()
            };
            self.session
                .lock()
                .offer_share("Friday warm-up".into(), paths);
        }
        if self.dev.demo && self.dev.demo_started && f == self.dev.shot_frame.saturating_sub(40) {
            if let Some(item) = self.item_at(2, view) {
                self.load(0, item, view);
            }
        }
        if self.dev.screenshot.is_some() {
            if f == self.dev.shot_frame {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let (Some(img), Some(path)) = (shot, self.dev.screenshot.clone()) {
                if let Err(e) = write_bmp(&path, img.size, &img.pixels) {
                    log::error!("screenshot: {e}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

fn write_bmp(
    path: &std::path::Path,
    size: [usize; 2],
    px: &[egui::Color32],
) -> std::io::Result<()> {
    use std::io::Write;
    let (w, h) = (size[0] as u32, size[1] as u32);
    let data = w * h * 4;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"BM")?;
    f.write_all(&(54 + data).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(-(h as i32)).to_le_bytes())?; // top-down
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(&[0u8; 24])?;
    for c in px {
        f.write_all(&[c.b(), c.g(), c.r(), 255])?;
    }
    f.flush()
}

impl Drop for TandemApp {
    fn drop(&mut self) {
        self.session.lock().leave();
        self.settings.local = self.local_edit;
        self.settings.save(&self.paths.settings_file);
        self.library.lock().save_cache();
        self.midi.disconnect();
        self.paths.clear_session_cache();
    }
}
