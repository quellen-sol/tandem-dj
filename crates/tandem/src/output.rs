//! Sound card output (WASAPI on Windows, via cpal).
//!
//! Each device gets one stream. A stream renders the lanes routed to it:
//! the live lane for master/headphones and the delayed lane for broadcast.
//! A broadcaster booth plays only the delayed lane: on the master output
//! (and the broadcast feed, if it has one), with the headphones off.

use anyhow::{anyhow, bail};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use parking_lot::Mutex;
use std::sync::Arc;
use tandem_core::config::{AudioSettings, Route};
use tandem_core::mapping::{mixes_own_phones, CONTROLLER_PHONES_PAIR};
use tandem_core::meter::LevelMeters;
use tandem_core::render::{AudioSnapshot, Lane, LaneKind, StreamClock};
use tandem_core::time::SharedClock;

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub name: String,
    pub max_channels: u16,
    pub sample_rate: u32,
}

pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let mut out = Vec::new();
    if let Ok(devs) = host.output_devices() {
        for dev in devs {
            let Ok(name) = dev.name() else { continue };
            let max_channels = max_channels(&dev);
            let sample_rate = dev
                .default_output_config()
                .map(|c| c.sample_rate().0)
                .unwrap_or(0);
            out.push(DeviceInfo {
                name,
                max_channels,
                sample_rate,
            });
        }
    }
    out
}

/// The most output channels `dev` offers.
fn max_channels(dev: &cpal::Device) -> u16 {
    dev.supported_output_configs()
        .ok()
        .and_then(|it| it.map(|c| c.channels()).max())
        .unwrap_or(2)
}

