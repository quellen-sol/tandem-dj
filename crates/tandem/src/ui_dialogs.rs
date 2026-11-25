//! First-run terms, share consent popups, crate windows, host/join and
//! settings windows.

use crate::app::{CrateMode, SettingsTab, TandemApp};
use crate::output;
use eframe::egui::{self, Color32, RichText};
use std::net::IpAddr;
use tandem_core::config::{FrameRate, Route};
use tandem_core::mapping::{builtin_preset, Binding, Mapping, MidiEngine, RelMode, Target};
use tandem_core::session::{SessionView, ShareState};
use tandem_core::terms::{
    self, ConsentRecord, TERMS_SUMMARY, TERMS_TEXT, TERMS_TITLE, TERMS_VERSION,
};

const AMBER: Color32 = Color32::from_rgb(250, 190, 60);
const RED: Color32 = Color32::from_rgb(240, 90, 90);

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_048_576.0)
}

fn local_ips() -> Vec<(IpAddr, String)> {
    let mut v: Vec<(IpAddr, String)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
        .map(|i| {
            let ip = i.ip();
            let note = match ip {
                IpAddr::V4(v4) if v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]) => {
                    "Tailscale".to_string()
                }
                IpAddr::V4(v4) if v4.is_private() => "local network".to_string(),
                _ => i.name.clone(),
            };
            (ip, note)
        })
        .collect();
    v.sort_by_key(|(_, n)| if n == "Tailscale" { 0 } else { 1 });
    v.dedup();
    v
}

