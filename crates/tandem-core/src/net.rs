//! Rooms over TCP: one DJ hosts, and up to three more join, for
//! [`MAX_DJS`] in all.
//!
//! The host's clock *is* the room clock. A joining DJ measures its offset
//! with pings (see [`crate::clock`]) before applying the room state, and
//! keeps pinging so the estimate tracks drift.
//!
//! The host is the hub: every joining booth's one link is to it. It passes
//! each DJ's moves, track details and shares on to the other DJs, and their
//! tracks too (a joining DJ gets another's track from the host, which gets
//! it from the DJ who has it).
//!
//! Up to [`MAX_BROADCASTERS`] more booths can join as broadcasters: they
//! sync their clock and receive the room exactly like a joining DJ, so they
//! can play the broadcast feed and show the broadcast window. They can't
//! change anything: the host answers their pings and resync requests and
//! ignores the rest.
//!
//! A booth whose link to the host drops without anyone leaving (no `Bye`)
//! gets back in by itself: see [`rejoin`]. The host knows it's the same
//! booth by its id and booth key, and gives it its seat back. A DJ's booth
//! brings the moves it made while cut off, and the host puts them into the
//! room at the times they were made (see `Session::take_cut_off`).

use crate::clock::ClockEstimator;
use crate::model::Timeline;
use crate::protocol::{
    read_msg, write_msg, ConnKind, CutOff, Msg, PeerInfo, TrackInfo, DEFAULT_PORT, PROTOCOL_VERSION,
};
use crate::session::{LinkEnd, Session, SessionHandle};
use anyhow::{anyhow, bail, Context};
use std::io::BufWriter;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// A live control connection. Sending never blocks: messages go to a writer thread.
pub struct PeerConn {
    tx: Sender<Msg>,
    stream: TcpStream,
    alive: Arc<AtomicBool>,
}

impl PeerConn {
    fn new(stream: TcpStream) -> anyhow::Result<Self> {
        let (tx, rx) = channel::<Msg>();
        let alive = Arc::new(AtomicBool::new(true));
        let mut w = BufWriter::new(stream.try_clone()?);
        let alive2 = alive.clone();
        thread::Builder::new()
            .name("tandem-net-writer".into())
            .spawn(move || {
                for m in rx {
                    let bye = matches!(m, Msg::Bye);
                    if write_msg(&mut w, &m, &[]).is_err() {
                        break;
                    }
                    // Nothing follows a Bye: end the connection behind it,
                    // so the other end reads it before the connection closes.
                    if bye {
                        let _ = w.get_ref().shutdown(Shutdown::Write);
                        break;
                    }
                }
                alive2.store(false, Ordering::Relaxed);
            })?;
        Ok(Self { tx, stream, alive })
    }

    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
    }

    /// Another handle for queueing messages on this connection.
    fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    /// False once the writer has stopped: after a Bye, or when the
    /// connection broke.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// How many DJs a room takes, the host included.
pub const MAX_DJS: usize = 4;

/// How many booths can join a room as broadcasters, besides the DJs.
pub const MAX_BROADCASTERS: usize = 4;

/// A control link that hears nothing for this long is dropped.
pub const LINK_TIMEOUT: Duration = Duration::from_secs(15);

/// After this long without a Ping or Pong, the link has stalled and the last
/// round trip is out of date (Pings run about once a second).
pub const LINK_QUIET: Duration = Duration::from_secs(3);

/// How long a booth whose link dropped keeps trying to get back in.
pub const REJOIN_FOR: Duration = Duration::from_secs(5 * 60);

/// The wait before the first try to get back in. It doubles after each
/// failed try, up to [`REJOIN_MAX_WAIT`].
const REJOIN_FIRST_WAIT: Duration = Duration::from_secs(1);
const REJOIN_MAX_WAIT: Duration = Duration::from_secs(5);

/// A host from before booth keys keeps a booth's old link (and so its seat)
/// until that link times out, and turns the booth away as "room full" until
/// then. Keep trying that long before taking it as a real answer.
const SEAT_GRACE: Duration = Duration::from_secs(25);

/// The host turned this booth away (wrong room code, another version, room
/// full). A booth getting back into a room stops trying at one of these.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

impl Rejected {
    /// The seat was taken (see [`Session::seat_for`]).
    fn seat_taken(&self) -> bool {
        self.0.starts_with(ROOM_FULL)
    }
}

/// How every "room full" answer starts.
pub(crate) const ROOM_FULL: &str = "This room already has";

pub fn random_room_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..6)
        .map(|_| ALPHABET[fastrand::usize(..ALPHABET.len())] as char)
        .collect()
}

/// Start hosting a room. Returns the port actually bound (pass 0 for any).
pub fn host(h: &SessionHandle, port: u16, room: &str) -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("couldn't listen on port {port}"))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    h.lock().set_hosting(port, room.to_string(), stop.clone());
    log::info!("Hosting room {room} on port {port}");
    let (h, room) = (h.clone(), room.to_string());
    thread::Builder::new()
        .name("tandem-accept".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, addr)) => {
                        let (h, room) = (h.clone(), room.clone());
                        thread::spawn(move || {
                            if let Err(e) = handle_incoming(h, s, &room) {
                                if is_disconnect_err(&e) {
                                    log::debug!("connection from {addr} closed");
                                } else {
                                    log::warn!("Connection from {addr} ended: {e:#}");
                                }
                            }
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50))
                    }
                    Err(e) => {
                        log::warn!("accept failed: {e}");
                        thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            log::info!("Stopped hosting");
        })?;
    Ok(port)
}

fn handle_incoming(h: SessionHandle, mut s: TcpStream, room: &str) -> anyhow::Result<()> {
    s.set_nonblocking(false)?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (hello, _) = read_msg(&mut s)?;
    let Msg::Hello {
        proto,
        kind,
        name,
        room: their_room,
        peer_id,
        broadcaster,
        booth_key,
        cut_off,
    } = hello
    else {
        bail!("expected hello")
    };
    let reject = |s: &mut TcpStream, reason: &str| {
        let _ = write_msg(
            s,
            &Msg::Reject {
                reason: reason.into(),
            },
            &[],
        );
    };
    if proto != PROTOCOL_VERSION {
        reject(
            &mut s,
            "Different Tandem versions. Both of you need the same release.",
        );
        bail!("protocol mismatch ({proto})");
    }
    if !their_room.trim().eq_ignore_ascii_case(room) {
        reject(&mut s, "Wrong room code.");
        bail!("wrong room code");
    }
    let info = PeerInfo { id: peer_id, name };
    match kind {
        ConnKind::Files => crate::files::serve(&h, s, &info, broadcaster),
        ConnKind::Control if closed_already(&s) => {
            log::debug!(
                "{} gave up on this connection before it got here",
                info.name
            );
            Ok(())
        }
        ConnKind::Control => host_control(h, s, info, booth_key, broadcaster, cut_off),
    }
}

/// True if the booth has already closed this connection. When this host was
/// stalled (frozen, or its PC busy), a booth trying to get back in leaves a
/// connection behind at each try that timed out, and they all arrive at
/// once. Seating one would throw out the booth's live try.
fn closed_already(s: &TcpStream) -> bool {
    if s.set_nonblocking(true).is_err() {
        return false;
    }
    // A booth that's waiting for its Welcome sends nothing more.
    let closed = match s.peek(&mut [0u8; 1]) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => is_disconnect(&e),
    };
    let _ = s.set_nonblocking(false);
    closed
}

fn host_control(
    h: SessionHandle,
    mut s: TcpStream,
    info: PeerInfo,
    key: u64,
    broadcaster: bool,
    cut_off: CutOff,
) -> anyhow::Result<()> {
    let (clock, reply, serial, timeout) = {
        let mut ses = h.lock();
        if let Err(reason) = ses.seat_for(info.id, key, broadcaster) {
            drop(ses);
            let _ = write_msg(&mut s, &Msg::Reject { reason }, &[]);
            bail!("room full");
        }
        let conn = PeerConn::new(s.try_clone()?)?;
        let reply = conn.sender();
        // A DJ getting back in: what it did while cut off goes into the
        // room before the room goes to it.
        let merged = !broadcaster && ses.take_cut_off(info.id, key, cut_off);
        conn.send(ses.welcome(merged));
        let serial = if broadcaster {
            ses.add_broadcaster(conn, info.clone(), key)
        } else {
            ses.add_peer(conn, info.clone(), key)
        };
        (ses.clock(), reply, serial, ses.link_timeout)
    };
    s.set_read_timeout(Some(timeout))?;
    let end = loop {
        match read_msg(&mut s) {
            Ok((Msg::Ping { id, t1, rtt }, _)) => {
                let remote = clock.now();
                let _ = reply.send(Msg::Pong { id, t1, remote });
                h.lock().on_peer_ping(serial, rtt);
            }
            Ok((Msg::Bye, _)) => break LinkEnd::Bye,
            Ok((Msg::ResyncRequest, _)) if broadcaster => {
                let timeline = h.lock().timeline().clone();
                let _ = reply.send(Msg::Resync { timeline });
            }
            // A broadcaster can't change the room, whatever it sends.
            Ok((m, _)) if broadcaster => {
                log::debug!("ignoring {m:?} from broadcaster {}", info.name)
            }
            Ok((m, _)) => h.lock().on_control_msg(serial, m),
            Err(e) => break LinkEnd::Lost(why_lost(&e, timeout)),
        }
    };
    h.lock().on_link_ended(serial, end);
    Ok(())
}

fn is_disconnect(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe
    )
}

/// True if an error is just the other side going away.
pub fn is_disconnect_err(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .map(is_disconnect)
            .unwrap_or(false)
    })
}

/// Why a link broke, in a few words for the log.
fn why_lost(e: &std::io::Error, timeout: Duration) -> String {
    use std::io::ErrorKind::*;
    if matches!(e.kind(), WouldBlock | TimedOut) {
        format!("nothing heard for {} s", timeout.as_secs())
    } else if is_disconnect(e) {
        "connection closed".into()
    } else {
        e.to_string()
    }
}

/// Parse "host", "host:port" or "[v6]:port".
pub fn resolve(addr: &str) -> anyhow::Result<SocketAddr> {
    let a = addr.trim();
    if a.is_empty() {
        bail!("enter the host's address");
    }
    let with_port = if a.parse::<SocketAddr>().is_ok()
        || (a.contains(':') && !a.contains("::") && a.matches(':').count() == 1)
    {
        a.to_string()
    } else {
        format!("{a}:{DEFAULT_PORT}")
    };
    with_port
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("couldn't resolve {a}"))
}