pub fn default_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LaneSel {
    Live,
    Broadcast,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bus {
    Master,
    Cue,
}

#[derive(Clone, Copy, Debug)]
struct RouteOut {
    lane: LaneSel,
    bus: Bus,
    /// First channel index of the stereo pair.
    ch: usize,
    /// The master output: its lane fills the level meters.
    metered: bool,
    /// Headphones on a controller that mixes its own: send only the cued
    /// channels and let its HEADPHONES MIX knob blend in the master.
    cue_only: bool,
}

pub struct AudioOutputs {
    _streams: Vec<cpal::Stream>,
    pub status: Vec<String>,
    pub errors: Arc<Mutex<Vec<String>>>,
    /// The sound card playing the master (the system default if none is
    /// picked), and the one playing the headphones.
    pub master_device: Option<String>,
    pub cue_device: Option<String>,
    /// Some output is playing the headphones.
    pub phones: bool,
    /// The headphones play through this controller's jack, and it blends
    /// cue and master itself (its knob, not Tandem's slider).
    pub phones_mixer: Option<&'static str>,
    /// No headphone output was picked, so they went to that jack.
    pub phones_auto: bool,
}

impl AudioOutputs {
    pub fn none() -> Self {
        Self {
            _streams: Vec::new(),
            status: Vec::new(),
            errors: Arc::new(Mutex::new(Vec::new())),
            master_device: None,
            cue_device: None,
            phones: false,
            phones_mixer: None,
            phones_auto: false,
        }
    }
}

/// Headphones when none are picked: the jack of the controller the master
/// plays through, if it's one that mixes its own.
fn auto_phones(master: &Route) -> Option<Route> {
    mixes_own_phones(&master.device)?;
    if master.pair == CONTROLLER_PHONES_PAIR {
        return None;
    }
    let dev = cpal::default_host()
        .output_devices()
        .ok()?
        .find(|d| d.name().map(|n| n == master.device).unwrap_or(false))?;
    (max_channels(&dev) >= (CONTROLLER_PHONES_PAIR + 1) * 2).then(|| Route {
        device: master.device.clone(),
        pair: CONTROLLER_PHONES_PAIR,
    })
}

/// Open every device the settings route to. The lane that plays the master
/// output fills `meters`: the live lane, or on a `broadcaster` booth the
/// broadcast lane, which is all it plays.
pub fn start(
    cfg: &AudioSettings,
    snap: Arc<ArcSwap<AudioSnapshot>>,
    clock: Arc<SharedClock>,
    meters: Arc<LevelMeters>,
    broadcaster: bool,
) -> AudioOutputs {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let mut by_dev: Vec<(String, Vec<RouteOut>)> = Vec::new();
    let mut add =
        |route: &Route, out: RouteOut| match by_dev.iter_mut().find(|(d, _)| *d == route.device) {
            Some((_, v)) => v.push(out),
            None => by_dev.push((route.device.clone(), vec![out])),
        };
    let master = cfg
        .master
        .clone()
        .or_else(|| default_device_name().map(|device| Route { device, pair: 0 }));
    if let Some(r) = &master {
        add(
            r,
            RouteOut {
                lane: if broadcaster {
                    LaneSel::Broadcast
                } else {
                    LaneSel::Live
                },
                bus: Bus::Master,
                ch: r.pair as usize * 2,
                metered: true,
                cue_only: false,
            },
        );
    }
    // A broadcaster's booth has no headphones.
    let auto_cue = match (&cfg.cue, &master, broadcaster) {
        (None, Some(m), false) => auto_phones(m),
        _ => None,
    };
    let cue = cfg
        .cue
        .clone()
        .filter(|_| !broadcaster)
        .or_else(|| auto_cue.clone());
    let phones_mixer = cue
        .as_ref()
        .filter(|r| r.pair == CONTROLLER_PHONES_PAIR)
        .and_then(|r| mixes_own_phones(&r.device));
    if let Some(r) = &cue {
        add(
            r,
            RouteOut {
                lane: LaneSel::Live,
                bus: Bus::Cue,
                ch: r.pair as usize * 2,
                metered: false,
                cue_only: phones_mixer.is_some(),
            },
        );
    }
    // A broadcaster's master already plays the feed: not twice on one pair.
    if let Some(r) = cfg
        .broadcast
        .as_ref()
        .filter(|r| !(broadcaster && master.as_ref() == Some(*r)))
    {
        add(
            r,
            RouteOut {
                lane: LaneSel::Broadcast,
                bus: Bus::Master,
                ch: r.pair as usize * 2,
                metered: false,
                cue_only: false,
            },
        );
    }

    let delay_ms = if broadcaster {
        cfg.broadcaster_delay_ms
    } else {
        cfg.broadcast_delay_ms
    };
    let mut streams = Vec::new();
    let mut status = Vec::new();
    let mut phones = false;
    for (dev, routes) in by_dev {
        let metered = routes.iter().any(|r| r.metered);
        let has_cue = routes.iter().any(|r| r.bus == Bus::Cue);
        match open_device(
            &dev,
            routes,
            delay_ms,
            snap.clone(),
            clock.clone(),
            metered.then(|| meters.clone()),
            errors.clone(),
        ) {
            Ok((stream, desc)) => {
                log::info!("Audio: {desc}");
                streams.push(stream);
                status.push(desc);
                phones |= has_cue;
            }
            Err(e) => {
                log::error!("Audio device \"{dev}\": {e:#}");
                status.push(format!("{dev}: {e:#}"));
            }
        }
    }
    if streams.is_empty() && status.is_empty() {
        status.push("No audio output device found.".into());
    }
    AudioOutputs {
        _streams: streams,
        status,
        errors,
        master_device: master.map(|r| r.device),
        cue_device: cue.map(|r| r.device),
        phones,
        phones_mixer: phones_mixer.filter(|_| phones),
        phones_auto: auto_cue.is_some() && phones,
    }
}

fn open_device(
    name: &str,
    routes: Vec<RouteOut>,
    delay_ms: u32,
    snap: Arc<ArcSwap<AudioSnapshot>>,
    clock: Arc<SharedClock>,
    meters: Option<Arc<LevelMeters>>,
    errors: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<(cpal::Stream, String)> {
    let host = cpal::default_host();
    let dev = host
        .output_devices()?
        .find(|d| d.name().map(|n| n == name).unwrap_or(false))
        .ok_or_else(|| anyhow!("device not found (unplugged?)"))?;
    let def = dev.default_output_config()?;
    let sr = def.sample_rate();
    let need = routes.iter().map(|r| r.ch + 2).max().unwrap_or(2) as u16;
    let usable = |f: cpal::SampleFormat| {
        matches!(
            f,
            cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::I32
        )
    };
    let chosen = if def.channels() >= need && usable(def.sample_format()) {
        def
    } else {
        dev.supported_output_configs()?
            .filter(|r| {
                r.channels() >= need
                    && usable(r.sample_format())
                    && r.min_sample_rate() <= sr
                    && r.max_sample_rate() >= sr
            })
            .min_by_key(|r| r.channels())
            .map(|r| r.with_sample_rate(sr))
            .ok_or_else(|| {
                anyhow!(
                    "needs {need} output channels at {} Hz; this device can't",
                    sr.0
                )
            })?
    };
    let config: cpal::StreamConfig = chosen.config();
    let desc = format!(
        "{name}: {} ch @ {} Hz ({})",
        config.channels,
        config.sample_rate.0,
        routes
            .iter()
            .map(|r| format!(
                "{} on ch {}/{}",
                match (r.lane, r.bus) {
                    (LaneSel::Live, Bus::Master) => "master",
                    (LaneSel::Live, Bus::Cue) if r.cue_only => "headphones (cue only)",
                    (LaneSel::Live, Bus::Cue) => "headphones",
                    (LaneSel::Broadcast, _) => "broadcast",
                },
                r.ch + 1,
                r.ch + 2
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let stream = match chosen.sample_format() {
        cpal::SampleFormat::F32 => {
            build::<f32>(&dev, &config, routes, delay_ms, snap, clock, meters, errors)?
        }
        cpal::SampleFormat::I16 => {
            build::<i16>(&dev, &config, routes, delay_ms, snap, clock, meters, errors)?
        }
        cpal::SampleFormat::I32 => {
            build::<i32>(&dev, &config, routes, delay_ms, snap, clock, meters, errors)?
        }
        other => bail!("unsupported sample format {other:?}"),
    };
    stream.play()?;
    Ok((stream, desc))
}

#[allow(clippy::too_many_arguments)]
fn build<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    config: &cpal::StreamConfig,
    routes: Vec<RouteOut>,
    delay_ms: u32,
    snap: Arc<ArcSwap<AudioSnapshot>>,
    clock: Arc<SharedClock>,
    meters: Option<Arc<LevelMeters>>,
    errors: Arc<Mutex<Vec<String>>>,
) -> anyhow::Result<cpal::Stream> {
    const MAXF: usize = 4096;
    let ch = config.channels as usize;
    let sr = config.sample_rate.0 as f64;
    let metered = routes.iter().find(|r| r.metered).map(|r| r.lane);
    let lane = |sel: LaneSel, lane: Lane| match (&meters, metered) {
        (Some(m), Some(l)) if l == sel => lane.with_meters(m.clone()),
        _ => lane,
    };
    let cue_only = routes.iter().any(|r| r.cue_only);
    let mut live = routes.iter().any(|r| r.lane == LaneSel::Live).then(|| {
        lane(
            LaneSel::Live,
            Lane::new(sr, 0, LaneKind::Live).cue_only(cue_only),
        )
    });
    let mut bcast = routes
        .iter()
        .any(|r| r.lane == LaneSel::Broadcast)
        .then(|| {
            lane(
                LaneSel::Broadcast,
                Lane::new(sr, delay_ms as i64 * 1000, LaneKind::Broadcast),
            )
        });
    let (mut lm, mut lc, mut bm, mut bc) = (
        vec![0f32; MAXF * 2],
        vec![0f32; MAXF * 2],
        vec![0f32; MAXF * 2],
        vec![0f32; MAXF * 2],
    );
    let mut mix = vec![0f32; MAXF * ch];
    let mut sclock = StreamClock::new(sr);
    let err_cb = move |e: cpal::StreamError| {
        log::error!("audio stream error: {e}");
        errors.lock().push(e.to_string());
    };
    let stream = dev.build_output_stream(
        config,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            let frames = data.len() / ch;
            let ts = info.timestamp();
            let latency = ts
                .playback
                .duration_since(&ts.callback)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0);
            let t0 = sclock.frame0(clock.now() + latency, frames);
            let s = snap.load();
            let mut done = 0;
            while done < frames {
                let n = (frames - done).min(MAXF);
                let t = t0 + (done as f64 * 1e6 / sr) as i64;
                if let Some(l) = live.as_mut() {
                    l.render(&s, t, &mut lm[..n * 2], &mut lc[..n * 2]);
                }
                if let Some(b) = bcast.as_mut() {
                    b.render(&s, t, &mut bm[..n * 2], &mut bc[..n * 2]);
                }
                let m = &mut mix[..n * ch];
                m.fill(0.0);
                for r in &routes {
                    let src = match (r.lane, r.bus) {
                        (LaneSel::Live, Bus::Master) => &lm,
                        (LaneSel::Live, Bus::Cue) => &lc,
                        (LaneSel::Broadcast, _) => &bm,
                    };
                    for i in 0..n {
                        m[i * ch + r.ch] += src[i * 2];
                        m[i * ch + r.ch + 1] += src[i * 2 + 1];
                    }
                }
                for (o, v) in data[done * ch..(done + n) * ch].iter_mut().zip(m.iter()) {
                    *o = T::from_sample(*v);
                }
                done += n;
            }
        },
        err_cb,
        None,
    )?;
    Ok(stream)
}
