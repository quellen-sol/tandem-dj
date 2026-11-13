//! Track transfer over a dedicated TCP connection per file.
//!
//! Connections always go from the joining DJ to the host (so only the host
//! needs an open port): the joiner either downloads (`FileGet`) or uploads
//! (`FilePut`, when the host asked for one of the joiner's tracks). Every
//! file is checked against its content hash before it's used.

use crate::model::TrackId;
use crate::net::connect;
use crate::protocol::{read_msg, write_msg, ConnKind, Msg, PeerInfo};
use crate::session::{SessionHandle, TransferState};
use anyhow::{bail, Context};
use std::io::{BufWriter, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CHUNK: usize = 128 * 1024;

/// Host side of a files connection (after the Hello).
pub fn serve(h: &SessionHandle, mut s: TcpStream) -> anyhow::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    s.set_write_timeout(Some(Duration::from_secs(30)))?;
    let (m, _) = read_msg(&mut s)?;
    match m {
        Msg::FileGet { id } => {
            let (path, rate) = {
                let ses = h.lock();
                (ses.servable_path(id), ses.max_upload_bytes_s)
            };
            let Some(path) = path else {
                write_msg(
                    &mut s,
                    &Msg::FileError {
                        reason: "That track isn't shared.".into(),
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
            let Some(dest) = h.lock().accept_push(id) else {
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
    id: TrackId,
    dest: &Path,
) -> anyhow::Result<()> {
    let mut s = connect(addr, ConnKind::Files, room, me)?;
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
    let mut s = connect(addr, ConnKind::Files, room, me)?;
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
    let start = Instant::now();
    let mut last = Instant::now();
    let mut sent = 0u64;
    let mut w = BufWriter::with_capacity(CHUNK + 1024, s.try_clone()?);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        write_msg(&mut w, &Msg::Chunk, &buf[..n])?;
        sent += n as u64;
        if last.elapsed() > Duration::from_millis(150) {
            h.lock().transfer_progress(id, true, sent, size);
            last = Instant::now();
        }
        // Stay under the upload cap so transfers don't swamp the control link.
        if rate > 0.0 {
            let due = Duration::from_secs_f64(sent as f64 / rate);
            let el = start.elapsed();
            if due > el {
                std::thread::sleep(due - el);
            }
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
