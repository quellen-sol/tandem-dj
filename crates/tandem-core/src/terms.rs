//! The personal-use terms and the local consent log.
//!
//! Plain language on purpose: short enough that people actually read it.
//! Every acceptance is appended to `consent-log.jsonl` in the data folder,
//! so there's a record of who agreed to what and when.

use serde::Serialize;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bump when the terms change; everyone is asked to accept again.
pub const TERMS_VERSION: u32 = 1;

pub const TERMS_TITLE: &str = "Tandem: personal use terms";

pub const TERMS_SUMMARY: &str = "Tandem is for personal, non-commercial DJing between friends. Only share music you have the right to use, keep what you receive private, and never use Tandem to make money.";

pub const TERMS_TEXT: &str = "\
1. Personal, non-commercial use only.
   Tandem is not for commercial use of any kind: no paid gigs, sponsored or monetised streams, client work, or business use.

2. Only share music you have the right to use.
   When you share a folder or crate, you confirm you own those files or are licensed to use them.

3. Shared music stays private.
   Tracks someone shares with you are for your own personal use. Don't sell, upload, re-share or redistribute them. Delete them if the person who shared them asks you to.

4. Live sessions.
   When your partner loads a track you don't have, Tandem copies it to your computer so you can hear the mix. These session copies are temporary and are deleted when Tandem closes.

5. Broadcasting is your responsibility.
   If you stream a set publicly, you are responsible for any permission or licence your platform or country requires.

6. Copyright law still applies.
   These terms are an agreement between the people using Tandem. They don't grant anyone rights in any music, and they don't make sharing legal where it otherwise isn't.

7. No warranty.
   Tandem is provided as is, without warranty. Its authors aren't liable for how it's used.

Tandem keeps a local record of what you agree to (consent-log.jsonl in its data folder). Nothing is sent anywhere except to the people in your room.";

/// What the sharer confirms before an offer goes out.
pub fn sender_pledge(count: usize) -> String {
    format!(
        "I own or am licensed to use these {count} tracks, and I'm sharing them privately with the people in this room for personal, non-commercial use only."
    )
}

/// What the receiver agrees to in the popup.
pub fn receiver_pledge(from: &str) -> String {
    format!(
        "I'll use these tracks for personal, non-commercial purposes only. I won't sell, upload, re-share or redistribute them, and I'll delete them if {from} asks me to."
    )
}

#[derive(Serialize)]
pub struct ConsentRecord<'a> {
    pub unix_time: u64,
    /// "terms", "share-sent", "share-accepted" or "share-declined".
    pub kind: &'a str,
    pub terms_version: u32,
    pub me: &'a str,
    pub other: Option<&'a str>,
    pub share_title: Option<&'a str>,
    pub statement: Option<&'a str>,
    pub tracks: Vec<String>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn append_consent(path: &Path, rec: &ConsentRecord) {
    let line = match serde_json::to_string(rec) {
        Ok(l) => l,
        Err(_) => return,
    };
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = res {
        log::warn!("couldn't write consent log: {e}");
    }
}
