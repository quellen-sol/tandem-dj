//! Library browser: sources on the left, tracks on the right.

use crate::app::{lib_bpm, room_bpm, BrowseFocus, ShareDraft, Source, TandemApp};
use crate::ui_decks::{fmt_time, DeckLayout, DECK_COLORS};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, RichText, Sense};
use tandem_core::config::SortColumn;
use tandem_core::library::{KEY_ALL, KEY_SHARED};
use tandem_core::session::{deck_letter, SessionView, ShareState, TransferState};

/// The outline round the side of the library the browse knob moves through.
const KNOB_FOCUS: Color32 = Color32::from_rgb(70, 130, 210);

/// Scroll the source list to `r` if the knob just moved there.
fn follow(r: &egui::Response, on: bool) {
    if on {
        r.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
    }
}

struct Row {
    title: String,
    artist: String,
    bpm: Option<f64>,
    key: Option<i8>,
    duration: f64,
}

/// The library's height on a window with room for it, and the least it goes.
pub(crate) const LIBRARY_H: f32 = 330.0;
pub(crate) const LIBRARY_MIN_H: f32 = 150.0;
/// Below this, the two-deck view gives the library more room by swapping
/// to compact deck panels.
pub(crate) const LIBRARY_ROOMY_H: f32 = 180.0;
/// The central panel's margins, above and below the decks area (and either
/// side of it).
pub(crate) const CENTRAL_MARGIN: f32 = 16.0;

/// The library's height until the DJ drags its edge. `room` is the height
/// the library and the decks area share, `decks_w` the decks area's width.
/// With room, it's 330 points; on a shorter window it gives up height first,
/// down to 180, then the two-deck view takes compact panels, which leave it
/// more; it never goes under 150.
pub(crate) fn library_height(room: f32, decks_w: f32, four: bool) -> f32 {
    if !four && DeckLayout::full_fits_width(decks_w) {
        let h = room - DeckLayout::Full.roomy_h();
        if h >= LIBRARY_ROOMY_H {
            return h.min(LIBRARY_H);
        }
    }
    let layout = if four {
        DeckLayout::Four
    } else {
        DeckLayout::Compact
    };
    (room - layout.roomy_h()).clamp(LIBRARY_MIN_H, LIBRARY_H)
}

/// The most the library can be dragged up to, leaving the decks area its
/// least (waveforms included).
pub(crate) fn library_max_height(room: f32, decks_w: f32, four: bool) -> f32 {
    let layout = DeckLayout::pick(four, decks_w, 0.0);
    (room - layout.least_h()).max(LIBRARY_MIN_H)
}

impl TandemApp {
    pub(crate) fn library_panel(&mut self, ctx: &egui::Context, view: &SessionView) {
        let id = egui::Id::new("library");
        let avail = ctx.available_rect();
        let (room, decks_w) = (
            avail.height() - CENTRAL_MARGIN,
            avail.width() - CENTRAL_MARGIN,
        );
        let four = self.settings.four_decks;
        let max = library_max_height(room, decks_w, four);
        let auto = library_height(room, decks_w, four).min(max);
        // Until the DJ drags its edge, the library follows the window's
        // height: egui keeps a dragged height, and otherwise starts from
        // the default.
        if ctx
            .read_response(id.with("__resize"))
            .is_some_and(|r| r.dragged())
        {
            self.ui.library_dragged = true;
        }
        if !self.ui.library_dragged {
            ctx.data_mut(|d| d.remove::<egui::containers::panel::PanelState>(id));
        }
        egui::TopBottomPanel::bottom(id)
            .resizable(true)
            .min_height(LIBRARY_MIN_H)
            .max_height(max)
            .default_height(auto)
            .show(ctx, |ui| {
                let sources = egui::SidePanel::left("sources")
                    .resizable(true)
                    .default_width(250.0)
                    .show_inside(ui, |ui| self.sources(ui, view))
                    .response
                    .rect;
                let tracks = egui::CentralPanel::default()
                    .show_inside(ui, |ui| self.tracks(ui, view))
                    .response
                    .rect;
                let focus = match self.ui.browse_focus {
                    BrowseFocus::Sources => sources,
                    BrowseFocus::Tracks => tracks,
                };
                ui.painter().rect_stroke(
                    focus.shrink(1.0),
                    3.0,
                    egui::Stroke::new(1.5_f32, KNOB_FOCUS),
                    egui::StrokeKind::Inside,
                );
            });
    }