impl TandemApp {
    /// Shown until the terms are accepted and a name is set.
    pub(crate) fn terms_gate(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label(RichText::new("TANDEM").size(34.0).strong().color(crate::ui_decks::DECK_COLORS[0]));
                ui.label(RichText::new("Back-to-back DJing with a friend, from anywhere.").size(16.0));
                ui.add_space(14.0);
                if self.settings.terms_accepted > 0 && self.settings.terms_accepted < TERMS_VERSION {
                    ui.label(RichText::new("The terms have changed since you last agreed to them.").color(AMBER));
                }
            });
            let width = ui.available_width().min(720.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(width);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(TERMS_TITLE).strong().size(17.0));
                    ui.label(RichText::new(TERMS_SUMMARY).italics());
                    ui.add_space(6.0);
                    // Leave room below for the name, the checkbox and the button.
                    let h = (ui.available_height() - 130.0).clamp(120.0, 300.0);
                    egui::ScrollArea::vertical().max_height(h).show(ui, |ui| {
                        ui.label(TERMS_TEXT);
                    });
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label("Your DJ name:");
                    ui.add(egui::TextEdit::singleline(&mut self.ui.name_draft).hint_text("shown to the other DJs").desired_width(220.0));
                });
                ui.checkbox(&mut self.ui.terms_checked, "I've read these terms. I'll use Tandem for personal, non-commercial purposes only.");
                ui.add_space(8.0);
                let ok = self.ui.terms_checked && !self.ui.name_draft.trim().is_empty();
                if ui.add_enabled(ok, egui::Button::new(RichText::new("Agree and continue").size(16.0))).clicked() {
                    let name: String = self.ui.name_draft.trim().chars().take(32).collect();
                    terms::append_consent(
                        &self.paths.consent_log,
                        &ConsentRecord {
                            unix_time: terms::now_unix(),
                            kind: "terms",
                            terms_version: TERMS_VERSION,
                            me: &name,
                            other: None,
                            share_title: None,
                            statement: Some("I've read these terms. I'll use Tandem for personal, non-commercial purposes only."),
                            tracks: Vec::new(),
                        },
                    );
                    self.settings.terms_accepted = TERMS_VERSION;
                    self.settings.name = name.clone();
                    self.session.lock().set_name(name);
                    self.settings.save(&self.paths.settings_file);
                }
            });
        });
    }

    pub(crate) fn windows(&mut self, ctx: &egui::Context, view: &SessionView) {
        self.incoming_share_modal(ctx, view);
        self.outgoing_share_modal(ctx, view);
        self.crate_modal(ctx);
        self.host_window(ctx);
        self.join_window(ctx);
        self.settings_window(ctx);
        self.wizard_window(ctx);
        self.deck_check_window(ctx);
        self.jog_check_window(ctx);
        self.terms_window(ctx);
    }

    fn incoming_share_modal(&mut self, ctx: &egui::Context, view: &SessionView) {
        let Some(s) = view
            .incoming
            .iter()
            .find(|s| s.state == ShareState::Pending)
        else {
            return;
        };
        let mut decision = None;
        egui::Modal::new(egui::Id::new(("incoming-share", s.share_id))).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(RichText::new(format!("{} wants to share music with you", s.from)).size(18.0).strong());
            ui.label(format!("\"{}\" · {} tracks · {}", s.title, s.tracks.len(), mb(s.bytes)));
            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                for t in &s.tracks {
                    ui.label(RichText::new(t).small());
                }
            });
            ui.separator();
            ui.label(RichText::new(format!("{} is sharing their music with you for NON-COMMERCIAL purposes only.", s.from)).strong().color(AMBER));
            ui.label("These tracks are for your personal use. Don't sell, upload, re-share or redistribute them. Copyright law still applies.");
            if ui.link("Read the full terms").clicked() {
                self.ui.show_terms = true;
            }
            ui.add_space(4.0);
            ui.checkbox(&mut self.ui.incoming_checked, terms::receiver_pledge(&s.from));
            ui.label(RichText::new(format!("Tracks will be saved to {}", s.dir.display())).small().weak());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Decline").clicked() {
                    decision = Some(false);
                }
                if ui.add_enabled(self.ui.incoming_checked, egui::Button::new(RichText::new("Accept and download").strong())).clicked() {
                    decision = Some(true);
                }
            });
        });
        if let Some(accept) = decision {
            self.ui.incoming_checked = false;
            self.session.lock().respond_share(s.share_id, accept);
            if accept {
                self.toast(format!("Downloading \"{}\" to Shared with me", s.title));
            }
        }
    }

    fn outgoing_share_modal(&mut self, ctx: &egui::Context, view: &SessionView) {
        let Some(draft) = &mut self.ui.share_draft else {
            return;
        };
        let mut done = None;
        egui::Modal::new(egui::Id::new("outgoing-share")).show(ctx, |ui| {
            ui.set_width(500.0);
            ui.label(RichText::new("Share with the room").size(18.0).strong());
            let size = if draft.bytes > 0 {
                format!(" · {}", mb(draft.bytes))
            } else {
                String::new()
            };
            ui.label(format!("{} tracks{size}", draft.paths.len()));
            ui.add_space(4.0);
            let live = ui.add_enabled_ui(draft.live_no.is_none(), |ui| {
                ui.radio_value(&mut draft.live, true, RichText::new("📡 Share it live").strong());
                ui.indent("live", |ui| {
                    ui.label(RichText::new("The other DJs see this list in their library and can load any track from it. A track only comes over when someone loads it, as a copy for the session that goes when Tandem closes. Changes you make to the list reach them, until you stop sharing it or leave the room."));
                });
            });
            if let Some(why) = &draft.live_no {
                live.response.on_disabled_hover_text(why);
                ui.label(RichText::new(why).small().weak());
            }
            ui.add_space(2.0);
            let copies = ui.add_enabled_ui(draft.copies_no.is_none(), |ui| {
                ui.radio_value(&mut draft.live, false, RichText::new("⬇ Send copies to keep").strong());
                ui.indent("copies", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut draft.title);
                    });
                    ui.label(RichText::new("Every other DJ in the room sees a request and can accept or decline. Tracks they already have aren't sent again; the rest go to their Shared with me."));
                });
            });
            if let Some(why) = &draft.copies_no {
                copies.response.on_disabled_hover_text(why);
            }
            if draft.live && draft.live_no.is_some() {
                draft.live = false;
            }
            ui.add_space(6.0);
            ui.checkbox(&mut draft.pledge, terms::sender_pledge(draft.paths.len()));
            ui.add_space(6.0);
            let ok = draft.pledge
                && match draft.live {
                    true => draft.live_no.is_none(),
                    false => draft.copies_no.is_none() && !draft.title.trim().is_empty() && !draft.paths.is_empty(),
                };
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
                let label = if draft.live { "Share live" } else { "Send copies" };
                if ui.add_enabled(ok, egui::Button::new(RichText::new(label).strong())).clicked() {
                    done = Some(true);
                }
            });
        });
        if let Some(go) = done {
            let draft = self.ui.share_draft.take().unwrap();
            match (go, draft.live) {
                (false, _) => {}
                (true, true) => {
                    let src = self.ui.source.clone();
                    if src.key() == draft.key {
                        self.share_live(&src, view);
                        self.toast("Shared live: the other DJs can load from it now.");
                    }
                }
                (true, false) => self
                    .session
                    .lock()
                    .offer_share(draft.title.trim().to_string(), draft.paths),
            }
        }
    }

    /// Making, renaming or deleting one of your crates.
    fn crate_modal(&mut self, ctx: &egui::Context) {
        let Some(d) = &mut self.ui.crate_dialog else {
            return;
        };
        let mut done = None;
        egui::Modal::new(egui::Id::new("crate-dialog")).show(ctx, |ui| {
            ui.set_width(380.0);
            match d.mode {
                CrateMode::Delete(_) => {
                    ui.label(RichText::new(format!("Delete \"{}\"?", d.name)).size(18.0).strong());
                    ui.label("Its tracks stay on your computer and in your other lists. If it's shared live, it's gone from the other DJs' libraries too.");
                }
                CrateMode::New | CrateMode::Rename(_) => {
                    let title = match d.mode {
                        CrateMode::New => "New crate",
                        _ => "Rename crate",
                    };
                    ui.label(RichText::new(title).size(18.0).strong());
                    if !d.add.is_empty() {
                        let n = d.add.len();
                        ui.label(if n == 1 { "With the track you picked in it.".to_string() } else { format!("With the {n} tracks you picked in it.") });
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut d.name).hint_text("Name, e.g. Friday warm-up").desired_width(340.0));
                    if std::mem::take(&mut d.focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !d.name.trim().is_empty() {
                        done = Some(true);
                    }
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
                let (label, ok) = match d.mode {
                    CrateMode::Delete(_) => ("Delete", true),
                    CrateMode::New => ("Make crate", !d.name.trim().is_empty()),
                    CrateMode::Rename(_) => ("Rename", !d.name.trim().is_empty()),
                };
                if ui.add_enabled(ok, egui::Button::new(RichText::new(label).strong())).clicked() {
                    done = Some(true);
                }
            });
        });
        let Some(go) = done else { return };
        let d = self.ui.crate_dialog.take().expect("open");
        if !go {
            return;
        }
        match d.mode {
            CrateMode::New => self.make_crate(&d.name, d.add),
            CrateMode::Rename(id) => tandem_core::library::rename_crate(&self.library, id, &d.name),
            CrateMode::Delete(id) => self.delete_crate(id),
        }
    }

    fn host_window(&mut self, ctx: &egui::Context) {
        if !self.ui.host_open {
            return;
        }
        let mut open = true;
        let mut start = false;
        egui::Window::new("Host a room").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label("Up to three more DJs join with your address and this room code.");
            egui::Grid::new("host-grid").num_columns(2).show(ui, |ui| {
                ui.label("Port");
                if ui.add(egui::DragValue::new(&mut self.settings.host_port).range(1024..=65535)).changed() {
                    self.mark_dirty();
                }
                ui.end_row();
                ui.label("Room code");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.ui.room_code).desired_width(90.0));
                    if ui.small_button("New").clicked() {
                        self.ui.room_code = tandem_core::net::random_room_code();
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Give the other DJs one of these addresses:").strong());
            for (ip, note) in local_ips() {
                ui.horizontal(|ui| {
                    let addr = format!("{ip}:{}", self.settings.host_port);
                    ui.monospace(&addr);
                    ui.label(RichText::new(note).weak());
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(addr);
                    }
                });
            }
            ui.add_space(6.0);
            ui.label(RichText::new(
                "Not on the same network? Easiest: both install Tailscale (free) and use the 100.x address. \
                 Otherwise forward TCP port above on your router to this PC. \
                 Click \"Allow\" if Windows Firewall asks.",
            )
            .small()
            .weak());
            ui.add_space(6.0);
            start = ui.button(RichText::new("Start hosting").strong()).clicked();
        });
        if start {
            let code = self.ui.room_code.trim().to_uppercase();
            match tandem_core::net::host(&self.session, self.settings.host_port, &code) {
                Ok(_) => {
                    self.ui.host_open = false;
                    self.toast(format!("Hosting room {code}. Waiting for the other DJs."));
                }
                Err(e) => self.toast(format!("Couldn't host: {e:#}")),
            }
            return;
        }
        self.ui.host_open = open;
    }

    fn join_window(&mut self, ctx: &egui::Context) {
        if !self.ui.join_open {
            return;
        }
        let finished = self.ui.join_result.lock().take();
        if let Some(res) = finished {
            self.ui.joining = false;
            match res {
                Ok(()) => {
                    self.ui.join_open = false;
                    self.mark_dirty();
                    return;
                }
                Err(e) => self.toast(format!("Couldn't join: {e}")),
            }
        }
        let mut open = true;
        let mut go = false;
        egui::Window::new("Join a room")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("join-grid").num_columns(2).show(ui, |ui| {
                    ui.label("Host address");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.last_join)
                            .hint_text("e.g. 100.101.102.103")
                            .desired_width(220.0),
                    );
                    ui.end_row();
                    ui.label("Room code");
                    ui.add(egui::TextEdit::singleline(&mut self.ui.join_code).desired_width(90.0));
                    ui.end_row();
                });
                ui.add_space(4.0);
                if ui
                    .checkbox(&mut self.settings.join_as_broadcaster, "Broadcaster only")
                    .on_hover_text("For a PC that streams the set: it can't change anything in the room.")
                    .changed()
                {
                    self.mark_dirty();
                }
                if self.settings.join_as_broadcaster {
                    ui.label(
                        RichText::new(
                            "This PC joins just to stream the set. It plays only the broadcast feed, \
                             with no decks, mixer or library, and nothing you do here changes the room. \
                             Share the broadcast window with its sound on: no virtual cable needed.",
                        )
                        .small()
                        .weak(),
                    );
                }
                ui.add_space(6.0);
                if self.ui.joining {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Connecting and syncing clocks…");
                    });
                } else {
                    go = ui.button(RichText::new("Join").strong()).clicked();
                }
            });
        if go {
            self.ui.joining = true;
            let (h, addr, code, out, broadcaster) = (
                self.session.clone(),
                self.settings.last_join.clone(),
                self.ui.join_code.clone(),
                self.ui.join_result.clone(),
                self.settings.join_as_broadcaster,
            );
            std::thread::spawn(move || {
                let res = tandem_core::net::join(&h, &addr, &code, broadcaster)
                    .map_err(|e| format!("{e:#}"));
                *out.lock() = Some(res);
            });
        }
        self.ui.join_open = open;
    }

    fn terms_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_terms {
            return;
        }
        let mut open = true;
        egui::Window::new(TERMS_TITLE)
            .open(&mut open)
            .default_width(560.0)
            .show(ctx, |ui| {
                ui.label(RichText::new(TERMS_SUMMARY).italics());
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 220.0).clamp(150.0, 420.0))
                    .show(ui, |ui| ui.label(TERMS_TEXT));
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "You agreed to version {} of these terms.",
                        self.settings.terms_accepted
                    ))
                    .small()
                    .weak(),
                );
                if ui.small_button("Open consent log folder").clicked() {
                    let _ = std::process::Command::new("explorer")
                        .arg(&self.paths.data_dir)
                        .spawn();
                }
            });
        self.ui.show_terms = open;
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.ui.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(640.0)
            .default_height(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.ui.settings_tab, SettingsTab::Audio, "Audio");
                    ui.selectable_value(
                        &mut self.ui.settings_tab,
                        SettingsTab::Controller,
                        "Controller",
                    );
                    ui.selectable_value(&mut self.ui.settings_tab, SettingsTab::General, "General");
                });
                ui.separator();
                // On a short window the tab scrolls rather than run off
                // the bottom of the screen.
                let tab = self.ui.settings_tab;
                egui::ScrollArea::vertical()
                    .id_salt(("settings", tab as u8))
                    .max_height((ctx.screen_rect().height() - 120.0).max(200.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| match tab {
                        SettingsTab::Audio => self.audio_settings(ui),
                        SettingsTab::Controller => self.controller_settings(ui),
                        SettingsTab::General => self.general_settings(ui),
                    });
            });
        self.ui.show_settings = open;
    }

    fn audio_settings(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.audio.clone();
        if self.audio_broadcaster {
            ui.label(
                RichText::new(
                    "Broadcasting: Speakers (master) plays the broadcast feed, the headphones are off, \
                     and the broadcast window is in step with what you hear. \
                     The broadcast delay below is this PC's delay as a broadcaster (1000 ms to start with); \
                     your delay as a DJ stays as it was.",
                )
                .color(AMBER),
            );
            ui.add_space(4.0);
        }
        if ui.button("Refresh device list").clicked() {
            self.devices = output::list_devices();
        }
        let devices = self.devices.clone();
        route_editor(
            ui,
            "Speakers (master)",
            "System default",
            &mut self.settings.audio.master,
            &devices,
        );
        let cue_off = match (self.audio.phones_auto, self.audio.phones_mixer) {
            (true, Some(c)) => format!("Auto ({c} headphones)"),
            _ => "Off".to_string(),
        };
        route_editor(
            ui,
            "Headphones (cue)",
            &cue_off,
            &mut self.settings.audio.cue,
            &devices,
        );
        ui.label(RichText::new("Most DJ controllers with a built-in sound card show up with 4 channels: master on 1/2, headphones on 3/4. With the master on a Pioneer DDJ-FLX6, FLX4 or DDJ-400, the headphones go to its jack automatically, and its own HEADPHONES MIX knob blends cue and master.").small().weak());
        ui.add_space(6.0);
        route_editor(
            ui,
            "Broadcast feed",
            "Off",
            &mut self.settings.audio.broadcast,
            &devices,
        );
        ui.label(
            RichText::new(
                "For streaming: pick a virtual cable (e.g. \"CABLE Input\" from VB-Audio) and capture its other end in OBS. \
                 This feed is delayed so both DJs' moves land exactly on time in it. \
                 For the picture, share or capture the Broadcast view window (top bar): it runs the same delay behind, so it matches this feed.",
            )
            .small()
            .weak(),
        );
        ui.horizontal(|ui| {
            ui.label("Broadcast delay");
            ui.add(
                egui::Slider::new(
                    if self.audio_broadcaster {
                        &mut self.settings.audio.broadcaster_delay_ms
                    } else {
                        &mut self.settings.audio.broadcast_delay_ms
                    },
                    tandem_core::config::BROADCAST_DELAY_MS,
                )
                .suffix(" ms"),
            );
        });
        if self.settings.audio != before {
            self.restart_audio();
        }
        ui.separator();
        ui.label(RichText::new("Status").strong());
        for s in self.audio.status.clone() {
            ui.label(RichText::new(s).small());
        }
        if ui.button("Restart audio").clicked() {
            self.restart_audio();
        }
    }

    fn controller_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("MIDI controller");
            let current = self.midi.port.clone().unwrap_or_else(|| "None".into());
            let mut choice: Option<Option<String>> = None;
            egui::ComboBox::from_id_salt("midi-port")
                .selected_text(&current)
                .width(260.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.midi.port.is_none(), "None")
                        .clicked()
                    {
                        choice = Some(None);
                    }
                    for p in &self.midi_ports {
                        if ui
                            .selectable_label(self.midi.port.as_deref() == Some(p), p)
                            .clicked()
                        {
                            choice = Some(Some(p.clone()));
                        }
                    }
                });
            if ui.button("Refresh").clicked() {
                self.midi_ports = crate::midi::ports();
            }
            if let Some(c) = choice {
                match c {
                    None => self.midi.disconnect(),
                    Some(p) => {
                        if let Err(e) =
                            self.midi
                                .connect(&p, self.session.clone(), &self.paths.mappings_dir)
                        {
                            self.midi.error = Some(format!("{e:#}"));
                        }
                    }
                }
                self.settings.midi_device = self.midi.port.clone();
                self.mark_dirty();
            }
        });
        if let Some(e) = &self.midi.error {
            ui.label(RichText::new(e).color(RED));
        }
        if self.midi.port.is_none() {
            ui.label(RichText::new("Plug in your controller, close other DJ software (Serato, rekordbox) so the port is free, then pick it above.").weak());
            return;
        }
        let mut changed = false;
        let preset = self.midi.port.as_deref().and_then(builtin_preset);
        let room = self.session.lock().latest();
        let engine_arc = self.midi.engine.clone();
        let mut engine = engine_arc.lock();
        let mut set_up = false;
        ui.horizontal(|ui| {
            set_up = ui
                .button(RichText::new("Set up step by step…").strong())
                .on_hover_text("Tandem asks for one control at a time, and you press or move it on the controller.")
                .clicked();
            let mapped = engine.mapping.bindings.len();
            ui.label(
                RichText::new(if mapped == 0 {
                    "Nothing is mapped yet: this goes through your controller one control at a time.".to_string()
                } else {
                    format!("{mapped} controls mapped. This goes through the rest, or all of them again, one at a time.")
                })
                .small()
                .weak(),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Last message:");
            ui.monospace(
                engine
                    .last_message
                    .clone()
                    .unwrap_or_else(|| "(move something)".into()),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Tempo fader range");
            let mut range = self.settings.tempo_range;
            egui::ComboBox::from_id_salt("tempo-range")
                .selected_text(format!("±{:.0}%", range * 100.0))
                .show_ui(ui, |ui| {
                    for r in [0.06, 0.08, 0.10, 0.16, 0.25, 0.50] {
                        ui.selectable_value(&mut range, r, format!("±{:.0}%", r * 100.0));
                    }
                });
            if range != self.settings.tempo_range {
                self.settings.tempo_range = range;
                engine.tempo_range = range;
                self.mark_dirty();
            }
        });
        ui.horizontal(|ui| {
            changed |= ui
                .checkbox(&mut engine.mapping.vinyl, "Vinyl mode")
                .on_hover_text("Touching the top of the jog wheel holds the record, so you can scratch. Off: the jog only nudges.")
                .changed();
            ui.label("Scratch speed");
            changed |= ui
                .add(egui::Slider::new(&mut engine.mapping.scratch_sensitivity, 0.25..=4.0).logarithmic(true))
                .changed();
            ui.label("Ring pitch-bend");
            changed |= ui
                .add(egui::Slider::new(&mut engine.mapping.jog_sensitivity, 0.1..=5.0).logarithmic(true))
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            match &engine.mapping.preset {
                Some(p) => ui.label(
                    RichText::new(format!("Started from the built-in {p} mapping.")).small(),
                ),
                None => ui.label(RichText::new("Custom mapping.").small()),
            };
            if let Some(preset) = &preset {
                // It can't be undone, so it asks first.
                let asked = egui::Id::new("reset-whole-mapping");
                let mut asking = ui.data(|d| d.get_temp(asked).unwrap_or(false));
                if !asking {
                    asking = ui
                        .small_button("Reset the whole mapping…")
                        .on_hover_text("Every control, the jog calibration and the jog settings, back to the built-in mapping. To put back one control, use Reset beside it in the list.")
                        .clicked();
                } else {
                    ui.label(
                        RichText::new("Every control, the jog calibration and the jog settings go back, and there's no undo:")
                            .small()
                            .color(AMBER),
                    );
                    if ui.small_button("Reset everything").clicked() {
                        engine.edit(|m| *m = preset.clone());
                        changed = true;
                        asking = false;
                    }
                    if ui.small_button("Cancel").clicked() {
                        asking = false;
                    }
                }
                ui.data_mut(|d| d.insert_temp(asked, asking));
            }
            ui.label(
                RichText::new(if self.midi.has_output {
                    "· lights: on"
                } else {
                    "· lights: no output port"
                })
                .small()
                .weak(),
            );
        });
        ui.horizontal(|ui| {
            ui.label(format!("Jog: {:.0} ticks per turn", engine.mapping.ticks_per_rev()));
            match engine.calibration {
                None => {
                    if ui
                        .small_button("Calibrate…")
                        .on_hover_text("Measure your jog wheel so one turn of the platter moves the record exactly one turn.")
                        .clicked()
                    {
                        engine.calibration = Some(0);
                    }
                }
                Some(count) => {
                    ui.label(
                        RichText::new(format!(
                            "Line up a mark on the platter, then turn it exactly one full turn (either way). Counted: {}",
                            count.abs()
                        ))
                        .color(AMBER),
                    );
                    if ui.add_enabled(count.abs() >= 100, egui::Button::new("Use this")).clicked() {
                        engine.mapping.jog_ticks_per_rev = Some(count.unsigned_abs() as f32);
                        engine.calibration = None;
                        changed = true;
                    }
                    if ui.small_button("Cancel").clicked() {
                        engine.calibration = None;
                    }
                }
            }
        });
        let mut open_check = false;
        let port = self.midi.port.clone().unwrap_or_default();
        if TandemApp::deck_check_fits(&engine.mapping, &port) {
            ui.horizontal(|ui| {
                open_check = ui
                    .button("Check the DECK buttons…")
                    .on_hover_text("Press PLAY, LOAD and DECK when it asks: it finds out what your controller sends, and saves a report to send to us.")
                    .clicked();
                ui.label(
                    RichText::new("If DECK doesn't switch decks.")
                        .small()
                        .weak(),
                );
            });
        }
        let mut open_jog_check = false;
        ui.horizontal(|ui| {
            open_jog_check = ui
                .button("Check the jog wheels…")
                .on_hover_text("Touch and turn them as it asks: it finds out what your controller sends, can fix the mapping, and saves a report to send to us.")
                .clicked();
            ui.label(
                RichText::new("If turning or touching one does the wrong thing.")
                    .small()
                    .weak(),
            );
        });
        if let Some(note) = engine.learn_note() {
            ui.label(RichText::new(note).color(AMBER));
        }
        let waiting = engine.waiting_for_pickup_in(&room);
        if !waiting.is_empty() {
            let names: Vec<String> = waiting.iter().map(|t| t.label()).collect();
            ui.label(
                RichText::new(format!("Move to pick up: {}", names.join(", ")))
                    .small()
                    .color(AMBER),
            );
        }
        let back = match &preset {
            Some(_) => "; Reset puts the built-in one back",
            None => "",
        };
        ui.label(
            RichText::new(format!("To change one control, open its group below and click Learn, then press or move the control on your controller. + Add gives an action another control as well, and ✖ takes one off it{back}. If your SHIFT or pad mode buttons are mapped here, hold SHIFT or pick the pad mode first: the control then does it only with SHIFT, or only in that pad mode. Faders pick up once they reach the current value, so you never jump over your partner. Tempo faders never wait: when SYNC has moved the tempo they move it relatively (SHIFT + SYNC puts the deck back on the fader). SHIFT + tempo fader moves the fader but not the tempo, so you can slide it back and keep going."))
                .small()
                .weak(),
        );
        // What the level knobs do depends on where the sound goes.
        let levels = [
            (
                Target::MasterLevel,
                "master",
                "the master plays",
                "it",
                engine.card.master,
                &self.audio.master_device,
            ),
            (
                Target::HeadphoneLevel,
                "headphone",
                "the headphones play",
                "them",
                engine.card.phones,
                &self.audio.cue_device,
            ),
        ];
        for (target, what, output, it, on_card, device) in levels {
            let knobs = engine.mapping.bound(target).count();
            if knobs == 0 {
                continue;
            }
            let name = target.label();
            let text = match device {
                _ if on_card && knobs > 1 => format!(
                    "{name} controls: {output} through this controller's own sound card, and its own knob \
                     turns {it} up and down there by itself, so they all leave Tandem's {what} level alone."
                ),
                _ if knobs > 1 => format!("{name} controls: each sets Tandem's {what} level."),
                _ if on_card => format!(
                    "{name} knob: {output} through this controller's own sound card, and the knob \
                     turns {it} up and down there by itself, so it leaves Tandem's {what} level alone."
                ),
                Some(d) => format!("{name} knob: sets Tandem's {what} level, as {output} through {d}."),
                None => format!("{name} knob: sets Tandem's {what} level."),
            };
            ui.label(RichText::new(text).small().weak());
        }
        if engine.card.phones_mix && engine.mapping.binding_for(Target::CueMix).is_some() {
            ui.label(
                RichText::new(
                    "Headphone mix knob: the headphones play through this controller's own jack, \
                     and it blends cue and master there by itself, so the knob leaves Tandem's mix alone.",
                )
                .small()
                .weak(),
            );
        }
        ui.separator();
        let learning = engine.learn;
        let list_h = (ui.ctx().screen_rect().height() - 430.0).clamp(150.0, 380.0);
        let all = Target::all();
        egui::ScrollArea::vertical()
            .max_height(list_h)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                // One header for each part of the controller (see `Target::group`).
                let mut from = 0;
                while from < all.len() {
                    let group = all[from].group();
                    let to = (from..all.len())
                        .find(|&i| all[i].group() != group)
                        .unwrap_or(all.len());
                    let targets = &all[from..to];
                    from = to;
                    let mapped = targets
                        .iter()
                        .filter(|t| engine.mapping.binding_for(**t).is_some())
                        .count();
                    let mut title = format!(
                        "{} · {mapped} of {} mapped",
                        group.0.title(group.1),
                        targets.len()
                    );
                    let changes = preset.as_ref().map_or(0, |p| {
                        targets
                            .iter()
                            .filter(|t| engine.mapping.differs(**t, p))
                            .count()
                    });
                    if changes > 0 {
                        title.push_str(&format!(" · {changes} changed"));
                    }
                    // The group of the control being learned stays open.
                    let open = targets.iter().any(|t| learning == Some(*t)).then_some(true);
                    egui::CollapsingHeader::new(title)
                        .id_salt(("mapping-group", group))
                        .open(open)
                        .show(ui, |ui| {
                            // Each control has a line, and a target's lines share a stripe.
                            let lines = |t: &Target| engine.mapping.bound(*t).count().max(1);
                            let stripes: Vec<bool> = targets
                                .iter()
                                .enumerate()
                                .flat_map(|(i, t)| std::iter::repeat_n(i % 2 == 1, lines(t)))
                                .collect();
                            let stripe = move |row: usize, style: &egui::Style| {
                                stripes
                                    .get(row)
                                    .copied()
                                    .unwrap_or(false)
                                    .then_some(style.visuals.faint_bg_color)
                            };
                            egui::Grid::new(("mapping", group))
                                .num_columns(5)
                                .with_row_color(stripe)
                                .show(ui, |ui| {
                                    for &target in targets {
                                        changed |=
                                            mapping_rows(ui, &mut engine, target, preset.as_ref());
                                    }
                                });
                        });
                }
            });
        drop(engine);
        if changed {
            self.midi.save_mapping(&self.paths.mappings_dir);
        }
        if set_up {
            self.open_wizard();
        }
        if open_check {
            self.open_deck_check();
        }
        if open_jog_check {
            self.open_jog_check();
        }
    }

    fn general_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("DJ name");
            if ui
                .add(egui::TextEdit::singleline(&mut self.settings.name).desired_width(200.0))
                .changed()
            {
                let name: String = self.settings.name.chars().take(32).collect();
                self.session.lock().set_name(name);
                self.mark_dirty();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Upload speed limit (all uploads together)");
            if ui
                .add(
                    egui::DragValue::new(&mut self.settings.max_upload_mb_s)
                        .range(0.25..=100.0)
                        .speed(0.1)
                        .suffix(" MB/s"),
                )
                .changed()
            {
                self.session.lock().max_upload_bytes_s =
                    self.settings.max_upload_mb_s as f64 * 1024.0 * 1024.0;
                self.mark_dirty();
            }
        });
        ui.label(RichText::new("Keeps track transfers from swamping your connection while you play: every track you send, to another DJ or a broadcaster, shares it. Lower it if the other DJs' controls feel laggy, or a broadcaster warns that moves arrive late, while tracks are being sent.").small().weak());
        ui.separator();
        if ui
            .checkbox(
                &mut self.settings.quantize_loops,
                "Quantize loop IN and OUT",
            )
            .changed()
        {
            self.midi.engine.lock().quantize_loops = self.settings.quantize_loops;
            self.mark_dirty();
        }
        ui.label(RichText::new("IN goes on the nearest beat, and OUT makes the loop 1 bar, ½ bar, ¼ bar … or 2, 4, 8 bars …, whichever is nearest to when you press it: OUT about a bar after IN makes a 1-bar loop. Off: IN and OUT go exactly where the playhead is. Tracks without a beatgrid always loop exactly. This is your own setting; the other DJs' loops follow theirs.").small().weak());
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Screen frame rate");
            let mut rate = self.settings.frame_rate;
            egui::ComboBox::from_id_salt("frame-rate")
                .selected_text(rate.label())
                .show_ui(ui, |ui| {
                    for f in FrameRate::CHOICES {
                        ui.selectable_value(&mut rate, f, f.label());
                    }
                });
            if rate != self.settings.frame_rate {
                self.settings.frame_rate = rate;
                self.mark_dirty();
            }
        });
        ui.label(RichText::new("How often the decks and waveforms redraw, here and in the broadcast window. Higher is smoother but uses more CPU and GPU. Your controller never waits for the screen. For the smoothest scrolling, pick your monitor's rate or half of it.").small().weak());
        ui.separator();
        for (label, path) in [
            ("Shared with me", &self.paths.shared_dir),
            ("Logs and consent record", &self.paths.data_dir),
            ("Settings and mappings", &self.paths.config_dir),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                if ui.small_button("Open").clicked() {
                    let _ = std::process::Command::new("explorer").arg(path).spawn();
                }
                ui.label(RichText::new(path.display().to_string()).small().weak());
            });
        }
        ui.separator();
        if ui.button("View terms").clicked() {
            self.ui.show_terms = true;
        }
        ui.label(
            RichText::new(format!("Tandem {}", env!("CARGO_PKG_VERSION")))
                .small()
                .weak(),
        );
    }
}

