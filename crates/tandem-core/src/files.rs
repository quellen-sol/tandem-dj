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

use crate::model::TrackId;
use crate::net::connect;
use crate::protocol::{read_msg, write_msg, ConnKind, Msg, PeerInfo};
use crate::session::{Servable, SessionHandle, TransferState};
use anyhow::{bail, Context};
use parking_lot::Mutex;
use std::io::{BufWriter, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
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

/// Host side of a files connection (after the Hello).
pub fn serve(h: &SessionHandle, mut s: TcpStream, broadcaster: bool) -> anyhow::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (m, _) = read_msg(&mut s)?;
    match m {
        Msg::FileGet { id } => {
            let start = Instant::now();
            let path = loop {
                match h.lock().servable(id) {
                    Servable::Ready(path) => break Some(path),
                    Servable::Pending if start.elapsed() < PENDING_WAIT => {}
                    Servable::Pending | Servable::No => break None,
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            let rate = h.lock().max_upload_bytes_s;
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
            let file_name = file_name(&path);
            write_msg(
                &mut s,
                &Msg::FileHeader {
                    id,
                    size,
                    file_name: file_name.clone(),
                },
                &[],
            )?;
            h.lock()
                .transfer_set(id, true, &file_name, 0, size, TransferState::Active);
            let res = send_body(h, &mut s, id, &path, size, rate);
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
        Msg::FilePut { id, size, .. } => {
            let dest = (!broadcaster).then(|| h.lock().accept_push(id)).flatten();
            let Some(dest) = dest else {
                write_msg(
                    &mut s,
                    &Msg::FileError {
                        reason: "Not expecting that file.".into(),
                    },
                    &[],
                )?;
                return Ok(());
            };
            write_msg(&mut s, &Msg::FileOk, &[])?;
            let res = recv_body(h, &mut s, id, size, &dest);
            let ok = res.is_ok();
            h.lock()
                .on_file_received(id, res.map(|_| dest).map_err(|e| format!("{e:#}")));
            if ok {
                write_msg(&mut s, &Msg::FileOk, &[])?;
            }
            Ok(())
        }
        other => bail!("unexpected {other:?} on files connection"),
    }
}

/// Joiner: download a track from the host into `dest`.
pub fn fetch(
    h: &SessionHandle,
    addr: SocketAddr,
    room: &str,
    me: &PeerInfo,
    broadcaster: bool,
    id: TrackId,
    dest: &Path,
) -> anyhow::Result<()> {
    let key = h.lock().booth_key();
    let mut s = connect(addr, ConnKind::Files, room, me, key, broadcaster)?;
    write_msg(&mut s, &Msg::FileGet { id }, &[])?;
    let (m, _) = read_msg(&mut s)?;
    let size = match m {
        Msg::FileHeader { size, .. } => size,
        Msg::FileError { reason } => bail!(reason),
        Msg::Reject { reason } => bail!(reason),
        other => bail!("unexpected reply {other:?}"),
    };
    h.lock().transfer_progress(id, false, 0, size);
    recv_body(h, &mut s, id, size, dest)
}

/// Joiner: upload one of our tracks because the host asked for it.
pub fn push(
    h: &SessionHandle,
    addr: SocketAddr,
    room: &str,
    me: &PeerInfo,
    id: TrackId,
    path: &Path,
) -> anyhow::Result<()> {
    let size = std::fs::metadata(path)?.len();
    let name = file_name(path);
    let rate = h.lock().max_upload_bytes_s;
    let key = h.lock().booth_key();
    let mut s = connect(addr, ConnKind::Files, room, me, key, false)?;
    write_msg(
        &mut s,
        &Msg::FilePut {
            id,
            size,
            file_name: name.clone(),
        },
        &[],
    )?;
    match read_msg(&mut s)?.0 {
        Msg::FileOk => {}
        Msg::FileError { reason } | Msg::Reject { reason } => bail!(reason),
        other => bail!("unexpected reply {other:?}"),
    }
    h.lock()
        .transfer_set(id, true, &name, 0, size, TransferState::Active);
    send_body(h, &mut s, id, path, size, rate)?;
    match read_msg(&mut s)?.0 {
        Msg::FileOk => {
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

fn send_body(
    h: &SessionHandle,
    s: &mut TcpStream,
    id: TrackId,
    path: &Path,
    size: u64,
    rate: f64,
) -> anyhow::Result<()> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; CHUNK];
    let pacer = h.lock().upload_pacer();
    let mut last = Instant::now();
    let mut sent = 0u64;
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

fn recv_body(
    h: &SessionHandle,
    s: &mut TcpStream,
    id: TrackId,
    size: u64,
    dest: &Path,
) -> anyhow::Result<()> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = PathBuf::from(format!("{}.part", dest.display()));
    let result = (|| -> anyhow::Result<()> {
        let mut f = BufWriter::new(
            std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?,
        );
        let mut hasher = blake3::Hasher::new();
        let mut done = 0u64;
        let mut last = Instant::now();
        loop {
            let (m, payload) = read_msg(s)?;
            match m {
                Msg::Chunk => {
                    f.write_all(&payload)?;
                    hasher.update(&payload);
                    done += payload.len() as u64;
                    if done > size {
                        bail!("more data than expected");
                    }
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
        drop(f);
        if done != size || TrackId(*hasher.finalize().as_bytes()) != id {
            bail!("file failed its integrity check");
        }
        h.lock().transfer_progress(id, false, done, size);
        std::fs::rename(&part, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
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
