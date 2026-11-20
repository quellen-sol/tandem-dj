//! Track transfer over a dedicated TCP connection per file.
//!
//! Connections always go from the joining DJ to the host (so only the host
//! needs an open port): the joiner either downloads (`FileGet`) or uploads
//! (`FilePut`, when the host asked for one of the joiner's tracks). Every
//! file is checked against its content hash before it's used.
//!
//! Broadcasters only download. A track the joining DJ loaded reaches them
//! through the host: if the host is still receiving it, the download waits.
//!
//! The upload speed limit is for all of a booth's uploads together (one
//! [`UploadPacer`] per session), so a host sending tracks to its partner and
//! to broadcasters at once doesn't fill its uplink and delay its moves.
//!
//! A transfer cut off part-way carries on from where it got to. The
//! receiver keeps what arrived in a `.part` file named after the track's
//! hash, in the session cache, and asks for the rest by offset: `FileGet`
//! says where to start, and for an upload the host's `FileOk` does. The
//! whole file is still checked against its hash at the end. A partial file
//! that doesn't check out is cleared and the track sent again from the
//! start. Every transfer is registered in [`Running`], so a dropped link
//! stops its transfers at once, and a transfer that wants a `.part` file
//! that another is still writing stops that one and waits for it to let
//! go: two transfers never write one file.

use crate::config::unique_path;
use crate::model::TrackId;
use crate::net::connect;
use crate::protocol::{read_msg, write_msg, ConnKind, Msg, PeerInfo};
use crate::session::{Expected, Servable, SessionHandle, TransferState};
use anyhow::{bail, Context};
use parking_lot::{Condvar, Mutex};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shares one upload speed limit between every upload a booth is sending.
/// Each chunk books the next free slot on the line, so concurrent uploads
/// take turns and together never go faster than the limit.
pub struct UploadPacer {
    next: Mutex<Instant>,
}

impl Default for UploadPacer {
    fn default() -> Self {
        Self {
            next: Mutex::new(Instant::now()),
        }
    }
}

impl UploadPacer {
    /// Wait until `bytes` more can be sent at `rate` bytes/s (0 = no limit).
    pub fn take(&self, bytes: usize, rate: f64) {
        if rate <= 0.0 {
            return;
        }
        let start = {
            let mut next = self.next.lock();
            // Time nobody used isn't saved up for a burst later.
            let start = (*next).max(Instant::now());
            *next = start + Duration::from_secs_f64(bytes as f64 / rate);
            start
        };
        let now = Instant::now();
        if start > now {
            std::thread::sleep(start - now);
        }
    }
}

const CHUNK: usize = 128 * 1024;
/// How long a download waits for a track the host is still receiving.
/// Under the asker's 30 s read timeout; it asks again if this runs out.
const PENDING_WAIT: Duration = Duration::from_secs(20);
/// How long a transfer waits for an earlier one to let go of a `.part`
/// file after stopping it. A stopped transfer lets go as soon as its
/// connection closes, or, if it's still connecting, when that gives up.
const PART_WAIT: Duration = Duration::from_secs(15);

/// The transfers running in this booth, so they can be stopped: all of
/// them with a booth whose link dropped, and one writing a `.part` file
/// that a newer transfer of the same track wants.
#[derive(Default)]
pub struct Running {
    runs: Mutex<Vec<Arc<Run>>>,
    freed: Condvar,
}

/// One running transfer.
struct Run {
    /// The booth at the other end.
    peer: u64,
    /// The `.part` file it writes, if it's receiving.
    part: Option<PathBuf>,
    stopped: AtomicBool,
    conn: Mutex<Option<TcpStream>>,
}

impl Run {
    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(s) = self.conn.lock().as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

/// A transfer's place in [`Running`], until it's dropped.
pub struct RunGuard {
    running: Arc<Running>,
    run: Arc<Run>,
}

impl RunGuard {
    /// Something stopped this transfer: its link dropped, or a newer
    /// transfer wants its `.part` file.
    pub fn stopped(&self) -> bool {
        self.run.stopped.load(Ordering::Relaxed)
    }

