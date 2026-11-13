//! The session: one booth's view of the room.
//!
//! Owns the event timeline, the decoded tracks, the link to the other DJ,
//! transfers and shares. The UI, MIDI and network threads all talk to it
//! through a [`SessionHandle`] (a mutex). The audio threads never lock it:
//! they read an [`AudioSnapshot`] that is swapped in atomically after every
//! change.

use crate::analysis;
use crate::audio::{decode_file, TrackAudio};
use crate::clock::ClockEstimator;
use crate::config::{sanitize_file_name, unique_path, Paths};
use crate::grid::BeatGrid;
use crate::library::{self, Library};
use crate::model::{Action, DeckState, Event, RoomState, Timeline, TrackId, DECKS};
use crate::net::PeerConn;
use crate::protocol::{Msg, PeerInfo, ShareOffer, TrackInfo};
use crate::render::{AudioSnapshot, LocalParams};
use crate::terms::{self, ConsentRecord, TERMS_VERSION};
use crate::time::SharedClock;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

pub type SessionHandle = Arc<Mutex<Session>>;

#[derive(Clone, Debug, PartialEq)]
pub enum TrackStatus {
    Searching,
    Decoding,
    Fetching,
    Ready,
    Failed(String),
}

struct TrackSlot {
    status: TrackStatus,
    audio: Option<Arc<TrackAudio>>,
    last_used: Instant,
}

#[derive(Clone, Debug)]
pub enum Purpose {
    /// Temporary copy so we can hear a partner's deck.
    Session,
    /// Part of a share we accepted; saved to `dir`.
    Share { share_id: u64, dir: PathBuf },
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferState {
    Queued,
    Active,
    Done,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub id: TrackId,
    pub name: String,
    pub upload: bool,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
    pub updated: Instant,
}

pub enum Role {
    Solo,
    Host {
        port: u16,
        room: String,
        stop: Arc<AtomicBool>,
    },
    Client {
        addr: SocketAddr,
        room: String,
    },
}

pub struct Peer {
    pub conn: PeerConn,
    pub info: PeerInfo,
    pub rtt_us: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ShareState {
    Pending,
    Accepted,
    Declined,
}

pub struct IncomingShare {
    pub offer: ShareOffer,
    pub state: ShareState,
    pub received: usize,
    pub already_had: usize,
    pub failed: usize,
    pub dir: PathBuf,
}

pub struct OutgoingShare {
    pub offer: ShareOffer,
    pub replies: Vec<(String, bool)>,
}

// ---------------------------------------------------------------- views

#[derive(Clone)]
pub struct DeckView {
    pub state: DeckState,
    pub meta: Option<TrackInfo>,
    pub status: Option<TrackStatus>,
    pub audio: Option<Arc<TrackAudio>>,
    pub loading: Option<String>,
}

#[derive(Clone, Debug)]
pub enum RoleView {
    Solo,
    Hosting { port: u16, room: String },
    Joined { addr: String, room: String },
}

#[derive(Clone, Debug)]
pub struct LinkView {
    pub role: RoleView,
    pub peer: Option<PeerInfo>,
    pub rtt_us: Option<i64>,
    pub clock_spread_us: Option<i64>,
    pub in_sync: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ShareView {
    pub share_id: u64,
    pub from: String,
    pub title: String,
    pub tracks: Vec<String>,
    pub bytes: u64,
    pub state: ShareState,
    pub received: usize,
    pub already_had: usize,
    pub failed: usize,
    pub dir: PathBuf,
}

#[derive(Clone)]
pub struct SessionView {
    pub now: i64,
    pub me: PeerInfo,
    pub room: RoomState,
    pub decks: [DeckView; DECKS],
    pub local: LocalParams,
    pub link: LinkView,
    pub transfers: Vec<Transfer>,
    pub incoming: Vec<ShareView>,
    pub outgoing: Vec<(String, usize, Vec<(String, bool)>)>,
    pub room_tracks: Vec<TrackInfo>,
    pub preparing_share: Option<String>,
}

impl SessionView {
    pub fn name_of(&self, id: u64) -> Option<String> {
        if id == 0 {
            None
        } else if id == self.me.id {
            Some("you".into())
        } else {
            self.link
                .peer
                .as_ref()
                .filter(|p| p.id == id)
                .map(|p| p.name.clone())
                .or(Some("partner".into()))
        }
    }
}

// ---------------------------------------------------------------- session

pub struct Session {
    pub me: PeerInfo,
    pub paths: Paths,
    clock: Arc<SharedClock>,
    library: Arc<Mutex<Library>>,
    timeline: Timeline,
    seq: u64,
    track_meta: HashMap<TrackId, TrackInfo>,
    tracks: HashMap<TrackId, TrackSlot>,
    loading: [Option<String>; DECKS],
    servable: HashMap<TrackId, PathBuf>,
    fetch_queue: VecDeque<(TrackId, Purpose)>,
    active_fetch: HashMap<TrackId, Purpose>,
    transfers: Vec<Transfer>,
    pub local: LocalParams,
    snapshot: Arc<ArcSwap<AudioSnapshot>>,
    keepalive: VecDeque<Arc<AudioSnapshot>>,
    role: Role,
    peer: Option<Peer>,
    est: ClockEstimator,
    in_sync: Option<bool>,
    digest_mismatch: u32,
    last_digest: Instant,
    incoming: Vec<IncomingShare>,
    outgoing: Vec<OutgoingShare>,
    preparing_share: Option<String>,
    pub max_upload_bytes_s: f64,
    pub bcast_delay_us: i64,
    /// Per deck, as of the last maintenance tick: track, gen and the BPM of
    /// the grid section under the playhead. A change means the deck crossed
    /// into a section with another tempo.
    grid_watch: [(Option<TrackId>, u64, Option<f64>); DECKS],
    /// Relock events already scheduled: (deck, shared time).
    relocks: Vec<(u8, i64)>,
    weak: Weak<Mutex<Session>>,
}

const MAX_CONCURRENT_FETCHES: usize = 2;
/// How far ahead (us) to schedule a relock for a deck about to reach a tempo
/// change. Longer than the maintenance tick so none slips through, short so
/// the event is only briefly in the future.
const RELOCK_HORIZON_US: i64 = 300_000;
const TRANSFER_STALL: Duration = Duration::from_secs(45);

impl Session {
    pub fn create(
        name: String,
        paths: Paths,
        library: Arc<Mutex<Library>>,
        clock: Arc<SharedClock>,
    ) -> SessionHandle {
        let now = clock.now();
        let timeline = Timeline::new(now - 10_000_000);
        let snapshot = Arc::new(ArcSwap::from_pointee(AudioSnapshot::empty()));
        let handle = Arc::new_cyclic(|weak| {
            Mutex::new(Session {
                me: PeerInfo {
                    id: fastrand::u64(1..),
                    name,
                },
                paths,
                clock,
                library,
                timeline,
                seq: 0,
                track_meta: HashMap::new(),
                tracks: HashMap::new(),
                loading: Default::default(),
                servable: HashMap::new(),
                fetch_queue: VecDeque::new(),
                active_fetch: HashMap::new(),
                transfers: Vec::new(),
                local: LocalParams::default(),
                snapshot,
                keepalive: VecDeque::new(),
                role: Role::Solo,
                peer: None,
                est: ClockEstimator::default(),
                in_sync: None,
                digest_mismatch: 0,
                last_digest: Instant::now(),
                incoming: Vec::new(),
                outgoing: Vec::new(),
                preparing_share: None,
                max_upload_bytes_s: 2.0 * 1024.0 * 1024.0,
                bcast_delay_us: 500_000,
                grid_watch: [(None, 0, None); DECKS],
                relocks: Vec::new(),
                weak: weak.clone(),
            })
        });
        handle.lock().publish();
        let weak = Arc::downgrade(&handle);
        std::thread::Builder::new()
            .name("tandem-maintenance".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(200));
                let Some(h) = weak.upgrade() else { break };
                h.lock().maintenance();
            })
            .expect("spawn maintenance thread");
        handle
    }

