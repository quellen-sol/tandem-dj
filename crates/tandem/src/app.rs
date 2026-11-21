//! Application state and the per-frame loop. Drawing lives in `ui_*.rs`.

use crate::midi::Midi;
use crate::output::{self, AudioOutputs, DeviceInfo};
use crate::ui_decks::LevelKnobs;
use crate::ui_meters::{MeterHistory, MeterView};
use eframe::egui;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_core::config::{FrameRate, LibrarySort, Paths, Settings};
use tandem_core::library::{self, LibTrack, Library, SortRow, SourceKind};
use tandem_core::mapping::{ControllerCard, MidiOut};
use tandem_core::meter::LevelMeters;
use tandem_core::model::{deck_letter, Action, RoomState, TrackId, XfCurve, DECKS};
use tandem_core::protocol::{RoomCrate, TrackInfo, ROOM_CRATE_MAX};
use tandem_core::render::LocalParams;
use tandem_core::serato::SeratoCrate;
use tandem_core::session::{RoleView, Session, SessionHandle, SessionView};
use tandem_core::time::{local_us, SharedClock};

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Every folder, "Shared with me", every crate of yours and every
    /// Serato crate.
    All,
    Room,
    Shared,
    Folder(PathBuf),
    Crate(SeratoCrate),
    /// One of your own crates, by id.
    Mine(u64),
    /// A list another DJ in the room shares live: their id and its.
    Live {
        owner: u64,
        id: u64,
    },
}

impl Source {
    pub fn key(&self) -> String {
        match self {
            Source::All => library::KEY_ALL.into(),
            Source::Room => "room".into(),
            Source::Shared => library::KEY_SHARED.into(),
            Source::Folder(p) => library::folder_key(p),
            Source::Crate(c) => library::crate_key(c),
            Source::Mine(id) => library::my_crate_key(*id),
            Source::Live { owner, id } => format!("live:{owner}:{id}"),
        }
    }

    /// Its name, for lists that know it themselves (see
    /// [`TandemApp::list_label`] for the rest).
    pub fn label(&self) -> String {
        match self {
            Source::All => "All Music".into(),
            Source::Room => "Tracks in this room".into(),
            Source::Shared => "Shared with me".into(),
            Source::Folder(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string()),
            Source::Crate(c) => format!("Serato crate: {}", c.name),
            Source::Mine(_) => "Crate".into(),
            Source::Live { .. } => "Shared live".into(),
        }
    }

    pub fn kind(&self) -> Option<SourceKind> {
        match self {
            Source::All => Some(SourceKind::All),
            Source::Room | Source::Live { .. } => None,
            Source::Shared => Some(SourceKind::Shared),
            Source::Folder(p) => Some(SourceKind::Folder(p.clone())),
            Source::Crate(c) => Some(SourceKind::Crate(c.clone())),
            Source::Mine(id) => Some(SourceKind::Mine(*id)),
        }
    }

    /// Its tracks are files on this computer (not the room's).
    pub fn local(&self) -> bool {
        self.kind().is_some()
    }

    /// Can be shared live with the room: your own lists, not All Music
    /// (too big) or what others shared with you (that's theirs).
    pub fn live_shareable(&self) -> bool {
        matches!(self, Source::Folder(_) | Source::Crate(_) | Source::Mine(_))
    }

    /// The list a room crate is, in the view.
    pub fn room_crate<'a>(&self, view: &'a SessionView) -> Option<&'a RoomCrate> {
        let Source::Live { owner, id } = self else {
            return None;
        };
        view.room_crates
            .iter()
            .find(|c| c.owner == *owner && c.crate_id == *id)
            .map(|c| &**c)
    }

    /// The tracks of a list the room has (this room's, or one shared
    /// live), rather than this computer's library.
    pub fn room_tracks<'a>(&self, view: &'a SessionView) -> Option<&'a [TrackInfo]> {
        match self {
            Source::Room => Some(&view.room_tracks),
            Source::Live { .. } => Some(self.room_crate(view).map_or(&[], |c| &c.tracks)),
            _ => None,
        }
    }
}

/// Which side of the library the controller's browse knob moves through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowseFocus {
    /// The lists on the left: All Music, this room, folders, crates…
    Sources,
    /// The tracks of the list on show.
    #[default]
    Tracks,
}

/// The list `delta` browse-knob ticks away from `current` in `lists`,
/// stopping at either end; `None` if that's where it already is. From a
/// list that isn't there (a crate in a closed section), it goes to the
/// last one.
pub fn step_source<'a>(lists: &'a [Source], current: &Source, delta: i32) -> Option<&'a Source> {
    let key = current.key();
    let at = lists.iter().position(|s| s.key() == key);
    let last = lists.len().checked_sub(1)?;
    let to = match at {
        Some(at) => (at as i64 + delta as i64).clamp(0, last as i64) as usize,
        None => last,
    };
    (Some(to) != at).then(|| &lists[to])
}

/// Lists rescanned every time they're picked: Serato may have changed them.
/// (Your own crates change only in Tandem, and rescan when they do.)
fn rescanned(source: &Source) -> bool {
    matches!(source, Source::Crate(_) | Source::All)
}

#[derive(Clone, Debug)]
pub enum Item {
    File(PathBuf),
    Known(TrackId),
    /// From a list another DJ shares live.
    Live(Box<TrackInfo>),
}

/// What a dragged track row carries: its file, to drop on a crate.
#[derive(Clone, Debug)]
pub struct DragTracks(pub Vec<PathBuf>, pub String);