/// How a control of the mapping list is shown: "CC 0x36 · ch 5, 1/127
/// style".
fn control_label(b: &Binding) -> String {
    let mut s = b.label();
    if b.target.is_relative() && b.rel == RelMode::TwosComplement {
        s.push_str(", 1/127 style");
    }
    if b.invert {
        s.push_str(", inverted");
    }
    s
}

/// One target of the mapping list: its buttons, and a line for each of its
/// controls. Returns whether it changed the mapping.
fn mapping_rows(
    ui: &mut egui::Ui,
    engine: &mut MidiEngine,
    target: Target,
    preset: Option<&Mapping>,
) -> bool {
    let controls: Vec<(usize, Binding)> = engine
        .mapping
        .bindings
        .iter()
        .cloned()
        .enumerate()
        .filter(|(_, b)| b.target == target)
        .collect();
    let (mut remove, mut reset, mut changed) = (None, None, false);
    ui.label(target.label());
    ui.horizontal(|ui| {
        let learning = engine.learn == Some(target);
        let mut learn = |ui: &mut egui::Ui, text: &str, adds: bool, hover: &str| {
            let on = learning && engine.learn_adds == adds;
            if learning && !on {
                // The one that's waiting is enough, and wide.
                return;
            }
            let b = egui::Button::new(if on { "Move a control…" } else { text }).selected(on);
            if ui.add(b).on_hover_text(hover).clicked() {
                engine.learn = (!on).then_some(target);
                engine.learn_adds = adds && !on;
            }
        };
        let mut hover = "Press or move the control that does this";
        if !controls.is_empty() {
            hover = "Press or move the control that does this, instead of what's here";
        }
        learn(ui, "Learn", false, hover);
        if !controls.is_empty() {
            let hover = "Press or move another control that does this too";
            learn(ui, "+ Add", true, hover);
        }
        if let Some(preset) = preset.filter(|p| engine.mapping.differs(target, p)) {
            let theirs: Vec<String> = preset.bound(target).map(control_label).collect();
            let hover = match &theirs[..] {
                [] => "The built-in mapping has nothing here: this clears it".into(),
                [one] => format!("Back to the built-in control: {one}"),
                more => format!("Back to the built-in controls:\n{}", more.join("\n")),
            };
            if ui.button("Reset").on_hover_text(hover).clicked() {
                reset = Some(preset);
            }
        }
    });
    if controls.is_empty() {
        ui.label(RichText::new("-").weak());
        ui.end_row();
    }
    for (n, (i, b)) in controls.iter().enumerate() {
        if n > 0 {
            ui.label("");
            ui.label("");
        }
        ui.monospace(b.label());
        if ui
            .small_button("✖")
            .on_hover_text("Take this control off it")
            .clicked()
        {
            remove = Some(*i);
        }
        ui.horizontal(|ui| {
            let (mut invert, mut two) = (b.invert, b.rel == RelMode::TwosComplement);
            let mut edited = false;
            // A wheel that says where it is doesn't count either way.
            if target.is_relative() && !b.rel.is_position() {
                edited |= ui
                    .checkbox(&mut two, "1/127 style")
                    .on_hover_text("Tick this if the wheel/encoder moves the wrong way or jumps")
                    .changed();
            }
            if target.is_absolute() || target.is_relative() {
                edited |= ui.checkbox(&mut invert, "invert").changed();
            }
            if edited {
                let b = &mut engine.mapping.bindings[*i];
                b.invert = invert;
                b.rel = b.rel.counting(two);
                changed = true;
            }
            let also: Vec<String> = engine.mapping.also(b).iter().map(Target::label).collect();
            if !also.is_empty() {
                let text = format!("also does {}", also.join(", "));
                ui.label(RichText::new(text).small().color(AMBER));
            }
        });
        ui.end_row();
    }
    if let Some(preset) = reset {
        engine.edit(|m| m.reset_target(target, preset));
        changed = true;
    } else if let Some(i) = remove {
        engine.edit(|m| {
            m.bindings.remove(i);
        });
        changed = true;
    }
    changed
}