    /// The transfer runs over `s` from now on: stopping it closes `s`,
    /// which ends a read or write on it at once.
    fn attach(&self, s: &TcpStream) -> anyhow::Result<()> {
        let mut conn = self.run.conn.lock();
        if self.stopped() {
            let _ = s.shutdown(Shutdown::Both);
            return Err(Stopped.into());
        }
        *conn = Some(s.try_clone()?);
        Ok(())
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        self.running
            .runs
            .lock()
            .retain(|r| !Arc::ptr_eq(r, &self.run));
        self.running.freed.notify_all();
    }
}

impl Running {
    /// Register a transfer with booth `peer`. One that writes `part` gets
    /// that file to itself: a transfer still writing it is stopped first,
    /// and this waits until it has let go.
    pub fn start(self: &Arc<Self>, peer: u64, part: Option<&Path>) -> anyhow::Result<RunGuard> {
        let deadline = Instant::now() + PART_WAIT;
        let mut runs = self.runs.lock();
        while let Some(old) =
            part.and_then(|p| runs.iter().find(|r| r.part.as_deref() == Some(p)).cloned())
        {
            old.stop();
            if Instant::now() >= deadline {
                bail!("an earlier transfer of this track is still finishing");
            }
            self.freed.wait_until(&mut runs, deadline);
        }
        let run = Arc::new(Run {
            peer,
            part: part.map(Path::to_path_buf),
            stopped: AtomicBool::new(false),
            conn: Mutex::new(None),
        });
        runs.push(run.clone());
        Ok(RunGuard {
            running: self.clone(),
            run,
        })
    }

    /// Stop every transfer with booth `peer`: the link to it dropped.
    pub fn stop_with(&self, peer: u64) {
        for r in self.runs.lock().iter().filter(|r| r.peer == peer) {
            r.stop();
        }
    }
}

/// The transfer was stopped (see [`RunGuard::stopped`]). Whatever stopped
/// it takes over from it, so it isn't a failure.
#[derive(Debug)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("transfer stopped")
    }
}

impl std::error::Error for Stopped {}

/// A transfer that carried on from a partial file ended with a file that
/// doesn't match the track's hash: the partial file was bad. It's cleared,
/// so the next try starts from the beginning.
#[derive(Debug)]
struct BadPartial;

impl std::fmt::Display for BadPartial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the partial file didn't check out")
    }
}

impl std::error::Error for BadPartial {}

/// True if a transfer ended because it was stopped, not because it failed.
pub fn was_stopped<T>(res: &anyhow::Result<T>) -> bool {
    matches!(res, Err(e) if e.downcast_ref::<Stopped>().is_some())
}

/// True if a transfer was cut off by the network (the connection closed,
/// went quiet or couldn't be made), so it can carry on later from where it
/// got to, rather than failing for good.
pub fn interrupted(e: &anyhow::Error) -> bool {
    use std::io::ErrorKind::*;
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                UnexpectedEof
                    | ConnectionReset
                    | ConnectionAborted
                    | ConnectionRefused
                    | NotConnected
                    | BrokenPipe
                    | TimedOut
                    | WouldBlock
            )
        })
    })
}

/// Where a received track goes once it checks out.
#[derive(Clone, Debug)]
pub enum Landing {
    /// This path (a session copy), replacing what's there.
    At(PathBuf),
    /// A new file of this name in this folder (a share), or `name (2)` and
    /// so on if that's taken.
    NewIn(PathBuf, String),
}

impl Landing {
    /// Move the checked file `part` here. Returns where it went.
    fn place(&self, part: &Path) -> std::io::Result<PathBuf> {
        let dest = match self {
            Landing::At(p) => {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                p.clone()
            }
            Landing::NewIn(dir, name) => {
                std::fs::create_dir_all(dir)?;
                take_name(dir.join(name))?
            }
        };
        move_file(part, &dest)?;
        Ok(dest)
    }
}