    fn sources(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut pick: Option<Source> = None;
                let knob = std::mem::take(&mut self.ui.scroll_to_source);
                let all_label = match self.library.lock().lists.get(KEY_ALL) {
                    Some(l) if !l.scanning || !l.tracks.is_empty() => {
                        format!("🎶 All Music ({})", l.tracks.len())
                    }
                    _ => "🎶 All Music".to_string(),
                };
                let on = self.ui.source == Source::All;
                let r = ui.selectable_label(on, all_label).on_hover_text(
                    "Every track in your folders, Shared with me and your Serato crates",
                );
                follow(&r, on && knob);
                if r.clicked() {
                    pick = Some(Source::All);
                }
                let on = self.ui.source == Source::Room;
                let r = ui
                    .selectable_label(on, format!("🎵 This room ({})", view.room_tracks.len()))
                    .on_hover_text("Every track loaded in this session");
                follow(&r, on && knob);
                if r.clicked() {
                    pick = Some(Source::Room);
                }
                let receiving: Vec<_> = view
                    .incoming
                    .iter()
                    .filter(|s| {
                        s.state == ShareState::Accepted
                            && s.received + s.already_had + s.failed < s.tracks.len()
                    })
                    .collect();
                // Every track still to come is waiting for the link.
                let paused = !receiving.is_empty()
                    && receiving.iter().all(|s| {
                        s.received + s.already_had + s.failed + s.paused == s.tracks.len()
                    });
                let shared_label = match receiving.len() {
                    0 => "⬇ Shared with me".to_string(),
                    n if paused => format!("⬇ Shared with me (receiving {n}, paused)"),
                    n => format!("⬇ Shared with me (receiving {n})"),
                };
                let on = self.ui.source == Source::Shared;
                let mut r = ui.selectable_label(on, shared_label);
                if paused {
                    r = r.on_hover_text(
                        "The connection dropped. The downloads carry on from where they got to when it's back.",
                    );
                }
                follow(&r, on && knob);
                if r.clicked() {
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
                        let on = self.ui.source == Source::Folder(f.clone());
                        let r = ui
                            .selectable_label(on, format!("📁 {name}"))
                            .on_hover_text(f.display().to_string());
                        follow(&r, on && knob);
                        if r.clicked() {
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
                let section = egui::CollapsingHeader::new(
                    RichText::new(format!("SERATO CRATES ({})", crates.len()))
                        .small()
                        .weak(),
                )
                .id_salt("serato crates")
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
                        let r = ui.selectable_label(selected, format!("🗂 {}", c.name));
                        follow(&r, selected && knob);
                        if r.clicked() {
                            pick = Some(Source::Crate(c.clone()));
                        }
                    }
                    if ui.small_button("Rescan crates").clicked() {
                        self.library
                            .lock()
                            .refresh_crates(self.paths.music_dir.as_deref());
                        tandem_core::library::refresh_all(&self.library);
                    }
                });
                // The knob only moves through crates you can see.
                self.ui.crates_open = section.body_returned.is_some();

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
                        let pct = |b: u64| format!("{:.0}%", b as f64 * 100.0 / t.total.max(1) as f64);
                        let text = match &t.state {
                            TransferState::Queued if t.done > 0 => {
                                format!("{arrow} {} (resuming at {})", t.name, pct(t.done))
                            }
                            TransferState::Queued => format!("{arrow} {} (queued)", t.name),
                            TransferState::Active if t.from > 0 => {
                                format!("{arrow} {} (resumed at {})", t.name, pct(t.from))
                            }
                            TransferState::Active => format!("{arrow} {}", t.name),
                            TransferState::Paused => {
                                format!("{arrow} {} (paused until the link is back)", t.name)
                            }
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
                    // Picking a list goes into it, like pressing the knob
                    // on it: the knob moves through its tracks.
                    self.set_source(src);
                    self.ui.browse_focus = BrowseFocus::Tracks;
                }
            });
    }

    /// The search box. While it has text, a ✖ inside its right end clears
    /// it, and so does Escape in the box. On an empty box, Escape just
    /// leaves it, as before.
    fn search_box(&mut self, ui: &mut egui::Ui) {
        // The ✖ sits in the box's right margin, made wider by the room the
        // text gives up, so the box is as wide as ever.
        const CLEAR_W: f32 = 22.0;
        let margin = egui::Margin {
            left: 4,
            right: CLEAR_W as i8,
            top: 2,
            bottom: 2,
        };
        let id = ui.make_persistent_id("library search");
        // egui has already taken the focus away when Escape reaches us: give
        // it back, so the next search can be typed straight away.
        if !self.ui.search.is_empty()
            && ui.input(|i| i.key_pressed(egui::Key::Escape))
            && ui.memory(|m| m.had_focus_last_frame(id))
        {
            self.ui.search.clear();
            ui.memory_mut(|m| m.request_focus(id));
        }
        let text = egui::TextEdit::singleline(&mut self.ui.search)
            .id(id)
            .hint_text("🔍 Search title or artist")
            // 228 px in all, as before.
            .desired_width(228.0 - 4.0 - CLEAR_W)
            .margin(margin)
            .show(ui)
            .text_clip_rect;
        if self.ui.search.is_empty() {
            return;
        }
        let cell = egui::Rect::from_min_size(
            pos2(text.right(), text.top() - margin.top as f32),
            vec2(CLEAR_W, text.height() + margin.sum().y),
        );
        // On top of the box and sensing drags too, so pressing it doesn't
        // also start a text selection underneath.
        let resp = ui
            .interact(cell, id.with("clear"), Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Clear the search (Esc)");
        let p = ui.painter();
        if resp.hovered() {
            p.circle_filled(cell.center(), 8.0, Color32::from_gray(60));
        }
        p.text(
            cell.center(),
            Align2::CENTER_CENTER,
            "✖",
            FontId::proportional(11.0),
            Color32::from_gray(if resp.hovered() { 235 } else { 150 }),
        );
        if resp.clicked() {
            self.ui.search.clear();
            ui.memory_mut(|m| m.request_focus(id));
        }
    }

    fn tracks(&mut self, ui: &mut egui::Ui, view: &SessionView) {
        let shown = self.shown_decks();
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
            self.search_box(ui);
            ui.label("Load to");
            let spacing = ui.spacing().item_spacing.x;
            ui.spacing_mut().item_spacing.x = 3.0;
            for (d, &color) in DECK_COLORS.iter().enumerate().take(shown) {
                let letter = RichText::new(deck_letter(d).to_string())
                    .strong()
                    .color(color);
                if ui
                    .add(egui::Button::new(letter).min_size(vec2(24.0, 0.0)))
                    .on_hover_text(format!("Load the selected track onto deck {}", deck_letter(d)))
                    .clicked()
                {
                    if let Some(item) = self.item_at(self.ui.sel_index, view) {
                        self.load(d, item, view);
                    }
                }
            }
            ui.spacing_mut().item_spacing.x = spacing;
            if !matches!(src, Source::Room) {
                let can = view.link.linked()
                    && !all_paths.is_empty()
                    && view.preparing_share.is_none();
                let r = ui.add_enabled(can, egui::Button::new("Share this list…"));
                let r = if !view.link.linked() {
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
                Some(a) if a.stopping => {
                    ui.spinner();
                    ui.label(RichText::new("Stopping analysis…").weak());
                }
                Some(a) => {
                    ui.spinner();
                    ui.label(
                        RichText::new(format!("Analysing BPM {}/{}", a.done, a.total)).weak(),
                    );
                    if ui
                        .button("Cancel")
                        .on_hover_text("Stop analysing. Tracks already done keep their BPM and key; Analyze BPM carries on from where it stopped.")
                        .clicked()
                    {
                        tandem_core::library::cancel_analysis(&self.library);
                    }
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
        // Column headings sort the list: once ascending, again descending,
        // a third time back to the list's own order.
        let header = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover()).0;
        let hp = ui.painter_at(header);
        let sort = self.settings.library_sort;
        let mut sort_click = None;
        let headings = [
            (SortColumn::Title, "TITLE"),
            (SortColumn::Artist, "ARTIST"),
            (SortColumn::Bpm, "BPM"),
            (SortColumn::Key, "KEY"),
            (SortColumn::Time, "TIME"),
        ];
        for (c, (column, label)) in headings.into_iter().enumerate() {
            let left = header.left() + cols[c];
            let right = cols
                .get(c + 1)
                .map_or(header.right(), |x| header.left() + x);
            let cell =
                egui::Rect::from_min_max(pos2(left, header.top()), pos2(right, header.bottom()));
            let resp = ui
                .interact(cell, ui.id().with(("sort", c)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(match sort {
                    Some(s) if s.column == column && !s.descending => {
                        "Sorted A→Z / low→high. Click for the other way round."
                    }
                    Some(s) if s.column == column => {
                        "Sorted Z→A / high→low. Click to go back to the list's own order."
                    }
                    _ => "Sort by this column",
                });
            let active = sort.filter(|s| s.column == column);
            if resp.hovered() {
                hp.rect_filled(cell.shrink2(vec2(1.0, 0.0)), 2.0, Color32::from_gray(34));
            }
            let color = Color32::from_gray(match (active.is_some(), resp.hovered()) {
                (true, _) => 225,
                (false, true) => 190,
                _ => 130,
            });
            let text = hp.text(
                pos2(left + 6.0, header.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(11.0),
                color,
            );
            if let Some(s) = active {
                // A small arrow: up for ascending, down for descending.
                let (x, y) = (text.right() + 7.0, header.center().y);
                let (tip, base) = if s.descending {
                    (3.0, -2.5)
                } else {
                    (-3.0, 2.5)
                };
                hp.add(egui::Shape::convex_polygon(
                    vec![
                        pos2(x, y + tip),
                        pos2(x + 3.5, y + base),
                        pos2(x - 3.5, y + base),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
            }
            if resp.clicked() {
                sort_click = Some(column);
            }
        }
        if let Some(column) = sort_click {
            self.sort_by(column);
        }

        let n = self.ui.filtered.len();
        if n == 0 {
            ui.add_space(12.0);
            let hint = match src {
                Source::Room => "Tracks you or the other DJs load will appear here.",
                Source::Shared => "Nothing shared with you yet.",
                _ if scanning => "Scanning…",
                _ if !self.ui.search.is_empty() => "No matches.",
                Source::All => "No music yet. Add a music folder on the left.",
                _ => "No audio files here.",
            };
            ui.label(RichText::new(hint).weak());
            return;
        }

        let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if self.ui.scroll_to_sel {
            // Keep the selected row near the top, but never scroll past the
            // end of the list: egui only pulls an offset back after drawing,
            // so near the end every knob tick drew one frame scrolled too
            // far before snapping back (a flicker, very visible in short lists).
            let gap = ui.spacing().item_spacing.y;
            let content = n as f32 * (row_h + gap) - gap;
            let max = (content - ui.available_height()).max(0.0);
            scroll = scroll.vertical_scroll_offset(
                (self.ui.sel_index as f32 * (row_h + gap) - 60.0).clamp(0.0, max),
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
                        bpm: room_bpm(t),
                        key: t.key,
                        duration: t.duration,
                    }),
                    _ => list.and_then(|l| l.tracks.get(idx)).map(|t| Row {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        bpm: lib_bpm(t),
                        key: t.key,
                        duration: t.duration,
                    }),
                };
                let Some(row) = row else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                let p = ui.painter_at(rect);
                let selected = i == self.ui.sel_index;
                if selected {
                    // Dimmer while the knob is on the lists.
                    let fill = match self.ui.browse_focus {
                        BrowseFocus::Tracks => Color32::from_rgb(60, 70, 95),
                        BrowseFocus::Sources => Color32::from_rgb(46, 50, 62),
                    };
                    p.rect_filled(rect, 2.0, fill);
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
                    for d in 0..shown {
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
        if clicked.is_some() || double.is_some() || menu_load.is_some() {
            self.ui.browse_focus = BrowseFocus::Tracks;
        }
        if let Some(i) = clicked {
            self.ui.sel_index = i;
        }
        if let Some(i) = double {
            self.ui.sel_index = i;
            match (self.free_deck(view), self.item_at(i, view)) {
                (Ok(d), Some(item)) => self.load(d, item, view),
                (Err(why), Some(_)) => self.toast(why),
                (_, None) => {}
            }
        }
        if let Some((i, d)) = menu_load {
            if let Some(item) = self.item_at(i, view) {
                self.load(d, item, view);
            }
        }
    }
}
