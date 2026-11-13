//! Read-only support for Serato crates (`_Serato_/Subcrates/*.crate`).
//!
//! A crate file is a flat list of fields: a 4-byte ASCII tag, a big-endian
//! u32 length, then the data. Each `otrk` field holds nested fields, and its
//! `ptrk` field is the track path as UTF-16BE, relative to the root of the
//! drive the `_Serato_` folder lives on. Tandem never writes Serato files.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeratoCrate {
    /// Display name; nested crates are shown as "Parent / Child".
    pub name: String,
    pub file: PathBuf,
    /// Drive root that the crate's relative paths start from.
    pub root: PathBuf,
}

fn fields(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let tag = [data[0], data[1], data[2], data[3]];
        let len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let end = (8 + len).min(data.len());
        out.push((tag, &data[8..end]));
        data = &data[end..];
    }
    out
}

fn utf16be(data: &[u8]) -> String {
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}

/// Track paths exactly as stored in the crate.
pub fn parse_crate_paths(data: &[u8]) -> Vec<String> {
    fields(data)
        .into_iter()
        .filter(|(tag, _)| tag == b"otrk")
        .filter_map(|(_, inner)| {
            fields(inner)
                .into_iter()
                .find(|(t, _)| t == b"ptrk")
                .map(|(_, p)| utf16be(p))
        })
        .filter(|p| !p.is_empty())
        .collect()
}

pub fn resolve(root: &Path, stored: &str) -> PathBuf {
    let normalised: String = stored
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' {
                std::path::MAIN_SEPARATOR
            } else {
                c
            }
        })
        .collect();
    let p = PathBuf::from(&normalised);
    if p.is_absolute() {
        p
    } else {
        root.join(normalised.trim_start_matches(std::path::MAIN_SEPARATOR))
    }
}

pub fn crate_tracks(c: &SeratoCrate) -> std::io::Result<Vec<PathBuf>> {
    let data = std::fs::read(&c.file)?;
    Ok(parse_crate_paths(&data)
        .iter()
        .map(|p| resolve(&c.root, p))
        .collect())
}

/// The drive root a path lives on ("C:\" on Windows, "/" elsewhere).
fn drive_root(p: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => root.push(c.as_os_str()),
            _ => break,
        }
    }
    if root.as_os_str().is_empty() {
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string())
    } else {
        root
    }
}

/// Find crates in `<Music>/_Serato_` and, on Windows, `X:\_Serato_` on every drive.
pub fn discover(music_dir: Option<&Path>) -> Vec<SeratoCrate> {
    let mut serato_dirs: Vec<PathBuf> = Vec::new();
    if let Some(m) = music_dir {
        serato_dirs.push(m.join("_Serato_"));
    }
    if cfg!(windows) {
        for letter in b'C'..=b'Z' {
            serato_dirs.push(PathBuf::from(format!("{}:\\_Serato_", letter as char)));
        }
    }
    let mut out = Vec::new();
    for dir in serato_dirs {
        let sub = dir.join("Subcrates");
        let Ok(rd) = std::fs::read_dir(&sub) else {
            continue;
        };
        let root = drive_root(&dir);
        let mut crates: Vec<SeratoCrate> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .map(|e| e.eq_ignore_ascii_case("crate"))
                    .unwrap_or(false)
            })
            .map(|file| SeratoCrate {
                name: file
                    .file_stem()
                    .map(|s| s.to_string_lossy().replace("%%", " / "))
                    .unwrap_or_default(),
                file,
                root: root.clone(),
            })
            .collect();
        crates.sort_by_key(|c| c.name.to_lowercase());
        for c in crates {
            if !out.iter().any(|o: &SeratoCrate| o.file == c.file) {
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    fn u16be(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_be_bytes()).collect()
    }

    #[test]
    fn parses_crate_tracks() {
        let mut data = field(b"vrsn", &u16be("1.0/Serato ScratchLive Crate"));
        data.extend(field(b"osrt", &field(b"tvcn", &u16be("song"))));
        data.extend(field(
            b"otrk",
            &field(b"ptrk", &u16be("Users/dj/Music/House/Track One.mp3")),
        ));
        data.extend(field(
            b"otrk",
            &field(b"ptrk", &u16be("Users/dj/Music/Ünïcödé – Two.flac")),
        ));
        let paths = parse_crate_paths(&data);
        assert_eq!(
            paths,
            vec![
                "Users/dj/Music/House/Track One.mp3",
                "Users/dj/Music/Ünïcödé – Two.flac"
            ]
        );
        let serato_dir = if cfg!(windows) {
            "C:\\Users\\x\\Music\\_Serato_"
        } else {
            "/home/x/Music/_Serato_"
        };
        let root = drive_root(Path::new(serato_dir));
        let resolved = resolve(&root, &paths[0]);
        assert!(resolved.ends_with(
            Path::new("Users")
                .join("dj")
                .join("Music")
                .join("House")
                .join("Track One.mp3")
        ));
        assert!(resolved.is_absolute());
    }

    #[test]
    fn tolerates_truncated_files() {
        let mut data = field(b"otrk", &field(b"ptrk", &u16be("a.mp3")));
        data.truncate(data.len() - 3);
        let _ = parse_crate_paths(&data); // must not panic
    }
}