/// A free name for a new file at `path` (or `path (2)` …), taken by
/// creating it empty, so two tracks of one name finishing at once can't
/// both pick it.
fn take_name(path: PathBuf) -> std::io::Result<PathBuf> {
    let mut last = None;
    for _ in 0..10 {
        let p = unique_path(path.clone());
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free file name")))
}

/// Rename, or copy and delete: the session cache and the shared folder can
/// be on different drives.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
}

/// What a track's `.part` file holds already, hashed, so the whole file's
/// hash carries on from there.
struct Partial {
    file: File,
    have: u64,
    hasher: blake3::Hasher,
}

impl Partial {
    /// Open the `.part` file at `path`, which `run` writes (making it if
    /// need be), and hash what's in it.
    fn open(path: &Path, run: &RunGuard) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        let mut hasher = blake3::Hasher::new();
        let mut have = 0u64;
        let mut buf = vec![0u8; CHUNK];
        loop {
            if run.stopped() {
                return Err(Stopped.into());
            }
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
        Ok(Self { file, have, hasher })
    }

    /// Write from byte `offset` on: where the file ends (carrying on), or
    /// 0 (starting over).
    fn start_at(&mut self, offset: u64) -> anyhow::Result<()> {
        if offset == 0 {
            self.clear()?;
        } else if offset != self.have {
            bail!(
                "asked to carry on from byte {offset} of a partial file that has {}",
                self.have
            );
        }
        self.file.seek(SeekFrom::Start(offset))?;
        Ok(())
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.have = 0;
        self.hasher = blake3::Hasher::new();
        Ok(())
    }
}

fn percent(done: u64, size: u64) -> f64 {
    if size == 0 {
        100.0
    } else {
        done as f64 * 100.0 / size as f64
    }
}