pub(crate) fn connect(
    addr: SocketAddr,
    kind: ConnKind,
    room: &str,
    me: &PeerInfo,
    booth_key: u64,
    broadcaster: bool,
) -> anyhow::Result<TcpStream> {
    let cut_off = CutOff::default();
    connect_with(addr, kind, room, me, booth_key, broadcaster, cut_off)
}

/// [`connect`], with a DJ's booth's moves from while its link to the room
/// was down in the Hello (see [`CutOff`]).
fn connect_with(
    addr: SocketAddr,
    kind: ConnKind,
    room: &str,
    me: &PeerInfo,
    booth_key: u64,
    broadcaster: bool,
    cut_off: CutOff,
) -> anyhow::Result<TcpStream> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(6))
        .with_context(|| format!("couldn't reach {addr}"))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    write_msg(
        &mut s,
        &Msg::Hello {
            proto: PROTOCOL_VERSION,
            kind,
            name: me.name.clone(),
            room: room.trim().to_uppercase(),
            peer_id: me.id,
            broadcaster,
            booth_key,
            cut_off,
        },
        &[],
    )?;
    Ok(s)
}

/// Where a joining booth's room is.
#[derive(Clone, Debug)]
pub struct RoomAddr {
    pub addr: SocketAddr,
    /// The room code, upper case.
    pub room: String,
    /// Joined only to broadcast.
    pub broadcaster: bool,
}

/// A control connection the host has welcomed, with the clock measured,
/// ready to become this booth's link to the room.
struct Welcomed {
    stream: TcpStream,
    host: PeerInfo,
    timeline: Timeline,
    tracks: Vec<TrackInfo>,
    /// The host put this booth's `CutOff` moves into the room.
    merged: bool,
    est: ClockEstimator,
    offset_us: i64,
    /// What arrived while the clock was being measured.
    buffered: Vec<Msg>,
}

impl Welcomed {
    /// Not wanted after all: tell the host, so it doesn't keep the seat.
    fn abandon(mut self) {
        let _ = write_msg(&mut self.stream, &Msg::Bye, &[]);
        let _ = self.stream.shutdown(Shutdown::Write);
    }
}

