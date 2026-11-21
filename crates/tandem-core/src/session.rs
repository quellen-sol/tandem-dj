//! The session: one booth's view of the room.
//!
//! Owns the event timeline, the decoded tracks, the link to the other DJ
//! (and, on the host, to any broadcasters), transfers and shares. The UI,
//! MIDI and network threads all talk to it through a [`SessionHandle`] (a
//! mutex). The audio threads never lock it: they read an [`AudioSnapshot`]
//! that is swapped in atomically after every change.

use crate::analysis;
use crate::audio::{decode_file, TrackAudio};
use crate::clock::ClockEstimator;
use crate::config::{sanitize_file_name, Paths};
use crate::files::{Landing, Running};
use crate::grid::BeatGrid;
use crate::library::{self, Library};
use crate::meter::LevelMeters;
pub use crate::model::deck_letter;
use crate::model::{Action, DeckState, Event, RoomState, Timeline, TrackId, DECKS};
use crate::net::{PeerConn, RoomAddr};
use crate::protocol::{CutOff, Msg, PeerInfo, ShareOffer, TrackInfo};
use crate::render::{AudioSnapshot, LocalParams};
use crate::terms::{self, ConsentRecord, TERMS_VERSION};
use crate::time::SharedClock;
use arc_swap::ArcSwap;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
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
    /// A download from the host failed: try again at this time if the
    /// track is still on a deck.
    retry_at: Option<Instant>,
}

impl TrackSlot {
    fn new(status: TrackStatus, audio: Option<Arc<TrackAudio>>) -> Self {
        Self {
            status,
            audio,
            last_used: Instant::now(),
            retry_at: None,
        }
    }
}

/// Whether the host can send a track to a booth that asks for it.
pub enum Servable {
    Ready(PathBuf),
    /// Not yet: it's still being found, downloaded or decoded here.
    Pending,
    No,
}

#[derive(Clone, Debug)]
pub enum Purpose {
    /// Temporary copy so we can hear a partner's deck.
    Session,
    /// Part of a share we accepted; saved to `dir`.
    Share { share_id: u64, dir: PathBuf },
}

/// A download this booth has started.
struct Fetch {
    purpose: Purpose,
    /// Tells this try apart from an earlier one of the same track, whose
    /// thread may still be finishing. 0 for one that hasn't started.
    serial: u64,
}

/// Host: an upload from the joining DJ that this booth asked for.
pub struct Expected {
    /// The download it's for (see `Fetch::serial`).
    pub serial: u64,
    /// The track's `.part` file.
    pub part: PathBuf,
    /// Where it goes once it checks out.
    pub landing: Landing,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferState {
    Queued,
    Active,
    /// The link dropped part-way: it carries on from here when the same
    /// booth is back.
    Paused,
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
    /// Where this try started: past 0 when it carried on from where an
    /// earlier one was cut off.
    pub from: u64,
    pub state: TransferState,
    pub updated: Instant,
    /// Tells this try apart from an earlier one of the same transfer (see
    /// [`Session::transfer_ended`]).
    serial: u64,
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
        /// Joined only to broadcast: no controls.
        broadcaster: bool,
    },
}

pub struct Peer {
    pub conn: PeerConn,
    pub info: PeerInfo,
    pub rtt_us: Option<i64>,
    /// When the link last showed it's alive: a Ping arriving (host) or a
    /// Pong (joiner). They come about once a second.
    heard: Instant,
    /// Tells this link apart from an earlier or later one to the same booth.
    serial: u64,
    /// Host: the joining booth's key (see [`Session::seat_for`]).
    key: u64,
}

/// Host: a booth that joined only to broadcast. It gets everything the room
/// does and changes nothing.
struct Broadcaster {
    conn: PeerConn,
    info: PeerInfo,
    /// Tells a rejoin apart from the connection it replaces.
    serial: u64,
    key: u64,
}

/// How a link to another booth ended.
#[derive(Clone, Debug)]
pub enum LinkEnd {
    /// The other end said goodbye: it left, or closed the room.
    Bye,
    /// It broke without one (why, for the log).
    Lost(String),
}

/// Client: the link to the host dropped without anyone leaving, and this
/// booth is getting back in (see `net::rejoin_loop`).
struct Rejoin {
    /// Who this booth was in the room with.
    host: PeerInfo,
    /// When the link dropped.
    since: Instant,
    /// Tries so far.
    attempt: u32,
    /// Why the last try failed.
    error: Option<String>,
    /// Tells this rejoin apart from a later one.
    gen: u64,
}

/// Host: the other DJ's link dropped, and their booth is expected back.
struct Awaiting {
    info: PeerInfo,
    key: u64,
    since: Instant,
    /// When the dropped link was last heard from.
    heard: Instant,
}

/// Client DJ: the room this booth's link to was lost, while it may still
/// get back in (by itself, or joined again by hand). The host holds its
/// place meanwhile, and this booth keeps its moves for it.
struct LostRoom {
    addr: SocketAddr,
    room: String,
    since: Instant,
}

/// Where an event went (see [`Session::insert_event`]).
#[derive(PartialEq)]
enum Landed {
    /// The room already had it.
    Already,
    /// In the room now.
    Now,
    /// Too late for the live timeline, so it's in the held one, and the
    /// live one is made again from that at the next maintenance tick.
    Later,
}

/// Who's in the room.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Roster {
    /// The DJs, host first.
    pub djs: Vec<PeerInfo>,
    pub broadcasters: Vec<PeerInfo>,
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
    Hosting {
        port: u16,
        room: String,
    },
    Joined {
        addr: String,
        room: String,
    },
    /// In a room only to broadcast it.
    Broadcasting {
        addr: String,
        room: String,
    },
}

/// Getting back into a room after the link dropped.
#[derive(Clone, Debug)]
pub struct RejoinView {
    /// The host's name.
    pub host: String,
    /// Tries so far: 0 while waiting for the first.
    pub attempt: u32,
    /// How long since the link dropped.
    pub since: Duration,
    /// Why the last try failed.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LinkView {
    pub role: RoleView,
    /// The other DJ; a broadcaster's link is to the host.
    pub peer: Option<PeerInfo>,
    pub roster: Roster,
    /// How late the other booths' moves have reached this one at most, in
    /// the last [`LATE_WINDOW`]: from when they happened to when they got
    /// here. `None` until one has, or not in a room.
    pub late_us: Option<i64>,
    pub rtt_us: Option<i64>,
    /// How long since the other end was last heard from. Pings run about once
    /// a second, so a few seconds of quiet means the link has stalled and
    /// `rtt_us` is out of date. `None` when not linked.
    pub quiet_for: Option<Duration>,
    pub clock_spread_us: Option<i64>,
    pub in_sync: Option<bool>,
    /// The link dropped and this booth is getting back in. `role` is still
    /// the room's meanwhile, and `peer` is `None`.
    pub rejoin: Option<RejoinView>,
    /// Host: the other DJ's link dropped and their booth is expected back.
    pub awaiting: Option<String>,
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
    /// Tracks of it the link dropped under, waiting to carry on.
    pub paused: usize,
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

/// The room as the broadcast lane is playing it right now: `bcast_delay_us`
/// in the past. The broadcast window draws this, so a stream's picture
/// shows each move at the same moment its sound arrives.
#[derive(Clone)]
pub struct BroadcastView {
    /// Shared time on screen: now minus the broadcast delay.
    pub t: i64,
    pub room: RoomState,
    pub decks: [DeckView; DECKS],
    /// Everyone in the room (this booth first), to name who touched what.
    pub djs: Vec<PeerInfo>,
}

impl BroadcastView {
    /// A DJ's real name (never "you": the audience is watching).
    pub fn dj_name(&self, id: u64) -> Option<&str> {
        self.djs
            .iter()
            .find(|p| p.id == id && id != 0)
            .map(|p| p.name.as_str())
    }
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
    /// A secret only this booth and the host it joins know, the same for
    /// every connection it makes (see `Msg::Hello::booth_key`). With `me.id`
    /// it's the booth's identity across rejoins.
    booth_key: u64,
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
    active_fetch: HashMap<TrackId, Fetch>,
    /// Downloads a dropped link cut off. They carry on from where they got
    /// to when the booth they come from (`paused_from`) is back.
    paused: Vec<(TrackId, Fetch)>,
    paused_from: u64,
    paused_since: Instant,
    fetch_serial: u64,
    transfer_serial: u64,
    /// Every transfer running in this booth (see [`crate::files`]).
    running: Arc<Running>,
    transfers: Vec<Transfer>,
    pub local: LocalParams,
    snapshot: Arc<ArcSwap<AudioSnapshot>>,
    keepalive: VecDeque<Arc<AudioSnapshot>>,
    meters: Arc<LevelMeters>,
    role: Role,
    peer: Option<Peer>,
    /// Host: booths that joined only to broadcast.
    broadcasters: Vec<Broadcaster>,
    next_serial: u64,
    /// Client: who's in the room, as the host last said.
    roster: Roster,
    /// Client: getting back into the room after the link dropped.
    rejoin: Option<Rejoin>,
    rejoin_gen: u64,
    /// Host: the other DJ whose link dropped, while they're expected back.
    awaiting: Option<Awaiting>,
    /// Host: the room with its history kept further back than `timeline`
    /// keeps it, to [`HELD_MARGIN_US`] before the other DJ was last heard
    /// from, while they're linked or expected back. It gets every event
    /// `timeline` gets, so `timeline` is always this compacted to its own
    /// base. A DJ coming back after their link dropped has the moves they
    /// made meanwhile put in here at their own times, and the room is taken
    /// from it. Never published: the audio threads only see `timeline`.
    held: Option<Timeline>,
    /// Host: a move arrived too late for `timeline` but not for `held`.
    rebuild_due: bool,
    /// Client DJ: this booth's own moves that may not have reached the room
    /// (see [`CutOff`]): from [`HELD_MARGIN_US`] before it last heard from
    /// the host, and every one since while its link is down.
    own_moves: VecDeque<Event>,
    /// Client DJ: the room whose link was lost, while it may get back in.
    lost: Option<LostRoom>,
    /// A link that hears nothing for this long is dropped
    /// ([`crate::net::LINK_TIMEOUT`]; tests shorten it).
    pub link_timeout: Duration,
    /// How long a booth whose link dropped tries to get back in
    /// ([`crate::net::REJOIN_FOR`]).
    pub rejoin_for: Duration,
    /// Moves that happened from this (shared) time on count towards
    /// lateness: those since joining. `i64::MAX` when not in a room.
    late_from: i64,
    /// The latest move to arrive in each second: (second's start, us).
    late: VecDeque<(Instant, i64)>,
    upload_pacer: Arc<crate::files::UploadPacer>,
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
    /// Things to tell the DJ (a load refused), for the UI to pick up with
    /// [`Session::take_notices`].
    notices: Vec<String>,
    /// Loads this booth made that are still recent enough to be dropped by
    /// a partner's earlier move arriving late: (time, seq, deck, track).
    my_loads: Vec<(i64, u64, usize, TrackId)>,
    weak: Weak<Mutex<Session>>,
}

const MAX_CONCURRENT_FETCHES: usize = 2;
/// How far ahead (us) to schedule a relock for a deck about to reach a tempo
/// change. Longer than the maintenance tick so none slips through, short so
/// the event is only briefly in the future.
const RELOCK_HORIZON_US: i64 = 300_000;
const TRANSFER_STALL: Duration = Duration::from_secs(45);
/// How long a download the network cut off waits before carrying on while
/// the link looks fine. When the link is dropping as well, it notices
/// within this, and the download waits for it to be back instead.
const RESUME_AFTER: Duration = Duration::from_secs(1);
/// How long a client waits before asking the host again for a track it
/// couldn't download.
const FETCH_RETRY: Duration = Duration::from_secs(5);
/// How far back [`LinkView::late_us`] looks.
pub const LATE_WINDOW: Duration = Duration::from_secs(60);
/// How far back, from when the other DJ's booth was last heard from, the
/// host keeps the room's history, and a joining DJ's booth keeps its own
/// moves, for when the link drops. A move that never arrived was sent after
/// the last message that did, so it was made at most one trip before that
/// message got here: this only has to outlast a slow trip.
const HELD_MARGIN_US: i64 = 10_000_000;

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
                booth_key: secret_u64(),
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
                paused: Vec::new(),
                paused_from: 0,
                paused_since: Instant::now(),
                fetch_serial: 0,
                transfer_serial: 0,
                running: Arc::default(),
                transfers: Vec::new(),
                local: LocalParams::default(),
                snapshot,
                keepalive: VecDeque::new(),
                meters: Arc::new(LevelMeters::new()),
                role: Role::Solo,
                peer: None,
                broadcasters: Vec::new(),
                next_serial: 0,
                roster: Roster::default(),
                rejoin: None,
                rejoin_gen: 0,
                awaiting: None,
                held: None,
                rebuild_due: false,
                own_moves: VecDeque::new(),
                lost: None,
                link_timeout: crate::net::LINK_TIMEOUT,
                rejoin_for: crate::net::REJOIN_FOR,
                late_from: i64::MAX,
                late: VecDeque::new(),
                upload_pacer: Arc::default(),
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
                notices: Vec::new(),
                my_loads: Vec::new(),
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