/// Host side of a files connection (after the Hello from booth `from`).
pub fn serve(
    h: &SessionHandle,
    mut s: TcpStream,
    from: &PeerInfo,
    broadcaster: bool,
) -> anyhow::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (m, _) = read_msg(&mut s)?;
    match m {
        Msg::FileGet { id, offset } => {
            let start = Instant::now();
            let path = loop {
                match h.lock().servable(id) {
                    Servable::Ready(path) => break Some(path),
                    Servable::Pending if start.elapsed() < PENDING_WAIT => {}
                    Servable::Pending | Servable::No => break None,
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            let (rate, running) = {
                let ses = h.lock();
                (ses.max_upload_bytes_s, ses.running())
            };
            let Some(path) = path else {
                write_msg(
                    &mut s,
                    &Msg::FileError {
                        reason: "The host doesn't have that track yet.".into(),
                    },
                    &[],
                )?;
                return Ok(());
            };
            let size = std::fs::metadata(&path)?.len();
            // Carry on from where the asker got to, if the file has that.
            let offset = if offset <= size { offset } else { 0 };
            let file_name = file_name(&path);
            let run = running.start(from.id, None)?;
            run.attach(&s)?;
            write_msg(
                &mut s,
                &Msg::FileHeader {
                    id,
                    size,
                    file_name: file_name.clone(),
                    offset,
                },
                &[],
            )?;
            if offset > 0 {
                log::info!(
                    "Resuming the upload of {file_name} to {} from byte {offset} of {size} ({:.0}%)",
                    from.name,
                    percent(offset, size)
                );
            }
            h.lock()
                .transfer_set(id, true, &file_name, offset, size, TransferState::Active);
            let res = send_body(h, &mut s, id, &path, offset, size, rate);
            h.lock().transfer_state(
                id,
                true,
                match &res {
                    Ok(_) => TransferState::Done,
                    Err(e) => TransferState::Failed(describe(e)),
                },
            );
            res
        }
        Msg::FilePut {
            id, size, resume, ..
        } => {
            let expected = (!broadcaster).then(|| h.lock().accept_push(id)).flatten();
            let Some(expected) = expected else {
                write_msg(
                    &mut s,
                    &Msg::FileError {
                        reason: "Not expecting that file.".into(),
                    },
                    &[],
                )?;
                return Ok(());
            };
            let running = h.lock().running();
            let run = running.start(from.id, Some(&expected.part))?;
            let res = recv_push(h, &mut s, &run, id, size, resume, &expected, from);
            match res {
                // Its link dropped, or a newer upload of it took over.
                Err(_) if run.stopped() => Ok(()),
                Err(e) if interrupted(&e) => {
                    h.lock().fetch_interrupted(id, expected.serial);
                    Ok(())
                }
                Err(e) if e.downcast_ref::<BadPartial>().is_some() => {
                    log::warn!(
                        "{}'s upload of {} didn't check out after carrying on: asking for it again from the start",
                        from.name,
                        h.lock().track_file_name(id)
                    );
                    let _ = write_msg(
                        &mut s,
                        &Msg::FileError {
                            reason: "the file didn't check out".into(),
                        },
                        &[],
                    );
                    h.lock().push_again(id, expected.serial);
                    Ok(())
                }
                res => {
                    let ok = res.is_ok();
                    h.lock()
                        .file_received(id, expected.serial, res.map_err(|e| describe(&e)));
                    if ok {
                        write_msg(&mut s, &Msg::FileOk { offset: 0 }, &[])?;
                    }
                    Ok(())
                }
            }
        }
        other => bail!("unexpected {other:?} on files connection"),
    }
}

/// Host: receive the upload the joining DJ (`from`) offered, carrying on
/// from the `.part` file if it can.
#[allow(clippy::too_many_arguments)]
fn recv_push(
    h: &SessionHandle,
    s: &mut TcpStream,
    run: &RunGuard,
    id: TrackId,
    size: u64,
    resume: bool,
    expected: &Expected,
    from: &PeerInfo,
) -> anyhow::Result<PathBuf> {
    run.attach(s)?;
    let mut p = Partial::open(&expected.part, run)?;
    let offset = if resume && p.have <= size { p.have } else { 0 };
    p.start_at(offset)?;
    write_msg(s, &Msg::FileOk { offset }, &[])?;
    let name = {
        let mut ses = h.lock();
        if run.stopped() {
            return Err(Stopped.into());
        }
        let name = ses.track_file_name(id);
        ses.transfer_set(id, false, &name, offset, size, TransferState::Active);
        name
    };
    if offset > 0 {
        log::info!(
            "Resuming the download of {name} from {} from byte {offset} of {size} ({:.0}%)",
            from.name,
            percent(offset, size)
        );
    }
    recv_body(h, s, id, size, &expected.part, p, &expected.landing)
}

/// Joiner: download a track from the host (booth `host`) and put it at
/// `landing`, carrying on from its `.part` file if an earlier try was cut
/// off. Returns where it went.
#[allow(clippy::too_many_arguments)]
pub fn fetch(
    h: &SessionHandle,
    addr: SocketAddr,
    room: &str,
    me: &PeerInfo,
    broadcaster: bool,
    host: u64,
    id: TrackId,
    landing: &Landing,
) -> anyhow::Result<PathBuf> {
    let (running, key, part, name) = {
        let s = h.lock();
        (
            s.running(),
            s.booth_key(),
            s.part_path(id),
            s.track_file_name(id),
        )
    };
    let run = running.start(host, Some(&part))?;
    let mut again = false;
    let res = loop {
        let res = (|| -> anyhow::Result<PathBuf> {
            let mut p = Partial::open(&part, &run)?;
            let mut s = connect(addr, ConnKind::Files, room, me, key, broadcaster)?;
            run.attach(&s)?;
            write_msg(&mut s, &Msg::FileGet { id, offset: p.have }, &[])?;
            let (size, offset) = match read_msg(&mut s)?.0 {
                Msg::FileHeader { size, offset, .. } => (size, offset),
                Msg::FileError { reason } => bail!(reason),
                Msg::Reject { reason } => bail!(reason),
                other => bail!("unexpected reply {other:?}"),
            };
            p.start_at(offset)?;
            {
                let mut ses = h.lock();
                if run.stopped() {
                    return Err(Stopped.into());
                }
                ses.transfer_set(id, false, &name, offset, size, TransferState::Active);
            }
            if offset > 0 {
                log::info!(
                    "Resuming the download of {name} from byte {offset} of {size} ({:.0}%)",
                    percent(offset, size)
                );
            }
            recv_body(h, &mut s, id, size, &part, p, landing)
        })();
        match res {
            Err(e) if e.downcast_ref::<BadPartial>().is_some() && !again => {
                log::warn!(
                    "The partial download of {name} didn't check out: downloading it again from the start"
                );
                again = true;
            }
            res => break res,
        }
    };
    match res {
        Err(_) if run.stopped() => Err(Stopped.into()),
        res => res,
    }
}

/// Joiner: upload one of our tracks because the host (booth `host`) asked
/// for it, from where the host says it got to.
pub fn push(
    h: &SessionHandle,
    addr: SocketAddr,
    room: &str,
    me: &PeerInfo,
    host: u64,
    id: TrackId,
    path: &Path,
) -> anyhow::Result<()> {
    let size = std::fs::metadata(path)?.len();
    let name = file_name(path);
    let (rate, key, running) = {
        let s = h.lock();
        (s.max_upload_bytes_s, s.booth_key(), s.running())
    };
    let run = running.start(host, None)?;
    let mut s = connect(addr, ConnKind::Files, room, me, key, false)?;
    run.attach(&s)?;
    write_msg(
        &mut s,
        &Msg::FilePut {
            id,
            size,
            file_name: name.clone(),
            resume: true,
        },
        &[],
    )?;
    let offset = match read_msg(&mut s)?.0 {
        Msg::FileOk { offset } if offset <= size => offset,
        Msg::FileOk { offset } => bail!("the host asked for byte {offset} of {size}"),
        Msg::FileError { reason } | Msg::Reject { reason } => bail!(reason),
        other => bail!("unexpected reply {other:?}"),
    };
    if offset > 0 {
        log::info!(
            "Resuming the upload of {name} from byte {offset} of {size} ({:.0}%)",
            percent(offset, size)
        );
    }
    h.lock()
        .transfer_set(id, true, &name, offset, size, TransferState::Active);
    send_body(h, &mut s, id, path, offset, size, rate)?;
    match read_msg(&mut s)?.0 {
        Msg::FileOk { .. } => {
            h.lock().transfer_state(id, true, TransferState::Done);
            Ok(())
        }
        other => bail!("host didn't confirm the upload ({other:?})"),
    }
}

/// Short, human description of a transfer error.
pub fn describe(e: &anyhow::Error) -> String {
    if crate::net::is_disconnect_err(e) {
        "connection closed".into()
    } else {
        format!("{e:#}")
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Send the file at `path` from byte `from` on.
fn send_body(
    h: &SessionHandle,
    s: &mut TcpStream,
    id: TrackId,
    path: &Path,
    from: u64,
    size: u64,
    rate: f64,
) -> anyhow::Result<()> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = vec![0u8; CHUNK];
    let pacer = h.lock().upload_pacer();
    let mut last = Instant::now();
    let mut sent = from;
    let mut w = BufWriter::with_capacity(CHUNK + 1024, s.try_clone()?);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // Stay under the upload limit, shared with this booth's other
        // uploads, so transfers don't swamp the control link.
        pacer.take(n, rate);
        write_msg(&mut w, &Msg::Chunk, &buf[..n])?;
        sent += n as u64;
        if last.elapsed() > Duration::from_millis(150) {
            h.lock().transfer_progress(id, true, sent, size);
            last = Instant::now();
        }
    }
    write_msg(&mut w, &Msg::End, &[])?;
    h.lock().transfer_progress(id, true, sent, size);
    Ok(())
}

/// Receive the rest of a track (from `p.have` on) into its `.part` file at
/// `part`, check the whole file against the track's hash and move it to
/// `landing`. Returns where it went. If the connection breaks, the `.part`
/// file keeps what arrived, for the next try to carry on from. A file
/// that doesn't check out is cleared; when it carried on from a partial
/// file, that's a [`BadPartial`].
fn recv_body(
    h: &SessionHandle,
    s: &mut TcpStream,
    id: TrackId,
    size: u64,
    part: &Path,
    mut p: Partial,
    landing: &Landing,
) -> anyhow::Result<PathBuf> {
    let resumed = p.have > 0;
    let mut done = p.have;
    let mut fits = true;
    {
        let mut f = BufWriter::with_capacity(CHUNK, &p.file);
        let mut last = Instant::now();
        loop {
            let (m, payload) = read_msg(s)?;
            match m {
                Msg::Chunk => {
                    if done + payload.len() as u64 > size {
                        fits = false;
                        break;
                    }
                    f.write_all(&payload)?;
                    p.hasher.update(&payload);
                    done += payload.len() as u64;
                    if last.elapsed() > Duration::from_millis(150) {
                        h.lock().transfer_progress(id, false, done, size);
                        last = Instant::now();
                    }
                }
                Msg::End => break,
                Msg::FileError { reason } => bail!(reason),
                other => bail!("unexpected {other:?} during transfer"),
            }
        }
        f.flush()?;
    }
    if !fits || done != size || TrackId(*p.hasher.finalize().as_bytes()) != id {
        p.clear()?;
        if resumed {
            return Err(BadPartial.into());
        }
        bail!("file failed its integrity check");
    }
    h.lock().transfer_progress(id, false, done, size);
    drop(p); // closed before it moves
    Ok(landing.place(part)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Two uploads at once share the limit: 2 MB at 10 MB/s takes about
    /// 0.2 s in total, not 0.1 s each side by side.
    #[test]
    fn concurrent_uploads_share_one_limit() {
        let pacer = Arc::new(UploadPacer::default());
        let rate = 10.0 * 1e6;
        let start = Instant::now();
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let p = pacer.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        p.take(100_000, rate);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let took = start.elapsed();
        // The first chunk goes straight away; the other 19 wait their turn.
        assert!(took >= Duration::from_millis(185), "took {took:?}");
        assert!(took < Duration::from_millis(1000), "took {took:?}");
    }

    /// A transfer that wants a `.part` file another is still writing (one
    /// the link dropped under, blocked on its dead connection) stops it and
    /// gets the file only once that one has let go: never both at once.
    #[test]
    fn one_writer_per_part_file() {
        let running = Arc::new(Running::default());
        let part = std::env::temp_dir().join(format!("tandem-part-{}.part", fastrand::u64(..)));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut old_conn = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server_end, _) = listener.accept().unwrap();

        let first = running.start(7, Some(&part)).unwrap();
        first.attach(&old_conn).unwrap();
        let writing = Arc::new(AtomicBool::new(true));
        let w = writing.clone();
        let old = std::thread::spawn(move || {
            // Blocked on a connection that sends nothing, like one whose
            // network went away.
            let mut buf = [0u8; 16];
            let res = old_conn.read(&mut buf);
            std::thread::sleep(Duration::from_millis(100)); // finishing a write
            assert!(
                first.stopped(),
                "read returned {res:?} without being stopped"
            );
            w.store(false, Ordering::SeqCst);
            drop(first);
        });

        let start = Instant::now();
        let second = running.start(7, Some(&part)).unwrap();
        assert!(
            !writing.load(Ordering::SeqCst),
            "got the file while the old transfer was still writing it"
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        old.join().unwrap();
        assert!(!second.stopped());

        // Another track's file, or an upload, doesn't wait.
        let other = running
            .start(7, Some(&part.with_extension("other")))
            .unwrap();
        let upload = running.start(7, None).unwrap();
        assert!(!second.stopped());

        // The link to booth 7 drops: all of them stop; booth 8's don't.
        let elsewhere = running.start(8, None).unwrap();
        running.stop_with(7);
        assert!(second.stopped() && other.stopped() && upload.stopped());
        assert!(!elsewhere.stopped());
        drop((second, other, upload, elsewhere));
        assert!(running.runs.lock().is_empty());
    }

    #[test]
    fn no_limit_never_waits() {
        let pacer = UploadPacer::default();
        let start = Instant::now();
        for _ in 0..1000 {
            pacer.take(1 << 20, 0.0);
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
