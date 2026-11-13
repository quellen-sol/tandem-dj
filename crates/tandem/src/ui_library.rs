//! Library browser: sources on the left, tracks on the right.

use crate::app::{ShareDraft, Source, TandemApp};
use crate::ui_decks::fmt_time;
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, RichText, Sense};
use tandem_core::library::KEY_SHARED;
use tandem_core::session::{deck_letter, SessionView, ShareState, TransferState};

struct Row {
    title: String,
    artist: String,
    bpm: Option<f64>,
    key: Option<i8>,
    duration: f64,
}

impl TandemApp {
    pub(crate) fn library_panel(&mut self, ctx: &egui::Context, view: &SessionView) {
        egui::TopBottomPanel::bottom("library")
            .resizable(true)
            .default_height(330.0)
            .min_height(160.0)
            .show(ctx, |ui| {
                egui::SidePanel::left("sources")
                    .resizable(true)
                    .default_width(250.0)
                    .show_inside(ui, |ui| self.sources(ui, view));
                egui::CentralPanel::default().show_inside(ui, |ui| self.tracks(ui, view));
            });
    }

    fn sources(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut pick: Option<Source> = None;
                if ui
                    .selectable_label(
                        self.ui.source == Source::Room,
                        format!("🎵 This room ({})", view.room_tracks.len()),
                    )
                    .on_hover_text("Every track loaded in this session")
                    .clicked()
                {
                    pick = Some(Source::Room);
                }
                let receiving = view
                    .incoming
                    .iter()
                    .filter(|s| {
                        s.state == ShareState::Accepted
                            && s.received + s.already_had + s.failed < s.tracks.len()
                    })
                    .count();
                let shared_label = if receiving > 0 {
                    format!("⬇ Shared with me (receiving {receiving})")
                } else {
                    "⬇ Shared with me".to_string()
                };
                if ui
                    .selectable_label(self.ui.source == Source::Shared, shared_label)
                    .clicked()
                {
                    pick = Some(Source::Shared);
                }

                ui.add_space(6.0);
                ui.label(RichText::new("FOLDERS").small().weak());
                let mut remove = None;
                for f in self.settings.library_folders.clone() {
                    ui.horizontal(|ui| {
                        let name = f
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| f.display().to_string());
                        if ui
                            .selectable_label(
                                self.ui.source == Source::Folder(f.clone()),
                                format!("📁 {name}"),
                            )
                            .on_hover_text(f.display().to_string())
                            .clicked()
                        {
                            pick = Some(Source::Folder(f.clone()));
                        }
                        if ui
                            .small_button("✖")
                            .on_hover_text("Remove from library (doesn't delete files)")
                            .clicked()
                        {
                            remove = Some(f.clone());
                        }
                    });
                }
                if ui.button("➕ Add music folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new()
                        .set_title("Add a music folder")
                        .pick_folder()
                    {
                        self.add_folder(dir);
                    }
                }

                ui.add_space(6.0);
                let crates = self.library.lock().crates.clone();
                egui::CollapsingHeader::new(
                    RichText::new(format!("SERATO CRATES ({})", crates.len()))
                        .small()
                        .weak(),
                )
                .default_open(true)
                .show(ui, |ui| {
                    if crates.is_empty() {
                        ui.label(
                            RichText::new(
                                "No Serato crates found in Music\\_Serato_ or on other drives.",
                            )
                            .small()
                            .weak(),
                        );
                    }
                    for c in crates {
                        let selected =
                            matches!(&self.ui.source, Source::Crate(x) if x.file == c.file);
                        if ui
                            .selectable_label(selected, format!("🗂 {}", c.name))
                            .clicked()
                        {
                            pick = Some(Source::Crate(c.clone()));
                        }
                    }
                    if ui.small_button("Rescan crates").clicked() {
                        self.library
                            .lock()
                            .refresh_crates(self.paths.music_dir.as_deref());
                    }
                });