    // ------------------------------------------------------------ basics

    pub fn clock(&self) -> Arc<SharedClock> {
        self.clock.clone()
    }

    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    pub fn snapshot_handle(&self) -> Arc<ArcSwap<AudioSnapshot>> {
        self.snapshot.clone()
    }

    pub fn library(&self) -> Arc<Mutex<Library>> {
        self.library.clone()
    }

    pub fn latest(&self) -> RoomState {
        self.timeline.latest()
    }

    pub fn state_at(&self, t: i64) -> RoomState {
        self.timeline.state_at(t)
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn is_connected(&self) -> bool {
        self.peer.is_some()
    }

    pub fn track_status(&self, id: TrackId) -> Option<TrackStatus> {
        self.tracks.get(&id).map(|s| s.status.clone())
    }

    pub fn set_name(&mut self, name: String) {
        self.me.name = name;
    }

    fn send(&self, m: Msg) {
        if let Some(p) = &self.peer {
            p.conn.send(m);
        }
    }

    /// Swap a fresh snapshot in for the audio threads.
    pub fn publish(&mut self) {
        let tracks = self
            .tracks
            .values()
            .filter_map(|s| s.audio.clone())
            .collect();
        let snap = Arc::new(AudioSnapshot {
            timeline: self.timeline.clone(),
            tracks,
            local: self.local,
        });
        self.snapshot.store(snap.clone());
        // Old snapshots are dropped here, on this thread, never on an audio thread.
        self.keepalive.push_back(snap);
        while self.keepalive.len() > 64 {
            self.keepalive.pop_front();
        }
    }

    // ------------------------------------------------------------ actions

    pub fn act(&mut self, a: Action) {
        let t = self.now();
        self.act_at(a, t);
    }

    /// Apply a local action at shared time `t` and send it to the partner.
    pub fn act_at(&mut self, a: Action, t: i64) {
        self.seq += 1;
        let ev = Event {
            time: t,
            origin: self.me.id,
            seq: self.seq,
            action: a,
        };
        self.timeline.insert(ev.clone());
        self.after_event(&ev);
        self.send(Msg::Event { ev });
        self.publish();
    }

    fn after_event(&mut self, ev: &Event) {
        match &ev.action {
            Action::Load { track, .. } => self.ensure_track(*track),
            Action::SetGrid { track, grid, .. } => {
                self.remember_grid(*track, grid.as_deref().copied())
            }
            _ => {}
        }
    }

    /// A deck's grid was edited (here or by the partner): use it the next
    /// time this track is loaded, and save it to this booth's library.
    fn remember_grid(&mut self, id: TrackId, grid: Option<BeatGrid>) {
        if let Some(m) = self.track_meta.get_mut(&id) {
            m.grid = grid;
        }
        let path = self.servable.get(&id).cloned();
        library::set_user_grid(&self.library, id, path.as_deref(), grid);
    }

    /// Put a deck's beatgrid back to what analysis found (analysing the
    /// track now if this booth hasn't already).
    pub fn reset_grid(&mut self, deck: usize) {
        let Some(id) = self.timeline.latest().decks.get(deck).and_then(|d| d.track) else {
            return;
        };
        let path = self.servable.get(&id).cloned();
        let audio = self.tracks.get(&id).and_then(|s| s.audio.clone());
        let hint = self.track_meta.get(&id).and_then(|m| m.bpm);
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let grid = path
                .and_then(|p| library::detected_grid(&lib, &p))
                .or_else(|| audio.and_then(|a| analysis::analyze(&a, hint.map(|b| b as f64))));
            let Some(h) = weak.upgrade() else { return };
            h.lock().act(Action::SetGrid {
                deck: deck as u8,
                track: id,
                grid: grid.map(Box::new),
            });
        });
    }

