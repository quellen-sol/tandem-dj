//! Rooms over TCP: one DJ hosts, the other joins.
//!
//! The host's clock *is* the room clock. The joining DJ measures its offset
//! with pings (see [`crate::clock`]) before applying the room state, and
//! keeps pinging so the estimate tracks drift.
//!
//! Up to [`MAX_BROADCASTERS`] more booths can join as broadcasters: they
//! sync their clock and receive the room exactly like the joining DJ (the
//! host relays the DJ's events and tracks to them), so they can play the
//! broadcast feed and show the broadcast window. They can't change anything:
//! the host answers their pings and resync requests and ignores the rest.

use crate::clock::ClockEstimator;
use crate::protocol::{
    read_msg, write_msg, ConnKind, Msg, PeerInfo, DEFAULT_PORT, PROTOCOL_VERSION,
};
use crate::session::SessionHandle;
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
                    if write_msg(&mut w, &m, &[]).is_err() {
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

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn close(&self) {
        self.alive.store(false, Ordering::Relaxed);
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// How many booths can join a room as broadcasters, besides the two DJs.
pub const MAX_BROADCASTERS: usize = 4;

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
        ConnKind::Files => crate::files::serve(&h, s, broadcaster),
        ConnKind::Control => host_control(h, s, info, broadcaster),
    }
}

fn host_control(
    h: SessionHandle,
    mut s: TcpStream,
    info: PeerInfo,
    broadcaster: bool,
) -> anyhow::Result<()> {
    let (clock, reply, serial) = {
        let mut ses = h.lock();
        let full = if broadcaster {
            (ses.broadcaster_count() >= MAX_BROADCASTERS)
                .then(|| format!("This room already has {MAX_BROADCASTERS} broadcasters."))
        } else {
            ses.has_peer()
                .then(|| "This room already has two DJs. Tick \"Broadcaster only\" to join just to stream it.".to_string())
        };
        if let Some(reason) = full {
            drop(ses);
            let _ = write_msg(&mut s, &Msg::Reject { reason }, &[]);
            bail!("room full");
        }
        let conn = PeerConn::new(s.try_clone()?)?;
        let reply = conn.sender();
        conn.send(ses.welcome());
        let serial = if broadcaster {
            Some(ses.add_broadcaster(conn, info.clone()))
        } else {
            ses.add_peer(conn, info.clone());
            None
        };
        (ses.clock(), reply, serial)
    };
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    let result = loop {
        match read_msg(&mut s) {
            Ok((Msg::Ping { id, t1, rtt }, _)) => {
                let remote = clock.now();
                let _ = reply.send(Msg::Pong { id, t1, remote });
                if let (Some(r), None) = (rtt, serial) {
                    h.lock().set_peer_rtt(r);
                }
            }
            Ok((Msg::Bye, _)) => break Ok(()),
            Ok((Msg::ResyncRequest, _)) if serial.is_some() => {
                let timeline = h.lock().timeline().clone();
                let _ = reply.send(Msg::Resync { timeline });
            }
            // A broadcaster can't change the room, whatever it sends.
            Ok((m, _)) if serial.is_some() => {
                log::debug!("ignoring {m:?} from broadcaster {}", info.name)
            }
            Ok((m, _)) => h.lock().on_control_msg(m),
            Err(e) if is_disconnect(&e) => break Ok(()),
            Err(e) => break Err(anyhow!(e)),
        }
    };
    match serial {
        Some(serial) => h.lock().on_broadcaster_left(serial),
        None => h.lock().on_peer_left(info.id),
    }
    result
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
    broadcaster: bool,
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
        },
        &[],
    )?;
    Ok(s)
}

/// Join a host's room, as the second DJ or, with `broadcaster`, only to
/// play and show the set for a stream. Blocks for the handshake and initial
/// clock sync (about a second), then runs in the background.
pub fn join(
    h: &SessionHandle,
    addr_text: &str,
    room: &str,
    broadcaster: bool,
) -> anyhow::Result<()> {
    let addr = resolve(addr_text)?;
    let (me, clock) = {
        let s = h.lock();
        (s.me.clone(), s.clock())
    };
    let mut s = connect(addr, ConnKind::Control, room, &me, broadcaster)?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (first, _) = read_msg(&mut s).context("no answer from host")?;
    let (host, timeline, tracks) = match first {
        Msg::Welcome {
            host,
            timeline,
            tracks,
        } => (host, timeline, tracks),
        Msg::Reject { reason } => bail!(reason),
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
    clock.set_offset(best.offset_us);
    log::info!(
        "Clock synced to {}: round trip {:.1} ms",
        host.name,
        best.rtt_us as f64 / 1000.0
    );

    let conn = PeerConn::new(s.try_clone()?)?;
    let alive = conn.alive.clone();
    let alive_r = conn.alive.clone();
    let host_id = host.id;
    h.lock().on_joined(
        conn,
        host,
        timeline,
        tracks,
        est,
        addr,
        room.trim().to_uppercase(),
        broadcaster,
    );
    for m in buffered {
        h.lock().on_control_msg(m);
    }

    // Reader.
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    let (h2, clock2) = (h.clone(), clock.clone());
    let mut reader = s.try_clone()?;
    thread::Builder::new()
        .name("tandem-net-reader".into())
        .spawn(move || {
            loop {
                match read_msg(&mut reader) {
                    Ok((Msg::Pong { t1, remote, .. }, _)) => {
                        let t2 = clock2.local();
                        h2.lock().on_pong(t1, remote, t2);
                    }
                    Ok((Msg::Bye, _)) => break,
                    Ok((m, _)) => h2.lock().on_control_msg(m),
                    Err(e) => {
                        if !is_disconnect(&e) && alive_r.load(Ordering::Relaxed) {
                            log::warn!("Lost connection to host: {e}");
                        }
                        break;
                    }
                }
            }
            h2.lock().on_peer_left(host_id);
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
                if !alive.load(Ordering::Relaxed) {
                    break;
                }
                let ses = h3.lock();
                let Some(conn) = ses.peer_conn() else { break };
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