fn route_editor(
    ui: &mut egui::Ui,
    label: &str,
    none_label: &str,
    route: &mut Option<Route>,
    devices: &[output::DeviceInfo],
) {
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 18.0], egui::Label::new(label));
        let current = route
            .as_ref()
            .map(|r| r.device.clone())
            .unwrap_or_else(|| none_label.to_string());
        egui::ComboBox::from_id_salt(("dev", label))
            .selected_text(current)
            .width(300.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(route.is_none(), none_label).clicked() {
                    *route = None;
                }
                for d in devices {
                    let sel = route.as_ref().map(|r| r.device == d.name).unwrap_or(false);
                    if ui
                        .selectable_label(sel, format!("{} ({} ch)", d.name, d.max_channels))
                        .clicked()
                    {
                        *route = Some(Route {
                            device: d.name.clone(),
                            pair: 0,
                        });
                    }
                }
            });
        if let Some(r) = route.as_mut() {
            let pairs = devices
                .iter()
                .find(|d| d.name == r.device)
                .map(|d| (d.max_channels / 2).max(1))
                .unwrap_or(1);
            egui::ComboBox::from_id_salt(("pair", label))
                .selected_text(format!("ch {}/{}", r.pair * 2 + 1, r.pair * 2 + 2))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for p in 0..pairs {
                        ui.selectable_value(
                            &mut r.pair,
                            p,
                            format!("ch {}/{}", p * 2 + 1, p * 2 + 2),
                        );
                    }
                });
        }
    });
}