    pub fn set_local(&mut self, local: LocalParams) {
        if local != self.local {
            self.local = local;
            self.publish();
        }
    }

    pub fn toggle_pfl(&mut self, deck: u8) {
        if let Some(p) = self.local.pfl.get_mut(deck as usize) {
            *p = !*p;
            self.publish();
        }
    }

    pub fn set_cue_mix(&mut self, v: f32) {
        self.local.cue_mix = v.clamp(0.0, 1.0);
        self.publish();
    }

    // ------------------------------------------------------------ tracks

    /// Load a file from this computer onto a deck (hash + decode in the background).
    pub fn load_path(&mut self, deck: usize, path: PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        self.loading[deck] = Some(name.clone());
        log::info!("Loading {name} onto deck {}", deck_letter(deck));
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<(TrackInfo, TrackAudio)> {
                let id = library::hash_path(&lib, &path)?;
                let (_, _, bpm, _) = library::read_tags(&path);
                let (title, artist) = library::display_names(&lib, &path);
                let audio = decode_file(&path, id)?;
                let size = std::fs::metadata(&path)?.len();
                let (grid, key) = library::analysis_for(&lib, &path, &audio, bpm);
                let info = TrackInfo {
                    id,
                    title,
                    artist,
                    bpm,
                    duration: audio.duration(),
                    size,
                    file_name: name.clone(),
                    grid,
                    key,
                };
                Ok((info, audio))
            })();
            let Some(h) = weak.upgrade() else { return };
            let mut s = h.lock();
            s.loading[deck] = None;
            match res {
                Ok((info, audio)) => s.finish_local_load(deck, path, info, audio),
                Err(e) => log::error!("Couldn't load {name}: {e:#}"),
            }
        });
    }

    fn finish_local_load(
        &mut self,
        deck: usize,
        path: PathBuf,
        info: TrackInfo,
        audio: TrackAudio,
    ) {
        let id = info.id;
        self.servable.insert(id, path);
        self.tracks.insert(
            id,
            TrackSlot {
                status: TrackStatus::Ready,
                audio: Some(Arc::new(audio)),
                last_used: Instant::now(),
            },
        );
        self.track_meta.insert(id, info.clone());
        self.send(Msg::Track { info: info.clone() });
        self.act(load_action(deck, &info));
    }

    /// Load a track already known in this room (e.g. one your partner played).
    pub fn load_known(&mut self, deck: usize, id: TrackId) {
        let Some(info) = self.track_meta.get(&id).cloned() else {
            return;
        };
        self.act(load_action(deck, &info));
    }

    /// Make sure we have audio for a track a deck refers to: decode it from
    /// our library if we have the same file, otherwise fetch it.
    pub fn ensure_track(&mut self, id: TrackId) {
        if let Some(s) = self.tracks.get_mut(&id) {
            s.last_used = Instant::now();
            if !matches!(s.status, TrackStatus::Failed(_)) {
                return;
            }
        }
        self.tracks.insert(
            id,
            TrackSlot {
                status: TrackStatus::Searching,
                audio: None,
                last_used: Instant::now(),
            },
        );
        let size = self.track_meta.get(&id).map(|m| m.size);
        let known = self.servable.get(&id).cloned();
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let found = known.or_else(|| size.and_then(|sz| library::find_by_hash(&lib, id, sz)));
            let Some(h) = weak.upgrade() else { return };
            match found {
                Some(path) => {
                    h.lock().set_status(id, TrackStatus::Decoding);
                    decode_into(&h, id, &path);
                }
                None => h.lock().on_track_missing(id),
            }
        });
    }

    fn set_status(&mut self, id: TrackId, status: TrackStatus) {
        if let Some(s) = self.tracks.get_mut(&id) {
            s.status = status;
        }
    }

    fn on_track_missing(&mut self, id: TrackId) {
        if self.peer.is_some() {
            self.set_status(id, TrackStatus::Fetching);
            self.request_fetch(id, Purpose::Session);
        } else {
            self.set_status(id, TrackStatus::Failed("not in your library".into()));
        }
    }

    fn on_decoded(&mut self, id: TrackId, res: anyhow::Result<TrackAudio>) {
        let name = self
            .track_meta
            .get(&id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| id.short());
        match res {
            Ok(a) => {
                self.tracks.insert(
                    id,
                    TrackSlot {
                        status: TrackStatus::Ready,
                        audio: Some(Arc::new(a)),
                        last_used: Instant::now(),
                    },
                );
                log::info!("Ready: {name}");
                self.publish();
            }
            Err(e) => {
                log::error!("Couldn't decode {name}: {e:#}");
                self.set_status(id, TrackStatus::Failed(format!("{e:#}")));
            }
        }
    }

    // ------------------------------------------------------------ transfers

    pub fn servable_path(&self, id: TrackId) -> Option<PathBuf> {
        self.servable.get(&id).cloned()
    }

    fn request_fetch(&mut self, id: TrackId, purpose: Purpose) {
        if self.active_fetch.contains_key(&id) || self.fetch_queue.iter().any(|(i, _)| *i == id) {
            return;
        }
        let name = self
            .track_meta
            .get(&id)
            .map(|m| m.file_name.clone())
            .unwrap_or_else(|| id.short());
        let total = self.track_meta.get(&id).map(|m| m.size).unwrap_or(0);
        self.transfer_set(id, false, &name, 0, total, TransferState::Queued);
        match purpose {
            Purpose::Session => self.fetch_queue.push_front((id, purpose)),
            Purpose::Share { .. } => self.fetch_queue.push_back((id, purpose)),
        }
        self.pump_fetches();
    }

    fn pump_fetches(&mut self) {
        while self.active_fetch.len() < MAX_CONCURRENT_FETCHES {
            let Some((id, purpose)) = self.fetch_queue.pop_front() else {
                break;
            };
            if self.peer.is_none() {
                self.finish_fetch(id, purpose, Err("not connected".into()));
                continue;
            }
            let dest = self.dest_for(id, &purpose);
            self.active_fetch.insert(id, purpose);
            self.transfer_state(id, false, TransferState::Active);
            match &self.role {
                Role::Client { addr, room } => {
                    let (addr, room, me, weak) =
                        (*addr, room.clone(), self.me.clone(), self.weak.clone());
                    std::thread::spawn(move || {
                        let Some(h) = weak.upgrade() else { return };
                        let res = crate::files::fetch(&h, addr, &room, &me, id, &dest);
                        h.lock().on_file_received(
                            id,
                            res.map(|_| dest).map_err(|e| crate::files::describe(&e)),
                        );
                    });
                }
                Role::Host { .. } => self.send(Msg::PushRequest { id }),
                Role::Solo => {}
            }
        }
    }

    fn dest_for(&self, id: TrackId, purpose: &Purpose) -> PathBuf {
        let name = self
            .track_meta
            .get(&id)
            .map(|m| m.file_name.clone())
            .unwrap_or_else(|| format!("{}.bin", id.short()));
        let name = sanitize_file_name(&name);
        match purpose {
            Purpose::Session => self.paths.cache_dir.join(format!("{}-{name}", id.short())),
            Purpose::Share { dir, .. } => unique_path(dir.join(name)),
        }
    }

    /// Host side of an upload from the client: where should it go?
    pub fn accept_push(&mut self, id: TrackId) -> Option<PathBuf> {
        let purpose = self.active_fetch.get(&id)?.clone();
        Some(self.dest_for(id, &purpose))
    }

    pub fn on_file_received(&mut self, id: TrackId, res: Result<PathBuf, String>) {
        let Some(purpose) = self.active_fetch.remove(&id) else {
            return;
        };
        self.finish_fetch(id, purpose, res);
        self.pump_fetches();
    }

    fn finish_fetch(&mut self, id: TrackId, purpose: Purpose, res: Result<PathBuf, String>) {
        let name = self
            .track_meta
            .get(&id)
            .map(|m| m.file_name.clone())
            .unwrap_or_else(|| id.short());
        match &res {
            Ok(_) => self.transfer_state(id, false, TransferState::Done),
            Err(e) => {
                log::warn!("Transfer of {name} failed: {e}");
                self.transfer_state(id, false, TransferState::Failed(e.clone()));
            }
        }
        match purpose {
            Purpose::Session => match res {
                Ok(path) => {
                    self.set_status(id, TrackStatus::Decoding);
                    let weak = self.weak.clone();
                    std::thread::spawn(move || {
                        if let Some(h) = weak.upgrade() {
                            decode_into(&h, id, &path);
                        }
                    });
                }
                Err(e) => self.set_status(id, TrackStatus::Failed(e)),
            },
            Purpose::Share { share_id, .. } => {
                if let Ok(path) = &res {
                    self.library.lock().record_hash(path, id);
                }
                if let Some(s) = self
                    .incoming
                    .iter_mut()
                    .find(|s| s.offer.share_id == share_id)
                {
                    if res.is_ok() {
                        s.received += 1;
                    } else {
                        s.failed += 1;
                    }
                    if s.received + s.already_had + s.failed == s.offer.tracks.len() {
                        log::info!(
                            "Share \"{}\" finished: {} new, {} already had, {} failed",
                            s.offer.title,
                            s.received,
                            s.already_had,
                            s.failed
                        );
                        library::scan(
                            &self.library,
                            library::KEY_SHARED.into(),
                            library::SourceKind::Shared,
                        );
                    }
                }
            }
        }
    }

    pub fn transfer_set(
        &mut self,
        id: TrackId,
        upload: bool,
        name: &str,
        done: u64,
        total: u64,
        state: TransferState,
    ) {
        let now = Instant::now();
        match self
            .transfers
            .iter_mut()
            .find(|t| t.id == id && t.upload == upload)
        {
            Some(t) => {
                t.done = done;
                t.total = total;
                t.state = state;
                t.updated = now;
            }
            None => self.transfers.push(Transfer {
                id,
                name: name.to_string(),
                upload,
                done,
                total,
                state,
                updated: now,
            }),
        }
    }

    pub fn transfer_progress(&mut self, id: TrackId, upload: bool, done: u64, total: u64) {
        if let Some(t) = self
            .transfers
            .iter_mut()
            .find(|t| t.id == id && t.upload == upload)
        {
            t.done = done;
            t.total = total;
            t.updated = Instant::now();
        }
    }

    pub fn transfer_state(&mut self, id: TrackId, upload: bool, state: TransferState) {
        if let Some(t) = self
            .transfers
            .iter_mut()
            .find(|t| t.id == id && t.upload == upload)
        {
            t.state = state;
            t.updated = Instant::now();
        }
    }

    pub fn track_file_name(&self, id: TrackId) -> String {
        self.track_meta
            .get(&id)
            .map(|m| m.file_name.clone())
            .unwrap_or_else(|| id.short())
    }

    // ------------------------------------------------------------ shares

    /// Offer a list of files to the room (hashing happens in the background).
    pub fn offer_share(&mut self, title: String, paths: Vec<PathBuf>) {
        if self.peer.is_none() {
            log::warn!("Join or host a room before sharing");
            return;
        }
        self.preparing_share = Some(title.clone());
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let mut tracks = Vec::new();
            let mut files = Vec::new();
            for p in paths {
                let Ok(id) = library::hash_path(&lib, &p) else {
                    continue;
                };
                let Ok(meta) = std::fs::metadata(&p) else {
                    continue;
                };
                let (_, _, bpm, dur) = library::read_tags(&p);
                let (t, a) = library::display_names(&lib, &p);
                let file_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                tracks.push(TrackInfo {
                    id,
                    title: t,
                    artist: a,
                    bpm,
                    duration: dur,
                    size: meta.len(),
                    file_name,
                    grid: library::cached_grid(&lib, &p),
                    key: library::cached_key(&lib, &p),
                });
                files.push((id, p));
            }
            let Some(h) = weak.upgrade() else { return };
            h.lock().send_share(title, tracks, files);
        });
    }

    fn send_share(
        &mut self,
        title: String,
        tracks: Vec<TrackInfo>,
        files: Vec<(TrackId, PathBuf)>,
    ) {
        self.preparing_share = None;
        if tracks.is_empty() {
            log::warn!("Nothing to share in \"{title}\"");
            return;
        }
        let offer = ShareOffer {
            share_id: fastrand::u64(1..),
            from_id: self.me.id,
            from_name: self.me.name.clone(),
            title: title.clone(),
            tracks,
        };
        for (id, p) in files {
            self.servable.insert(id, p);
        }
        for t in &offer.tracks {
            self.track_meta.entry(t.id).or_insert_with(|| t.clone());
        }
        let statement = terms::sender_pledge(offer.tracks.len());
        terms::append_consent(
            &self.paths.consent_log,
            &ConsentRecord {
                unix_time: terms::now_unix(),
                kind: "share-sent",
                terms_version: TERMS_VERSION,
                me: &self.me.name,
                other: self.peer.as_ref().map(|p| p.info.name.as_str()),
                share_title: Some(&title),
                statement: Some(&statement),
                tracks: offer.tracks.iter().map(|t| t.file_name.clone()).collect(),
            },
        );
        log::info!("Offered \"{title}\" ({} tracks)", offer.tracks.len());
        self.send(Msg::Share {
            offer: offer.clone(),
        });
        self.outgoing.push(OutgoingShare {
            offer,
            replies: Vec::new(),
        });
    }

    /// Accept or decline a share someone offered us.
    pub fn respond_share(&mut self, share_id: u64, accept: bool) {
        let me = self.me.name.clone();
        let Some(s) = self
            .incoming
            .iter_mut()
            .find(|s| s.offer.share_id == share_id && s.state == ShareState::Pending)
        else {
            return;
        };
        s.state = if accept {
            ShareState::Accepted
        } else {
            ShareState::Declined
        };
        let from = s.offer.from_name.clone();
        let statement = terms::receiver_pledge(&from);
        terms::append_consent(
            &self.paths.consent_log,
            &ConsentRecord {
                unix_time: terms::now_unix(),
                kind: if accept {
                    "share-accepted"
                } else {
                    "share-declined"
                },
                terms_version: TERMS_VERSION,
                me: &me,
                other: Some(&from),
                share_title: Some(&s.offer.title),
                statement: if accept { Some(&statement) } else { None },
                tracks: s.offer.tracks.iter().map(|t| t.file_name.clone()).collect(),
            },
        );
        let tracks = s.offer.tracks.clone();
        let dir = s.dir.clone();
        log::info!(
            "{} \"{}\" from {from}",
            if accept { "Accepted" } else { "Declined" },
            s.offer.title
        );
        self.send(Msg::ShareReply {
            share_id,
            accepted: accept,
            name: me,
        });
        if !accept {
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::error!("Couldn't create {}: {e}", dir.display());
            return;
        }
        for t in &tracks {
            self.track_meta.entry(t.id).or_insert_with(|| t.clone());
        }
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            for t in tracks {
                let have = library::find_by_hash(&lib, t.id, t.size).is_some();
                let Some(h) = weak.upgrade() else { return };
                let mut s = h.lock();
                if have {
                    if let Some(sh) = s.incoming.iter_mut().find(|x| x.offer.share_id == share_id) {
                        sh.already_had += 1;
                    }
                } else {
                    s.request_fetch(
                        t.id,
                        Purpose::Share {
                            share_id,
                            dir: dir.clone(),
                        },
                    );
                }
            }
        });
    }

    // ------------------------------------------------------------ network callbacks

    pub fn welcome(&self) -> Msg {
        Msg::Welcome {
            host: self.me.clone(),
            timeline: self.timeline.clone(),
            tracks: self.track_meta.values().cloned().collect(),
        }
    }

    pub fn set_hosting(&mut self, port: u16, room: String, stop: Arc<AtomicBool>) {
        self.role = Role::Host { port, room, stop };
        self.clock.set_offset(0);
    }

    pub fn has_peer(&self) -> bool {
        self.peer.is_some()
    }

    pub fn peer_conn(&self) -> Option<&PeerConn> {
        self.peer.as_ref().map(|p| &p.conn)
    }

    /// Host: a DJ joined.
    pub fn add_peer(&mut self, conn: PeerConn, info: PeerInfo) {
        log::info!("{} joined the room", info.name);
        self.peer = Some(Peer {
            conn,
            info,
            rtt_us: None,
        });
        self.in_sync = None;
        self.last_digest = Instant::now();
    }

    /// Client: we joined a host's room.
    pub fn on_joined(
        &mut self,
        conn: PeerConn,
        host: PeerInfo,
        timeline: Timeline,
        tracks: Vec<TrackInfo>,
        est: ClockEstimator,
        addr: SocketAddr,
        room: String,
    ) {
        log::info!("Joined {}'s room", host.name);
        self.role = Role::Client { addr, room };
        self.peer = Some(Peer {
            conn,
            info: host,
            rtt_us: est.last_rtt(),
        });
        self.est = est;
        self.in_sync = None;
        self.timeline = timeline;
        for t in tracks {
            self.track_meta.insert(t.id, t);
        }
        self.ensure_room_tracks();
        self.publish();
    }

    fn ensure_room_tracks(&mut self) {
        let mut ids: Vec<TrackId> = self
            .timeline
            .latest()
            .decks
            .iter()
            .filter_map(|d| d.track)
            .collect();
        for e in self.timeline.events() {
            if let Action::Load { track, .. } = e.action {
                ids.push(track);
            }
        }
        ids.sort();
        ids.dedup();
        for id in ids {
            self.ensure_track(id);
        }
    }

    pub fn on_pong(&mut self, t1: i64, remote: i64, t2: i64) {
        if let Some(best) = self.est.add(t1, remote, t2) {
            self.clock.set_offset(best.offset_us);
        }
        let rtt = self.est.last_rtt();
        if let Some(p) = &mut self.peer {
            p.rtt_us = rtt;
        }
    }

    pub fn set_peer_rtt(&mut self, rtt: i64) {
        if let Some(p) = &mut self.peer {
            p.rtt_us = Some(rtt);
        }
    }

    pub fn last_rtt(&self) -> Option<i64> {
        self.est.last_rtt()
    }

    /// The link to the partner dropped (either side).
    pub fn on_peer_left(&mut self, peer_id: u64) {
        self.drop_peer(peer_id, true);
    }

    fn drop_peer(&mut self, peer_id: u64, announce: bool) {
        let Some(p) = &self.peer else { return };
        if p.info.id != peer_id {
            return;
        }
        if announce {
            log::info!("{} left the room. You can keep playing.", p.info.name);
        }
        self.peer = None;
        self.in_sync = None;
        if matches!(self.role, Role::Client { .. }) {
            self.role = Role::Solo;
        }
        let pending: Vec<(TrackId, Purpose)> = self
            .active_fetch
            .drain()
            .chain(self.fetch_queue.drain(..))
            .collect();
        for (id, purpose) in pending {
            self.finish_fetch(id, purpose, Err("partner left".into()));
        }
        for t in self
            .transfers
            .iter_mut()
            .filter(|t| t.state == TransferState::Active)
        {
            t.state = TransferState::Failed("partner left".into());
        }
    }

    pub fn leave(&mut self) {
        self.send(Msg::Bye);
        if let Some(p) = self.peer.take() {
            p.conn.close();
            let id = p.info.id;
            self.peer = Some(p);
            self.drop_peer(id, false);
        }
        if let Role::Host { stop, .. } = &self.role {
            stop.store(true, Ordering::Relaxed);
        }
        self.role = Role::Solo;
        log::info!("Left the room");
    }

    pub fn on_control_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Event { ev } => {
                if self.timeline.insert(ev.clone()) {
                    self.after_event(&ev);
                    self.publish();
                }
            }
            Msg::Track { info } => {
                self.track_meta.insert(info.id, info);
            }
            Msg::Digest { t, digest } => {
                if self.timeline.state_at(t).digest() == digest {
                    self.in_sync = Some(true);
                    self.digest_mismatch = 0;
                } else {
                    self.in_sync = Some(false);
                    self.digest_mismatch += 1;
                    if self.digest_mismatch >= 2 {
                        log::warn!("Out of step with the host; resyncing");
                        self.send(Msg::ResyncRequest);
                        self.digest_mismatch = 0;
                    }
                }
            }
            Msg::ResyncRequest => self.send(Msg::Resync {
                timeline: self.timeline.clone(),
            }),
            Msg::Resync { timeline } => {
                let mine: Vec<Event> = self
                    .timeline
                    .events()
                    .iter()
                    .filter(|e| e.origin == self.me.id)
                    .cloned()
                    .collect();
                self.timeline = timeline;
                for e in mine {
                    self.timeline.insert(e);
                }
                self.ensure_room_tracks();
                self.publish();
            }
            Msg::Share { offer } => {
                log::info!(
                    "{} wants to share \"{}\" ({} tracks)",
                    offer.from_name,
                    offer.title,
                    offer.tracks.len()
                );
                let dir = self
                    .paths
                    .shared_dir
                    .join(sanitize_file_name(&offer.from_name))
                    .join(sanitize_file_name(&offer.title));
                self.incoming.push(IncomingShare {
                    offer,
                    state: ShareState::Pending,
                    received: 0,
                    already_had: 0,
                    failed: 0,
                    dir,
                });
            }
            Msg::ShareReply {
                share_id,
                accepted,
                name,
            } => {
                if let Some(s) = self
                    .outgoing
                    .iter_mut()
                    .find(|s| s.offer.share_id == share_id)
                {
                    log::info!(
                        "{name} {} \"{}\"",
                        if accepted { "accepted" } else { "declined" },
                        s.offer.title
                    );
                    s.replies.push((name, accepted));
                }
            }
            Msg::PushRequest { id } => self.start_push(id),
            Msg::NoFile { id } => {
                self.on_file_received(id, Err("partner doesn't have that file".into()))
            }
            Msg::Bye => {}
            other => log::debug!("ignoring unexpected control message {other:?}"),
        }
    }

    /// Client: the host asked for one of our files.
    fn start_push(&mut self, id: TrackId) {
        let (Some(path), Role::Client { addr, room }) =
            (self.servable.get(&id).cloned(), &self.role)
        else {
            self.send(Msg::NoFile { id });
            return;
        };
        let (addr, room, me, weak) = (*addr, room.clone(), self.me.clone(), self.weak.clone());
        std::thread::spawn(move || {
            let Some(h) = weak.upgrade() else { return };
            if let Err(e) = crate::files::push(&h, addr, &room, &me, id, &path) {
                log::warn!("Upload failed: {}", crate::files::describe(&e));
                h.lock().transfer_state(
                    id,
                    true,
                    TransferState::Failed(crate::files::describe(&e)),
                );
            }
        });
    }

    // ------------------------------------------------------------ housekeeping

    fn maintenance(&mut self) {
        let now = self.now();
        self.timeline.compact(now - self.bcast_delay_us - 3_000_000);
        self.follow_grid_tempos(now);
        self.library.lock().save_grids();

        if matches!(self.role, Role::Host { .. })
            && self.peer.is_some()
            && self.last_digest.elapsed() > Duration::from_secs(2)
        {
            let t = now - 1_500_000;
            let digest = self.timeline.state_at(t).digest();
            self.send(Msg::Digest { t, digest });
            self.last_digest = Instant::now();
        }

        // Give up on transfers that stopped moving.
        let stalled: Vec<TrackId> = self
            .transfers
            .iter()
            .filter(|t| {
                !t.upload
                    && t.state == TransferState::Active
                    && t.updated.elapsed() > TRANSFER_STALL
            })
            .map(|t| t.id)
            .collect();
        for id in stalled {
            self.on_file_received(id, Err("transfer stalled".into()));
        }
        self.transfers.retain(|t| {
            !matches!(t.state, TransferState::Done) || t.updated.elapsed() < Duration::from_secs(20)
        });

        // Forget decoded tracks nobody has used for a while.
        let in_use: Vec<TrackId> = {
            let mut v: Vec<TrackId> = self
                .timeline
                .latest()
                .decks
                .iter()
                .filter_map(|d| d.track)
                .collect();
            v.extend(
                self.timeline
                    .events()
                    .iter()
                    .filter_map(|e| match e.action {
                        Action::Load { track, .. } => Some(track),
                        _ => None,
                    }),
            );
            v
        };
        let ready = self.tracks.values().filter(|s| s.audio.is_some()).count();
        if ready > 6 {
            let mut old: Vec<(TrackId, Instant)> = self
                .tracks
                .iter()
                .filter(|(id, s)| {
                    s.audio.is_some()
                        && !in_use.contains(id)
                        && s.last_used.elapsed() > Duration::from_secs(60)
                })
                .map(|(id, s)| (*id, s.last_used))
                .collect();
            old.sort_by_key(|(_, t)| *t);
            for (id, _) in old.into_iter().take(ready - 6) {
                self.tracks.remove(&id);
            }
        }
        for id in &in_use {
            if let Some(s) = self.tracks.get_mut(id) {
                s.last_used = Instant::now();
            }
        }
        self.publish();
    }

    /// Keep synced decks locked through tempo changes in their grids. When
    /// a playing deck is about to reach a marker where its tempo changes,
    /// schedule a Relock for the exact moment; if one was missed (the deck
    /// sped up, say), relock as soon as we notice. Relocks are harmless
    /// when not needed, so only the host (or a booth on its own) sends them.
    fn follow_grid_tempos(&mut self, now: i64) {
        let room = self.timeline.state_at(now);
        self.relocks.retain(|&(_, t)| t > now - 1_000_000);
        let quiet = matches!(self.role, Role::Client { .. }) && self.peer.is_some();
        for c in 0..DECKS {
            let d = &room.decks[c];
            let watched = (d.track, d.gen, d.bpm_at(now));
            let prev = std::mem::replace(&mut self.grid_watch[c], watched);
            let other = &room.decks[(c + 1) % DECKS];
            if quiet || !(d.synced || other.synced) || !d.playing {
                continue;
            }
            let deck = c as u8;
            if prev.0 == watched.0 && prev.1 == watched.1 && prev.2 != watched.2 {
                self.act_at(Action::Relock { deck }, now);
            }
            if let Some(tc) = d.next_tempo_change(now, RELOCK_HORIZON_US) {
                // A hair after the marker, so the deck is surely past it.
                let at = tc + 1_000;
                if !self
                    .relocks
                    .iter()
                    .any(|&(dd, t)| dd == deck && (t - at).abs() < 1_000)
                {
                    self.relocks.push((deck, at));
                    self.act_at(Action::Relock { deck }, at);
                }
            }
        }
    }

    // ------------------------------------------------------------ UI view

    pub fn view(&self) -> SessionView {
        let now = self.now();
        let room = self.timeline.latest();
        let decks = std::array::from_fn(|d| {
            let state = room.decks[d];
            let slot = state.track.and_then(|id| self.tracks.get(&id));
            DeckView {
                state,
                meta: state.track.and_then(|id| self.track_meta.get(&id).cloned()),
                status: slot.map(|s| s.status.clone()),
                audio: slot.and_then(|s| s.audio.clone()),
                loading: self.loading[d].clone(),
            }
        });
        let role = match &self.role {
            Role::Solo => RoleView::Solo,
            Role::Host { port, room, .. } => RoleView::Hosting {
                port: *port,
                room: room.clone(),
            },
            Role::Client { addr, room } => RoleView::Joined {
                addr: addr.to_string(),
                room: room.clone(),
            },
        };
        let mut room_tracks: Vec<TrackInfo> = self.track_meta.values().cloned().collect();
        room_tracks.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        SessionView {
            now,
            me: self.me.clone(),
            room,
            decks,
            local: self.local,
            link: LinkView {
                role,
                peer: self.peer.as_ref().map(|p| p.info.clone()),
                rtt_us: self.peer.as_ref().and_then(|p| p.rtt_us),
                clock_spread_us: self.est.spread(),
                in_sync: self.in_sync,
            },
            transfers: self.transfers.clone(),
            incoming: self
                .incoming
                .iter()
                .map(|s| ShareView {
                    share_id: s.offer.share_id,
                    from: s.offer.from_name.clone(),
                    title: s.offer.title.clone(),
                    tracks: s
                        .offer
                        .tracks
                        .iter()
                        .map(|t| {
                            if t.artist.is_empty() {
                                t.title.clone()
                            } else {
                                format!("{} – {}", t.artist, t.title)
                            }
                        })
                        .collect(),
                    bytes: s.offer.total_bytes(),
                    state: s.state.clone(),
                    received: s.received,
                    already_had: s.already_had,
                    failed: s.failed,
                    dir: s.dir.clone(),
                })
                .collect(),
            outgoing: self
                .outgoing
                .iter()
                .map(|s| {
                    (
                        s.offer.title.clone(),
                        s.offer.tracks.len(),
                        s.replies.clone(),
                    )
                })
                .collect(),
            room_tracks,
            preparing_share: self.preparing_share.clone(),
        }
    }
}

fn decode_into(h: &SessionHandle, id: TrackId, path: &Path) {
    let res = decode_file(path, id);
    h.lock().on_decoded(id, res);
}

fn load_action(deck: usize, info: &TrackInfo) -> Action {
    Action::Load {
        deck: deck as u8,
        track: info.id,
        duration: info.duration,
        cue: 0.0,
        grid: info.grid.map(Box::new),
        key: info.key.unwrap_or(-1),
    }
}

pub fn deck_letter(d: usize) -> char {
    (b'A' + d as u8) as char
}