    /// The mixer's level meters, which the master output's live lane fills.
    pub fn meters(&self) -> Arc<LevelMeters> {
        self.meters.clone()
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

    /// True while this booth is in a room only to broadcast it: nothing it
    /// does changes the room.
    pub fn is_broadcaster(&self) -> bool {
        matches!(
            self.role,
            Role::Client {
                broadcaster: true,
                ..
            }
        )
    }

    /// To the other DJ (a broadcaster's link is to the host).
    fn send(&self, m: Msg) {
        if let Some(p) = &self.peer {
            p.conn.send(m);
        }
    }

    /// To everyone this booth is linked to: the other DJ and, on the host,
    /// the broadcasters.
    fn send_all(&self, m: Msg) {
        self.relay(&m);
        self.send(m);
    }

    /// Host: pass something from the joining DJ on to the broadcasters.
    fn relay(&self, m: &Msg) {
        for b in &self.broadcasters {
            b.conn.send(m.clone());
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
    /// A broadcaster's actions are dropped: it only watches. So is a load
    /// onto a deck the room can hear (the model would drop it anyway), with
    /// a notice saying why.
    pub fn act_at(&mut self, a: Action, t: i64) {
        if self.is_broadcaster() {
            log::debug!("broadcasting only: ignoring {a:?}");
            return;
        }
        if let Action::Load { deck, .. } = a {
            if self.refuse_load(deck as usize, t) {
                return;
            }
        }
        self.seq += 1;
        let ev = Event {
            time: t,
            origin: self.me.id,
            seq: self.seq,
            action: a,
        };
        self.insert_event(ev.clone(), true);
        if self.keeps_moves() {
            self.own_moves.push_back(ev.clone());
        }
        if let Action::Load { deck, track, .. } = ev.action {
            self.my_loads.push((t, self.seq, deck as usize, track));
        }
        self.after_event(&ev);
        self.send_all(Msg::Event { ev });
        self.publish();
    }

    /// Tell the DJ if the room dropped one of this booth's recent loads: a
    /// partner's move from just before it (bringing that deck's fader up,
    /// say) arrived after it, so the deck could be heard when it landed.
    fn check_my_loads(&mut self) {
        // The host looks back as far as it keeps the room's history.
        let timeline = self.held.as_ref().unwrap_or(&self.timeline);
        let base = timeline.base_time();
        let mut dropped = Vec::new();
        self.my_loads.retain(|&(t, _, deck, track)| {
            if t < base {
                return false;
            }
            let st = timeline.state_at(t);
            if st.decks[deck].track == Some(track) {
                return true;
            }
            dropped.extend(st.load_refusal(deck, t));
            false
        });
        for why in dropped {
            log::debug!("Load dropped: {why}");
            self.notices.push(why);
        }
    }

    /// Getting back in, this booth's moves from while it was cut off went
    /// into the room (`timeline`, reaching back as far as they do). Moves it
    /// made after sending them aren't in it yet: they go in here, at their
    /// own times, and to the host. Every load it made meanwhile is checked
    /// against the room at its own time, so one the room refused (the
    /// partner's deck was playing out loud by then) is reported. Then the
    /// room is compacted back to the usual length before anyone plays it.
    fn took_merged_room(&mut self, mut timeline: Timeline) {
        let (me, base) = (self.me.id, timeline.base_time());
        let have: HashSet<u64> = timeline
            .events()
            .iter()
            .filter(|e| e.origin == me)
            .map(|e| e.seq)
            .collect();
        let since: Vec<Event> = self
            .own_moves
            .iter()
            .filter(|e| e.time >= base && !have.contains(&e.seq))
            .cloned()
            .collect();
        for ev in &since {
            timeline.insert_new(ev.clone());
        }
        for e in &self.own_moves {
            if let Action::Load { deck, track, .. } = e.action {
                if !self.my_loads.iter().any(|l| l.1 == e.seq) {
                    self.my_loads.push((e.time, e.seq, deck as usize, track));
                }
            }
        }
        self.timeline = timeline;
        self.check_my_loads();
        self.timeline
            .compact(self.now() - self.bcast_delay_us - 3_000_000);
        let base = self.timeline.base_time();
        self.my_loads.retain(|l| l.0 >= base);
        for ev in since {
            // A load's track goes first, as from `finish_local_load`.
            if let Action::Load { track, .. } = ev.action {
                if let Some(info) = self.track_meta.get(&track).cloned() {
                    self.send(Msg::Track { info });
                }
            }
            self.send(Msg::Event { ev });
        }
    }

    /// The room's timeline has just replaced this booth's (joining, or
    /// getting back in to a room that had stopped holding its place). Loads
    /// it made while cut off never reached the room, so they're forgotten
    /// (the room's notice says so); any that did reach it are checked.
    fn took_room_timeline(&mut self) {
        let (me, events) = (self.me.id, self.timeline.events());
        self.my_loads
            .retain(|l| events.iter().any(|e| e.origin == me && e.seq == l.1));
        self.check_my_loads();
    }

    /// Put an event in the room: this booth's own (`mine`) or one that
    /// arrived. On the host it goes in the held room too (see `held`), and
    /// one that arrives later than `timeline` reaches back, but within
    /// `held`, goes in at its own time there rather than being clamped, and
    /// `timeline` is made again from `held` at the next maintenance tick.
    fn insert_event(&mut self, mut ev: Event, mine: bool) -> Landed {
        let Some(held) = self.held.as_mut() else {
            return match self.timeline.insert(ev) {
                true => Landed::Now,
                false => Landed::Already,
            };
        };
        let base = self.timeline.base_time();
        if ev.time < held.base_time() {
            log::warn!(
                "event arrived {} ms too late; applying it now",
                (base - ev.time) / 1000
            );
            ev.time = base;
        }
        if mine {
            held.insert_new(ev.clone());
        } else if !held.insert(ev.clone()) {
            return Landed::Already;
        }
        if ev.time >= base {
            // New to `held`, so new here too.
            self.timeline.insert_new(ev);
            Landed::Now
        } else {
            self.rebuild_due = true;
            Landed::Later
        }
    }

    /// Host: make the live timeline again from the held room, after moves
    /// went in further back than it reaches, and send it to the
    /// broadcasters, whose rooms reach back no further.
    fn rebuild_from_held(&mut self) {
        self.rebuild_due = false;
        let Some(held) = &self.held else { return };
        let mut tl = held.clone();
        tl.compact(self.timeline.base_time());
        self.timeline = tl;
        if !self.broadcasters.is_empty() {
            self.relay(&Msg::Resync {
                timeline: self.timeline.clone(),
            });
        }
    }

    /// Client DJ: keep this booth's moves for the room in case they don't
    /// reach it: while in a room, and after its link to one was lost for
    /// as long as the host holds its place.
    fn keeps_moves(&self) -> bool {
        match self.role {
            Role::Client { broadcaster, .. } => !broadcaster,
            Role::Solo => self
                .lost
                .as_ref()
                .is_some_and(|l| l.since.elapsed() < self.rejoin_for + self.link_timeout),
            Role::Host { .. } => false,
        }
    }

    /// Client DJ: the moves to send the host when joining (see [`CutOff`]).
    /// Empty unless this booth's link to a room was lost: the host only
    /// takes them from a booth whose place it was holding.
    pub fn cut_off(&self) -> CutOff {
        if !self.keeps_moves() {
            return CutOff::default();
        }
        let events: Vec<Event> = self.own_moves.iter().cloned().collect();
        let mut ids: Vec<TrackId> = events
            .iter()
            .filter_map(|e| match e.action {
                Action::Load { track, .. } => Some(track),
                _ => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        let tracks = ids
            .iter()
            .filter_map(|id| self.track_meta.get(id).cloned())
            .collect();
        CutOff { events, tracks }
    }

    /// Host: a DJ's booth (`id`, `key`) is joining. If it's the one whose
    /// place this room is holding (its link dropped, or has gone quiet),
    /// the moves it made that may not have reached the room (`cut_off`) go
    /// in at the times they were made, among everything that happened here
    /// meanwhile: on a control both DJs touched, the later move wins, and a
    /// load goes through the load guard at its own time. Returns whether
    /// they did, for the Welcome.
    pub fn take_cut_off(&mut self, id: u64, key: u64, cut_off: CutOff) -> bool {
        let same = |pid: u64, pkey: u64| key != 0 && pid == id && pkey == key;
        let window = self.rejoin_for + self.link_timeout;
        let name = self
            .awaiting
            .as_ref()
            .filter(|a| same(a.info.id, a.key) && a.since.elapsed() < window)
            .map(|a| a.info.name.clone())
            .or_else(|| {
                self.peer
                    .as_ref()
                    .filter(|p| same(p.info.id, p.key))
                    .map(|p| p.info.name.clone())
            });
        let (Some(name), Some(held)) = (name, self.held.as_mut()) else {
            return false;
        };
        // Its own moves only: it can't speak for anyone else.
        let events = cut_off.events.into_iter().filter(|e| e.origin == id);
        let added = held.merge(events.collect());
        for info in cut_off.tracks {
            if !self.track_meta.contains_key(&info.id) {
                self.relay(&Msg::Track { info: info.clone() });
                self.track_meta.insert(info.id, info);
            }
        }
        if !added.is_empty() {
            log::info!(
                "{} moves {name} made while cut off came into the room",
                added.len()
            );
            self.rebuild_from_held();
            for ev in &added {
                if let Action::SetGrid { track, grid, .. } = &ev.action {
                    self.remember_grid(*track, grid.as_deref().copied());
                }
            }
            self.check_my_loads();
            self.publish();
        }
        true
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
        if self.is_broadcaster() {
            return;
        }
        let Some(id) = self.timeline.latest().decks.get(deck).and_then(|d| d.track) else {
            return;
        };
        let path = self.servable.get(&id).cloned();
        let audio = self.tracks.get(&id).and_then(|s| s.audio.clone());
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let grid = path
                .and_then(|p| library::detected_grid(&lib, &p))
                .or_else(|| audio.and_then(|a| analysis::analyze(&a)));
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

    /// This booth's headphone level (the controller's knob).
    pub fn set_hp_level(&mut self, v: f32) {
        self.local.hp_level = v.clamp(0.0, 1.0);
        self.publish();
    }

    /// This booth's master level (the controller's knob).
    pub fn set_master_level(&mut self, v: f32) {
        self.local.master_level = v.clamp(0.0, 1.0);
        self.publish();
    }

    /// What to tell the DJ since the last call: loads refused, oldest first.
    pub fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    // ------------------------------------------------------------ tracks

    /// True (and a notice for the DJ) if deck `deck` can be heard in the
    /// master at time `t`, so a track can't be loaded onto it.
    fn refuse_load(&mut self, deck: usize, t: i64) -> bool {
        let Some(why) = self.timeline.state_at(t).load_refusal(deck, t) else {
            return false;
        };
        // Debug: the toast already says it, beside the status bar's log line.
        log::debug!("Not loading: {why}");
        self.notices.push(why);
        true
    }

    /// Load a file from this computer onto a deck (hash + decode in the
    /// background). Refused up front if the deck can be heard, and again
    /// once the track is ready if it can be by then.
    pub fn load_path(&mut self, deck: usize, path: PathBuf) {
        if self.is_broadcaster() {
            log::info!("Broadcasting only: the DJs load the decks");
            return;
        }
        if deck >= DECKS || self.refuse_load(deck, self.now()) {
            return;
        }
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
                let (grid, key) = library::analysis_for(&lib, &path, &audio);
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
        // The deck may have been brought up while the track was decoding.
        if self.refuse_load(deck, self.now()) {
            return;
        }
        let id = info.id;
        self.servable.insert(id, path);
        self.tracks.insert(
            id,
            TrackSlot::new(TrackStatus::Ready, Some(Arc::new(audio))),
        );
        self.track_meta.insert(id, info.clone());
        self.send_all(Msg::Track { info: info.clone() });
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
        self.tracks
            .insert(id, TrackSlot::new(TrackStatus::Searching, None));
        let size = self.track_meta.get(&id).map(|m| m.size);
        let known = self.servable.get(&id).cloned();
        let lib = self.library.clone();
        let weak = self.weak.clone();
        std::thread::spawn(move || {
            let found = known.or_else(|| size.and_then(|sz| library::find_by_hash(&lib, id, sz)));
            let Some(h) = weak.upgrade() else { return };
            match found {
                Some(path) => {
                    {
                        let mut s = h.lock();
                        // It's on a deck: a broadcaster may ask the host for it.
                        s.servable.insert(id, path.clone());
                        s.set_status(id, TrackStatus::Decoding);
                    }
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
                self.tracks
                    .insert(id, TrackSlot::new(TrackStatus::Ready, Some(Arc::new(a))));
                // A deck that played on while it waited comes in where the room is.
                let now = self.now();
                let room = self.timeline.state_at(now);
                let playing = (0..DECKS).find(|&d| {
                    let st = &room.decks[d];
                    st.track == Some(id) && st.playing && st.position_at(now) < st.duration
                });
                match playing {
                    Some(d) => log::info!(
                        "Ready: {name}; deck {} comes in playing, at {:.3} s",
                        deck_letter(d),
                        room.decks[d].position_at(now)
                    ),
                    None => log::info!("Ready: {name}"),
                }
                self.publish();
            }
            Err(e) => {
                log::error!("Couldn't decode {name}: {e:#}");
                self.set_status(id, TrackStatus::Failed(format!("{e:#}")));
            }
        }
    }

    // ------------------------------------------------------------ transfers

    /// The one limit every upload from this booth shares.
    pub fn upload_pacer(&self) -> Arc<crate::files::UploadPacer> {
        self.upload_pacer.clone()
    }

    /// Every transfer running in this booth.
    pub fn running(&self) -> Arc<Running> {
        self.running.clone()
    }

    /// Where a download of this track keeps what has arrived so far. It's
    /// named after the track's hash, so any later download of the same
    /// track this session (after the link drops, or a rejoin by hand)
    /// carries on from it.
    pub fn part_path(&self, id: TrackId) -> PathBuf {
        self.paths.cache_dir.join(format!("{}.part", id.to_hex()))
    }

    pub fn servable_path(&self, id: TrackId) -> Option<PathBuf> {
        self.servable.get(&id).cloned()
    }

    /// Host: can this track go to a booth that asks for it? A track the
    /// joining DJ just loaded is pending while it's on its way here.
    pub fn servable(&self, id: TrackId) -> Servable {
        if let Some(p) = self.servable.get(&id) {
            return Servable::Ready(p.clone());
        }
        let busy = matches!(
            self.track_status(id),
            Some(TrackStatus::Searching | TrackStatus::Fetching | TrackStatus::Decoding)
        );
        if busy
            || self.active_fetch.contains_key(&id)
            || self.fetch_queue.iter().any(|(i, _)| *i == id)
        {
            Servable::Pending
        } else {
            Servable::No
        }
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
        // One the link cut off shows where it will carry on from.
        let done = self
            .transfers
            .iter()
            .find(|t| t.id == id && !t.upload && t.state == TransferState::Paused)
            .map(|t| t.done)
            .unwrap_or(0);
        self.transfer_set(id, false, &name, done, total, TransferState::Queued);
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
            let Some(peer) = self.peer.as_ref().map(|p| p.info.id) else {
                self.finish_fetch(id, purpose, Err("not connected".into()));
                continue;
            };
            let landing = self.landing(id, &purpose);
            self.fetch_serial += 1;
            let serial = self.fetch_serial;
            self.active_fetch.insert(id, Fetch { purpose, serial });
            self.transfer_state(id, false, TransferState::Active);
            match &self.role {
                Role::Client {
                    addr,
                    room,
                    broadcaster,
                } => {
                    let (addr, room, me, weak, bc) = (
                        *addr,
                        room.clone(),
                        self.me.clone(),
                        self.weak.clone(),
                        *broadcaster,
                    );
                    std::thread::spawn(move || {
                        let Some(h) = weak.upgrade() else { return };
                        let res = crate::files::fetch(&h, addr, &room, &me, bc, peer, id, &landing);
                        match res {
                            // Its link dropped (it's paused) or a newer try
                            // took over.
                            Err(_) if crate::files::was_stopped(&res) => {}
                            Err(e) if crate::files::interrupted(&e) => {
                                h.lock().fetch_interrupted(id, serial)
                            }
                            res => h.lock().file_received(
                                id,
                                serial,
                                res.map_err(|e| crate::files::describe(&e)),
                            ),
                        }
                    });
                }
                Role::Host { .. } => self.send(Msg::PushRequest { id }),
                Role::Solo => {}
            }
        }
    }

    /// Where a downloaded track goes once it checks out: a session copy in
    /// the cache, or a new file in the share's folder.
    fn landing(&self, id: TrackId, purpose: &Purpose) -> Landing {
        let name = self
            .track_meta
            .get(&id)
            .map(|m| m.file_name.clone())
            .unwrap_or_else(|| format!("{}.bin", id.short()));
        let name = sanitize_file_name(&name);
        match purpose {
            Purpose::Session => {
                Landing::At(self.paths.cache_dir.join(format!("{}-{name}", id.short())))
            }
            Purpose::Share { dir, .. } => Landing::NewIn(dir.clone(), name),
        }
    }

    /// Host side of an upload from the client: is it one this booth asked
    /// for, and where does it go?
    pub fn accept_push(&mut self, id: TrackId) -> Option<Expected> {
        let f = self.active_fetch.get(&id)?;
        Some(Expected {
            serial: f.serial,
            part: self.part_path(id),
            landing: self.landing(id, &f.purpose),
        })
    }

    /// Host: the joining DJ's upload of a track carried on from a partial
    /// file that didn't check out. That file is cleared now, so ask for the
    /// whole track again.
    pub fn push_again(&mut self, id: TrackId, serial: u64) {
        if self
            .active_fetch
            .get(&id)
            .is_some_and(|f| f.serial == serial)
        {
            self.send(Msg::PushRequest { id });
        }
    }

    /// Download `serial` of a track ended. A try that's no longer the
    /// current one is ignored, except a finished one the link cut off.
    pub fn file_received(&mut self, id: TrackId, serial: u64, res: Result<PathBuf, String>) {
        if self
            .active_fetch
            .get(&id)
            .is_some_and(|f| f.serial == serial)
        {
            if let Some(f) = self.active_fetch.remove(&id) {
                self.finish_fetch(id, f.purpose, res);
            }
            self.pump_fetches();
        } else if res.is_ok() {
            // It got all of it just as the link dropped.
            if let Some(i) = self
                .paused
                .iter()
                .position(|(i, f)| *i == id && f.serial == serial)
            {
                let (_, f) = self.paused.remove(i);
                self.finish_fetch(id, f.purpose, res);
            }
        }
    }

    /// Download `serial` of a track was cut off by the network. Often the
    /// link is dropping too, and its own end comes a moment later, so it
    /// waits with the paused ones: it carries on when the link is back, or
    /// in a moment if the link is fine.
    pub fn fetch_interrupted(&mut self, id: TrackId, serial: u64) {
        let Some(peer) = self.peer.as_ref().map(|p| p.info.id) else {
            return;
        };
        if !self
            .active_fetch
            .get(&id)
            .is_some_and(|f| f.serial == serial)
        {
            return;
        }
        if let Some(f) = self.active_fetch.remove(&id) {
            self.pause(peer, vec![(id, f)]);
        }
        self.pump_fetches();
    }

    /// The current download of a track failed (the partner hasn't got it,
    /// or it stalled).
    fn fetch_failed(&mut self, id: TrackId, why: &str) {
        if let Some(f) = self.active_fetch.remove(&id) {
            self.finish_fetch(id, f.purpose, Err(why.into()));
            self.pump_fetches();
        }
    }

    /// The link dropped: the downloads through it stop where they are, and
    /// carry on from there when booth `from` is back.
    fn pause_fetches(&mut self, from: u64) {
        let cut: Vec<(TrackId, Fetch)> = self
            .active_fetch
            .drain()
            .chain(
                self.fetch_queue
                    .drain(..)
                    .map(|(id, purpose)| (id, Fetch { purpose, serial: 0 })),
            )
            .collect();
        self.pause(from, cut);
    }

    /// Hold these downloads from booth `from` where they got to.
    fn pause(&mut self, from: u64, cut: Vec<(TrackId, Fetch)>) {
        if self.paused_from != from {
            self.fail_paused("connection lost");
        }
        self.paused_from = from;
        self.paused_since = Instant::now();
        for (id, f) in cut {
            if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id && !t.upload) {
                if t.total > 0 && t.done > 0 {
                    log::info!(
                        "Paused the download of {} at byte {} of {} ({:.0}%) until the link is back",
                        t.name,
                        t.done,
                        t.total,
                        t.done as f64 * 100.0 / t.total as f64
                    );
                }
                t.state = TransferState::Paused;
                t.updated = Instant::now();
            }
            self.paused.push((id, f));
        }
    }

    /// Booth `peer` is linked again: carry on with the downloads from it
    /// that a dropped link cut off. Any from another booth have failed.
    fn resume_fetches(&mut self, peer: u64) {
        if self.paused.is_empty() {
            return;
        }
        if peer != self.paused_from {
            self.fail_paused("connection lost");
            return;
        }
        let paused = std::mem::take(&mut self.paused);
        log::info!(
            "Carrying on with {} download{} from where the link dropped",
            paused.len(),
            if paused.len() == 1 { "" } else { "s" }
        );
        for (id, f) in paused {
            self.request_fetch(id, f.purpose);
        }
    }

    fn fail_paused(&mut self, why: &str) {
        for (id, f) in std::mem::take(&mut self.paused) {
            self.finish_fetch(id, f.purpose, Err(why.into()));
        }
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
                    // The host passes a partner's track on to broadcasters.
                    self.servable.insert(id, path.clone());
                    self.set_status(id, TrackStatus::Decoding);
                    let weak = self.weak.clone();
                    std::thread::spawn(move || {
                        if let Some(h) = weak.upgrade() {
                            decode_into(&h, id, &path);
                        }
                    });
                }
                Err(e) => {
                    self.set_status(id, TrackStatus::Failed(e));
                    if matches!(self.role, Role::Client { .. }) {
                        if let Some(s) = self.tracks.get_mut(&id) {
                            s.retry_at = Some(Instant::now() + FETCH_RETRY);
                        }
                    }
                }
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
                        library::refresh_all(&self.library);
                    }
                }
            }
        }
    }

    /// Start (or restart) a transfer's entry, at `done` of `total` bytes.
    /// Returns this try's serial, for [`Session::transfer_ended`].
    pub fn transfer_set(
        &mut self,
        id: TrackId,
        upload: bool,
        name: &str,
        done: u64,
        total: u64,
        state: TransferState,
    ) -> u64 {
        let now = Instant::now();
        self.transfer_serial += 1;
        let serial = self.transfer_serial;
        match self
            .transfers
            .iter_mut()
            .find(|t| t.id == id && t.upload == upload)
        {
            Some(t) => {
                t.done = done;
                t.total = total;
                t.from = done;
                t.state = state;
                t.updated = now;
                t.serial = serial;
            }
            None => self.transfers.push(Transfer {
                id,
                name: name.to_string(),
                upload,
                done,
                total,
                from: done,
                state,
                updated: now,
                serial,
            }),
        }
        serial
    }

    /// Try `serial` of a transfer ended. A try that a newer one has taken
    /// over from (an upload stuck on a dropped connection until it timed
    /// out, say) doesn't change the entry.
    pub fn transfer_ended(&mut self, id: TrackId, upload: bool, serial: u64, state: TransferState) {
        if let Some(t) = self
            .transfers
            .iter_mut()
            .find(|t| t.id == id && t.upload == upload && t.serial == serial)
        {
            t.state = state;
            t.updated = Instant::now();
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
        if self.peer.is_none() || self.is_broadcaster() {
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

    /// The room for a booth joining it. One whose moves from while it was
    /// cut off just went in (`merged`, see [`Session::take_cut_off`]) gets
    /// it reaching back as far as they do, to see which of its loads the
    /// room took.
    pub fn welcome(&self, merged: bool) -> Msg {
        let timeline = match &self.held {
            Some(held) if merged => held.clone(),
            _ => self.timeline.clone(),
        };
        Msg::Welcome {
            host: self.me.clone(),
            timeline,
            tracks: self.track_meta.values().cloned().collect(),
            merged,
        }
    }

    pub fn set_hosting(&mut self, port: u16, room: String, stop: Arc<AtomicBool>) {
        self.role = Role::Host { port, room, stop };
        // Hosting now: no room is holding this booth's place.
        self.lost = None;
        self.own_moves.clear();
        self.clock.set_offset(0);
    }

    pub fn has_peer(&self) -> bool {
        self.peer.is_some()
    }

    /// The link with this serial, while it's still the current one.
    pub fn link(&self, serial: u64) -> Option<&PeerConn> {
        self.peer
            .as_ref()
            .filter(|p| p.serial == serial)
            .map(|p| &p.conn)
    }

    /// The secret this booth sends in every Hello.
    pub fn booth_key(&self) -> u64 {
        self.booth_key
    }

    /// Host: can this booth come in? `Err` is the reason to give it. A
    /// booth coming back after its link dropped (the same id and booth key)
    /// gets its seat back, even before its old link has timed out here.
    pub fn seat_for(&self, id: u64, key: u64, broadcaster: bool) -> Result<(), String> {
        let same = |pid: u64, pkey: u64| key != 0 && pid == id && pkey == key;
        use crate::net::{MAX_BROADCASTERS, ROOM_FULL};
        if broadcaster {
            let others = self
                .broadcasters
                .iter()
                .filter(|b| !same(b.info.id, b.key))
                .count();
            if others >= MAX_BROADCASTERS {
                return Err(format!("{ROOM_FULL} {MAX_BROADCASTERS} broadcasters."));
            }
        } else if self.peer.as_ref().is_some_and(|p| !same(p.info.id, p.key)) {
            return Err(format!(
                "{ROOM_FULL} two DJs. Tick \"Broadcaster only\" to join just to stream it."
            ));
        }
        Ok(())
    }

    /// Host: close any link this booth still has here, because it's coming
    /// back before the old one timed out. True if there was one.
    fn evict(&mut self, id: u64, key: u64) -> bool {
        if key == 0 {
            return false;
        }
        let same = |pid: u64, pkey: u64| pid == id && pkey == key;
        let mut found = false;
        if self.peer.as_ref().is_some_and(|p| same(p.info.id, p.key)) {
            // Its downloads here carry on over the new link.
            self.unlink("connection replaced", true);
            found = true;
        }
        let running = self.running.clone();
        self.broadcasters.retain(|b| {
            let old = same(b.info.id, b.key);
            if old {
                b.conn.close();
                running.stop_with(b.info.id);
                found = true;
            }
            !old
        });
        found
    }

    /// Host: a DJ joined, or came back after their link dropped. Returns the
    /// link's serial.
    pub fn add_peer(&mut self, conn: PeerConn, info: PeerInfo, key: u64) -> u64 {
        let expected = self
            .awaiting
            .take()
            .is_some_and(|a| key != 0 && a.info.id == info.id && a.key == key);
        let back = self.evict(info.id, key) || expected;
        if back {
            log::info!("{} is back in the room", info.name);
        } else {
            log::info!("{} joined the room", info.name);
        }
        self.next_serial += 1;
        let serial = self.next_serial;
        let info_id = info.id;
        self.peer = Some(Peer {
            conn,
            info,
            rtt_us: None,
            heard: Instant::now(),
            serial,
            key,
        });
        self.in_sync = None;
        self.last_digest = Instant::now();
        // Keep the room's history in case their link drops.
        self.held.get_or_insert_with(|| self.timeline.clone());
        self.announce_members();
        // Uploads from them the dropped link cut off carry on from where
        // they got to, and tracks on a deck that didn't arrive from them
        // before are asked for again.
        self.resume_fetches(info_id);
        self.ensure_room_tracks();
        serial
    }

    pub fn broadcaster_count(&self) -> usize {
        self.broadcasters.len()
    }

    /// Host: a booth joined to broadcast, or came back. Returns its link's
    /// serial.
    pub fn add_broadcaster(&mut self, conn: PeerConn, info: PeerInfo, key: u64) -> u64 {
        if self.evict(info.id, key) {
            log::info!("{} is back, broadcasting", info.name);
        } else {
            log::info!("{} joined to broadcast", info.name);
        }
        self.next_serial += 1;
        let serial = self.next_serial;
        self.broadcasters.push(Broadcaster {
            conn,
            info,
            serial,
            key,
        });
        self.announce_members();
        serial
    }

    /// Who's in the room. A client knows from the host's last word.
    pub fn roster(&self) -> Roster {
        match &self.role {
            Role::Client { broadcaster, .. } => {
                if !self.roster.djs.is_empty() {
                    return self.roster.clone();
                }
                // Before the host's first word: the host, and this booth if a DJ.
                let mut djs: Vec<PeerInfo> = self.peer.iter().map(|p| p.info.clone()).collect();
                if !broadcaster {
                    djs.push(self.me.clone());
                }
                Roster {
                    djs,
                    broadcasters: Vec::new(),
                }
            }
            Role::Host { .. } | Role::Solo => Roster {
                djs: std::iter::once(self.me.clone())
                    .chain(self.peer.iter().map(|p| p.info.clone()))
                    .collect(),
                broadcasters: self.broadcasters.iter().map(|b| b.info.clone()).collect(),
            },
        }
    }

    /// Host: tell everyone who's in the room.
    fn announce_members(&self) {
        if matches!(self.role, Role::Host { .. }) {
            let Roster { djs, broadcasters } = self.roster();
            self.send_all(Msg::Members { djs, broadcasters });
        }
    }

    /// Client: we joined a host's room, or got back in after the link
    /// dropped (`rejoin`, or joining the same room again by hand). The room
    /// is taken as the host has it. Getting back in, the host has already
    /// put this booth's moves from while it was cut off into it (`merged`,
    /// see [`Session::take_cut_off`]), and any it made since sending them
    /// go now. Its own settings, library and the tracks it has stay, and
    /// tracks on the decks it doesn't have yet are fetched again. Returns
    /// the link's serial.
    #[allow(clippy::too_many_arguments)]
    pub fn on_joined(
        &mut self,
        conn: PeerConn,
        host: PeerInfo,
        timeline: Timeline,
        tracks: Vec<TrackInfo>,
        est: ClockEstimator,
        to: RoomAddr,
        rejoin: bool,
        merged: bool,
    ) -> u64 {
        let (to_addr, to_room, host_name) = (to.addr, to.room.clone(), host.name.clone());
        if rejoin {
            let down = self.rejoin.as_ref().map(|r| r.since.elapsed().as_secs());
            log::info!(
                "Back in {}'s room ({} s after the link dropped)",
                host.name,
                down.unwrap_or(0)
            );
        } else if to.broadcaster {
            log::info!("Joined {}'s room to broadcast", host.name);
        } else {
            log::info!("Joined {}'s room", host.name);
        }
        self.rejoin = None;
        self.role = Role::Client {
            addr: to.addr,
            room: to.room,
            broadcaster: to.broadcaster,
        };
        self.roster = Roster::default();
        self.late_from = self.now();
        self.late.clear();
        self.next_serial += 1;
        let serial = self.next_serial;
        let host_id = host.id;
        self.peer = Some(Peer {
            conn,
            info: host,
            rtt_us: est.last_rtt(),
            heard: Instant::now(),
            serial,
            key: 0,
        });
        self.est = est;
        self.in_sync = None;
        self.digest_mismatch = 0;
        let lost = self.lost.take();
        if merged {
            self.took_merged_room(timeline);
        } else {
            let here = lost.is_some_and(|l| l.addr == to_addr && l.room == to_room);
            if here && !self.own_moves.is_empty() {
                // The host had stopped holding this booth's place.
                let why = format!(
                    "Back in {host_name}'s room, but it had stopped waiting for you: what you did while cut off didn't reach it."
                );
                log::warn!("{why}");
                self.notices.push(why);
            }
            self.own_moves.clear();
            self.timeline = timeline;
            self.took_room_timeline();
        }
        for t in tracks {
            self.track_meta.insert(t.id, t);
        }
        // Downloads the dropped link cut off carry on from where they got
        // to; tracks on the decks this booth hasn't got are fetched.
        self.resume_fetches(host_id);
        self.ensure_room_tracks();
        self.publish();
        serial
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
            p.heard = Instant::now();
        }
    }

    /// Host: a Ping arrived on link `serial`, carrying its latest round trip.
    /// Only the other DJ's count (a broadcaster's link is to this booth only).
    pub fn on_peer_ping(&mut self, serial: u64, rtt: Option<i64>) {
        if let Some(p) = self.peer.as_mut().filter(|p| p.serial == serial) {
            if rtt.is_some() {
                p.rtt_us = rtt;
            }
            p.heard = Instant::now();
        }
    }

    pub fn last_rtt(&self) -> Option<i64> {
        self.est.last_rtt()
    }

    /// The link with this serial ended (a link that was already replaced
    /// or left is ignored). On a client whose link broke without a Bye, it
    /// starts getting back in: the caller runs the rejoin for the returned
    /// generation.
    pub fn on_link_ended(&mut self, serial: u64, end: LinkEnd) -> Option<u64> {
        if let Some(i) = self.broadcasters.iter().position(|b| b.serial == serial) {
            let b = self.broadcasters.remove(i);
            b.conn.close();
            self.running.stop_with(b.info.id);
            match end {
                LinkEnd::Bye => log::info!("{} stopped broadcasting", b.info.name),
                LinkEnd::Lost(why) => log::info!(
                    "Lost the link to {}, who was broadcasting ({why})",
                    b.info.name
                ),
            }
            self.announce_members();
            return None;
        }
        let p = self.peer.as_ref().filter(|p| p.serial == serial)?;
        let (info, key, heard) = (p.info.clone(), p.key, p.heard);
        let client = matches!(self.role, Role::Client { .. });
        let broadcaster = self.is_broadcaster();
        match end {
            LinkEnd::Bye if broadcaster => log::info!(
                "{} closed the room. Join again to keep broadcasting.",
                info.name
            ),
            LinkEnd::Bye => log::info!("{} left the room. You can keep playing.", info.name),
            LinkEnd::Lost(why) if client => {
                log::warn!(
                    "Lost the link to {}'s room ({why}). Reconnecting…",
                    info.name
                );
                if let Role::Client {
                    addr,
                    room,
                    broadcaster: false,
                } = &self.role
                {
                    self.lost = Some(LostRoom {
                        addr: *addr,
                        room: room.clone(),
                        since: Instant::now(),
                    });
                }
                self.unlink("connection lost", true);
                self.rejoin_gen += 1;
                self.rejoin = Some(Rejoin {
                    host: info,
                    since: Instant::now(),
                    attempt: 0,
                    error: None,
                    gen: self.rejoin_gen,
                });
                return Some(self.rejoin_gen);
            }
            LinkEnd::Lost(why) if key != 0 => {
                log::warn!(
                    "Lost the link to {} ({why}). You can keep playing while they reconnect.",
                    info.name
                );
                self.awaiting = Some(Awaiting {
                    info,
                    key,
                    since: Instant::now(),
                    heard,
                });
                self.unlink("connection lost", true);
                return None;
            }
            LinkEnd::Lost(why) => {
                log::warn!(
                    "Lost the link to {} ({why}). You can keep playing.",
                    info.name
                );
                self.unlink("connection lost", false);
                return None;
            }
        }
        self.unlink("partner left", false);
        // Left on purpose: nobody holds anyone's place.
        self.lost = None;
        self.own_moves.clear();
        self.end_client();
        None
    }

    /// Forget the link to the other DJ (or, on a client, the host), however
    /// it ended. Transfers with that booth stop. With `pause` (the link was
    /// lost, and the booth may be back) downloads from it carry on from
    /// where they got to when it is; otherwise they fail.
    fn unlink(&mut self, why: &str, pause: bool) {
        let peer = self.peer.take();
        if let Some(p) = &peer {
            p.conn.close();
            self.running.stop_with(p.info.id);
        }
        self.in_sync = None;
        self.late_from = i64::MAX;
        self.late.clear();
        self.announce_members();
        match peer {
            Some(p) if pause => self.pause_fetches(p.info.id),
            _ => {
                let pending: Vec<(TrackId, Purpose)> = self
                    .active_fetch
                    .drain()
                    .map(|(id, f)| (id, f.purpose))
                    .chain(self.fetch_queue.drain(..))
                    .collect();
                for (id, purpose) in pending {
                    self.finish_fetch(id, purpose, Err(why.into()));
                }
                self.fail_paused(why);
            }
        }
        for t in self
            .transfers
            .iter_mut()
            .filter(|t| t.state == TransferState::Active)
        {
            t.state = TransferState::Failed(why.into());
        }
    }

    /// Client: out of the room, back on its own.
    fn end_client(&mut self) {
        if !matches!(self.role, Role::Client { .. }) {
            return;
        }
        let broadcaster = self.is_broadcaster();
        self.rejoin = None;
        self.roster = Roster::default();
        self.role = Role::Solo;
        if broadcaster {
            // It only ever played the DJs' room: don't carry on with it.
            self.timeline = Timeline::new(self.now() - 10_000_000);
            self.publish();
        }
    }

    /// Is rejoin `gen` still wanted (this booth hasn't left, given up or
    /// joined again)?
    pub fn rejoin_wanted(&self, gen: u64) -> bool {
        self.rejoin.as_ref().is_some_and(|r| r.gen == gen)
    }

    /// Rejoin `gen` is about to try again. Returns how long to keep trying
    /// in all, or `None` if it's no longer wanted.
    pub fn rejoin_attempt(&mut self, gen: u64) -> Option<Duration> {
        let r = self.rejoin.as_mut().filter(|r| r.gen == gen)?;
        r.attempt += 1;
        log::info!("Reconnecting to {}'s room: try {}", r.host.name, r.attempt);
        Some(self.rejoin_for)
    }

    /// A try to get back in failed. With `give_up`, the booth stops trying
    /// and is on its own (a broadcaster's decks empty).
    pub fn rejoin_failed(&mut self, gen: u64, why: String, give_up: bool) {
        let Some(r) = self.rejoin.as_mut().filter(|r| r.gen == gen) else {
            return;
        };
        if give_up {
            log::warn!(
                "Couldn't get back into {}'s room: {why}. Join again to carry on.",
                r.host.name
            );
            self.end_client();
        } else {
            log::info!("Couldn't get back in yet: {why}");
            r.error = Some(why);
        }
    }

    /// Drop the room link the way a network failure does: no Bye, the
    /// connection just closes. For trying out getting back in.
    pub fn cut_link(&mut self) {
        if let Some(p) = &self.peer {
            log::warn!("Cutting the link to {} (test)", p.info.name);
            p.conn.close();
        }
    }

    pub fn leave(&mut self) {
        if let Some(r) = &self.rejoin {
            log::info!("Stopped reconnecting to {}'s room", r.host.name);
        }
        if self.peer.is_some() {
            // The host hears this booth leave, and stops holding its place.
            // Giving up while cut off leaves the host holding it, so joining
            // again by hand soon still brings its moves.
            self.lost = None;
            self.own_moves.clear();
        }
        self.send_all(Msg::Bye);
        // Give the Byes a moment to go out, so the others know this was on
        // purpose (the app may be closing).
        let deadline = Instant::now() + Duration::from_millis(300);
        let conns = self
            .peer
            .iter()
            .map(|p| &p.conn)
            .chain(self.broadcasters.iter().map(|b| &b.conn));
        for c in conns {
            while c.is_alive() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        for b in self.broadcasters.drain(..) {
            b.conn.close();
            self.running.stop_with(b.info.id);
        }
        self.unlink("partner left", false);
        self.awaiting = None;
        self.held = None;
        if let Role::Host { stop, .. } = &self.role {
            stop.store(true, Ordering::Relaxed);
        }
        self.end_client();
        self.role = Role::Solo;
        log::info!("Left the room");
    }

    pub fn on_control_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Event { ev } => {
                if ev.time >= self.late_from {
                    self.note_lateness(self.now() - ev.time);
                }
                let landed = self.insert_event(ev.clone(), false);
                if landed != Landed::Already {
                    self.after_event(&ev);
                    // Only a move from before one of this booth's loads can
                    // have the room drop it.
                    if self.my_loads.iter().any(|l| l.0 >= ev.time) {
                        self.check_my_loads();
                    }
                    self.publish();
                    // One that went in further back reaches the broadcasters
                    // with the remade room.
                    if landed == Landed::Now {
                        self.relay(&Msg::Event { ev });
                    }
                }
            }
            Msg::Track { info } => {
                self.relay(&Msg::Track { info: info.clone() });
                self.track_meta.insert(info.id, info);
            }
            Msg::Members { djs, broadcasters } => {
                if matches!(self.role, Role::Client { .. }) {
                    self.roster = Roster { djs, broadcasters };
                }
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
            Msg::NoFile { id } => self.fetch_failed(id, "partner doesn't have that file"),
            Msg::Bye => {}
            other => log::debug!("ignoring unexpected control message {other:?}"),
        }
    }

    /// A move reached this booth `us` after it happened (a scheduled one,
    /// ahead of time, counts as on time).
    fn note_lateness(&mut self, us: i64) {
        let us = us.max(0);
        let now = Instant::now();
        match self.late.back_mut() {
            Some((t, m)) if now.duration_since(*t) < Duration::from_secs(1) => *m = (*m).max(us),
            _ => self.late.push_back((now, us)),
        }
        while self
            .late
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > LATE_WINDOW)
        {
            self.late.pop_front();
        }
    }

    /// The latest any move reached this booth in the last [`LATE_WINDOW`].
    pub fn late_peak_us(&self) -> Option<i64> {
        self.late
            .iter()
            .filter(|(t, _)| t.elapsed() <= LATE_WINDOW)
            .map(|&(_, us)| us)
            .max()
    }

    /// Client: the host asked for one of our files.
    fn start_push(&mut self, id: TrackId) {
        let (Some(path), Role::Client { addr, room, .. }, Some(host)) = (
            self.servable.get(&id).cloned(),
            &self.role,
            self.peer.as_ref().map(|p| p.info.id),
        ) else {
            self.send(Msg::NoFile { id });
            return;
        };
        let (addr, room, me, weak) = (*addr, room.clone(), self.me.clone(), self.weak.clone());
        std::thread::spawn(move || {
            let Some(h) = weak.upgrade() else { return };
            let res = crate::files::push(&h, addr, &room, &me, host, id, &path);
            match res {
                // The link dropped, or the host asked again and a newer
                // upload carries on.
                Err(_) if crate::files::was_stopped(&res) => {}
                Err(e) => log::warn!("Upload failed: {}", crate::files::describe(&e)),
                Ok(()) => {}
            }
        });
    }

    // ------------------------------------------------------------ housekeeping

    fn maintenance(&mut self) {
        let now = self.now();
        self.timeline.compact(now - self.bcast_delay_us - 3_000_000);
        self.keep_history(now);
        let base = self.held.as_ref().unwrap_or(&self.timeline).base_time();
        self.my_loads.retain(|l| l.0 >= base);
        self.follow_grid_tempos(now);
        self.library.lock().save_grids();

        if matches!(self.role, Role::Host { .. })
            && (self.peer.is_some() || !self.broadcasters.is_empty())
            && self.last_digest.elapsed() > Duration::from_secs(2)
        {
            let t = now - 1_500_000;
            let digest = self.timeline.state_at(t).digest();
            self.send_all(Msg::Digest { t, digest });
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
            self.fetch_failed(id, "transfer stalled");
        }
        // Downloads a dropped link cut off fail once nobody is coming back:
        // this booth gave up getting back in, or the host stopped waiting.
        let coming_back = self.rejoin.is_some()
            || self
                .awaiting
                .as_ref()
                .is_some_and(|a| a.since.elapsed() < self.rejoin_for + self.link_timeout);
        if !self.paused.is_empty() && self.peer.is_none() && !coming_back {
            self.fail_paused("connection lost");
        }
        // A download cut off while the link stayed up carries on.
        let linked = self
            .peer
            .as_ref()
            .map(|p| p.info.id)
            .filter(|&id| id == self.paused_from);
        if let Some(id) = linked {
            if !self.paused.is_empty() && self.paused_since.elapsed() >= RESUME_AFTER {
                self.resume_fetches(id);
            }
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
        // Ask the host again for tracks on a deck that didn't download (a
        // broadcaster can ask for a partner's track before the host has it).
        if matches!(self.role, Role::Client { .. }) && self.peer.is_some() {
            let due: Vec<TrackId> = in_use
                .iter()
                .filter(|id| {
                    self.tracks
                        .get(id)
                        .and_then(|s| s.retry_at)
                        .is_some_and(|t| t <= Instant::now())
                })
                .copied()
                .collect();
            for id in due {
                self.ensure_track(id);
            }
        }
        self.publish();
    }

    /// Host: keep the held room (see `held`) while the other DJ is linked or
    /// expected back, reaching back to [`HELD_MARGIN_US`] before they were
    /// last heard from, and remake the live timeline from it after a late
    /// move. Client DJ: forget own moves that have surely reached the room,
    /// and all of them once nobody is holding this booth's place.
    fn keep_history(&mut self, now: i64) {
        let window = self.rejoin_for + self.link_timeout;
        let shared = |i: Instant| now - i.elapsed().as_micros() as i64;
        if matches!(self.role, Role::Host { .. }) {
            let heard = self.peer.as_ref().map(|p| p.heard).or_else(|| {
                self.awaiting
                    .as_ref()
                    .filter(|a| a.since.elapsed() < window)
                    .map(|a| a.heard)
            });
            if let Some(held) = &mut self.held {
                let keep_from = heard.map_or(i64::MAX, |h| shared(h) - HELD_MARGIN_US);
                held.compact(keep_from.min(self.timeline.base_time()));
            }
            if self.rebuild_due {
                self.rebuild_from_held();
            }
            if heard.is_none() && self.held.take().is_some() {
                log::debug!("No DJ to hold a place for: the room keeps its usual history");
            }
        } else if !self.keeps_moves() {
            self.own_moves.clear();
            if self
                .lost
                .as_ref()
                .is_some_and(|l| l.since.elapsed() >= window)
            {
                self.lost = None;
            }
        } else if let Some(p) = &self.peer {
            let keep_from = shared(p.heard) - HELD_MARGIN_US;
            self.own_moves.retain(|e| e.time >= keep_from);
        }
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
            if quiet || !room.in_sync_group(c) || !d.playing {
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

    fn deck_views(&self, room: &RoomState) -> [DeckView; DECKS] {
        std::array::from_fn(|d| {
            let state = room.decks[d];
            let slot = state.track.and_then(|id| self.tracks.get(&id));
            DeckView {
                state,
                meta: state.track.and_then(|id| self.track_meta.get(&id).cloned()),
                status: slot.map(|s| s.status.clone()),
                audio: slot.and_then(|s| s.audio.clone()),
                loading: self.loading[d].clone(),
            }
        })
    }

    /// The room `bcast_delay_us` ago, in step with the broadcast lane.
    /// Tracks that just left a deck are still decoded: the maintenance tick
    /// keeps any track a deck used in the last minute.
    pub fn broadcast_view(&self) -> BroadcastView {
        let t = self.now() - self.bcast_delay_us;
        let room = self.timeline.state_at(t);
        let mut decks = self.deck_views(&room);
        for d in &mut decks {
            d.loading = None; // this booth's library at work, not the room's
        }
        // The DJs only: on a DJ's booth "you" first, on a broadcaster the host.
        let djs = if self.is_broadcaster() {
            self.roster().djs
        } else {
            let mut djs = vec![self.me.clone()];
            djs.extend(self.peer.as_ref().map(|p| p.info.clone()));
            djs
        };
        BroadcastView {
            t,
            room,
            decks,
            djs,
        }
    }

    pub fn view(&self) -> SessionView {
        let now = self.now();
        let room = self.timeline.latest();
        let decks = self.deck_views(&room);
        let role = match &self.role {
            Role::Solo => RoleView::Solo,
            Role::Host { port, room, .. } => RoleView::Hosting {
                port: *port,
                room: room.clone(),
            },
            Role::Client {
                addr,
                room,
                broadcaster: false,
            } => RoleView::Joined {
                addr: addr.to_string(),
                room: room.clone(),
            },
            Role::Client {
                addr,
                room,
                broadcaster: true,
            } => RoleView::Broadcasting {
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
                roster: self.roster(),
                late_us: self.late_peak_us(),
                rtt_us: self.peer.as_ref().and_then(|p| p.rtt_us),
                quiet_for: self.peer.as_ref().map(|p| p.heard.elapsed()),
                clock_spread_us: self.est.spread(),
                in_sync: self.in_sync,
                rejoin: self.rejoin.as_ref().map(|r| RejoinView {
                    host: r.host.name.clone(),
                    attempt: r.attempt,
                    since: r.since.elapsed(),
                    error: r.error.clone(),
                }),
                // The other booth gives up after `rejoin_for` from when it
                // noticed, which can be up to a link timeout after this one.
                awaiting: self
                    .awaiting
                    .as_ref()
                    .filter(|a| a.since.elapsed() < self.rejoin_for + self.link_timeout)
                    .map(|a| a.info.name.clone()),
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
                    paused: self
                        .paused
                        .iter()
                        .filter(|(_, f)| {
                            matches!(f.purpose, Purpose::Share { share_id, .. }
                                if share_id == s.offer.share_id)
                        })
                        .count(),
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

/// A random number nobody can work out from this booth's other ones (its
/// id, say): std seeds every hasher from the operating system's randomness.
fn secret_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(fastrand::u64(..));
    h.finish().max(1)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn solo(root: &Path) -> SessionHandle {
        let paths = Paths::under(root);
        paths.ensure().unwrap();
        let lib = Arc::new(Mutex::new(Library::new(
            paths.shared_dir.clone(),
            paths.library_cache.clone(),
        )));
        Session::create("Alice".into(), paths, lib, Arc::new(SharedClock::new()))
    }

    /// Lateness counts moves made since joining, from when they happened to
    /// when they arrived; one scheduled ahead counts as on time.
    #[test]
    fn lateness_of_arriving_moves() {
        let root = std::env::temp_dir().join(format!("tandem-late-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            let ev = |time: i64, seq: u64| Msg::Event {
                ev: Event {
                    time,
                    origin: 7,
                    seq,
                    action: Action::Crossfader { v: 0.5 },
                },
            };
            let now = s.now();
            s.on_control_msg(ev(now - 300_000, 1));
            assert_eq!(s.late_peak_us(), None, "not in a room: not measured");

            s.late_from = now - 1_000_000;
            s.on_control_msg(ev(now - 2_000_000, 2)); // before joining
            assert_eq!(s.late_peak_us(), None);
            s.on_control_msg(ev(now - 300_000, 3));
            s.on_control_msg(ev(now - 120_000, 4));
            s.on_control_msg(ev(now + 200_000, 5)); // a relock, scheduled ahead
            let peak = s.late_peak_us().unwrap();
            assert!((300_000..400_000).contains(&peak), "peak {peak}");
            assert_eq!(s.view().link.late_us, Some(peak));
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A load onto a deck the room can hear is refused before anything is
    /// decoded or sent, with a notice saying why; once the deck is paused,
    /// it goes through.
    #[test]
    fn loads_onto_an_audible_deck_are_refused_with_a_notice() {
        let root = std::env::temp_dir().join(format!("tandem-refuse-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            let load = |n: u8| Action::Load {
                deck: 0,
                track: TrackId([n; 32]),
                duration: 300.0,
                cue: 0.0,
                grid: None,
                key: -1,
            };
            s.act(load(1));
            s.act(Action::Play { deck: 0 });
            assert!(s.take_notices().is_empty());
            let events = s.timeline.events().len();

            s.act(load(2));
            s.load_path(0, root.join("no such file.mp3"));
            assert_eq!(s.timeline.events().len(), events, "nothing sent");
            assert!(s.loading[0].is_none(), "nothing decoding");
            let notices = s.take_notices();
            assert_eq!(notices.len(), 2);
            assert_eq!(
                notices[0],
                "Deck A is playing: pause it or pull its fader down to load onto it."
            );
            assert!(s.take_notices().is_empty());

            s.act(Action::Pause { deck: 0 });
            s.act(load(2));
            assert!(s.take_notices().is_empty());
            assert_eq!(s.view().room.decks[0].track, Some(TrackId([2; 32])));
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The partner brought a deck up just before this booth loaded onto it,
    /// but their move arrived after the load: the room drops the load, and
    /// the DJ is told why.
    #[test]
    fn a_load_dropped_by_a_late_move_is_reported() {
        let root = std::env::temp_dir().join(format!("tandem-race-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            let load = |n: u8| Action::Load {
                deck: 1,
                track: TrackId([n; 32]),
                duration: 300.0,
                cue: 0.0,
                grid: None,
                key: -1,
            };
            let now = s.now();
            s.act_at(load(1), now - 1_000_000);
            s.act_at(Action::Volume { deck: 1, v: 0.0 }, now - 900_000);
            s.act_at(Action::Play { deck: 1 }, now - 800_000);
            // Fader down: this booth may load onto it.
            s.act_at(load(2), now - 100_000);
            assert_eq!(s.view().room.decks[1].track, Some(TrackId([2; 32])));
            assert!(s.take_notices().is_empty());
            // The partner's fader move from before the load arrives.
            s.on_control_msg(Msg::Event {
                ev: Event {
                    time: now - 200_000,
                    origin: 7,
                    seq: 1,
                    action: Action::Volume { deck: 1, v: 0.9 },
                },
            });
            let room = s.view().room;
            assert_eq!(room.decks[1].track, Some(TrackId([1; 32])));
            assert!(room.decks[1].playing);
            assert_eq!(
                s.take_notices(),
                vec!["Deck B is playing: pause it or pull its fader down to load onto it."]
            );
            // Told once.
            s.on_control_msg(Msg::Event {
                ev: Event {
                    time: now - 50_000,
                    origin: 7,
                    seq: 2,
                    action: Action::Crossfader { v: 0.4 },
                },
            });
            assert!(s.take_notices().is_empty());
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Getting back into a room that wasn't holding this booth's place
    /// replaces its timeline. A load it made while cut off never reached
    /// the room and isn't reported as refused (the room says the moves
    /// didn't come through); one that reached it and lost a race there is.
    #[test]
    fn loads_lost_to_a_dropped_link_arent_reported_as_refused() {
        let root = std::env::temp_dir().join(format!("tandem-rejoin-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            let load = |deck: u8, n: u8| Action::Load {
                deck,
                track: TrackId([n; 32]),
                duration: 300.0,
                cue: 0.0,
                grid: None,
                key: -1,
            };
            let now = s.now();
            // Made while the link was down: never reached the room.
            s.act_at(load(0, 1), now - 500_000);
            // Reached the room just before the link went.
            s.act_at(load(1, 2), now - 100_000);
            let reached = s.timeline.events().last().unwrap().clone();
            assert!(s.take_notices().is_empty());

            // The room as the host has it: A and B both playing out loud
            // (B since just before this booth's load).
            let mut room = Timeline::new(now - 10_000_000);
            let mut seq = 0;
            let mut host = |time: i64, action: Action| {
                seq += 1;
                room.insert(Event {
                    time,
                    origin: 7,
                    seq,
                    action,
                });
            };
            host(now - 2_000_000, load(0, 8));
            host(now - 2_000_000, load(1, 9));
            host(now - 1_900_000, Action::Play { deck: 0 });
            host(now - 200_000, Action::Play { deck: 1 });
            room.insert(reached);
            s.timeline = room;
            s.took_room_timeline();

            let decks = s.view().room.decks;
            assert_eq!(decks[0].track, Some(TrackId([8; 32])));
            assert_eq!(decks[1].track, Some(TrackId([9; 32])));
            assert_eq!(
                s.take_notices(),
                vec!["Deck B is playing: pause it or pull its fader down to load onto it."]
            );
            assert!(s.my_loads.is_empty());
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn load_onto(deck: u8, n: u8) -> Action {
        Action::Load {
            deck,
            track: TrackId([n; 32]),
            duration: 300.0,
            cue: 0.0,
            grid: None,
            key: -1,
        }
    }

    /// On the host, while the other DJ may get cut off, a move of theirs
    /// that arrives later than the live timeline reaches back goes in at its
    /// own time, from the held room, instead of now: a pause from 6 s ago
    /// stops the deck where it was 6 s ago.
    #[test]
    fn the_host_places_a_late_move_at_its_own_time() {
        let root = std::env::temp_dir().join(format!("tandem-late-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            s.role = Role::Host {
                port: 0,
                room: "LATE12".into(),
                stop: Arc::default(),
            };
            let now = s.now();
            s.timeline = Timeline::new(now - 20_000_000);
            s.held = Some(s.timeline.clone());
            s.act_at(load_onto(0, 1), now - 10_000_000);
            s.act_at(Action::Play { deck: 0 }, now - 9_000_000);
            s.timeline.compact(now - 3_500_000);
            s.on_control_msg(Msg::Event {
                ev: Event {
                    time: now - 6_000_000,
                    origin: 7,
                    seq: 1,
                    action: Action::Pause { deck: 0 },
                },
            });
            assert!(s.rebuild_due);
            s.rebuild_from_held();
            let deck = s.timeline.latest().decks[0];
            assert!(!deck.playing);
            assert!((deck.position_at(now) - 3.0).abs() < 1e-9);
            assert_eq!(s.timeline.base_time(), now - 3_500_000);
            // Held and live agree on everything after the live base.
            let held = s.held.as_ref().unwrap();
            for t in [now - 3_000_000, now] {
                assert_eq!(held.state_at(t), s.timeline.state_at(t));
            }
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Getting back in, the host has put this booth's moves from while it
    /// was cut off into the room, reaching back as far as they do. A load
    /// among them that the room refused (the partner's deck was playing out
    /// loud by then) is reported, a move made after they were sent goes in
    /// too, and the room is compacted back to the usual length.
    #[test]
    fn a_merged_room_reports_refused_loads_and_takes_later_moves() {
        let root = std::env::temp_dir().join(format!("tandem-merged-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            let (now, me) = (s.now(), s.me.id);
            let mine = |seq: u64, time: i64, action: Action| Event {
                time,
                origin: me,
                seq,
                action,
            };
            let load = mine(1, now - 8_000_000, load_onto(1, 3));
            let fader = mine(2, now - 7_000_000, Action::Volume { deck: 0, v: 0.4 });
            // Made while the clock was being measured, after the Hello.
            let since = mine(3, now - 500_000, Action::Crossfader { v: 0.3 });
            s.own_moves = VecDeque::from([load.clone(), fader.clone(), since]);
            let mut room = Timeline::new(now - 20_000_000);
            for (seq, time, action) in [
                (1, now - 12_000_000, load_onto(1, 2)),
                (2, now - 10_000_000, Action::Play { deck: 1 }),
            ] {
                room.insert(Event {
                    time,
                    origin: 7,
                    seq,
                    action,
                });
            }
            room.insert(load);
            room.insert(fader);
            s.took_merged_room(room);
            assert_eq!(
                s.take_notices(),
                vec!["Deck B is playing: pause it or pull its fader down to load onto it."]
            );
            let room = s.timeline.latest();
            assert_eq!(room.decks[1].track, Some(TrackId([2; 32])));
            assert_eq!(room.decks[0].volume, 0.4);
            assert_eq!(room.crossfader, 0.3);
            assert!(s.timeline.base_time() > now - 5_000_000, "not compacted");
            assert!(s.my_loads.is_empty());
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A booth keeps its own moves for a room only while one may be holding
    /// its place, and sends the tracks they load with them.
    #[test]
    fn keeps_its_moves_only_while_a_room_holds_its_place() {
        let root = std::env::temp_dir().join(format!("tandem-keep-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            s.act(Action::Volume { deck: 0, v: 0.5 });
            assert!(s.own_moves.is_empty());
            assert!(s.cut_off().events.is_empty());

            s.lost = Some(LostRoom {
                addr: "127.0.0.1:47800".parse().unwrap(),
                room: "KEEP12".into(),
                since: Instant::now(),
            });
            let info = TrackInfo {
                id: TrackId([5; 32]),
                title: "Five".into(),
                artist: String::new(),
                bpm: None,
                duration: 300.0,
                size: 1,
                file_name: "five.wav".into(),
                grid: None,
                key: None,
            };
            s.track_meta.insert(info.id, info.clone());
            s.act(Action::Volume { deck: 0, v: 0.6 });
            s.act(load_onto(1, 5));
            let cut = s.cut_off();
            assert_eq!(cut.events.len(), 2);
            assert_eq!(cut.tracks, [info]);

            // The room stopped holding it.
            s.rejoin_for = Duration::ZERO;
            s.link_timeout = Duration::ZERO;
            let now = s.now();
            s.keep_history(now);
            assert!(s.own_moves.is_empty() && s.lost.is_none());
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The broadcast window shows the room as the broadcast lane plays it:
    /// a move made now isn't on it yet, one made a delay ago is.
    #[test]
    fn broadcast_view_lags_by_the_broadcast_delay() {
        let root = std::env::temp_dir().join(format!("tandem-bview-{}", fastrand::u64(..)));
        let h = solo(&root);
        {
            let mut s = h.lock();
            s.bcast_delay_us = 500_000;
            let now = s.now();
            s.act_at(Action::Crossfader { v: 0.1 }, now - 800_000);
            s.act_at(Action::Crossfader { v: 0.9 }, now);

            assert_eq!(s.view().room.crossfader, 0.9);
            let b = s.broadcast_view();
            assert!(
                (now - 500_000..now - 400_000).contains(&b.t),
                "t = {}",
                b.t - now
            );
            assert_eq!(b.room.crossfader, 0.1);
            assert_eq!(b.room.xf_touch, s.me.id);
            assert_eq!(b.dj_name(s.me.id), Some("Alice"));
            assert_eq!(b.dj_name(0), None);
        }
        drop(h);
        let _ = std::fs::remove_dir_all(&root);
    }
}
