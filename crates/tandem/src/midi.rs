//! MIDI controller input (WinMM on Windows, via midir).

use anyhow::anyhow;
use midir::{Ignore, MidiInput, MidiInputConnection};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tandem_core::mapping::{mapping_key, Mapping, MidiEngine, MidiOut};
use tandem_core::session::SessionHandle;

pub fn ports() -> Vec<String> {
    match MidiInput::new("tandem-list") {
        Ok(m) => m
            .ports()
            .iter()
            .filter_map(|p| m.port_name(p).ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

pub struct Midi {
    conn: Option<MidiInputConnection<()>>,
    pub engine: Arc<Mutex<MidiEngine>>,
    pub port: Option<String>,
    /// Things the UI handles (browse, load, learned bindings).
    pub ui_queue: Arc<Mutex<Vec<MidiOut>>>,
    pub error: Option<String>,
}

impl Midi {
    pub fn new(tempo_range: f64) -> Self {
        Self {
            conn: None,
            engine: Arc::new(Mutex::new(MidiEngine::new(Mapping::default(), tempo_range))),
            port: None,
            ui_queue: Arc::new(Mutex::new(Vec::new())),
            error: None,
        }
    }

    pub fn mapping_path(dir: &Path, port: &str) -> PathBuf {
        dir.join(format!("{}.json", mapping_key(port)))
    }

    pub fn disconnect(&mut self) {
        if let Some(c) = self.conn.take() {
            c.close();
        }
        self.port = None;
    }

    pub fn connect(
        &mut self,
        port_name: &str,
        session: SessionHandle,
        mappings_dir: &Path,
    ) -> anyhow::Result<()> {
        self.disconnect();
        self.error = None;
        let mapping: Mapping = std::fs::read(Self::mapping_path(mappings_dir, port_name))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| Mapping {
                device: port_name.to_string(),
                bindings: Vec::new(),
                jog_sensitivity: 1.0,
            });
        {
            let mut e = self.engine.lock();
            e.mapping = mapping;
            e.learn = None;
        }
        let mut input = MidiInput::new("tandem")?;
        input.ignore(Ignore::All);
        let port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).map(|n| n == port_name).unwrap_or(false))
            .ok_or_else(|| anyhow!("MIDI device not found"))?;
        let engine = self.engine.clone();
        let queue = self.ui_queue.clone();
        let clock = session.lock().clock();
        let conn = input
            .connect(
                &port,
                "tandem-in",
                move |_stamp, msg, _| {
                    let t = clock.now();
                    let (room, local) = {
                        let s = session.lock();
                        (s.latest(), s.local)
                    };
                    let outs = engine.lock().handle(msg, &room, &local);
                    if outs.is_empty() {
                        return;
                    }
                    let mut s = session.lock();
                    for o in outs {
                        match o {
                            MidiOut::Action(a) => s.act_at(a, t),
                            MidiOut::TogglePfl(d) => s.toggle_pfl(d),
                            MidiOut::CueMix(v) => s.set_cue_mix(v),
                            other => queue.lock().push(other),
                        }
                    }
                },
                (),
            )
            .map_err(|e| anyhow!("couldn't open {port_name}: {e} (is another DJ app using it?)"))?;
        self.conn = Some(conn);
        self.port = Some(port_name.to_string());
        log::info!("MIDI: connected to {port_name}");
        Ok(())
    }

    pub fn save_mapping(&self, mappings_dir: &Path) {
        let Some(port) = &self.port else { return };
        let mapping = self.engine.lock().mapping.clone();
        if let Ok(b) = serde_json::to_vec_pretty(&mapping) {
            if let Err(e) = std::fs::write(Self::mapping_path(mappings_dir, port), b) {
                log::warn!("couldn't save mapping: {e}");
            }
        }
    }
}