/// Say hello, get the room, and measure the clock offset (about a second).
/// Changes nothing in the session: [`start_link`] does that.
fn handshake(h: &SessionHandle, to: &RoomAddr) -> anyhow::Result<Welcomed> {
    let (me, key, clock, cut_off) = {
        let s = h.lock();
        let cut_off = match to.broadcaster {
            true => CutOff::default(),
            false => s.cut_off(),
        };
        (s.me.clone(), s.booth_key(), s.clock(), cut_off)
    };
    if !cut_off.events.is_empty() {
        log::info!(
            "Sending the {} moves made since the link was last heard",
            cut_off.events.len()
        );
    }
    let mut s = connect_with(
        to.addr,
        ConnKind::Control,
        &to.room,
        &me,
        key,
        to.broadcaster,
        cut_off,
    )?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (first, _) = read_msg(&mut s).context("no answer from host")?;
    let (host, timeline, tracks, merged) = match first {
        Msg::Welcome {
            host,
            timeline,
            tracks,
            merged,
        } => (host, timeline, tracks, merged),
        Msg::Reject { reason } => return Err(Rejected(reason).into()),
        other => bail!("unexpected reply {other:?}"),
    };

    // Measure the clock offset before applying the room state.
    let mut est = ClockEstimator::default();
    let mut buffered = Vec::new();
    for i in 0..10u64 {
        let t1 = clock.local();
        write_msg(
            &mut s,
            &Msg::Ping {
                id: i,
                t1,
                rtt: est.last_rtt(),
            },
            &[],
        )?;
        loop {
            let (m, _) = read_msg(&mut s)?;
            match m {
                Msg::Pong { t1, remote, .. } => {
                    est.add(t1, remote, clock.local());
                    break;
                }
                other => buffered.push(other),
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    let best = est.best().ok_or_else(|| anyhow!("clock sync failed"))?;
    log::info!(
        "Clock synced to {}: round trip {:.1} ms",
        host.name,
        best.rtt_us as f64 / 1000.0
    );
    Ok(Welcomed {
        stream: s,
        host,
        timeline,
        tracks,
        merged,
        est,
        offset_us: best.offset_us,
        buffered,
    })
}

/// Make a welcomed connection this booth's link to the room: take the room
/// state and start the reader and pinger. `rejoin` when getting back in
/// after the link dropped.
fn start_link(
    h: &SessionHandle,
    ses: &mut Session,
    w: Welcomed,
    to: RoomAddr,
    rejoin: bool,
) -> anyhow::Result<()> {
    let Welcomed {
        stream,
        host,
        timeline,
        tracks,
        merged,
        est,
        offset_us,
        buffered,
    } = w;
    let conn = PeerConn::new(stream.try_clone()?)?;
    let timeout = ses.link_timeout;
    let mut reader = stream;
    reader.set_read_timeout(Some(timeout))?;
    let clock = ses.clock();
    clock.start(offset_us);
    let serial = ses.on_joined(
        conn,
        host,
        timeline,
        tracks,
        est,
        to.clone(),
        rejoin,
        merged,
    );
    for m in buffered {
        ses.on_control_msg(serial, m);
    }

    // Reader. When the link breaks without a Bye, it stays on to get the
    // booth back into the room.
    let (h2, clock2) = (h.clone(), clock.clone());
    thread::Builder::new()
        .name("tandem-net-reader".into())
        .spawn(move || {
            let end = loop {
                match read_msg(&mut reader) {
                    Ok((Msg::Pong { t1, remote, .. }, _)) => {
                        let t2 = clock2.local();
                        h2.lock().on_pong(t1, remote, t2);
                    }
                    Ok((Msg::Bye, _)) => break LinkEnd::Bye,
                    Ok((m, _)) => h2.lock().on_control_msg(serial, m),
                    Err(e) => break LinkEnd::Lost(why_lost(&e, timeout)),
                }
            };
            drop(reader);
            let again = h2.lock().on_link_ended(serial, end);
            if let Some(gen) = again {
                rejoin_loop(&h2, gen, to);
            }
        })?;

    // Pinger: fast at first, then once a second.
    let h3 = h.clone();
    thread::Builder::new()
        .name("tandem-net-pinger".into())
        .spawn(move || {
            let start = Instant::now();
            let mut id = 100u64;
            loop {
                let period = if start.elapsed() < Duration::from_secs(10) {
                    250
                } else {
                    1000
                };
                thread::sleep(Duration::from_millis(period));
                let ses = h3.lock();
                let Some(conn) = ses.link(serial) else { break };
                if !conn.is_alive() {
                    break;
                }
                id += 1;
                conn.send(Msg::Ping {
                    id,
                    t1: clock.local(),
                    rtt: ses.last_rtt(),
                });
            }
        })?;
    Ok(())
}

/// Join a host's room, as one of its DJs or, with `broadcaster`, only to
/// play and show the set for a stream. Blocks for the handshake and initial
/// clock sync (about a second), then runs in the background.
pub fn join(
    h: &SessionHandle,
    addr_text: &str,
    room: &str,
    broadcaster: bool,
) -> anyhow::Result<()> {
    let to = RoomAddr {
        addr: resolve(addr_text)?,
        room: room.trim().to_uppercase(),
        broadcaster,
    };
    let w = handshake(h, &to)?;
    let mut ses = h.lock();
    start_link(h, &mut ses, w, to, false)
}

/// Get a booth whose link dropped back into the same room. It tries again
/// and again, waiting a little longer each time (1, 2, 4, then every 5 s),
/// for the session's `rejoin_for` ([`REJOIN_FOR`]). Getting back in is a
/// fresh join with the same booth id and key: the host gives the booth its
/// seat back and the room as it is now. It stops at an answer from the host
/// that won't change (wrong room code, another version, someone else in the
/// seat), or when this booth leaves or gives up (`gen` no longer current).
fn rejoin_loop(h: &SessionHandle, gen: u64, to: RoomAddr) {
    let since = Instant::now();
    let mut wait = REJOIN_FIRST_WAIT;
    loop {
        let wake = Instant::now() + wait;
        while Instant::now() < wake {
            thread::sleep(Duration::from_millis(50));
            if !h.lock().rejoin_wanted(gen) {
                return;
            }
        }
        let Some(rejoin_for) = h.lock().rejoin_attempt(gen) else {
            return;
        };
        let res = handshake(h, &to);
        let mut ses = h.lock();
        let failed = match res {
            Ok(w) if !ses.rejoin_wanted(gen) => {
                drop(ses);
                w.abandon();
                return;
            }
            Ok(w) => match start_link(h, &mut ses, w, to.clone(), true) {
                Ok(()) => return,
                Err(e) => e,
            },
            Err(e) => e,
        };
        let refused = failed.downcast_ref::<Rejected>();
        let give_up = refused.is_some_and(|r| !r.seat_taken() || since.elapsed() > SEAT_GRACE)
            || since.elapsed() >= rejoin_for;
        ses.rejoin_failed(gen, why_not_back(&failed, &to), give_up);
        if give_up {
            return;
        }
        wait = (wait * 2).min(REJOIN_MAX_WAIT);
    }
}

/// Why a try to get back in failed, in a few words for the top bar.
fn why_not_back(e: &anyhow::Error, to: &RoomAddr) -> String {
    use std::io::ErrorKind::*;
    if let Some(r) = e.downcast_ref::<Rejected>() {
        return r.0.clone();
    }
    let io = e.chain().find_map(|c| c.downcast_ref::<std::io::Error>());
    match io.map(|e| e.kind()) {
        Some(ConnectionRefused) => format!("nothing is hosting at {}", to.addr),
        Some(TimedOut | WouldBlock) => format!("no answer from {}", to.addr),
        Some(UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe) => {
            "the connection closed".into()
        }
        _ => format!("{e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Paths;
    use crate::files::Landing;
    use crate::library::Library;
    use crate::model::{Action, TrackId};
    use crate::session::{RoleView, ShareState, TrackStatus, Transfer, TransferState};
    use crate::time::SharedClock;
    use parking_lot::Mutex;
    use std::io::{Read, Write};

    fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if f() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("timed out waiting for {what}");
    }

    struct Rig {
        root: std::path::PathBuf,
    }

    impl Rig {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("tandem-{tag}-{}", fastrand::u64(..)));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        /// A booth whose links time out after `timeout`.
        fn booth(&self, name: &str, skew_us: i64, timeout: Duration) -> SessionHandle {
            let paths = Paths::under(&self.root.join(name));
            paths.ensure().unwrap();
            let lib = Arc::new(parking_lot::Mutex::new(Library::new(
                paths.shared_dir.clone(),
                paths.library_cache.clone(),
            )));
            let h = Session::create(
                name.into(),
                paths,
                lib,
                Arc::new(SharedClock::with_skew(skew_us)),
            );
            h.lock().link_timeout = timeout;
            h
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A TCP relay between booths and the host that fails the way the
    /// internet does: it can close every connection through it (`sever`),
    /// or go dark (`blackhole`), when the connections through it go silent
    /// for good without closing and new ones are dropped until `restore`.
    struct Relay {
        port: u16,
        state: Arc<Mutex<RelayState>>,
        stop: Arc<AtomicBool>,
    }

    struct RelayState {
        up: bool,
        upstream: u16,
        /// Both sockets of every connection, and whether it went dark.
        pipes: Vec<(TcpStream, TcpStream, Arc<AtomicBool>)>,
    }

    impl Relay {
        fn new(upstream: u16) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let state = Arc::new(Mutex::new(RelayState {
                up: true,
                upstream,
                pipes: Vec::new(),
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let (st, stop2) = (state.clone(), stop.clone());
            thread::spawn(move || {
                while !stop2.load(Ordering::Relaxed) {
                    let Ok((down, _)) = listener.accept() else {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    down.set_nonblocking(false).unwrap();
                    let mut s = st.lock();
                    if !s.up {
                        continue; // dropped: the booth sees the connection close
                    }
                    let Ok(up) = TcpStream::connect(("127.0.0.1", s.upstream)) else {
                        continue;
                    };
                    let dark = Arc::new(AtomicBool::new(false));
                    for (a, b) in [(&down, &up), (&up, &down)] {
                        let (mut a, mut b) = (a.try_clone().unwrap(), b.try_clone().unwrap());
                        let dark = dark.clone();
                        a.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
                        thread::spawn(move || {
                            let mut buf = vec![0u8; 64 * 1024];
                            loop {
                                if dark.load(Ordering::Relaxed) {
                                    return; // silent, never closed
                                }
                                match a.read(&mut buf) {
                                    Ok(0) => {
                                        let _ = b.shutdown(Shutdown::Write);
                                        return;
                                    }
                                    Ok(n) => {
                                        if dark.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        if b.write_all(&buf[..n]).is_err() {
                                            return;
                                        }
                                    }
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) => {}
                                    Err(_) => {
                                        let _ = b.shutdown(Shutdown::Both);
                                        return;
                                    }
                                }
                            }
                        });
                    }
                    s.pipes.push((down, up, dark));
                }
            });
            Self { port, state, stop }
        }

        fn addr(&self) -> String {
            format!("127.0.0.1:{}", self.port)
        }

        /// Close every connection through the relay, as a router that drops
        /// its connections does: both ends see it close at once.
        fn sever(&self) {
            for (a, b, _) in self.state.lock().pipes.drain(..) {
                let _ = a.shutdown(Shutdown::Both);
                let _ = b.shutdown(Shutdown::Both);
            }
        }

        /// The connection drops out: nothing more gets through either way,
        /// neither end hears it close, and new connections fail.
        fn blackhole(&self) {
            let mut s = self.state.lock();
            s.up = false;
            for (_, _, dark) in &s.pipes {
                dark.store(true, Ordering::Relaxed);
            }
        }

        fn restore(&self) {
            self.state.lock().up = true;
        }

        /// Send new connections to another host.
        fn retarget(&self, port: u16) {
            self.state.lock().upstream = port;
        }
    }

    impl Drop for Relay {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.sever();
        }
    }

    fn digests_match(a: &SessionHandle, b: &SessionHandle) -> bool {
        let t = a.lock().now() - 100_000;
        let da = a.lock().state_at(t).digest();
        let db = b.lock().state_at(t).digest();
        da == db
    }

    fn rejoining(h: &SessionHandle) -> bool {
        h.lock().view().link.rejoin.is_some()
    }

    /// In the room with its link up, and not getting back in.
    fn linked(h: &SessionHandle) -> bool {
        let v = h.lock().view();
        v.link.linked() && v.link.rejoin.is_none()
    }

    fn names(v: &[PeerInfo]) -> Vec<String> {
        v.iter().map(|p| p.name.clone()).collect()
    }

    /// The router drops the connection: both ends see it close. The host
    /// drops the joiner and waits for it; the joiner gets back in by itself
    /// and takes the room as the host has it, including what the host did
    /// meanwhile, and its own moves reach the host again.
    #[test]
    fn rejoins_after_the_connection_closes() {
        let rig = Rig::new("rejoin-close");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 3_600_000_000, t);
        let port = host(&alice, 0, "CLOSE1").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "close1", false).unwrap();
        let bob_id = bob.lock().me.id;
        alice.lock().act(Action::Volume { deck: 0, v: 0.5 });
        bob.lock().act(Action::Crossfader { v: 0.25 });
        wait_for("both booths to agree", Duration::from_secs(5), || {
            alice.lock().latest().crossfader == 0.25 && bob.lock().latest().decks[0].volume == 0.5
        });

        relay.sever();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        wait_for("Alice to notice", Duration::from_secs(5), || {
            alice.lock().view().link.awaiting == ["Bob"]
        });
        assert!(matches!(
            bob.lock().view().link.role,
            RoleView::Joined { .. }
        ));
        alice.lock().act(Action::Volume { deck: 1, v: 0.75 });

        wait_for("Bob to get back in", Duration::from_secs(10), || {
            linked(&bob)
        });
        wait_for("Alice to see Bob back", Duration::from_secs(5), || {
            let v = alice.lock().view();
            v.link.links.iter().any(|l| l.info.id == bob_id) && v.link.awaiting.is_empty()
        });
        assert_eq!(bob.lock().latest().decks[1].volume, 0.75);
        assert!(
            digests_match(&alice, &bob),
            "room state differs after rejoining"
        );
        let r = alice.lock().roster();
        assert_eq!(names(&r.djs), ["Alice", "Bob"]);

        bob.lock().act(Action::Crossfader { v: 0.5 });
        wait_for("Bob's move to reach Alice", Duration::from_secs(5), || {
            alice.lock().latest().crossfader == 0.5
        });
        bob.lock().leave();
        alice.lock().leave();
    }

    /// The connection goes silent, and the host still holds the joiner's
    /// old link when it comes back. The returning booths (a DJ and a
    /// broadcaster) take their own seats back: no second Bob or Carol, and
    /// the old link closing later doesn't throw them out. The move the DJ
    /// made while cut off comes into the room and reaches the broadcaster;
    /// the track it already had is still there, without downloading it again.
    #[test]
    fn rejoins_after_a_silent_stall_replacing_the_old_link() {
        let rig = Rig::new("rejoin-stall");
        // The host gives up on a silent link only after the booths are back.
        let alice = rig.booth("Alice", 0, Duration::from_secs(7));
        let bob = rig.booth("Bob", -7_200_000_000, Duration::from_secs(2));
        let carol = rig.booth("Carol", 1_800_000_000, Duration::from_secs(2));
        let port = host(&alice, 0, "STALL1").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "STALL1", false).unwrap();
        join(&carol, &relay.addr(), "STALL1", true).unwrap();

        let music = rig.root.join("music");
        std::fs::create_dir_all(&music).unwrap();
        let wav = music.join("Tone.wav");
        crate::audio::write_wav(&wav, 44_100, &crate::audio::test_signal(44_100, 4.0, 440.0))
            .unwrap();
        alice.lock().load_path(0, wav);
        let mut id = None;
        wait_for("Alice to load", Duration::from_secs(20), || {
            id = alice.lock().latest().decks[0].track;
            id.is_some()
        });
        let id = id.unwrap();
        wait_for(
            "Bob and Carol to get the track",
            Duration::from_secs(30),
            || {
                bob.lock().track_status(id) == Some(TrackStatus::Ready)
                    && carol.lock().track_status(id) == Some(TrackStatus::Ready)
            },
        );
        alice.lock().act(Action::Play { deck: 0 });
        wait_for("everyone to play it", Duration::from_secs(5), || {
            bob.lock().latest().decks[0].playing
                && carol.lock().latest().decks[0].playing
                && digests_match(&alice, &bob)
                && digests_match(&alice, &carol)
        });

        // Alice holds a loop roll through it all: it's room state.
        alice.lock().act(Action::Roll {
            deck: 0,
            beats: 1.0,
            on: true,
        });
        wait_for("everyone to hear the roll", Duration::from_secs(5), || {
            bob.lock().latest().decks[0].roll.is_some()
                && carol.lock().latest().decks[0].roll.is_some()
        });

        relay.blackhole();
        wait_for("Bob and Carol to notice", Duration::from_secs(5), || {
            rejoining(&bob) && rejoining(&carol)
        });
        // Cut off: Bob's move stays on his booth for now, Alice's can't
        // reach him, and Carol keeps playing the room as it was.
        bob.lock().act(Action::Volume { deck: 1, v: 0.3 });
        alice.lock().act(Action::Crossfader { v: 0.8 });
        {
            let c = carol.lock();
            assert!(c.is_broadcaster());
            assert!(c.latest().decks[0].playing, "Carol stopped playing");
        }
        assert!(
            alice.lock().view().link.linked(),
            "Alice dropped Bob before the test could check the replacement"
        );

        relay.restore();
        wait_for(
            "Bob and Carol to get back in",
            Duration::from_secs(10),
            || linked(&bob) && linked(&carol),
        );
        wait_for(
            "Bob's cut-off move to reach everyone",
            Duration::from_secs(5),
            || {
                [&alice, &bob, &carol]
                    .iter()
                    .all(|h| h.lock().latest().decks[1].volume == 0.3)
            },
        );
        {
            let b = bob.lock();
            assert_eq!(b.latest().crossfader, 0.8);
            assert_eq!(b.track_status(id), Some(TrackStatus::Ready));
            assert!(b.latest().decks[0].roll.is_some(), "Bob lost the roll");
        }
        assert!(carol.lock().latest().decks[0].roll.is_some());
        wait_for("everyone to agree", Duration::from_secs(5), || {
            digests_match(&alice, &bob) && digests_match(&alice, &carol)
        });
        let r = alice.lock().roster();
        assert_eq!(names(&r.djs), ["Alice", "Bob"]);
        assert_eq!(names(&r.broadcasters), ["Carol"]);

        // Past the host's timeout for the old links: nobody is thrown out.
        thread::sleep(Duration::from_secs(8));
        assert!(linked(&bob) && linked(&carol));
        let r = alice.lock().roster();
        assert_eq!(names(&r.djs), ["Alice", "Bob"]);
        assert_eq!(names(&r.broadcasters), ["Carol"]);
        bob.lock().act(Action::Crossfader { v: 0.4 });
        wait_for(
            "Bob's move to reach everyone",
            Duration::from_secs(5),
            || alice.lock().latest().crossfader == 0.4 && carol.lock().latest().crossfader == 0.4,
        );
        for h in [&bob, &carol, &alice] {
            h.lock().leave();
        }
    }

    /// A connection whose booth already gave up on it isn't seated; one
    /// whose booth is waiting for its Welcome is.
    #[test]
    fn spots_connections_their_booth_already_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut live = TcpStream::connect(addr).unwrap();
        let mut gone = TcpStream::connect(addr).unwrap();
        let me = PeerInfo {
            id: 7,
            name: "Bob".into(),
        };
        let hello = Msg::Hello {
            proto: PROTOCOL_VERSION,
            kind: ConnKind::Control,
            name: me.name.clone(),
            room: "ROOM12".into(),
            peer_id: me.id,
            broadcaster: false,
            booth_key: 9,
            cut_off: CutOff::default(),
        };
        write_msg(&mut live, &hello, &[]).unwrap();
        write_msg(&mut gone, &hello, &[]).unwrap();
        gone.shutdown(Shutdown::Both).unwrap();
        drop(gone);
        for want in [false, true] {
            let (mut s, _) = listener.accept().unwrap();
            assert!(matches!(read_msg(&mut s).unwrap().0, Msg::Hello { .. }));
            thread::sleep(Duration::from_millis(50));
            assert_eq!(closed_already(&s), want);
        }
        drop(live);
    }

    /// Someone in the room who knows Bob's id (every event carries it) but
    /// not his booth key can't take his seat.
    #[test]
    fn a_booth_id_alone_doesnt_take_a_seat() {
        let rig = Rig::new("rejoin-key");
        let alice = rig.booth("Alice", 0, LINK_TIMEOUT);
        let bob = rig.booth("Bob", 0, LINK_TIMEOUT);
        let port = host(&alice, 0, "KEYS12").unwrap();
        let addr = format!("127.0.0.1:{port}");
        join(&bob, &addr, "KEYS12", false).unwrap();
        let fake = bob.lock().me.clone();
        for key in [0, 12345] {
            let mut s = connect(
                resolve(&addr).unwrap(),
                ConnKind::Control,
                "KEYS12",
                &fake,
                key,
                false,
            )
            .unwrap();
            match read_msg(&mut s).unwrap().0 {
                Msg::Reject { reason } => assert!(reason.starts_with(ROOM_FULL), "{reason}"),
                other => panic!("got in with key {key}: {other:?}"),
            }
        }
        assert!(linked(&bob));
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Leaving on purpose is never taken for a dropped link: neither the
    /// host closing the room nor the joiner giving up starts a rejoin.
    #[test]
    fn no_rejoin_after_leaving() {
        let rig = Rig::new("rejoin-leave");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 0, t);
        let carol = rig.booth("Carol", 0, t);
        let port = host(&alice, 0, "LEAVE1").unwrap();
        let addr = format!("127.0.0.1:{port}");
        join(&bob, &addr, "LEAVE1", false).unwrap();
        join(&carol, &addr, "LEAVE1", true).unwrap();
        alice.lock().leave();
        wait_for(
            "Bob and Carol to be on their own",
            Duration::from_secs(5),
            || {
                matches!(bob.lock().view().link.role, RoleView::Solo)
                    && matches!(carol.lock().view().link.role, RoleView::Solo)
            },
        );
        assert!(!rejoining(&bob) && !rejoining(&carol));
        assert!(
            carol
                .lock()
                .latest()
                .decks
                .iter()
                .all(|d| d.track.is_none()),
            "the broadcaster kept the room"
        );

        // Giving up while getting back in stops it for good.
        let port = host(&alice, 0, "LEAVE2").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "LEAVE2", false).unwrap();
        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        bob.lock().leave();
        assert!(matches!(bob.lock().view().link.role, RoleView::Solo));
        relay.restore();
        thread::sleep(Duration::from_secs(3));
        assert!(!rejoining(&bob));
        assert!(
            !bob.lock().view().link.linked(),
            "Bob got back in after giving up"
        );
        alice.lock().leave();
    }

    /// Load stand-in track `n` onto `deck`: the room only needs its id and
    /// length (no booth has it, so each one's downloads of it just fail).
    fn load(deck: u8, n: u8) -> Action {
        Action::Load {
            deck,
            track: TrackId([n; 32]),
            duration: 300.0,
            cue: 0.0,
            grid: None,
            key: -1,
        }
    }

    /// Alice hosts, Bob joins through a relay. Both links time out after
    /// `alice_t` and `bob_t`.
    fn b2b(
        rig: &Rig,
        room: &str,
        alice_t: Duration,
        bob_t: Duration,
    ) -> (SessionHandle, SessionHandle, Relay) {
        let alice = rig.booth("Alice", 0, alice_t);
        let bob = rig.booth("Bob", 900_000_000, bob_t);
        let port = host(&alice, 0, room).unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), room, false).unwrap();
        (alice, bob, relay)
    }

    fn awaiting(h: &SessionHandle) -> bool {
        !h.lock().view().link.awaiting.is_empty()
    }

    /// Bob is back, and Alice and Bob have the same room.
    fn back_together(alice: &SessionHandle, bob: &SessionHandle) {
        wait_for("Bob to get back in", Duration::from_secs(15), || {
            linked(bob)
        });
        wait_for("Alice to see Bob back", Duration::from_secs(5), || {
            let v = alice.lock().view();
            v.link.linked() && v.link.awaiting.is_empty()
        });
        wait_for("Alice and Bob to agree", Duration::from_secs(5), || {
            digests_match(alice, bob)
        });
    }

    /// While Bob is cut off, Alice moves the crossfader and Bob deck A's
    /// fader. Once he's back, both moves are in the room, on both booths.
    #[test]
    fn moves_made_while_cut_off_come_through() {
        let rig = Rig::new("merge-both");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE1", t, t);
        relay.sever();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        bob.lock().act(Action::Volume { deck: 0, v: 0.4 });
        alice.lock().act(Action::Crossfader { v: 0.7 });
        back_together(&alice, &bob);
        for h in [&alice, &bob] {
            let room = h.lock().latest();
            assert_eq!(room.decks[0].volume, 0.4);
            assert_eq!(room.crossfader, 0.7);
        }
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Both DJs move the same controls while Bob is cut off, seconds
    /// apart: the later move wins, whoever made it, on both booths. The cut
    /// lasts longer than the room keeps its recent moves (3.5 s), so Bob's
    /// moves have to go in at the times he made them, not when he's back:
    /// his early crossfader move landing late would beat Alice's later one.
    /// His first move was made before he knew the link had gone.
    #[test]
    fn the_later_move_wins_after_a_long_cut() {
        let rig = Rig::new("merge-later");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE2", t, t);
        relay.blackhole();
        bob.lock().act(Action::Crossfader { v: 0.1 });
        alice.lock().act(Action::Volume { deck: 1, v: 0.2 });
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        thread::sleep(Duration::from_millis(2500));
        alice.lock().act(Action::Crossfader { v: 0.9 });
        bob.lock().act(Action::Volume { deck: 1, v: 0.6 });
        thread::sleep(Duration::from_secs(4));
        relay.restore();
        back_together(&alice, &bob);
        for h in [&alice, &bob] {
            let room = h.lock().latest();
            assert_eq!(room.crossfader, 0.9, "Alice moved it last");
            assert_eq!(room.decks[1].volume, 0.6, "Bob moved it last");
        }
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Bob presses PLAY on deck A while cut off: once he's back, deck A
    /// plays in the room from when he pressed it, so Alice hears it exactly
    /// where Bob does.
    #[test]
    fn a_deck_started_while_cut_off_plays_from_when_it_was_started() {
        let rig = Rig::new("merge-play");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE3", t, t);
        alice.lock().act(load(0, 1));
        wait_for("Bob to see the track", Duration::from_secs(5), || {
            bob.lock().latest().decks[0].track.is_some()
        });
        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        let pressed = {
            let mut b = bob.lock();
            let t = b.now();
            b.act_at(Action::Play { deck: 0 }, t);
            t
        };
        thread::sleep(Duration::from_secs(4));
        relay.restore();
        back_together(&alice, &bob);
        let a = alice.lock();
        let now = a.now();
        let deck = &a.latest().decks[0];
        assert!(deck.playing, "deck A isn't playing");
        let pos = deck.position_at(now);
        let want = (now - pressed) as f64 / 1e6;
        assert!((pos - want).abs() < 0.05, "at {pos:.3} s, not {want:.3} s");
        drop(a);
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Bob ejects decks A and C: A's eject reaches Alice, and C's, made
    /// while he's cut off, once he's back. Deck B's doesn't: Alice was
    /// playing it out loud by then, and he's told. A DJ joining then finds
    /// the decks the same.
    #[test]
    fn ejects_reach_the_room_like_loads() {
        let rig = Rig::new("eject");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "EJECT1", t, t);
        for deck in 0..3 {
            alice.lock().act(load(deck, 1 + deck));
        }
        wait_for("Bob to see the tracks", Duration::from_secs(5), || {
            bob.lock().latest().decks[2].track.is_some()
        });
        bob.lock().act(Action::Eject { deck: 0 });
        wait_for("the eject to reach Alice", Duration::from_secs(5), || {
            alice.lock().latest().decks[0].track.is_none()
        });
        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        alice.lock().act(Action::Play { deck: 1 });
        thread::sleep(Duration::from_millis(300));
        bob.lock().act(Action::Eject { deck: 1 });
        bob.lock().act(Action::Eject { deck: 2 });
        assert!(bob.lock().take_notices().is_empty(), "refused too early");
        relay.restore();
        back_together(&alice, &bob);
        let dave = rig.booth("Dave", -600_000_000, t);
        join(&dave, &relay.addr(), "EJECT1", false).unwrap();
        wait_for("Dave to agree", Duration::from_secs(5), || {
            digests_match(&alice, &dave)
        });
        for h in [&alice, &bob, &dave] {
            let tracks = h.lock().latest().decks.map(|d| d.track);
            assert_eq!(tracks, [None, Some(TrackId([2; 32])), None, None]);
        }
        assert_eq!(
            bob.lock().take_notices(),
            ["Deck B is playing: pause it or pull its fader down to eject it."]
        );
        for h in [&dave, &bob, &alice] {
            h.lock().leave();
        }
    }

    /// While Bob is cut off, Alice starts deck B (out loud) and Bob then
    /// loads onto it, and Bob starts deck A and Alice then loads onto that.
    /// Neither could see the other's deck playing, but the room can: both
    /// loads are refused, and each DJ is told about their own.
    #[test]
    fn a_load_made_while_cut_off_onto_a_playing_deck_is_refused_and_reported() {
        let rig = Rig::new("merge-load");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE4", t, t);
        alice.lock().act(load(0, 1));
        alice.lock().act(load(1, 2));
        wait_for("Bob to see the tracks", Duration::from_secs(5), || {
            bob.lock().latest().decks[1].track.is_some()
        });
        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        alice.lock().act(Action::Play { deck: 1 });
        bob.lock().act(Action::Play { deck: 0 });
        thread::sleep(Duration::from_millis(500));
        bob.lock().act(load(1, 3));
        alice.lock().act(load(0, 4));
        for h in [&alice, &bob] {
            assert!(h.lock().take_notices().is_empty(), "refused too early");
        }
        relay.restore();
        back_together(&alice, &bob);
        for h in [&alice, &bob] {
            let decks = h.lock().latest().decks;
            assert_eq!(decks[0].track, Some(TrackId([1; 32])));
            assert_eq!(decks[1].track, Some(TrackId([2; 32])));
            assert!(decks[0].playing && decks[1].playing);
        }
        assert_eq!(
            bob.lock().take_notices(),
            ["Deck B is playing: pause it or pull its fader down to load onto it."]
        );
        wait_for("Alice to be told", Duration::from_secs(2), || {
            let n = alice.lock().take_notices();
            assert!(n.len() <= 1, "{n:?}");
            n == ["Deck A is playing: pause it or pull its fader down to load onto it."]
        });
        bob.lock().leave();
        alice.lock().leave();
    }

    /// The link goes silent. Bob keeps going for the 3 s it takes him to
    /// notice, and every move he makes is sent into the void; Alice still
    /// holds his old link when he's back. They all come through.
    #[test]
    fn moves_sent_before_the_drop_was_noticed_come_through() {
        let rig = Rig::new("merge-inflight");
        let (alice, bob, relay) = b2b(
            &rig,
            "MERGE5",
            Duration::from_secs(30),
            Duration::from_secs(3),
        );
        alice.lock().act(Action::Volume { deck: 3, v: 0.5 });
        wait_for("Bob to see it", Duration::from_secs(5), || {
            bob.lock().latest().decks[3].volume == 0.5
        });
        relay.blackhole();
        for (i, v) in [0.11, 0.22, 0.33].into_iter().enumerate() {
            bob.lock().act(Action::Volume { deck: i as u8, v });
            thread::sleep(Duration::from_millis(800));
        }
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        assert!(!awaiting(&alice), "Alice dropped Bob's link too soon");
        relay.restore();
        back_together(&alice, &bob);
        for h in [&alice, &bob] {
            let decks = h.lock().latest().decks;
            let v: Vec<f32> = decks.iter().map(|d| d.volume).collect();
            assert_eq!(v, [0.11, 0.22, 0.33, 0.5]);
        }
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Bob gives up getting back in, plays on alone for a bit, then joins
    /// the room again by hand while Alice is still waiting for him: what he
    /// did before and after giving up comes into the room.
    #[test]
    fn joining_again_by_hand_soon_brings_the_moves_too() {
        let rig = Rig::new("merge-hand");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE6", t, t);
        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        bob.lock().act(Action::Volume { deck: 0, v: 0.45 });
        bob.lock().leave();
        assert!(matches!(bob.lock().view().link.role, RoleView::Solo));
        bob.lock().act(Action::Volume { deck: 1, v: 0.55 });
        alice.lock().act(Action::Crossfader { v: 0.35 });
        wait_for("Alice to wait for Bob", Duration::from_secs(5), || {
            awaiting(&alice)
        });
        relay.restore();
        join(&bob, &relay.addr(), "MERGE6", false).unwrap();
        back_together(&alice, &bob);
        for h in [&alice, &bob] {
            let room = h.lock().latest();
            assert_eq!(room.decks[0].volume, 0.45);
            assert_eq!(room.decks[1].volume, 0.55);
            assert_eq!(room.crossfader, 0.35);
        }
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Only the booth whose place the room is holding brings moves in: not
    /// one that knows its id (every event carries it) but not its key, which
    /// is turned away, nor another booth joining fresh.
    #[test]
    fn only_the_awaited_booth_brings_moves_in() {
        let rig = Rig::new("merge-others");
        let t = Duration::from_secs(2);
        let (alice, bob, relay) = b2b(&rig, "MERGE7", t, t);
        let port = match alice.lock().view().link.role {
            RoleView::Hosting { port, .. } => port,
            _ => unreachable!(),
        };
        let addr = resolve(&format!("127.0.0.1:{port}")).unwrap();
        relay.blackhole();
        wait_for("Alice to wait for Bob", Duration::from_secs(5), || {
            awaiting(&alice)
        });
        let bob_me = bob.lock().me.clone();
        let stranger = PeerInfo {
            id: 0xD15C0,
            name: "Dave".into(),
        };
        let key = bob.lock().booth_key() ^ 1;
        for who in [bob_me, stranger] {
            let now = alice.lock().now();
            let cut_off = CutOff {
                events: vec![crate::model::Event {
                    time: now - 500_000,
                    origin: who.id,
                    seq: 9_999,
                    action: Action::Crossfader { v: 0.05 },
                }],
                tracks: Vec::new(),
            };
            let mut s =
                connect_with(addr, ConnKind::Control, "MERGE7", &who, key, false, cut_off).unwrap();
            match read_msg(&mut s).unwrap().0 {
                Msg::Reject { reason } if who.name == "Bob" => {
                    assert!(reason.starts_with(ROOM_FULL), "{reason}")
                }
                Msg::Welcome {
                    merged, timeline, ..
                } if who.name != "Bob" => {
                    assert!(!merged, "{} brought moves in", who.name);
                    assert_eq!(timeline.latest().crossfader, 0.5);
                }
                other => panic!("{}: {other:?}", who.name),
            }
            assert_eq!(alice.lock().latest().crossfader, 0.5);
            drop(s);
            wait_for("Alice to see it go", Duration::from_secs(5), || {
                !alice.lock().view().link.linked()
            });
        }
        alice.lock().leave();
        bob.lock().leave();
    }

    /// A tone of `secs` seconds (about 176 KB a second) in `dir`.
    fn tone(dir: &std::path::Path, name: &str, secs: f64, hz: f64) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        crate::audio::write_wav(&p, 44_100, &crate::audio::test_signal(44_100, secs, hz)).unwrap();
        p
    }

    fn id_of(path: &std::path::Path) -> TrackId {
        TrackId::of_bytes(&std::fs::read(path).unwrap())
    }

    /// The track on a booth's deck once its load is done.
    fn loaded(h: &SessionHandle, deck: usize) -> TrackId {
        let mut id = None;
        wait_for("the load", Duration::from_secs(20), || {
            id = h.lock().latest().decks[deck].track;
            id.is_some()
        });
        id.unwrap()
    }

    /// The booth's download of a track, as its transfer list shows it.
    fn download(h: &SessionHandle, id: TrackId) -> Option<Transfer> {
        let v = h.lock().view();
        v.transfers.into_iter().find(|t| t.id == id && !t.upload)
    }

    fn ready(h: &SessionHandle, id: TrackId) -> bool {
        h.lock().track_status(id) == Some(TrackStatus::Ready)
    }

    /// Got the whole track, checked, and kept no `.part` file.
    fn arrived_whole(h: &SessionHandle, id: TrackId) {
        let s = h.lock();
        let path = s.servable_path(id).expect("where the track went");
        assert_eq!(
            id_of(&path),
            id,
            "{} differs from the track",
            path.display()
        );
        assert!(!s.part_path(id).exists(), "the .part file was left behind");
    }

    /// A track on its way when the link drops carries on from where it got
    /// to once the booths are back together, instead of starting over: from
    /// the host to the joining DJ and a broadcaster, and from the joining DJ
    /// to the host. Meanwhile it shows as paused, not failed. Every booth
    /// ends up with the whole, checked file, and the decks that waited for
    /// it play the room as the host does.
    #[test]
    fn a_transfer_cut_off_part_way_carries_on_after_rejoining() {
        let rig = Rig::new("resume-cut");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 3_600_000_000, t);
        let carol = rig.booth("Carol", -1_800_000_000, t);
        // Slow uploads, so the link drops part-way through.
        alice.lock().max_upload_bytes_s = 1_200_000.0;
        bob.lock().max_upload_bytes_s = 600_000.0;
        let port = host(&alice, 0, "RESUME").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "RESUME", false).unwrap();
        join(&carol, &relay.addr(), "RESUME", true).unwrap();

        // Long enough to still be playing when they've arrived.
        let a_wav = tone(&rig.root.join("a"), "Alice Tone.wav", 20.0, 440.0);
        let b_wav = tone(&rig.root.join("b"), "Bob Tone.wav", 20.0, 330.0);
        alice.lock().load_path(0, a_wav);
        bob.lock().load_path(1, b_wav);
        let (a_id, b_id) = (loaded(&alice, 0), loaded(&bob, 1));
        alice.lock().act(Action::Play { deck: 0 });
        bob.lock().act(Action::Play { deck: 1 });

        let part_way = |h: &SessionHandle, id| {
            download(h, id)
                .is_some_and(|t| t.total > 0 && t.done * 4 >= t.total && t.done < t.total)
        };
        wait_for("the tracks to be part-way", Duration::from_secs(30), || {
            part_way(&bob, a_id) && part_way(&carol, a_id) && part_way(&alice, b_id)
        });
        relay.sever();

        let paused = |h: &SessionHandle, id| {
            download(h, id).filter(|t| t.state == TransferState::Paused && t.done < t.total)
        };
        // Each shows as paused, not failed, while the booths are apart
        // (Bob and Carol get back in after a second or so).
        let cut = [(&bob, a_id), (&carol, a_id), (&alice, b_id)];
        let mut at = [None; 3];
        wait_for("the downloads to pause", Duration::from_secs(10), || {
            for (i, (h, id)) in cut.iter().enumerate() {
                if let (None, Some(t)) = (at[i], paused(h, *id)) {
                    assert_eq!(h.lock().track_status(*id), Some(TrackStatus::Fetching));
                    at[i] = Some(t.done);
                }
            }
            at.iter().all(Option::is_some)
        });

        wait_for(
            "everyone to have both tracks",
            Duration::from_secs(40),
            || {
                ready(&bob, a_id)
                    && ready(&alice, b_id)
                    && ready(&carol, a_id)
                    && ready(&carol, b_id)
            },
        );
        assert!(linked(&bob) && linked(&carol));
        for ((h, id), at) in cut.iter().zip(at.map(Option::unwrap)) {
            let t = download(h, *id).unwrap();
            assert_eq!(t.state, TransferState::Done);
            assert!(
                t.from >= at && t.from > 0,
                "{}: started again from byte {} (it had {at})",
                h.lock().me.name,
                t.from
            );
            arrived_whole(h, *id);
        }

        // The decks that waited play the room as the host does.
        let snap = |h: &SessionHandle| h.lock().snapshot_handle().load_full();
        let (sa, sb, sc) = (snap(&alice), snap(&bob), snap(&carol));
        let t = alice.lock().now() - 100_000;
        // (Not the digests: a detected BPM can come through JSON a bit off.)
        let render = |s: &crate::render::AudioSnapshot, kind| {
            let mut lane = crate::render::Lane::new(48_000.0, 0, kind);
            let (mut m, mut c) = (vec![0.0f32; 8192], vec![0.0f32; 8192]);
            lane.render(s, t, &mut m, &mut c);
            m
        };
        use crate::render::LaneKind::{Broadcast, Live};
        let ma = render(&sa, Live);
        assert!(ma.iter().any(|v| v.abs() > 0.01), "silence");
        assert_eq!(ma, render(&sb, Live), "Bob's booth sounds different");
        assert_eq!(
            render(&sa, Broadcast),
            render(&sc, Broadcast),
            "Carol's stream sounds different"
        );
        for h in [&bob, &carol, &alice] {
            h.lock().leave();
        }
    }

    /// The link goes silent mid-transfer, and the host still holds the old
    /// link and the transfers on it when the joining DJ is back (the case
    /// where two transfers could write one `.part` file). The old ones are
    /// stopped, and the new ones carry on from where they got to.
    #[test]
    fn a_silent_drop_mid_transfer_carries_on_over_the_new_link() {
        let rig = Rig::new("resume-stall");
        // The host gives up on a silent link only well after Bob is back.
        let alice = rig.booth("Alice", 0, LINK_TIMEOUT);
        let bob = rig.booth("Bob", -7_200_000_000, Duration::from_secs(2));
        alice.lock().max_upload_bytes_s = 300_000.0;
        bob.lock().max_upload_bytes_s = 300_000.0;
        let port = host(&alice, 0, "STALL2").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "STALL2", false).unwrap();

        // Each on its own deck: two loads onto one deck at once land in
        // whichever order the booths finish decoding.
        let a_wav = tone(&rig.root.join("a"), "A.wav", 10.0, 440.0);
        let b_wav = tone(&rig.root.join("b"), "B.wav", 10.0, 330.0);
        let (a_id, b_id) = (id_of(&a_wav), id_of(&b_wav));
        alice.lock().load_path(0, a_wav);
        bob.lock().load_path(1, b_wav);
        // About 6 s each at this speed: cut both a quarter of the way in.
        let part_way = |h: &SessionHandle, id| {
            download(h, id)
                .is_some_and(|t| t.total > 0 && t.done * 4 >= t.total && t.done < t.total)
        };
        wait_for(
            "both tracks to be on their way",
            Duration::from_secs(30),
            || part_way(&bob, a_id) && part_way(&alice, b_id),
        );

        relay.blackhole();
        wait_for("Bob to notice", Duration::from_secs(10), || rejoining(&bob));
        assert!(
            alice.lock().view().link.linked(),
            "Alice dropped Bob before he was back"
        );
        relay.restore();
        wait_for("both tracks to arrive", Duration::from_secs(40), || {
            ready(&bob, a_id) && ready(&alice, b_id)
        });
        for (h, id) in [(&bob, a_id), (&alice, b_id)] {
            let t = download(h, id).unwrap();
            assert!(t.from > 0, "{} started again from 0", h.lock().me.name);
            arrived_whole(h, id);
        }
        bob.lock().leave();
        alice.lock().leave();
    }

    /// A download carries on from whatever its `.part` file holds: it asks
    /// for the rest by offset, and the whole file checks out. A partial file
    /// that doesn't check out is thrown away and the track sent again from
    /// the start, whichever way it's going.
    #[test]
    fn carries_on_from_a_partial_file_or_starts_over_from_a_bad_one() {
        let rig = Rig::new("resume-partial");
        let alice = rig.booth("Alice", 0, LINK_TIMEOUT);
        let bob = rig.booth("Bob", 0, LINK_TIMEOUT);
        let port = host(&alice, 0, "PART12").unwrap();
        join(&bob, &format!("127.0.0.1:{port}"), "PART12", false).unwrap();

        let a_wav = tone(&rig.root.join("a"), "A.wav", 4.0, 440.0);
        let c_wav = tone(&rig.root.join("a"), "C.wav", 4.0, 550.0);
        let b_wav = tone(&rig.root.join("b"), "B.wav", 4.0, 330.0);
        let bytes = |p| std::fs::read(p).unwrap();
        let (a, b, c) = (bytes(&a_wav), bytes(&b_wav), bytes(&c_wav));
        let (a_id, b_id, c_id) = (id_of(&a_wav), id_of(&b_wav), id_of(&c_wav));
        let garbled = |x: &[u8]| x.iter().map(|v| v ^ 0x5a).collect::<Vec<u8>>();
        // Bob has the first 40% of A, and a garbled start of C; Alice a
        // garbled start of B.
        let good = a.len() * 2 / 5;
        std::fs::write(bob.lock().part_path(a_id), &a[..good]).unwrap();
        std::fs::write(bob.lock().part_path(c_id), garbled(&c[..c.len() / 3])).unwrap();
        std::fs::write(alice.lock().part_path(b_id), garbled(&b[..b.len() / 3])).unwrap();

        alice.lock().load_path(0, a_wav);
        alice.lock().load_path(2, c_wav);
        bob.lock().load_path(1, b_wav);
        wait_for("all three to arrive", Duration::from_secs(40), || {
            ready(&bob, a_id) && ready(&bob, c_id) && ready(&alice, b_id)
        });
        assert_eq!(download(&bob, a_id).unwrap().from, good as u64);
        for (h, id) in [(&bob, a_id), (&bob, c_id), (&alice, b_id)] {
            arrived_whole(h, id);
        }
        // The bad ones came again from the start.
        assert_eq!(download(&bob, c_id).unwrap().from, 0);
        assert_eq!(download(&alice, b_id).unwrap().from, 0);
        bob.lock().leave();
        alice.lock().leave();
    }

    /// Two downloads of one track at once, as when one the link dropped
    /// under is still running when the next starts, never write its `.part`
    /// file together: the newer one stops the older, which steps aside
    /// without failing the track, and carries on from where it got to.
    #[test]
    fn two_downloads_of_one_track_never_write_together() {
        let rig = Rig::new("resume-two");
        let alice = rig.booth("Alice", 0, LINK_TIMEOUT);
        let bob = rig.booth("Bob", 0, LINK_TIMEOUT);
        alice.lock().max_upload_bytes_s = 200_000.0;
        let port = host(&alice, 0, "TWO123").unwrap();
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        join(&bob, &addr.to_string(), "TWO123", false).unwrap();
        // Offered, so Alice will send it, but not downloading yet.
        let wav = tone(&rig.root.join("a"), "A.wav", 5.0, 440.0);
        let id = id_of(&wav);
        alice.lock().offer_share("Two".into(), vec![wav]);
        wait_for("the offer", Duration::from_secs(10), || {
            !bob.lock().view().incoming.is_empty()
        });
        let (me, host_id, part) = {
            let b = bob.lock();
            (b.me.clone(), alice.lock().me.id, b.part_path(id))
        };
        let get = |to: &str| {
            let (h, me) = (bob.clone(), me.clone());
            let landing = Landing::At(rig.root.join(to));
            thread::spawn(move || {
                crate::files::fetch(&h, addr, "TWO123", &me, false, host_id, id, &landing)
            })
        };
        let first = get("first.wav");
        wait_for("the first to get going", Duration::from_secs(10), || {
            std::fs::metadata(&part).is_ok_and(|m| m.len() > 200_000)
        });
        let second = get("second.wav");
        let first = first.join().unwrap();
        assert!(crate::files::was_stopped(&first), "{first:?}");
        let got = second.join().unwrap().unwrap();
        assert_eq!(id_of(&got), id);
        assert!(download(&bob, id).unwrap().from > 200_000);
        assert!(!rig.root.join("first.wav").exists());
        bob.lock().leave();
        alice.lock().leave();
    }

    /// The host is gone and a new room answers at its address (a restart
    /// with another room code): the booth stops trying and is on its own.
    #[test]
    fn stops_when_the_host_turns_it_away() {
        let rig = Rig::new("rejoin-refused");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 0, t);
        let dave = rig.booth("Dave", 0, t);
        let port = host(&alice, 0, "FIRST1").unwrap();
        let relay = Relay::new(port);
        join(&bob, &relay.addr(), "FIRST1", false).unwrap();
        let other = host(&dave, 0, "OTHER1").unwrap();
        relay.retarget(other);
        relay.sever();
        wait_for("Bob to give up", Duration::from_secs(10), || {
            matches!(bob.lock().view().link.role, RoleView::Solo)
        });
        assert!(!rejoining(&bob));
        assert!(!dave.lock().view().link.linked());
        for h in [&alice, &bob, &dave] {
            h.lock().leave();
        }
    }

    /// Four DJs share a room and a fifth is turned away, though a
    /// broadcaster still gets in. Each DJ's moves reach every booth, and a
    /// track only one joining DJ has reaches the others and the broadcaster
    /// through the host. One DJ leaving doesn't end the room for the rest,
    /// and frees their seat.
    #[test]
    fn four_djs_share_a_room() {
        let rig = Rig::new("four-djs");
        let t = LINK_TIMEOUT;
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 3_600_000_000, t);
        let carol = rig.booth("Carol", -1_800_000_000, t);
        let dave = rig.booth("Dave", 900_000_000, t);
        let eve = rig.booth("Eve", 0, t);
        let port = host(&alice, 0, "FOUR12").unwrap();
        let addr = format!("127.0.0.1:{port}");
        for h in [&bob, &carol, &dave] {
            join(h, &addr, "FOUR12", false).unwrap();
        }
        let err = join(&eve, &addr, "FOUR12", false).unwrap_err();
        assert!(format!("{err:#}").contains("4 DJs"), "{err:#}");
        join(&eve, &addr, "FOUR12", true).unwrap();
        let all = [&alice, &bob, &carol, &dave, &eve];
        wait_for("everyone to know who's in", Duration::from_secs(5), || {
            all.iter().all(|h| {
                let r = h.lock().roster();
                names(&r.djs) == ["Alice", "Bob", "Carol", "Dave"]
                    && names(&r.broadcasters) == ["Eve"]
            })
        });
        let links = alice.lock().view().link.links;
        let to: Vec<&str> = links.iter().map(|l| l.info.name.as_str()).collect();
        assert_eq!(to, ["Bob", "Carol", "Dave"]);

        alice.lock().act(Action::Crossfader { v: 0.2 });
        bob.lock().act(Action::Volume { deck: 0, v: 0.3 });
        carol.lock().act(Action::Volume { deck: 1, v: 0.4 });
        dave.lock().act(Action::Volume { deck: 2, v: 0.5 });
        wait_for(
            "every move to reach every booth",
            Duration::from_secs(5),
            || {
                all.iter().all(|h| {
                    let r = h.lock().latest();
                    let v: Vec<f32> = r.decks.iter().map(|d| d.volume).collect();
                    r.crossfader == 0.2 && v[..3] == [0.3, 0.4, 0.5]
                })
            },
        );

        let wav = tone(&rig.root.join("carol-music"), "Carol Tone.wav", 4.0, 550.0);
        carol.lock().load_path(3, wav);
        let id = loaded(&carol, 3);
        wait_for(
            "everyone to get Carol's track",
            Duration::from_secs(30),
            || all.iter().all(|h| ready(h, id)),
        );
        for h in [&alice, &bob, &dave, &eve] {
            arrived_whole(h, id);
        }
        wait_for("everyone to agree", Duration::from_secs(5), || {
            all[1..].iter().all(|h| digests_match(&alice, h))
        });

        carol.lock().leave();
        wait_for("everyone to see Carol go", Duration::from_secs(5), || {
            [&alice, &bob, &dave, &eve]
                .iter()
                .all(|h| names(&h.lock().roster().djs) == ["Alice", "Bob", "Dave"])
        });
        assert!(
            linked(&bob) && linked(&dave),
            "Carol leaving ended the room"
        );
        bob.lock().act(Action::Crossfader { v: 0.6 });
        wait_for("Bob's move to reach Dave", Duration::from_secs(5), || {
            dave.lock().latest().crossfader == 0.6
        });
        let frank = rig.booth("Frank", 0, t);
        join(&frank, &addr, "FOUR12", false).unwrap();
        wait_for("Frank to get the room", Duration::from_secs(5), || {
            frank.lock().latest().crossfader == 0.6 && digests_match(&alice, &frank)
        });
        for h in [&frank, &dave, &bob, &eve, &alice] {
            h.lock().leave();
        }
    }

    /// Three DJs join Alice's room, Bob through a link that goes dark.
    /// While he's cut off the room keeps his seat (a new DJ is turned away)
    /// and everyone plays on. Once he's back, every booth has every move
    /// from meanwhile, each at the time it was made: Bob's reach the other
    /// joining DJs, and theirs reach him. The cut is longer than the rooms
    /// keep their recent moves, so his have to go in further back.
    #[test]
    fn a_dj_back_from_a_cut_and_the_other_djs_share_every_move() {
        let rig = Rig::new("four-cut");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 900_000_000, t);
        let carol = rig.booth("Carol", -900_000_000, t);
        let dave = rig.booth("Dave", 0, t);
        let eve = rig.booth("Eve", 0, t);
        let port = host(&alice, 0, "CUT412").unwrap();
        let relay = Relay::new(port);
        let addr = format!("127.0.0.1:{port}");
        join(&bob, &relay.addr(), "CUT412", false).unwrap();
        join(&carol, &addr, "CUT412", false).unwrap();
        join(&dave, &addr, "CUT412", false).unwrap();

        relay.blackhole();
        bob.lock().act(Action::Volume { deck: 0, v: 0.25 });
        wait_for("Alice to wait for Bob", Duration::from_secs(5), || {
            alice.lock().view().link.awaiting == ["Bob"]
        });
        let err = join(&eve, &addr, "CUT412", false).unwrap_err();
        assert!(format!("{err:#}").contains("4 DJs"), "{err:#}");
        carol.lock().act(Action::Volume { deck: 1, v: 0.35 });
        dave.lock().act(Action::Crossfader { v: 0.45 });
        alice.lock().act(Action::Volume { deck: 2, v: 0.55 });
        thread::sleep(Duration::from_secs(4));
        relay.restore();

        wait_for("Bob to get back in", Duration::from_secs(15), || {
            linked(&bob)
        });
        let djs = [&alice, &bob, &carol, &dave];
        // Straight away: sooner than out-of-step digests could resync them.
        wait_for(
            "every DJ to have every move",
            Duration::from_millis(1500),
            || {
                djs.iter().all(|h| {
                    let r = h.lock().latest();
                    let v: Vec<f32> = r.decks.iter().map(|d| d.volume).collect();
                    v[..3] == [0.25, 0.35, 0.55] && r.crossfader == 0.45
                })
            },
        );
        wait_for("everyone to agree", Duration::from_secs(5), || {
            djs[1..].iter().all(|h| digests_match(&alice, h))
        });
        let mut r = names(&alice.lock().roster().djs);
        r.sort();
        assert_eq!(r, ["Alice", "Bob", "Carol", "Dave"]);
        for h in [&bob, &carol, &dave, &alice] {
            h.lock().leave();
        }
    }

    /// Bob offers a track only he has. Carol and Dave accept and get it
    /// through Alice, the host, who never accepts it: she fetches it from
    /// Bob to pass on. While it's on its way to her, their downloads are
    /// told to ask again, and wait rather than fail.
    #[test]
    fn a_share_between_joining_djs_goes_through_the_host() {
        let rig = Rig::new("four-share");
        let t = LINK_TIMEOUT;
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 0, t);
        let carol = rig.booth("Carol", 0, t);
        let dave = rig.booth("Dave", 0, t);
        // About 4 s for Bob to send it, a few asks' worth of waiting.
        alice.lock().pending_wait = Duration::from_millis(300);
        bob.lock().max_upload_bytes_s = 400_000.0;
        let port = host(&alice, 0, "SHARE4").unwrap();
        let addr = format!("127.0.0.1:{port}");
        for h in [&bob, &carol, &dave] {
            join(h, &addr, "SHARE4", false).unwrap();
        }
        let wav = tone(&rig.root.join("bob-music"), "Bob Shared.wav", 10.0, 330.0);
        let id = id_of(&wav);
        bob.lock().offer_share("Bob's crate".into(), vec![wav]);
        let offer = |h: &SessionHandle| h.lock().view().incoming.first().map(|s| s.share_id);
        wait_for(
            "the offer to reach everyone",
            Duration::from_secs(10),
            || [&alice, &carol, &dave].iter().all(|h| offer(h).is_some()),
        );
        let share = offer(&carol).unwrap();
        carol.lock().respond_share(share, true);
        dave.lock().respond_share(share, true);
        let got = |h: &SessionHandle| {
            let v = h.lock().view();
            let s = &v.incoming[0];
            assert_eq!(s.failed, 0, "{}'s download failed", h.lock().me.name);
            s.received == 1
        };
        wait_for("Carol and Dave to get it", Duration::from_secs(40), || {
            got(&carol) && got(&dave)
        });
        for h in [&carol, &dave] {
            arrived_whole(h, id);
        }
        wait_for("Bob to hear both answers", Duration::from_secs(5), || {
            let v = bob.lock().view();
            v.outgoing.first().is_some_and(|(_, _, r)| r.len() == 2)
        });
        // Alice passed it on without taking it.
        let a = alice.lock();
        assert_eq!(a.view().incoming[0].state, ShareState::Pending);
        assert!(!a.paths.shared_dir.join("Bob").exists());
        drop(a);
        for h in [&bob, &carol, &dave, &alice] {
            h.lock().leave();
        }
    }

    /// A folder of a booth's, scanned, as its library lists it.
    fn listed(h: &SessionHandle, dir: &std::path::Path) -> (String, Vec<crate::library::LibTrack>) {
        use crate::library::{folder_key, scan, SourceKind};
        let lib = h.lock().library();
        let key = folder_key(dir);
        scan(&lib, key.clone(), SourceKind::Folder(dir.to_path_buf()));
        wait_for("the folder to be listed", Duration::from_secs(10), || {
            lib.lock().lists.get(&key).is_some_and(|l| !l.scanning)
        });
        let tracks = lib.lock().lists[&key].tracks.clone();
        (key, tracks)
    }

    /// The lists other DJs share live, as a booth's library shows them:
    /// (owner's name, list name, track titles).
    fn live_lists(h: &SessionHandle) -> Vec<(String, String, Vec<String>)> {
        let v = h.lock().view();
        v.room_crates
            .iter()
            .map(|c| {
                let titles = c.tracks.iter().map(|t| t.title.clone()).collect();
                (c.owner_name.clone(), c.name.clone(), titles)
            })
            .collect()
    }

    /// A DJ shares a folder live: the other DJs (and only the DJs) see its
    /// tracks without anything being copied, and a DJ who joins later gets
    /// it too. A track loaded from it, by a joining DJ or the host, comes
    /// over from its owner to every booth. Changes to the list reach the
    /// room; stopping sharing it, or its owner leaving, takes it away.
    #[test]
    fn lists_shared_live_come_over_only_when_loaded() {
        let rig = Rig::new("live-lists");
        let t = LINK_TIMEOUT;
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 3_600_000_000, t);
        let carol = rig.booth("Carol", -1_800_000_000, t);
        let dave = rig.booth("Dave", 900_000_000, t);
        let eve = rig.booth("Eve", 0, t);
        let port = host(&alice, 0, "LIVE12").unwrap();
        let addr = format!("127.0.0.1:{port}");
        for h in [&bob, &carol] {
            join(h, &addr, "LIVE12", false).unwrap();
        }
        join(&eve, &addr, "LIVE12", true).unwrap();

        let picks = rig.root.join("bob-picks");
        let one = tone(&picks, "Bob - One.wav", 2.0, 330.0);
        let two = tone(&picks, "Bob - Two.wav", 2.5, 440.0);
        let (key, tracks) = listed(&bob, &picks);
        bob.lock().share_live(key.clone(), "Picks".into(), tracks);
        let want = |titles: &[&str]| {
            let titles = titles.iter().map(|t| t.to_string()).collect();
            vec![("Bob".to_string(), "Picks".to_string(), titles)]
        };
        wait_for("the DJs to see Bob's list", Duration::from_secs(5), || {
            [&alice, &carol]
                .iter()
                .all(|h| live_lists(h) == want(&["One", "Two"]))
        });
        assert!(
            live_lists(&bob).is_empty(),
            "Bob sees his own list as another's"
        );
        assert!(live_lists(&eve).is_empty(), "a broadcaster got the list");
        assert!(
            carol.lock().view().transfers.is_empty() && alice.lock().view().transfers.is_empty(),
            "sharing a list live copied tracks"
        );

        // Carol loads one Bob has analysed: it loads at once and comes
        // over from Bob to everyone.
        let info = |h: &SessionHandle, title: &str| {
            let v = h.lock().view();
            let c = v.room_crates.iter().find(|c| c.name == "Picks").unwrap();
            c.tracks.iter().find(|t| t.title == title).unwrap().clone()
        };
        let mut two_info = info(&carol, "Two");
        assert_eq!(two_info.id, id_of(&two));
        assert!(two_info.grid.is_none(), "Bob never analysed it");
        let grid = crate::analysis::BeatGrid::new(0.1, 120.0).unwrap();
        two_info.grid = Some(grid);
        carol.lock().load_room_track(1, two_info.clone());
        assert_eq!(carol.lock().latest().decks[1].track, Some(two_info.id));
        let all = [&alice, &bob, &carol, &eve];
        wait_for(
            "everyone to get the track Carol loaded",
            Duration::from_secs(30),
            || all.iter().all(|h| ready(h, two_info.id)),
        );
        for h in [&alice, &carol, &eve] {
            arrived_whole(h, two_info.id);
        }
        assert_eq!(carol.lock().latest().decks[1].grid, Some(grid));
        // The host loads one nobody has analysed: it comes over and is
        // analysed first, then loads (with what was found) everywhere.
        let one_info = info(&alice, "One");
        assert_eq!(one_info.id, id_of(&one));
        alice.lock().load_room_track(2, one_info.clone());
        assert_eq!(
            alice.lock().latest().decks[2].track,
            None,
            "loaded before it was analysed"
        );
        assert!(alice.lock().view().decks[2].loading.is_some());
        assert_eq!(loaded(&alice, 2), one_info.id);
        assert!(
            ready(&alice, one_info.id),
            "loaded before the track arrived"
        );
        assert!(alice.lock().view().decks[2].loading.is_none());
        wait_for(
            "everyone to get the track Alice loaded",
            Duration::from_secs(30),
            || all.iter().all(|h| ready(h, one_info.id)),
        );
        assert!(
            std::fs::read_dir(&carol.lock().paths.shared_dir)
                .unwrap()
                .next()
                .is_none(),
            "Carol's Shared with me got a copy"
        );
        wait_for("everyone to agree", Duration::from_secs(5), || {
            all[1..].iter().all(|h| digests_match(&alice, h))
        });

        // Bob adds a track: the room gets the new list, and so does a DJ
        // who joins after.
        tone(&picks, "Bob - Three.wav", 1.0, 550.0);
        let (_, tracks) = listed(&bob, &picks);
        bob.lock().update_live(&key, "Picks".into(), tracks);
        join(&dave, &addr, "LIVE12", false).unwrap();
        wait_for(
            "the new list to reach the DJs",
            Duration::from_secs(5),
            || {
                [&alice, &carol, &dave]
                    .iter()
                    .all(|h| live_lists(h) == want(&["One", "Three", "Two"]))
            },
        );

        // The host shares one of its own: the joining DJs get it.
        let alice_dir = rig.root.join("alice-set");
        tone(&alice_dir, "Alice - Opener.wav", 1.5, 220.0);
        let (alice_key, alice_tracks) = listed(&alice, &alice_dir);
        alice
            .lock()
            .share_live(alice_key, "Set".into(), alice_tracks);
        wait_for(
            "the joining DJs to see Alice's list",
            Duration::from_secs(5),
            || {
                [&bob, &carol, &dave].iter().all(|h| {
                    live_lists(h).iter().any(|(who, name, titles)| {
                        who == "Alice" && name == "Set" && titles == &["Opener"]
                    })
                })
            },
        );
        let opener = {
            let v = dave.lock().view();
            let c = v.room_crates.iter().find(|c| c.name == "Set").unwrap();
            c.tracks[0].clone()
        };
        dave.lock().load_room_track(3, opener.clone());
        wait_for(
            "Alice's track to reach every booth",
            Duration::from_secs(30),
            || {
                [&alice, &bob, &carol, &dave, &eve]
                    .iter()
                    .all(|h| ready(h, opener.id))
            },
        );

        // Bob stops sharing his: it's gone, and the track on the deck stays.
        bob.lock().unshare_live(&key);
        wait_for("Bob's list to go", Duration::from_secs(5), || {
            [&alice, &carol, &dave]
                .iter()
                .all(|h| live_lists(h).iter().all(|(who, _, _)| who != "Bob"))
        });
        assert_eq!(carol.lock().latest().decks[1].track, Some(two_info.id));

        // Shared again, then Bob leaves: it goes with him.
        let (_, tracks) = listed(&bob, &picks);
        bob.lock().share_live(key, "Picks".into(), tracks);
        wait_for("Bob's list to come back", Duration::from_secs(5), || {
            live_lists(&dave).iter().any(|(who, _, _)| who == "Bob")
        });
        bob.lock().leave();
        wait_for("Bob's list to go with him", Duration::from_secs(5), || {
            [&alice, &carol, &dave]
                .iter()
                .all(|h| live_lists(h).iter().all(|(who, _, _)| who != "Bob"))
        });
        assert!(alice
            .lock()
            .room_crates_of_test()
            .iter()
            .all(|o| *o != bob.lock().me.id));
        for h in [&carol, &dave, &eve, &alice] {
            h.lock().leave();
        }
        assert!(live_lists(&carol).is_empty() && carol.lock().view().live.is_empty());
    }

    /// A DJ whose link drops keeps sharing their lists: the others still
    /// have them once the DJ is back. One they stop sharing, or start
    /// sharing, while cut off is gone, or there, for the others once
    /// they're back.
    #[test]
    fn lists_shared_live_follow_a_dj_through_a_dropped_link() {
        let rig = Rig::new("live-cut");
        let t = Duration::from_secs(2);
        let alice = rig.booth("Alice", 0, t);
        let bob = rig.booth("Bob", 900_000_000, t);
        let carol = rig.booth("Carol", 0, t);
        let port = host(&alice, 0, "LIVECUT").unwrap();
        let addr = format!("127.0.0.1:{port}");
        join(&bob, &addr, "LIVECUT", false).unwrap();
        join(&carol, &addr, "LIVECUT", false).unwrap();
        let (dir_a, dir_b) = (rig.root.join("bob-a"), rig.root.join("bob-b"));
        tone(&dir_a, "A.wav", 1.0, 300.0);
        tone(&dir_b, "B.wav", 1.0, 400.0);
        let (one, a) = listed(&bob, &dir_a);
        let (two, b) = listed(&bob, &dir_b);
        bob.lock().share_live(one.clone(), "First".into(), a);
        let names_at = |h: &SessionHandle| -> Vec<String> {
            let mut v: Vec<String> = live_lists(h).into_iter().map(|(_, n, _)| n).collect();
            v.sort();
            v
        };
        wait_for(
            "Carol to see the first list",
            Duration::from_secs(5),
            || names_at(&carol) == ["First"],
        );

        bob.lock().cut_link();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        bob.lock().unshare_live(&one);
        bob.lock().share_live(two, "Second".into(), b);
        wait_for("Bob to get back in", Duration::from_secs(15), || {
            linked(&bob)
        });
        wait_for(
            "the others to have Bob's lists as they are now",
            Duration::from_secs(5),
            || names_at(&carol) == ["Second"] && names_at(&alice) == ["Second"],
        );

        // Back again with nothing changed: the host doesn't pass the same
        // list on a second time, since the moves wait behind it.
        let second_at = |h: &SessionHandle| h.lock().view().room_crates[0].clone();
        let before = second_at(&carol);
        bob.lock().cut_link();
        wait_for("Bob to notice", Duration::from_secs(5), || rejoining(&bob));
        wait_for("Bob to get back in", Duration::from_secs(15), || {
            linked(&bob)
        });
        // Bob's next list comes after anything he sent on getting back in.
        let dir_c = rig.root.join("bob-c");
        tone(&dir_c, "C.wav", 1.0, 500.0);
        let (three, c) = listed(&bob, &dir_c);
        bob.lock().share_live(three, "Third".into(), c);
        wait_for(
            "Carol to see the third list",
            Duration::from_secs(5),
            || names_at(&carol) == ["Second", "Third"],
        );
        let after = (carol.lock().view().room_crates.iter())
            .find(|c| c.name == "Second")
            .unwrap()
            .clone();
        assert!(
            Arc::ptr_eq(&before, &after),
            "the unchanged list came to Carol again"
        );
        for h in [&bob, &carol, &alice] {
            h.lock().leave();
        }
    }
}
