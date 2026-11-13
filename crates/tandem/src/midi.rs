//! MIDI controller input and LED output (WinMM on Windows, via midir).

use anyhow::anyhow;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tandem_core::mapping::{builtin_preset, mapping_key, Mapping, MidiEngine, MidiOut};
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

/// Output port matching an input port's name (Windows names them alike).
fn open_output(port_name: &str) -> Option<MidiOutputConnection> {
    let out = MidiOutput::new("tandem-out").ok()?;
    let key = mapping_key(port_name);
    let port = out.ports().into_iter().find(|p| {
        out.port_name(p)
            .map(|n| n == port_name || mapping_key(&n) == key)
            .unwrap_or(false)
    })?;
    out.connect(&port, "tandem-leds").ok()
}

/// Pioneer controllers drop into a demo light show unless the DJ software
/// keeps talking to them. This is the message Mixxx's DDJ-FLX4 mapping sends
/// every 200 ms (found with Wireshark by the Mixxx developers).
const PIONEER_KEEPALIVE: [u8; 12] = [
    0xF0, 0x00, 0x40, 0x05, 0x00, 0x00, 0x04, 0x05, 0x00, 0x50, 0x02, 0xF7,
];

pub struct Midi {
    conn: Option<MidiInputConnection<()>>,
    feedback_stop: Option<Arc<AtomicBool>>,
    pub engine: Arc<Mutex<MidiEngine>>,
    pub port: Option<String>,
    pub has_output: bool,
    /// Things the UI handles (browse, load, learned bindings).
    pub ui_queue: Arc<Mutex<Vec<MidiOut>>>,
    pub error: Option<String>,
}

impl Midi {
    pub fn new(tempo_range: f64) -> Self {
        Self {
            conn: None,
            feedback_stop: None,
            engine: Arc::new(Mutex::new(MidiEngine::new(Mapping::default(), tempo_range))),
            port: None,
            has_output: false,
            ui_queue: Arc::new(Mutex::new(Vec::new())),
            error: None,
        }
    }

    pub fn mapping_path(dir: &Path, port: &str) -> PathBuf {
        dir.join(format!("{}.json", mapping_key(port)))
    }

    pub fn disconnect(&mut self) {
        if let Some(stop) = self.feedback_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(c) = self.conn.take() {
            c.close();
        }
        self.port = None;
        self.has_output = false;
    }

    pub fn connect(
        &mut self,
        port_name: &str,
        session: SessionHandle,
        mappings_dir: &Path,
    ) -> anyhow::Result<()> {
        self.disconnect();
        self.error = None;
        let saved: Option<Mapping> = std::fs::read(Self::mapping_path(mappings_dir, port_name))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let mapping = match saved {
            Some(m) => m,
            None => match builtin_preset(port_name) {
                Some(p) => {
                    log::info!(
                        "MIDI: using the built-in {} mapping",
                        p.preset.clone().unwrap_or_default()
                    );
                    p
                }
                None => Mapping::empty(port_name),
            },
        };
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
        let s2 = session.clone();
        let conn = input
            .connect(
                &port,
                "tandem-in",
                move |_stamp, msg, _| {
                    let t = clock.now();
                    let (room, local) = {
                        let s = s2.lock();
                        (s.latest(), s.local)
                    };
                    let outs = engine.lock().handle(msg, &room, &local);
                    if outs.is_empty() {
                        return;
                    }
                    let mut s = s2.lock();
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

        match open_output(port_name) {
            Some(out) => {
                self.has_output = true;
                let stop = Arc::new(AtomicBool::new(false));
                self.feedback_stop = Some(stop.clone());
                let pioneer = port_name.to_uppercase().contains("DDJ");
                let engine = self.engine.clone();
                std::thread::Builder::new()
                    .name("tandem-midi-out".into())
                    .spawn(move || feedback_loop(out, session, engine, stop, pioneer))?;
            }
            None => log::info!("MIDI: no output port for {port_name}; lights won't follow Tandem"),
        }
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

/// Keep the controller's lights in step with the room, and keep Pioneer
/// controllers out of demo mode.
fn feedback_loop(
    mut out: MidiOutputConnection,
    session: SessionHandle,
    engine: Arc<Mutex<MidiEngine>>,
    stop: Arc<AtomicBool>,
    pioneer: bool,
) {
    let mut sent: HashMap<(u8, u8), bool> = HashMap::new();
    let mut last_keepalive = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        if pioneer && last_keepalive.elapsed() >= Duration::from_millis(200) {
            let _ = out.send(&PIONEER_KEEPALIVE);
            last_keepalive = Instant::now();
        }
        let (room, local) = {
            let s = session.lock();
            (s.latest(), s.local)
        };
        let leds = engine.lock().leds(&room, &local);
        for (key, on) in leds {
            if sent.get(&key) != Some(&on) {
                if out
                    .send(&[key.0, key.1, if on { 0x7F } else { 0x00 }])
                    .is_err()
                {
                    return;
                }
                sent.insert(key, on);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    for (key, on) in sent {
        if on {
            let _ = out.send(&[key.0, key.1, 0x00]);
        }
    }
}

/// `tandem.exe --midi-monitor [seconds]`: print everything a controller sends.
pub fn monitor(seconds: u64) -> i32 {
    let names = ports();
    println!("MIDI inputs: {names:?}");
    let Some(name) = names
        .iter()
        .find(|n| n.to_uppercase().contains("DDJ"))
        .or(names.first())
        .cloned()
    else {
        println!("No MIDI input found.");
        return 1;
    };
    let mut input = match MidiInput::new("tandem-monitor") {
        Ok(i) => i,
        Err(e) => {
            println!("MIDI unavailable: {e}");
            return 1;
        }
    };
    input.ignore(Ignore::None);
    let Some(port) = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).map(|n| n == name).unwrap_or(false))
    else {
        println!("Port vanished");
        return 1;
    };
    let start = Instant::now();
    let conn = input.connect(
        &port,
        "tandem-monitor",
        move |_, msg, _| {
            let hex: Vec<String> = msg.iter().map(|b| format!("{b:02X}")).collect();
            let label = tandem_core::mapping::parse(msg)
                .map(|r| r.label())
                .unwrap_or_else(|| "sysex/other".into());
            println!(
                "{:8.3}  {:<24} {label}",
                start.elapsed().as_secs_f64(),
                hex.join(" ")
            );
        },
        (),
    );
    let _conn = match conn {
        Ok(c) => c,
        Err(e) => {
            println!("Couldn't open {name}: {e}");
            return 1;
        }
    };
    println!("Listening to {name} for {seconds} s…");
    // Keep a Pioneer awake (and ask it to report its knob positions) while listening.
    let mut out = open_output(&name);
    let end = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < end {
        if let Some(o) = out.as_mut() {
            let _ = o.send(&PIONEER_KEEPALIVE);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    0
}
