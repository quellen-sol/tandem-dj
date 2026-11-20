//! Settings and file locations.

use crate::model::XfCurve;
use crate::protocol::DEFAULT_PORT;
use crate::render::LocalParams;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub device: String,
    /// Stereo pair: 0 = channels 1/2, 1 = channels 3/4, ...
    pub pair: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// `None` = the system default output.
    pub master: Option<Route>,
    pub cue: Option<Route>,
    /// Delayed copy of the master for streaming (e.g. a virtual cable into OBS).
    pub broadcast: Option<Route>,
    pub broadcast_delay_ms: u32,
    /// The broadcast delay while this booth is in a room as a broadcaster.
    /// Longer than a DJ's: the joining DJ's moves take two hops to get here,
    /// and the audience can't tell a stream that's a second late.
    pub broadcaster_delay_ms: u32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master: None,
            cue: None,
            broadcast: None,
            broadcast_delay_ms: 500,
            broadcaster_delay_ms: 1000,
        }
    }
}

/// The broadcast delays you can pick, in ms.
pub const BROADCAST_DELAY_MS: std::ops::RangeInclusive<u32> = 200..=2000;

/// Moves that reach a broadcaster within this much of its delay are too
/// close: the sound card's buffer and the next spike eat the rest.
pub const LATE_MARGIN_US: i64 = 100_000;

/// A broadcast delay that leaves room for moves arriving `late_us` after
/// they happen: a quarter second spare, rounded up to a quarter second and
/// no more than the longest delay.
pub fn broadcast_delay_for(late_us: i64) -> u32 {
    let want_ms = (late_us.max(0) + 250_000) as f64 / 1000.0;
    let ms = ((want_ms / 250.0).ceil() * 250.0) as u32;
    ms.clamp(*BROADCAST_DELAY_MS.start(), *BROADCAST_DELAY_MS.end())
}

/// A column of the library's track list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortColumn {
    Title,
    Artist,
    Bpm,
    Key,
    Time,
}

/// How the library's track list is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibrarySort {
    pub column: SortColumn,
    pub descending: bool,
}

/// How often the screen redraws. The decks and waveforms animate at this
/// rate; mouse and keyboard input still redraw straight away, and controller
/// moves never wait for the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameRate {
    /// At most this many frames a second.
    Cap(u32),
    /// Every time the monitor refreshes (144 times a second on a 144 Hz monitor).
    Monitor,
}

impl FrameRate {
    pub const CHOICES: [FrameRate; 6] = [
        FrameRate::Cap(30),
        FrameRate::Cap(60),
        FrameRate::Cap(75),
        FrameRate::Cap(120),
        FrameRate::Cap(144),
        FrameRate::Monitor,
    ];