                if !view.transfers.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("TRANSFERS").small().weak());
                    for t in view.transfers.iter().rev().take(8) {
                        let arrow = if t.upload { "⬆" } else { "⬇" };
                        let frac = if t.total > 0 {
                            t.done as f32 / t.total as f32
                        } else {
                            0.0
                        };
                        let text = match &t.state {
                            TransferState::Queued => format!("{arrow} {} (queued)", t.name),
                            TransferState::Active => format!("{arrow} {}", t.name),
                            TransferState::Done => format!("{arrow} {} ✔", t.name),
                            TransferState::Failed(e) => format!("{arrow} {} failed: {e}", t.name),
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .text(RichText::new(text).small())
                                .desired_height(16.0),
                        );
                    }
                }
                for (title, n, replies) in &view.outgoing {
                    for (who, ok) in replies {
                        ui.label(
                            RichText::new(format!(
                                "{who} {} \"{title}\" ({n})",
                                if *ok { "accepted" } else { "declined" }
                            ))
                            .small()
                            .weak(),
                        );
                    }
                }
                if let Some(dir) = remove {
                    self.remove_folder(&dir);
                }
                if let Some(src) = pick {
                    self.set_source(src);
                }
            });
    }

    fn tracks(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let src = self.ui.source.clone();
        let key = src.key();
        let (scanning, error, all_paths, bytes) = {
            let lib = self.library.lock();
            match lib.lists.get(&key) {
                Some(l) => (
                    l.scanning,
                    l.error.clone(),
                    l.tracks.iter().map(|t| t.path.clone()).collect::<Vec<_>>(),
                    l.tracks.iter().map(|t| t.size).sum::<u64>(),
                ),
                None => (false, None, Vec::new(), 0),
            }
        };

        ui.horizontal(|ui| {
            ui.label(RichText::new(src.label()).strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.ui.search)
                    .hint_text("🔍 Search title or artist")
                    .desired_width(220.0),
            );
            for d in 0..2 {
                if ui.button(format!("Load to {}", deck_letter(d))).clicked() {
                    if let Some(item) = self.item_at(self.ui.sel_index, view) {
                        self.load(d, item, view);
                    }
                }
            }
            if !matches!(src, Source::Room) {
                let can = view.link.peer.is_some()
                    && !all_paths.is_empty()
                    && view.preparing_share.is_none();
                let r = ui.add_enabled(can, egui::Button::new("Share this list…"));
                let r = if view.link.peer.is_none() {
                    r.on_disabled_hover_text("Host or join a room first")
                } else {
                    r
                };
                if r.clicked() {
                    self.ui.share_draft = Some(ShareDraft {
                        title: src.label(),
                        paths: all_paths.clone(),
                        bytes,
                        pledge: false,
                    });
                }
            }
            if let Some(t) = &view.preparing_share {
                ui.spinner();
                ui.label(RichText::new(format!("Preparing \"{t}\"…")).weak());
            }
            if scanning {
                ui.spinner();
                ui.label(RichText::new("Scanning…").weak());
            }
            let analyzing = self.library.lock().analyzing.clone();
            match analyzing {
                Some((_, done, total)) => {
                    ui.spinner();
                    ui.label(RichText::new(format!("Analysing BPM {done}/{total}")).weak());
                }
                None if !matches!(src, Source::Room) && !scanning && !all_paths.is_empty() => {
                    if ui
                        .button("Analyze BPM")
                        .on_hover_text("Detect the tempo and beatgrid of every track in this list (runs in the background; each track is only analysed once).")
                        .clicked()
                    {
                        tandem_core::library::analyze_list(&self.library, &key);
                    }
                }
                None => {}
            }
            if key == KEY_SHARED {
                if ui.small_button("Open folder").clicked() {
                    let _ = std::process::Command::new("explorer")
                        .arg(&self.paths.shared_dir)
                        .spawn();
                }
            }
        });
        if let Some(e) = error {
            ui.label(RichText::new(e).color(Color32::from_rgb(240, 90, 90)));
        }

        let row_h = 22.0;
        let width = ui.available_width();
        let cols = [0.0, width * 0.42, width * 0.74, width * 0.83, width * 0.91];
        let header = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover()).0;
        let hp = ui.painter_at(header);
        for (x, label) in cols.iter().zip(["TITLE", "ARTIST", "BPM", "KEY", "TIME"]) {
            hp.text(
                pos2(header.left() + x + 6.0, header.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                Color32::from_gray(130),
            );
        }

        let n = self.ui.filtered.len();
        if n == 0 {
            ui.add_space(12.0);
            let hint = match src {
                Source::Room => "Tracks you or your partner load will appear here.",
                Source::Shared => "Nothing shared with you yet.",
                _ if scanning => "Scanning…",
                _ if !self.ui.search.is_empty() => "No matches.",
                _ => "No audio files here.",
            };
            ui.label(RichText::new(hint).weak());
            return;
        }

        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.ui.scroll_to_sel {
            scroll = scroll.vertical_scroll_offset(
                (self.ui.sel_index as f32 * (row_h + ui.spacing().item_spacing.y) - 60.0).max(0.0),
            );
            self.ui.scroll_to_sel = false;
        }
        let lib = self.library.clone();
        let lib = lib.lock();
        let list = lib.lists.get(&key);
        let mut clicked = None;
        let mut double = None;
        let mut menu_load = None;
        scroll.show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let Some(&idx) = self.ui.filtered.get(i) else {
                    continue;
                };
                let row = match &src {
                    Source::Room => view.room_tracks.get(idx).map(|t| Row {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        bpm: t
                            .grid
                            .map(|g| g.main_bpm(t.duration))
                            .or(t.bpm.map(|b| b as f64)),
                        key: t.key,
                        duration: t.duration,
                    }),
                    _ => list.and_then(|l| l.tracks.get(idx)).map(|t| Row {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        bpm: t.grid_bpm.or(t.bpm.map(|b| b as f64)),
                        key: t.key,
                        duration: t.duration,
                    }),
                };
                let Some(row) = row else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                let p = ui.painter_at(rect);
                let selected = i == self.ui.sel_index;
                if selected {
                    p.rect_filled(rect, 2.0, Color32::from_rgb(60, 70, 95));
                } else if resp.hovered() {
                    p.rect_filled(rect, 2.0, Color32::from_gray(38));
                } else if i % 2 == 1 {
                    p.rect_filled(rect, 0.0, Color32::from_gray(24));
                }
                let texts = [
                    row.title,
                    row.artist,
                    row.bpm.map(|b| format!("{b:.1}")).unwrap_or_default(),
                    row.key.map(tandem_core::keys::camelot).unwrap_or_default(),
                    if row.duration > 0.0 {
                        fmt_time(row.duration)
                            .split('.')
                            .next()
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::new()
                    },
                ];
                for (c, text) in texts.iter().enumerate() {
                    let left = rect.left() + cols[c] + 6.0;
                    let right = if c + 1 < cols.len() {
                        rect.left() + cols[c + 1]
                    } else {
                        rect.right()
                    };
                    let clip = egui::Rect::from_min_max(
                        pos2(left, rect.top()),
                        pos2(right - 4.0, rect.bottom()),
                    );
                    ui.painter_at(clip).text(
                        pos2(left, rect.center().y),
                        Align2::LEFT_CENTER,
                        text,
                        FontId::proportional(13.5),
                        Color32::from_gray(if c == 0 { 230 } else { 170 }),
                    );
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
                if resp.double_clicked() {
                    double = Some(i);
                }
                resp.context_menu(|ui| {
                    for d in 0..2 {
                        if ui
                            .button(format!("Load to deck {}", deck_letter(d)))
                            .clicked()
                        {
                            menu_load = Some((i, d));
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        drop(lib);
        if let Some(i) = clicked {
            self.ui.sel_index = i;
        }
        if let Some(i) = double {
            self.ui.sel_index = i;
            match (self.free_deck(view), self.item_at(i, view)) {
                (Some(d), Some(item)) => self.load(d, item, view),
                (None, _) => {}
                _ => {}
            }
        }
        if let Some((i, d)) = menu_load {
            if let Some(item) = self.item_at(i, view) {
                self.load(d, item, view);
            }
        }
    }
}