/// The crate window open: making, renaming or deleting one.
pub struct CrateDialog {
    pub mode: CrateMode,
    pub name: String,
    /// Tracks to put in a new crate.
    pub add: Vec<PathBuf>,
    /// Focus the name box when it opens.
    pub focus: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CrateMode {
    New,
    Rename(u64),
    Delete(u64),
}

/// The smallest the main window goes, in points: a 1080p laptop at 175%
/// display scaling, maximised, is about 1097 × 540, and a 1280 × 800 one at
/// 125% is 1024 wide.
pub const MIN_WINDOW: [f32; 2] = [1024.0, 520.0];

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

/// The row to select after the list was refiltered or resorted, and whether
/// to scroll to it. `selected` is the track that was selected, `rows` the new
/// rows (track indices) and `sel_index` the row selected before.
fn reselect(
    prev: Option<&FilterKey>,
    key: &FilterKey,
    selected: Option<usize>,
    rows: &[usize],
    sel_index: usize,
) -> (usize, bool) {
    let found = selected.and_then(|s| rows.iter().position(|&i| i == s));
    let (row, scroll) = match prev {
        // A new list: its first track.
        None => (0, false),
        Some(p) if p.source != key.source => (0, false),
        // Search cleared: the whole list again, with the track that was
        // selected still selected and in view.
        Some(p) if p.query != key.query && key.query.is_empty() => (found.unwrap_or(0), true),
        // A new search: its first match, at the top.
        Some(p) if p.query != key.query => (0, true),
        // Same rows, re-sorted or with new details: follow the track. Re-sorted
        // by hand: bring it into view.
        Some(p) => match found {
            Some(row) => (row, p.sort != key.sort),
            None => (sel_index, false),
        },
    };
    (row.min(rows.len().saturating_sub(1)), scroll)
}

/// BPM shown for a library track: from analysis (or the DJ's edited grid),
/// never from tags.
pub fn lib_bpm(t: &LibTrack) -> Option<f64> {
    t.grid_bpm
}

/// BPM shown for a track in this room.
pub fn room_bpm(t: &TrackInfo) -> Option<f64> {
    t.grid.map(|g| g.main_bpm(t.duration))
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

/// What the room last got of a list shared live.
pub struct LiveSent {
    /// The list's version (see `TrackList::rev`).
    rev: u64,
    /// Which files it had: tracks coming or going go to the room within a
    /// second; new details alone (BPMs being analysed) every 10 seconds.
    files: u64,
    name: String,
    at: Instant,
}

/// Tells one set of files from another.
fn files_of(tracks: &[LibTrack]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for t in tracks {
        t.path.hash(&mut h);
    }
    h.finish()
}

pub struct ShareDraft {
    pub title: String,
    /// The list's key, to keep a live share up to date.
    pub key: String,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
    pub pledge: bool,
    /// Share it live (the other DJs load from it) rather than send copies.
    pub live: bool,
    /// Why it can't be shared live, if it can't.
    pub live_no: Option<String>,
    /// Why copies can't be sent, if they can't.
    pub copies_no: Option<String>,
}

pub struct UiState {
    pub source: Source,
    pub search: String,
    pub sel_index: usize,
    pub scroll_to_sel: bool,
    pub filtered: Vec<usize>,
    pub filter_key: Option<FilterKey>,
    /// What the browse knob moves through.
    pub browse_focus: BrowseFocus,
    /// Bring the list on show into view on the left (the knob moved).
    pub scroll_to_source: bool,
    /// The knob has shown another list since it went to the lists.
    pub source_browsed: bool,
    /// The Serato crates section was open last frame.
    pub crates_open: bool,
    /// The My crates section was open last frame.
    pub mine_open: bool,
    pub crate_dialog: Option<CrateDialog>,
    /// Lists shared live: what the room last got of each, by list key.
    pub live_sent: HashMap<String, LiveSent>,
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
    /// The broadcast window is open.
    pub broadcast_view: bool,
    /// Decks C and D as last seen (track, playing), to notice when one
    /// gets a track or starts playing.
    pub cd_seen: [(Option<TrackId>, bool); 2],
    /// The DJ has dragged the library's edge: keep their height instead of
    /// following the window's.
    pub library_dragged: bool,
}

/// Developer options: `--screenshot <file.bmp>` saves the window and exits;
/// `--demo` loads the first two library tracks and starts playing (deck B
/// synced, key-matched and on a 4-beat loop); `--demo4` does that on four
/// decks (C synced and playing too, D cued and through the crossfader);
/// `--broadcast-view` opens the broadcast window at start;
/// `--dev-host <port>` / `--dev-join <addr>` host or join room DEVTEST,
/// with `--dev-broadcaster` joining it as a broadcaster;
/// `--dev-late <ms>` shows moves arriving that late, to see the warnings;
/// `--dev-cut <frame>` drops the room link at that frame, as a network
/// failure would (no goodbye), to see the booth get back in;
/// `--dev-roll` leaves deck B's loop and holds a 1-beat loop roll on it
/// for the last 1.5 s before the screenshot;
/// `--window-size <W>x<H>` opens the window at that size (in points, and
/// never maximised to fit the screen); `--zoom <f>` scales the whole UI, as
/// Windows display scaling does (`--window-size 1920x1080 --zoom 1.5` looks
/// like a 1080p laptop at 150%); `--dev-settings <audio|controller|general>`
/// opens Settings on that tab; `--dev-crate` makes a crate "Friday warm-up"
/// of the first tracks on show and, hosting, shares it live once someone
/// joins, or, joining, shows the first list shared live in the room.
pub struct DevOpts {
    pub screenshot: Option<PathBuf>,
    pub shot_frame: u64,
    pub demo: bool,
    pub demo4: bool,
    pub host_port: Option<u16>,
    pub join: Option<String>,
    pub join_broadcaster: bool,
    pub late_ms: Option<i64>,
    pub cut_frame: Option<u64>,
    pub search: Option<String>,
    pub broadcast_view: bool,
    pub roll: bool,
    pub window_size: Option<[f32; 2]>,
    pub zoom: Option<f32>,
    pub settings: Option<String>,
    pub crate_demo: bool,
    pub crate_step: u8,
    /// The window has been checked against the screen (`fit_window`).
    pub fitted: bool,
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
    /// The audio is playing only the broadcast feed (this booth is in a
    /// room as a broadcaster).
    pub(crate) audio_broadcaster: bool,
    pub(crate) midi: Midi,
    pub(crate) devices: Vec<DeviceInfo>,
    pub(crate) midi_ports: Vec<String>,
    pub(crate) ui: UiState,
    /// Actions gathered while drawing, applied at the end of the frame.
    pub(crate) pending: Vec<Action>,
    /// Local (this booth only) settings edited this frame.
    pub(crate) local_edit: LocalParams,
    /// What the controller's master and headphone level knobs do.
    pub(crate) level_knobs: LevelKnobs,
    /// Which decks were cued last frame, to notice a CUE press.
    pfl_seen: [bool; DECKS],
    /// When the next frame is due, at the chosen frame rate.
    next_frame: Instant,
    /// Levels from the live lane, as the mixer shows them.
    pub(crate) meters: Arc<LevelMeters>,
    /// The main window's meters and held peaks.
    pub(crate) meter_view: MeterView,
    /// The broadcast window's: the same levels, a broadcast delay later.
    pub(crate) bcast_meter_view: MeterView,
    meter_history: MeterHistory,
}

impl TandemApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Paths, dev: DevOpts) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        if let Some(z) = dev.zoom {
            cc.egui_ctx.set_zoom_factor(z);
        }
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
            if settings.crossfader_curve != XfCurve::default() {
                s.act(Action::CrossfaderCurve {
                    curve: settings.crossfader_curve,
                });
            }
        }
        let (snap, clock, meters) = {
            let s = session.lock();
            (s.snapshot_handle(), s.clock(), s.meters())
        };
        let audio = output::start(&settings.audio, snap, clock, meters.clone(), false);

        let mut midi = Midi::new(settings.tempo_range, settings.quantize_loops);
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
            browse_focus: BrowseFocus::Tracks,
            scroll_to_source: false,
            source_browsed: false,
            crates_open: true,
            mine_open: true,
            crate_dialog: None,
            live_sent: HashMap::new(),
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
            broadcast_view: dev.broadcast_view,
            cd_seen: [(None, false); 2],
            library_dragged: false,
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
            audio_broadcaster: false,
            midi,
            devices: output::list_devices(),
            midi_ports: crate::midi::ports(),
            ui,
            pending: Vec::new(),
            local_edit,
            level_knobs: LevelKnobs::default(),
            pfl_seen: local_edit.pfl,
            next_frame: Instant::now(),
            meters,
            meter_view: MeterView::default(),
            bcast_meter_view: MeterView::default(),
            meter_history: MeterHistory::default(),
        }
    }

    /// Asks for the next frame at the chosen frame rate. Frames are due on a
    /// fixed beat, so vsync rounding each one up to a monitor refresh doesn't
    /// drag the average rate down. A frame that input brings forward (mouse,
    /// keyboard) doesn't move the beat. The broadcast window is drawn inside
    /// this window's frame, so it runs at the same rate.
    fn pace_frames(&mut self, ctx: &egui::Context) {
        let FrameRate::Cap(fps) = self.settings.frame_rate else {
            ctx.request_repaint();
            return;
        };
        let period = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
        let now = Instant::now();
        if now + period / 4 >= self.next_frame {
            self.next_frame += period;
            if self.next_frame <= now {
                // Behind: a slow frame, or a rate above the monitor's.
                self.next_frame = now + period;
            }
        }
        // egui brings a requested repaint forward by its predicted frame time
        // (1/60 s), which would turn any shorter wait into "right now".
        let ahead = Duration::from_secs_f32(ctx.input(|i| i.predicted_dt).max(0.0));
        ctx.request_repaint_after(self.next_frame.saturating_duration_since(now) + ahead);
    }

    /// Read the level meters for this frame. The broadcast window's are the
    /// ones from a broadcast delay ago, unless this booth is a broadcaster:
    /// its meters already measure the broadcast feed.
    fn take_levels(&mut self) {
        let now = local_us();
        let levels = self.meters.read();
        self.meter_view.update(&levels, now);
        let delay = if self.audio_broadcaster {
            0
        } else {
            self.bcast_delay_ms() as i64 * 1000
        };
        self.meter_history.push(now, levels, delay + 1_000_000);
        if self.ui.broadcast_view {
            let late = self.meter_history.at(now - delay);
            self.bcast_meter_view.update(&late, now);
        }
    }

    /// How far behind the room the broadcast feed and window run: a
    /// broadcaster booth has its own, longer delay.
    pub(crate) fn bcast_delay_ms(&self) -> u32 {
        if self.audio_broadcaster {
            self.settings.audio.broadcaster_delay_ms
        } else {
            self.settings.audio.broadcast_delay_ms
        }
    }

    /// Decks on screen: A and B, and C and D in the four-deck view.
    pub(crate) fn shown_decks(&self) -> usize {
        if self.settings.four_decks {
            DECKS
        } else {
            2
        }
    }

    /// Switch to the four-deck view when deck C or D gets a track or starts
    /// playing (from this booth, the controller or your partner), so
    /// nothing that's happening is off screen.
    fn follow_four_decks(&mut self, room: &RoomState) {
        for (i, d) in room.decks[2..].iter().enumerate() {
            let seen = (d.track, d.playing);
            let prev = std::mem::replace(&mut self.ui.cd_seen[i], seen);
            let news = (seen.0.is_some() && seen.0 != prev.0) || (seen.1 && !prev.1);
            // A broadcaster booth has no decks on screen to switch.
            if news && !self.settings.four_decks && !self.audio_broadcaster {
                self.settings.four_decks = true;
                self.mark_dirty();
                self.toast(format!(
                    "Deck {} is in use: showing four decks.",
                    deck_letter(i + 2)
                ));
            }
        }
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.settings_dirty.get_or_insert_with(Instant::now);
    }

    pub(crate) fn toast(&mut self, msg: impl Into<String>) {
        self.ui.toast = Some((msg.into(), Instant::now()));
    }

    /// A deck was just cued but no output plays the headphones: say where
    /// to set them up instead of staying silent.
    fn warn_unheard_cue(&mut self, view: &SessionView) {
        let pfl = view.local.pfl;
        let cued = pfl
            .iter()
            .zip(self.pfl_seen)
            .any(|(&now, before)| now && !before);
        self.pfl_seen = pfl;
        if cued && !self.audio.phones && !self.audio_broadcaster {
            self.toast("No headphone output: pick one in Settings → Audio → Headphones (cue)");
        }
    }

    pub(crate) fn restart_audio(&mut self) {
        self.audio = AudioOutputs::none(); // close devices before reopening them
        let (snap, clock) = {
            let mut s = self.session.lock();
            s.bcast_delay_us = self.bcast_delay_ms() as i64 * 1000;
            (s.snapshot_handle(), s.clock())
        };
        self.audio = output::start(
            &self.settings.audio,
            snap,
            clock,
            self.meters.clone(),
            self.audio_broadcaster,
        );
        self.mark_dirty();
    }

    /// Tell the controller which outputs play through its own sound card,
    /// as the audio settings and the controller change, and note what its
    /// level knobs do for the mixer to show.
    fn follow_controller_card(&mut self) {
        let card = ControllerCard::find(
            self.midi.port.as_deref(),
            self.audio.master_device.as_deref(),
            self.audio.cue_device.as_deref(),
        );
        let mut e = self.midi.engine.lock();
        e.card = ControllerCard {
            phones_mix: card.phones && self.audio.phones_mixer.is_some(),
            ..card
        };
        self.level_knobs = LevelKnobs::of(&e, self.midi.port.is_some());
    }

    /// Joining as a broadcaster plays only the broadcast feed and opens the
    /// broadcast window; leaving the room (or the host closing it) goes back.
    fn follow_role(&mut self, view: &SessionView) {
        let broadcasting = matches!(view.link.role, RoleView::Broadcasting { .. });
        if broadcasting == self.audio_broadcaster {
            return;
        }
        self.audio_broadcaster = broadcasting;
        self.restart_audio();
        self.meter_history = MeterHistory::default();
        if broadcasting {
            self.ui.broadcast_view = true;
            // The sync click would go out on the stream.
            self.local_edit.click = false;
            self.toast("Broadcasting: share the \"Tandem broadcast\" window with its sound on.");
        } else {
            self.toast("You're no longer broadcasting.");
        }
    }

    /// Press/release edges for a button that must be held (CUE, pitch bend).
    pub(crate) fn hold(&mut self, key: String, down: bool) -> (bool, bool) {
        let prev = self.ui.held.insert(key, down).unwrap_or(false);
        (down && !prev, !down && prev)
    }

    /// Show a list. Crates and All Music are rescanned every time, since
    /// Serato may have changed them.
    pub(crate) fn set_source(&mut self, source: Source) {
        let rescan = rescanned(&source);
        self.show_source(source, rescan);
    }

    /// Show a list from the top, scanning it if it hasn't been yet (or
    /// again, if `rescan`).
    fn show_source(&mut self, source: Source, rescan: bool) {
        if let Some(kind) = source.kind() {
            let key = source.key();
            if rescan || !self.library.lock().lists.contains_key(&key) {
                library::scan(&self.library, key, kind);
            }
        }
        self.ui.source = source;
        self.ui.sel_index = 0;
        self.ui.scroll_to_sel = true;
    }

    /// The lists the browse knob moves through, top to bottom as on screen.
    /// Crates only while their section is open.
    fn knob_sources(&self, view: &SessionView) -> Vec<Source> {
        let mut lists = vec![Source::All, Source::Room, Source::Shared];
        lists.extend(view.room_crates.iter().map(|c| Source::Live {
            owner: c.owner,
            id: c.crate_id,
        }));
        let folders = self.settings.library_folders.iter().cloned();
        lists.extend(folders.map(Source::Folder));
        if self.ui.mine_open {
            let mine = self.library.lock().my_crates.clone();
            lists.extend(mine.into_iter().map(|c| Source::Mine(c.id)));
        }
        if self.ui.crates_open {
            let crates = self.library.lock().crates.clone();
            lists.extend(crates.into_iter().map(Source::Crate));
        }
        lists
    }

    /// The browse knob turned: move through the tracks, or through the
    /// lists, showing each one's tracks on the way. Lists shown this way
    /// aren't rescanned yet; pressing the knob to go into one does that.
    fn browse(&mut self, delta: i32, view: &SessionView) {
        match self.ui.browse_focus {
            BrowseFocus::Tracks => {
                let n = self.ui.filtered.len();
                if n > 0 {
                    self.ui.sel_index =
                        (self.ui.sel_index as i64 + delta as i64).clamp(0, n as i64 - 1) as usize;
                    self.ui.scroll_to_sel = true;
                }
            }
            BrowseFocus::Sources => {
                let lists = self.knob_sources(view);
                if let Some(next) = step_source(&lists, &self.ui.source, delta) {
                    self.show_source(next.clone(), false);
                    self.ui.source_browsed = true;
                }
                self.ui.scroll_to_source = true;
            }
        }
    }

    /// Pressing the browse knob: go into the list on show, or back out to
    /// the lists.
    fn browse_press(&mut self) {
        match self.ui.browse_focus {
            BrowseFocus::Sources => {
                if std::mem::take(&mut self.ui.source_browsed) && rescanned(&self.ui.source) {
                    if let Some(kind) = self.ui.source.kind() {
                        library::scan(&self.library, self.ui.source.key(), kind);
                    }
                }
                self.ui.browse_focus = BrowseFocus::Tracks;
                self.ui.scroll_to_sel = true;
            }
            BrowseFocus::Tracks => self.browse_back(),
        }
    }

    /// BACK: the knob goes to the lists, starting at the one on show.
    fn browse_back(&mut self) {
        if self.ui.browse_focus != BrowseFocus::Sources {
            self.ui.browse_focus = BrowseFocus::Sources;
            self.ui.source_browsed = false;
        }
        self.ui.scroll_to_source = true;
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
            self.set_source(Source::All);
        } else {
            library::refresh_all(&self.library);
        }
        self.mark_dirty();
    }

    /// A list's name as the library shows it.
    pub(crate) fn list_label(&self, src: &Source, view: &SessionView) -> String {
        match src {
            Source::Mine(id) => (self.library.lock().my_crate(*id))
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "Crate".into()),
            Source::Live { .. } => match src.room_crate(view) {
                Some(c) => format!("{}'s {}", c.owner_name, c.name),
                None => "Shared live".into(),
            },
            src => src.label(),
        }
    }

    /// Look again for new, changed and removed songs in every folder,
    /// Shared with me and the crates. Says what it found when it's done.
    pub(crate) fn resync(&mut self) {
        library::resync(&self.library, self.paths.music_dir.as_deref());
        // A Serato crate that's gone can't stay on show.
        if let Source::Crate(c) = &self.ui.source {
            if !self.library.lock().crates.contains(c) {
                self.set_source(Source::All);
            }
        }
    }

    /// Report a finished resync.
    fn report_resync(&mut self) {
        let Some(r) = self.library.lock().resynced.take() else {
            return;
        };
        let n = |k: usize| {
            if k == 1 {
                "1 track".to_string()
            } else {
                format!("{k} tracks")
            }
        };
        self.toast(match (r.added, r.removed) {
            (0, 0) => "Folders resynced: nothing new.".to_string(),
            (a, 0) => format!("Folders resynced: {} new.", n(a)),
            (0, g) => format!("Folders resynced: {} gone.", n(g)),
            (a, g) => format!("Folders resynced: {} new, {} gone.", n(a), n(g)),
        });
    }

    /// Put tracks in one of your crates.
    pub(crate) fn add_to_crate(&mut self, id: u64, paths: Vec<PathBuf>) {
        let added = library::add_to_crate(&self.library, id, &paths);
        let name = (self.library.lock().my_crate(id))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.toast(match (added, paths.len()) {
            (0, 1) => format!("Already in \"{name}\"."),
            (0, _) => format!("They're all in \"{name}\" already."),
            (1, _) => format!("Added to \"{name}\"."),
            (k, _) => format!("Added {k} tracks to \"{name}\"."),
        });
    }

    /// Make a crate, with these tracks in it, and show it.
    pub(crate) fn make_crate(&mut self, name: &str, paths: Vec<PathBuf>) {
        let id = library::new_crate(&self.library, name);
        if !paths.is_empty() {
            library::add_to_crate(&self.library, id, &paths);
        }
        self.set_source(Source::Mine(id));
    }

    pub(crate) fn delete_crate(&mut self, id: u64) {
        let key = library::my_crate_key(id);
        self.session.lock().unshare_live(&key);
        library::delete_crate(&self.library, id);
        if self.ui.source == Source::Mine(id) {
            self.set_source(Source::All);
        }
    }

    /// The tracks of a list on this computer, as listed now.
    fn list_tracks(&self, key: &str) -> Vec<LibTrack> {
        (self.library.lock().lists.get(key))
            .map(|l| l.tracks.clone())
            .unwrap_or_default()
    }

    /// Share a list live with the room (see `Session::share_live`).
    pub(crate) fn share_live(&mut self, src: &Source, view: &SessionView) {
        let key = src.key();
        let name = self.list_label(src, view);
        let (tracks, rev) = {
            let lib = self.library.lock();
            match lib.lists.get(&key) {
                Some(l) => (l.tracks.clone(), l.rev),
                None => (Vec::new(), 0),
            }
        };
        let sent = LiveSent {
            rev,
            files: files_of(&tracks),
            name: name.clone(),
            at: Instant::now(),
        };
        self.ui.live_sent.insert(key.clone(), sent);
        self.session.lock().share_live(key, name, tracks);
    }

    /// Lists shared live follow every change to them (a track added to a
    /// crate, a resync, BPMs analysed): the room gets the new list, at most
    /// about once a second. A crate deleted stops being shared.
    fn follow_live_lists(&mut self, view: &SessionView) {
        self.ui
            .live_sent
            .retain(|k, _| view.live.iter().any(|l| &l.key == k));
        for live in &view.live {
            let src = self.source_of_key(&live.key);
            let Some(src) = src else {
                // The crate or folder is gone.
                self.session.lock().unshare_live(&live.key);
                continue;
            };
            let (rev, scanning) = {
                let lib = self.library.lock();
                match lib.lists.get(&live.key) {
                    Some(l) => (l.rev, l.scanning),
                    None => continue,
                }
            };
            let name = self.list_label(&src, view);
            let sent = self.ui.live_sent.get(&live.key);
            let renamed = sent.is_none_or(|s| s.name != name);
            let waited = sent.map_or(Duration::MAX, |s| s.at.elapsed());
            if scanning
                || (!renamed && sent.is_some_and(|s| s.rev == rev))
                || (!renamed && waited < Duration::from_secs(1))
            {
                continue;
            }
            let files =
                (self.library.lock().lists.get(&live.key)).map_or(0, |l| files_of(&l.tracks));
            let same_files = sent.is_some_and(|s| s.files == files);
            if !renamed && same_files && waited < Duration::from_secs(10) {
                continue;
            }
            let tracks = self.list_tracks(&live.key);
            let sent = LiveSent {
                rev,
                files,
                name: name.clone(),
                at: Instant::now(),
            };
            self.ui.live_sent.insert(live.key.clone(), sent);
            self.session.lock().update_live(&live.key, name, tracks);
        }
    }

    /// The list on the left with this key, if it's still there.
    fn source_of_key(&self, key: &str) -> Option<Source> {
        let lib = self.library.lock();
        let folders = self.settings.library_folders.iter().cloned();
        folders
            .map(Source::Folder)
            .chain(lib.my_crates.iter().map(|c| Source::Mine(c.id)))
            .chain(lib.crates.iter().cloned().map(Source::Crate))
            .find(|s| s.key() == key)
    }

    /// The share window for the list on show: live where it can be, copies
    /// otherwise.
    pub(crate) fn open_share(&mut self, src: &Source, view: &SessionView) {
        let key = src.key();
        let (paths, bytes) = {
            let lib = self.library.lock();
            match (lib.lists.get(&key), src) {
                (Some(l), _) if !l.scanning || !l.tracks.is_empty() => (
                    l.tracks.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                    l.tracks.iter().map(|t| t.size).sum::<u64>(),
                ),
                // A crate not listed yet: what's in it (its size unknown).
                (_, Source::Mine(id)) => (
                    lib.my_crate(*id)
                        .map(|c| c.tracks.clone())
                        .unwrap_or_default(),
                    0,
                ),
                _ => (Vec::new(), 0),
            }
        };
        let in_room = matches!(
            view.link.role,
            RoleView::Hosting { .. } | RoleView::Joined { .. }
        );
        let live_no = if !src.live_shareable() {
            Some("Only your own folders and crates can be shared live.".to_string())
        } else if !in_room {
            Some("Host or join a room first.".to_string())
        } else if paths.len() > ROOM_CRATE_MAX {
            Some(format!(
                "Lists of up to {ROOM_CRATE_MAX} tracks can be shared live: put the ones you want in a crate."
            ))
        } else {
            None
        };
        let copies_no =
            (!view.link.linked()).then(|| "Nobody else is in the room yet.".to_string());
        self.ui.share_draft = Some(ShareDraft {
            title: self.list_label(src, view),
            key,
            paths,
            bytes,
            pledge: false,
            live: live_no.is_none(),
            live_no,
            copies_no,
        });
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
        let room = self.ui.source.room_tracks(view);
        let (len, rev) = match room {
            Some(tracks) => (tracks.len(), room_rev(tracks)),
            None => lib
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
        self.ui.filtered = match room {
            Some(tracks) => tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| matches(&t.title, &t.artist))
                .map(|(i, _)| i)
                .collect(),
            None => lib
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
            match room {
                Some(tracks) => {
                    library::sort_rows(&mut self.ui.filtered, sort, |i| room_row(&tracks[i]))
                }
                None => {
                    if let Some(l) = lib.lists.get(&src) {
                        library::sort_rows(&mut self.ui.filtered, sort, |i| lib_row(&l.tracks[i]));
                    }
                }
            }
        }
        let prev = self.ui.filter_key.replace(key.clone());
        let (row, scroll) = reselect(
            prev.as_ref(),
            &key,
            selected,
            &self.ui.filtered,
            self.ui.sel_index,
        );
        self.ui.sel_index = row;
        self.ui.scroll_to_sel |= scroll;
    }

    pub(crate) fn item_at(&self, row: usize, view: &SessionView) -> Option<Item> {
        let idx = *self.ui.filtered.get(row)?;
        match &self.ui.source {
            Source::Room => view.room_tracks.get(idx).map(|t| Item::Known(t.id)),
            src @ Source::Live { .. } => src
                .room_crate(view)
                .and_then(|c| c.tracks.get(idx))
                .map(|t| Item::Live(Box::new(t.clone()))),
            src => {
                let lib = self.library.lock();
                lib.lists
                    .get(&src.key())
                    .and_then(|l| l.tracks.get(idx))
                    .map(|t| Item::File(t.path.clone()))
            }
        }
    }

    /// Load onto a deck. The track starts paused at its cue. A deck the
    /// room can hear can't be loaded: the session refuses, and its notice
    /// comes back as a toast (see [`RoomState::audible_in_master`]).
    pub(crate) fn load(&mut self, deck: usize, item: Item, _view: &SessionView) {
        let mut s = self.session.lock();
        match item {
            Item::File(p) => s.load_path(deck, p),
            Item::Known(id) => s.load_known(deck, id),
            Item::Live(info) => s.load_room_track(deck, *info),
        }
    }

    /// The deck to load onto on double-click: the first one on screen
    /// that's stopped, otherwise the quietest one that's silent in the
    /// master. If every deck on screen can be heard, says so instead.
    pub(crate) fn free_deck(&self, view: &SessionView) -> Result<usize, String> {
        let shown = self.shown_decks();
        let (room, t) = (&view.room, view.now);
        let gain = |d: usize| tandem_core::render::master_gain(room, d);
        (0..shown)
            .find(|&d| !room.decks[d].sounding_at(t))
            .or_else(|| {
                (0..shown)
                    .filter(|&d| !room.audible_in_master(d, t))
                    .min_by(|&a, &b| gain(a).total_cmp(&gain(b)))
            })
            .ok_or_else(|| {
                let letters: Vec<String> = (0..shown).map(|d| deck_letter(d).to_string()).collect();
                let (last, rest) = letters.split_last().expect("decks on screen");
                format!(
                    "Decks {} and {last} are playing: pause one or pull its fader down to load onto it.",
                    rest.join(", ")
                )
            })
    }

    fn handle_midi_queue(&mut self, view: &SessionView) {
        let outs: Vec<MidiOut> = std::mem::take(&mut *self.midi.ui_queue.lock());
        for o in outs {
            match o {
                MidiOut::Browse(delta) => self.browse(delta, view),
                MidiOut::BrowsePress => self.browse_press(),
                MidiOut::BrowseBack => self.browse_back(),
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
        self.pace_frames(ctx);
        self.fit_window(ctx);
        if self.settings.terms_accepted < tandem_core::terms::TERMS_VERSION
            || self.settings.name.trim().is_empty()
        {
            self.terms_gate(ctx);
            return;
        }
        let (mut view, notices) = {
            let mut s = self.session.lock();
            (s.view(), s.take_notices())
        };
        for n in notices {
            self.toast(n);
        }
        if let (Some(ms), Some(_)) = (self.dev.late_ms, view.link.late_us) {
            view.link.late_us = Some(ms * 1000);
        }
        let view = view;
        self.local_edit = view.local;
        self.warn_unheard_cue(&view);
        self.follow_controller_card();
        self.follow_role(&view);
        self.follow_four_decks(&view.room);
        self.take_levels();
        self.handle_midi_queue(&view);
        self.follow_live_lists(&view);
        self.report_resync();
        self.refresh_filter(&view);

        self.top_bar(ctx, &view);
        self.status_bar(ctx);
        if self.audio_broadcaster {
            // Nothing to play with: the DJs run the room.
            egui::CentralPanel::default().show(ctx, |ui| self.broadcaster_panel(ui, &view));
        } else {
            self.library_panel(ctx, &view);
            egui::CentralPanel::default().show(ctx, |ui| self.decks_area(ui, &view));
        }
        self.windows(ctx, &view);
        self.broadcast_window(ctx);
        self.end_frame(&view);
        self.dev_hooks(ctx, &view);
    }
}

impl TandemApp {
    /// On a screen too small for the window at its usual size, maximise it
    /// once, as soon as the screen's size is known. Windows would otherwise
    /// leave the bottom of the window (the library, and on a short screen
    /// the waveforms too) under the taskbar or off the screen.
    fn fit_window(&mut self, ctx: &egui::Context) {
        if self.dev.fitted || self.dev.window_size.is_some() {
            return;
        }
        let (monitor, outer, maximized) = ctx.input(|i| {
            let v = i.viewport();
            (v.monitor_size, v.outer_rect, v.maximized)
        });
        let (Some(monitor), Some(outer)) = (monitor, outer) else {
            return;
        };
        self.dev.fitted = true;
        log::info!(
            "Window {:.0} × {:.0} at ({:.0}, {:.0}) on a {:.0} × {:.0} screen (points)",
            outer.width(),
            outer.height(),
            outer.left(),
            outer.top(),
            monitor.x,
            monitor.y
        );
        if maximized != Some(true) && !fits_on_screen(outer, monitor) {
            log::info!("The window doesn't fit on the screen: maximising it");
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
    }

    fn dev_hooks(&mut self, ctx: &egui::Context, view: &SessionView) {
        self.dev.frame += 1;
        let f = self.dev.frame;
        if f == 2 {
            if let Some(q) = self.dev.search.clone() {
                self.ui.search = q;
            }
            if let Some(tab) = self.dev.settings.as_deref() {
                self.ui.show_settings = true;
                self.ui.settings_tab = match tab {
                    "controller" => SettingsTab::Controller,
                    "general" => SettingsTab::General,
                    _ => SettingsTab::Audio,
                };
            }
        }
        if self.dev.demo {
            let decks = if self.dev.demo4 { DECKS } else { 2 };
            if f == 120 {
                for d in 0..decks {
                    if let Some(item) = self.item_at(d, view) {
                        self.load(d, item, view);
                    }
                }
            }
            let loaded = view.decks[..decks]
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
                if self.dev.demo4 {
                    s.act(Action::Seek { deck: 2, pos: 30.0 });
                    s.act(Action::Sync { deck: 2, on: true });
                    s.act(Action::Play { deck: 2 });
                    s.act(Action::HotCue { deck: 2, slot: 3 });
                    s.act(Action::Volume { deck: 2, v: 0.6 });
                    s.act(Action::Seek { deck: 3, pos: 15.0 });
                    s.act(Action::Eq {
                        deck: 3,
                        band: 0,
                        v: 0.1,
                    });
                    s.act(Action::XfAssign {
                        deck: 3,
                        side: tandem_core::model::XfSide::Thru,
                    });
                }
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
            let broadcaster = self.dev.join_broadcaster;
            std::thread::spawn(move || {
                if let Err(e) = tandem_core::net::join(&h, &addr, "DEVTEST", broadcaster) {
                    log::error!("dev join: {e:#}");
                }
            });
        }
        if self.dev.cut_frame == Some(f) {
            self.session.lock().cut_link();
        }
        if self.dev.crate_demo {
            self.dev_crate(f, view);
        }
        if self.dev.demo
            && self.dev.host_port.is_some()
            && !self.dev.crate_demo
            && !self.dev.shared
            && f > 200
            && view.link.linked()
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
        if self.dev.demo && self.dev.roll && self.dev.demo_started {
            if f == self.dev.shot_frame.saturating_sub(120) && view.room.decks[1].looping {
                self.session.lock().act(Action::Reloop { deck: 1 });
            }
            if f == self.dev.shot_frame.saturating_sub(90) {
                self.session.lock().act(Action::Roll {
                    deck: 1,
                    beats: 1.0,
                    on: true,
                });
            }
        }
        // Try to load onto deck A while it plays, so the screenshot shows
        // the toast refusing it.
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

impl TandemApp {
    /// `--dev-crate`: a crate to look at, shared live by the host.
    fn dev_crate(&mut self, f: u64, view: &SessionView) {
        const NAME: &str = "Friday warm-up";
        let mine = (self.library.lock().my_crates.iter())
            .find(|c| c.name == NAME)
            .map(|c| c.id);
        match self.dev.crate_step {
            0 if f >= 150 && self.dev.join.is_none() => {
                let id = match mine {
                    Some(id) => {
                        self.set_source(Source::Mine(id));
                        id
                    }
                    None => {
                        let paths: Vec<PathBuf> = (self.library.lock().lists)
                            .get(&self.ui.source.key())
                            .map(|l| l.tracks.iter().take(9).map(|t| t.path.clone()).collect())
                            .unwrap_or_default();
                        self.make_crate(NAME, paths);
                        match &self.ui.source {
                            Source::Mine(id) => *id,
                            _ => return,
                        }
                    }
                };
                self.ui.sel_index = 2;
                self.dev.crate_step = 1;
                log::info!("dev: crate {id} on show");
            }
            1 if self.dev.host_port.is_some() && view.link.linked() && f >= 200 => {
                let src = self.ui.source.clone();
                self.share_live(&src, view);
                self.dev.crate_step = 2;
            }
            0 if self.dev.join.is_some() => {
                if let Some(c) = view.room_crates.first() {
                    self.set_source(Source::Live {
                        owner: c.owner,
                        id: c.crate_id,
                    });
                    self.ui.sel_index = 2;
                    self.dev.crate_step = 2;
                }
            }
            _ => {}
        }
    }
}

/// The height of the Windows taskbar, at the bottom of the screen.
const TASKBAR_H: f32 = 48.0;

/// Whether a window (`outer`: title bar and borders included, in points)
/// fits on a screen of `monitor` size, above the taskbar. Where it is only
/// counts on the main screen, whose top left corner is (0, 0): a window
/// that opens on another screen is checked by size alone.
pub(crate) fn fits_on_screen(outer: egui::Rect, monitor: egui::Vec2) -> bool {
    let room = egui::vec2(monitor.x, monitor.y - TASKBAR_H);
    let on_main = outer.left() >= 0.0
        && outer.top() >= 0.0
        && outer.left() < monitor.x
        && outer.top() < monitor.y;
    let size = if on_main {
        outer.max.to_vec2()
    } else {
        outer.size()
    };
    size.x <= room.x && size.y <= room.y
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

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Rect};
    use tandem_core::config::SortColumn;

    fn key(source: &str, query: &str, sort: Option<LibrarySort>) -> FilterKey {
        FilterKey {
            source: source.into(),
            query: query.into(),
            len: 10,
            rev: 0,
            sort,
        }
    }

    #[test]
    fn clearing_the_search_keeps_the_track_selected() {
        // "daft" matched tracks 7 and 3, and the second was selected.
        let searched = key("all", "daft", None);
        let cleared = key("all", "", None);
        let rows: Vec<usize> = (0..10).collect();
        assert_eq!(
            reselect(Some(&searched), &cleared, Some(3), &rows, 1),
            (3, true)
        );
        // In a sorted list too.
        let rows: Vec<usize> = (0..10).rev().collect();
        assert_eq!(
            reselect(Some(&searched), &cleared, Some(3), &rows, 1),
            (6, true)
        );
        // Nothing matched, so nothing was selected: the top of the list.
        assert_eq!(
            reselect(Some(&searched), &cleared, None, &rows, 0),
            (0, true)
        );
    }

    #[test]
    fn a_new_search_selects_its_first_match() {
        let rows = [4, 8];
        let typed = |from: &str, to: &str, rows: &[usize]| {
            reselect(
                Some(&key("all", from, None)),
                &key("all", to, None),
                Some(8),
                rows,
                5,
            )
        };
        assert_eq!(typed("", "da", &rows), (0, true));
        assert_eq!(typed("da", "daf", &rows), (0, true));
        assert_eq!(typed("da", "dazz", &[]), (0, true));
    }

    #[test]
    fn the_same_search_follows_the_track() {
        let by_bpm = Some(LibrarySort {
            column: SortColumn::Bpm,
            descending: false,
        });
        let rows = [2, 0, 1];
        // Sorted by hand: the track stays selected and comes into view.
        assert_eq!(
            reselect(
                Some(&key("all", "", None)),
                &key("all", "", by_bpm),
                Some(1),
                &rows,
                1
            ),
            (2, true)
        );
        // New details (BPM analysed): the track stays selected where it is.
        let mut analysed = key("all", "", by_bpm);
        analysed.rev = 1;
        let before = key("all", "", by_bpm);
        assert_eq!(
            reselect(Some(&before), &analysed, Some(0), &rows, 0),
            (1, false)
        );
        // The track has gone: the same row, inside the shorter list.
        assert_eq!(
            reselect(Some(&before), &analysed, Some(9), &rows, 5),
            (2, false)
        );
    }

    #[test]
    fn another_list_starts_at_its_first_track() {
        let rows = [0, 1, 2];
        assert_eq!(
            reselect(None, &key("all", "", None), None, &rows, 0),
            (0, false)
        );
        assert_eq!(
            reselect(
                Some(&key("all", "", None)),
                &key("crate", "", None),
                Some(1),
                &rows,
                1
            ),
            (0, false)
        );
    }

    #[test]
    fn the_knob_steps_through_the_lists_and_stops_at_the_ends() {
        let folder = |n: &str| Source::Folder(PathBuf::from(n));
        let lists = vec![
            Source::All,
            Source::Room,
            Source::Shared,
            folder("a"),
            folder("b"),
        ];
        assert_eq!(step_source(&lists, &Source::All, 1), Some(&Source::Room));
        assert_eq!(step_source(&lists, &Source::Room, 2), Some(&folder("a")));
        assert_eq!(step_source(&lists, &folder("a"), -3), Some(&Source::All));
        // A fast turn stops at the ends.
        assert_eq!(step_source(&lists, &Source::Room, 9), Some(&folder("b")));
        assert_eq!(step_source(&lists, &Source::Room, -9), Some(&Source::All));
        // Already at the end: nothing to change.
        assert_eq!(step_source(&lists, &folder("b"), 1), None);
        assert_eq!(step_source(&lists, &Source::All, -1), None);
        // From a list that isn't there (a crate in a closed section).
        assert_eq!(step_source(&lists, &folder("gone"), -1), Some(&folder("b")));
        assert_eq!(step_source(&[], &Source::All, 1), None);
    }

    fn window(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    #[test]
    fn usual_window_fits_big_screens() {
        // 1440 × 930 inside, with the title bar and borders.
        let w = |x, y| window(x, y, 1456.0, 969.0);
        assert!(fits_on_screen(w(100.0, 60.0), vec2(2560.0, 1440.0)));
        // A 1080p screen at 100%: fits near the top, not lower down.
        assert!(fits_on_screen(w(26.0, 26.0), vec2(1920.0, 1080.0)));
        assert!(!fits_on_screen(w(26.0, 100.0), vec2(1920.0, 1080.0)));
        // 1080p at 125% and 150%, and 1366 × 768: never.
        for screen in [
            vec2(1536.0, 864.0),
            vec2(1280.0, 720.0),
            vec2(1366.0, 768.0),
        ] {
            assert!(!fits_on_screen(w(0.0, 0.0), screen), "{screen:?}");
        }
    }

    #[test]
    fn other_screens_count_by_size() {
        // On a screen left of the main one, or below it, only the size counts.
        let screen = vec2(2560.0, 1440.0);
        assert!(fits_on_screen(
            window(-2400.0, 300.0, 1456.0, 969.0),
            screen
        ));
        assert!(fits_on_screen(window(300.0, 1500.0, 1456.0, 969.0), screen));
        assert!(!fits_on_screen(
            window(-2400.0, 300.0, 1456.0, 1400.0),
            screen
        ));
    }
}