    pub fn label(self) -> String {
        match self {
            FrameRate::Cap(fps) => format!("{fps} fps"),
            FrameRate::Monitor => "Match monitor".to_string(),
        }
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        FrameRate::Cap(60)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: String,
    /// Version of the terms this user accepted (0 = not yet).
    pub terms_accepted: u32,
    pub library_folders: Vec<PathBuf>,
    pub audio: AudioSettings,
    pub midi_device: Option<String>,
    pub host_port: u16,
    pub last_join: String,
    /// "Broadcaster only" was ticked the last time this booth joined.
    pub join_as_broadcaster: bool,
    pub max_upload_mb_s: f32,
    /// Tempo fader range, e.g. 0.08 = ±8%.
    pub tempo_range: f64,
    pub local: LocalParams,
    /// None: each list in its own order (folder, crate or share order).
    pub library_sort: Option<LibrarySort>,
    pub frame_rate: FrameRate,
    /// The crossfader curve this booth last picked. It starts every session
    /// on it; joining a room takes the host's curve instead.
    pub crossfader_curve: XfCurve,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: String::new(),
            terms_accepted: 0,
            library_folders: Vec::new(),
            audio: AudioSettings::default(),
            midi_device: None,
            host_port: DEFAULT_PORT,
            last_join: String::new(),
            join_as_broadcaster: false,
            max_upload_mb_s: 2.0,
            tempo_range: 0.08,
            local: LocalParams::default(),
            library_sort: None,
            frame_rate: FrameRate::default(),
            crossfader_curve: XfCurve::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        match serde_json::to_vec_pretty(self) {
            Ok(b) => {
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, b)
                    .and_then(|_| std::fs::rename(&tmp, path))
                    .is_err()
                {
                    log::warn!("couldn't save settings to {}", path.display());
                }
            }
            Err(e) => log::warn!("couldn't serialise settings: {e}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Temporary copies of partner tracks for the current session.
    pub cache_dir: PathBuf,
    /// Where accepted shares are saved.
    pub shared_dir: PathBuf,
    pub mappings_dir: PathBuf,
    pub music_dir: Option<PathBuf>,
    pub settings_file: PathBuf,
    pub library_cache: PathBuf,
    pub consent_log: PathBuf,
    pub log_file: PathBuf,
}

impl Paths {
    /// Standard per-user locations (%APPDATA%\Tandem and %LOCALAPPDATA%\Tandem on Windows).
    pub fn detect() -> anyhow::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "Tandem")
            .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        let music_dir =
            directories::UserDirs::new().and_then(|u| u.audio_dir().map(Path::to_path_buf));
        let data_dir = dirs.data_local_dir().to_path_buf();
        let shared_dir = music_dir
            .as_ref()
            .map(|m| m.join("Tandem Shared"))
            .unwrap_or_else(|| data_dir.join("Shared"));
        Ok(Self::build(
            dirs.config_dir().to_path_buf(),
            data_dir,
            shared_dir,
            music_dir,
        ))
    }

    /// Everything under one folder (tests and the self-test).
    pub fn under(root: &Path) -> Self {
        Self::build(
            root.join("config"),
            root.join("data"),
            root.join("Tandem Shared"),
            None,
        )
    }

    fn build(
        config_dir: PathBuf,
        data_dir: PathBuf,
        shared_dir: PathBuf,
        music_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            settings_file: config_dir.join("settings.json"),
            mappings_dir: config_dir.join("mappings"),
            cache_dir: data_dir.join("session-cache"),
            library_cache: data_dir.join("library-cache.json"),
            consent_log: data_dir.join("consent-log.jsonl"),
            log_file: data_dir.join("tandem.log"),
            config_dir,
            data_dir,
            shared_dir,
            music_dir,
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.mappings_dir,
            &self.shared_dir,
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    /// Session copies of partner tracks never outlive a session.
    pub fn clear_session_cache(&self) {
        if let Ok(rd) = std::fs::read_dir(&self.cache_dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Make a string safe to use as a file or folder name.
pub fn sanitize_file_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() {
        "untitled".into()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// `path`, or `path (2)`, `path (3)`... if it already exists.
pub fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 2..1000 {
        let p = path.with_file_name(format!("{stem} ({i}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_for_late_moves() {
        assert_eq!(broadcast_delay_for(0), 250);
        assert_eq!(broadcast_delay_for(450_000), 750);
        assert_eq!(broadcast_delay_for(500_000), 750);
        assert_eq!(broadcast_delay_for(620_000), 1000);
        assert_eq!(broadcast_delay_for(1_900_000), 2000);
        assert_eq!(broadcast_delay_for(5_000_000), 2000);
    }

    #[test]
    fn old_settings_get_the_broadcaster_delay() {
        let old = r#"{"audio": {"master": null, "cue": null, "broadcast": null, "broadcast_delay_ms": 700}}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.audio.broadcast_delay_ms, 700);
        assert_eq!(s.audio.broadcaster_delay_ms, 1000);
    }

    #[test]
    fn frame_rate_defaults_to_60_and_round_trips() {
        // A settings file from before the option existed.
        let old: Settings = serde_json::from_str(r#"{"name":"Alice"}"#).unwrap();
        assert_eq!(old.frame_rate, FrameRate::Cap(60));
        for f in FrameRate::CHOICES {
            let s = Settings {
                frame_rate: f,
                ..Settings::default()
            };
            let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(back.frame_rate, f);
        }
    }
}
