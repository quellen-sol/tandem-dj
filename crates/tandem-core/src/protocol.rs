//! Wire protocol: length-prefixed JSON messages with an optional binary
//! payload (used for file chunks).
//!
//! Frame: `[u32 json_len][json][u32 payload_len][payload]`, big-endian.
//!
//! Two kinds of TCP connection go from a joining DJ to the host:
//! * **Control** — one per peer: events, clock pings, shares. Kept small and
//!   latency-sensitive.
//! * **Files** — one per transfer, so a 60 MB track never queues in front of
//!   a fader move.

use crate::model::{Event, Timeline, TrackId};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const PROTOCOL_VERSION: u32 = 5;
pub const DEFAULT_PORT: u16 = 47800;
const MAX_JSON: u32 = 64 << 20;
const MAX_PAYLOAD: u32 = 8 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    pub bpm: Option<f32>,
    pub duration: f64,
    pub size: u64,
    pub file_name: String,
    /// Detected tempo and beat positions, if analysed.
    #[serde(default)]
    pub grid: Option<crate::analysis::BeatGrid>,
    /// Detected key (see [`crate::keys`]), if analysed.
    #[serde(default)]
    pub key: Option<i8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShareOffer {
    pub share_id: u64,
    pub from_id: u64,
    pub from_name: String,
    pub title: String,
    pub tracks: Vec<TrackInfo>,
}

impl ShareOffer {
    pub fn total_bytes(&self) -> u64 {
        self.tracks.iter().map(|t| t.size).sum()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnKind {
    Control,
    Files,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "msg")]
pub enum Msg {
    // --- handshake (both connection kinds)
    Hello {
        proto: u32,
        kind: ConnKind,
        name: String,
        room: String,
        peer_id: u64,
    },
    Welcome {
        host: PeerInfo,
        timeline: Timeline,
        tracks: Vec<TrackInfo>,
    },
    Reject {
        reason: String,
    },

    // --- control connection
    Ping {
        id: u64,
        t1: i64,
        rtt: Option<i64>,
    },
    Pong {
        id: u64,
        t1: i64,
        remote: i64,
    },
    Event {
        ev: Event,
    },
    Track {
        info: TrackInfo,
    },
    /// Host -> client: digest of the room state at shared time `t`.
    Digest {
        t: i64,
        digest: u64,
    },
    ResyncRequest,
    Resync {
        timeline: Timeline,
    },
    Share {
        offer: ShareOffer,
    },
    ShareReply {
        share_id: u64,
        accepted: bool,
        name: String,
    },
    /// Host -> client: please upload this track to me.
    PushRequest {
        id: TrackId,
    },
    /// Client -> host: I can't upload that one.
    NoFile {
        id: TrackId,
    },
    Bye,

    // --- files connection
    FileGet {
        id: TrackId,
    },
    FilePut {
        id: TrackId,
        size: u64,
        file_name: String,
    },
    FileHeader {
        id: TrackId,
        size: u64,
        file_name: String,
    },
    /// Payload carries the bytes.
    Chunk,
    End,
    FileOk,
    FileError {
        reason: String,
    },
}

pub fn write_msg<W: Write>(w: &mut W, msg: &Msg, payload: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(io::Error::other)?;
    let mut head = Vec::with_capacity(json.len() + 8);
    head.extend_from_slice(&(json.len() as u32).to_be_bytes());
    head.extend_from_slice(&json);
    head.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    w.write_all(&head)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

pub fn read_msg<R: Read>(r: &mut R) -> io::Result<(Msg, Vec<u8>)> {
    let json_len = read_u32(r)?;
    if json_len > MAX_JSON {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut json = vec![0u8; json_len as usize];
    r.read_exact(&mut json)?;
    let payload_len = read_u32(r)?;
    if payload_len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload too large",
        ));
    }
    let mut payload = vec![0u8; payload_len as usize];
    r.read_exact(&mut payload)?;
    let msg =
        serde_json::from_slice(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((msg, payload))
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Action;

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        let ev = Event {
            time: 5,
            origin: 1,
            seq: 2,
            action: Action::Crossfader { v: 0.25 },
        };
        write_msg(&mut buf, &Msg::Event { ev: ev.clone() }, &[]).unwrap();
        write_msg(&mut buf, &Msg::Chunk, &[1, 2, 3]).unwrap();
        let mut r = &buf[..];
        match read_msg(&mut r).unwrap() {
            (Msg::Event { ev: got }, p) => {
                assert_eq!(got, ev);
                assert!(p.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        let (m, p) = read_msg(&mut r).unwrap();
        assert!(matches!(m, Msg::Chunk));
        assert_eq!(p, vec![1, 2, 3]);
    }
}
