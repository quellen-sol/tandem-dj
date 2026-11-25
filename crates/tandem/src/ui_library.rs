//! Library browser: sources on the left, tracks on the right.

use crate::app::{
    lib_bpm, room_bpm, BrowseFocus, CrateDialog, CrateMode, DragTracks, Source, TandemApp,
};
use crate::ui_decks::{fmt_time, DeckLayout, FxStrip, DECK_COLORS};
use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, RichText, Sense};
use std::path::PathBuf;
use tandem_core::config::SortColumn;
use tandem_core::library::{self, KEY_ALL, KEY_SHARED};
use tandem_core::model::Action;
use tandem_core::session::{deck_letter, SessionView, ShareState, TransferState};

/// The outline round the side of the library the browse knob moves through.
const KNOB_FOCUS: Color32 = Color32::from_rgb(70, 130, 210);

/// Lists shared live: the 📡 mark on yours, and the other DJs' lists.
const LIVE: Color32 = Color32::from_rgb(110, 200, 150);

/// Round the title of a dragged track that no crate takes.
const REFUSED: Color32 = Color32::from_rgb(210, 120, 100);

/// What a crate on the left does when a dragged track hovers over it: lights
/// up, and takes it when it's let go. It doesn't light up for a track that
/// isn't the DJ's own (nor until that's known), and the pointer says no.
fn crate_drop(ui: &egui::Ui, r: &egui::Response) -> Option<DragTracks> {
    if let Some(d) = r.dnd_hover_payload::<DragTracks>() {
        match d.file_for(&[]) {
            Some(Ok(_)) => {
                ui.painter().rect_stroke(
                    r.rect.expand(1.0),
                    3.0,
                    egui::Stroke::new(1.5_f32, LIVE),
                    egui::StrokeKind::Outside,
                );
            }
            Some(Err(_)) => ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed),
            None => {}
        }
    }
    r.dnd_release_payload::<DragTracks>().map(|p| (*p).clone())
}

/// The title that follows the pointer while a track is dragged, and why no
/// crate takes it, if none does.
fn drag_label(ctx: &egui::Context, d: &DragTracks, at: egui::Pos2) {
    let no = d.file_for(&[]).and_then(|f| f.err());
    let p = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("dragged track"),
    ));
    let (fill, edge, text) = match &no {
        Some(no) => (
            Color32::from_rgb(58, 42, 40),
            REFUSED,
            format!("✖ {}\n{}", d.title, no.short()),
        ),
        None => (
            Color32::from_rgb(40, 60, 52),
            LIVE,
            format!("📦 {}", d.title),
        ),
    };
    let galley = p.layout_no_wrap(text, FontId::proportional(13.0), Color32::from_gray(235));
    let rect =
        egui::Rect::from_min_size(at + vec2(14.0, 10.0), galley.size()).expand2(vec2(8.0, 4.0));
    // Kept inside the window: a deck can be at its right, a crate at its
    // bottom.
    let over = rect.max - ctx.screen_rect().shrink(4.0).max;
    let rect = rect.translate(-over.max(egui::Vec2::ZERO));
    p.rect_filled(rect, 4.0, fill);
    p.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, edge),
        egui::StrokeKind::Inside,
    );
    p.galley(rect.min + vec2(8.0, 4.0), galley, Color32::from_gray(235));
}

/// With a track being dragged near the top or bottom of the source list
/// (`ui`, all of it), scroll it that way, so there's no crate out of reach.
fn drag_scroll(ui: &egui::Ui) {
    const EDGE: f32 = 24.0;
    /// Points a second, at the very edge.
    const SPEED: f32 = 700.0;
    let (shown, all) = (ui.clip_rect(), ui.min_rect());
    let Some(p) = (ui.ctx().pointer_latest_pos()).filter(|p| shown.contains(*p)) else {
        return;
    };
    let near = |d: f32| ((EDGE - d) / EDGE).max(0.0);
    let step = SPEED * ui.input(|i| i.stable_dt).min(0.1);
    // Never past either end: that would draw one frame scrolled too far.
    let up = (near(p.y - shown.top()) * step).min(shown.top() - all.top());
    let down = (near(shown.bottom() - p.y) * step).min(all.bottom() - shown.bottom());
    let dy = up.max(0.0) - down.max(0.0);
    if dy != 0.0 {
        ui.scroll_with_delta_animation(vec2(0.0, dy), egui::style::ScrollAnimation::none());
    }
}

/// Whether to open or close MY CRATES (`None`: leave it): it's open while a
/// track is being dragged, so there's somewhere to drop it, and if it was
/// closed (`open` is how it was last frame) it closes again after.
fn spring_open(dragging: bool, open: bool, sprung: &mut bool) -> Option<bool> {
    if dragging {
        *sprung |= !open;
        return Some(true);
    }
    std::mem::take(sprung).then_some(false)
}

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
/// the library and the decks area share, `decks_w` the decks area's width,
/// `strip` the FX strip's height. With room, it's 330 points; on a shorter
/// window it gives up height first, down to 180, then the two-deck view
/// takes compact panels, which leave it more; it never goes under 150.
///
/// The FX strip pushes the library down by its own height, never under
/// those 150 points (past that, the strip takes what the waveforms and the
/// four-deck panels can give up, see `FxStrip::pick`). It doesn't change
/// which panels the decks get: that's decided as if it weren't there.
pub(crate) fn library_height(room: f32, decks_w: f32, four: bool, strip: f32) -> f32 {
    if !four && DeckLayout::full_fits_width(decks_w) {
        let h = room - DeckLayout::Full.roomy_h();
        if h >= LIBRARY_ROOMY_H {
            return (h - strip).clamp(LIBRARY_MIN_H, LIBRARY_H);
        }
    }
    let layout = if four {
        DeckLayout::Four
    } else {
        DeckLayout::Compact
    };
    (room - layout.roomy_h() - strip).clamp(LIBRARY_MIN_H, LIBRARY_H)
}

/// The most the library can be dragged up to, leaving the decks area its
/// least (waveforms and the FX strip included).
pub(crate) fn library_max_height(room: f32, decks_w: f32, four: bool, strip: f32) -> f32 {
    let layout = DeckLayout::pick(four, decks_w, 0.0, strip);
    (room - layout.least_h(strip)).max(LIBRARY_MIN_H)
}

impl TandemApp {
    pub(crate) fn library_panel(&mut self, ctx: &egui::Context, view: &SessionView) {
        if let Some((to, d)) = self.ui.dropped.take() {
            self.drop_track(to, d);
        }
        if egui::DragAndDrop::has_payload_of_type::<DragTracks>(ctx) {
            // A crate that won't take it says so with another one.
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }
        let id = egui::Id::new("library");
        let avail = ctx.available_rect();
        let (room, decks_w) = (
            avail.height() - CENTRAL_MARGIN,
            avail.width() - CENTRAL_MARGIN,
        );
        let four = self.settings.four_decks;
        // The FX strip, open if the DJ has it open and the window has room.
        self.ui.fx_strip = FxStrip::pick(self.settings.fx_strip, room, decks_w, four);
        let strip = self.ui.fx_strip.height(four);
        let max = library_max_height(room, decks_w, four, strip);
        let auto = library_height(room, decks_w, four, strip).min(max);
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
        // A track being dragged: its title follows the pointer.
        if let (Some(d), Some(at)) = (
            egui::DragAndDrop::payload::<DragTracks>(ctx),
            ctx.pointer_latest_pos(),
        ) {
            drag_label(ctx, &d, at);
        }
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

                if !view.room_crates.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("SHARED LIVE IN THIS ROOM").small().weak());
                    for c in &view.room_crates {
                        let src = Source::Live {
                            owner: c.owner,
                            id: c.crate_id,
                        };
                        let on = self.ui.source == src;
                        let text = RichText::new(format!(
                            "📡 {} · {} ({})",
                            c.name,
                            c.owner_name,
                            c.tracks.len()
                        ))
                        .color(LIVE);
                        let r = ui.selectable_label(on, text).on_hover_text(format!(
                            "{} shares this list live. Load any track from it and it comes over from {}'s PC, like any track a DJ loads, and goes when Tandem closes.",
                            c.owner_name, c.owner_name
                        ));
                        follow(&r, on && knob);
                        if r.clicked() {
                            pick = Some(src);
                        }
                    }
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
                let mut resync = false;
                ui.horizontal_wrapped(|ui| {
                    if ui.button("➕ Add music folder…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new()
                            .set_title("Add a music folder")
                            .pick_folder()
                        {
                            self.add_folder(dir);
                        }
                    }
                    let busy = library::resyncing(&self.library);
                    let r = ui
                        .add_enabled(!busy, egui::Button::new("⟳ Resync folders"))
                        .on_hover_text(
                            "Look for songs added, changed or removed since Tandem started: in your folders, Shared with me, your crates and Serato's.",
                        );
                    if busy {
                        ui.spinner();
                    }
                    resync |= r.clicked();
                });

                ui.add_space(6.0);
                self.my_crates_section(ui, view, knob, &mut pick);

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
                    if ui
                        .small_button("Rescan crates")
                        .on_hover_text("Look again for Serato crates and what's in them (and your folders).")
                        .clicked()
                    {
                        resync = true;
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
                if resync {
                    self.resync();
                }
                if let Some(src) = pick {
                    // Picking a list goes into it, like pressing the knob
                    // on it: the knob moves through its tracks.
                    self.set_source(src);
                    self.ui.browse_focus = BrowseFocus::Tracks;
                }
                if egui::DragAndDrop::has_payload_of_type::<DragTracks>(ui.ctx()) {
                    drag_scroll(ui);
                }
            });
    }

    /// MY CRATES: the crates you made, with ➕ New crate. Tracks dragged
    /// onto one go in it; right-click one to rename, share or delete it.
    fn my_crates_section(
        &mut self,
        ui: &mut egui::Ui,
        view: &SessionView,
        knob: bool,
        pick: &mut Option<Source>,
    ) {
        let mine = self.library.lock().my_crates.clone();
        let in_room = matches!(
            view.link.role,
            tandem_core::session::RoleView::Hosting { .. }
                | tandem_core::session::RoleView::Joined { .. }
        );
        let mut dropped: Option<(Option<u64>, DragTracks)> = None;
        let mut share: Option<Source> = None;
        let mut unshare: Option<String> = None;
        let dragging = egui::DragAndDrop::has_payload_of_type::<DragTracks>(ui.ctx());
        let section = egui::CollapsingHeader::new(
            RichText::new(format!("MY CRATES ({})", mine.len()))
                .small()
                .weak(),
        )
        .id_salt("my crates")
        .default_open(true)
        .open(spring_open(
            dragging,
            self.ui.mine_open,
            &mut self.ui.mine_sprung,
        ))
        .show(ui, |ui| {
            if mine.is_empty() {
                ui.label(
                    RichText::new(
                        "Make a crate, then drag tracks onto it (or a deck's waveform, or right-click a track).",
                    )
                    .small()
                    .weak(),
                );
            }
            for c in &mine {
                let src = Source::Mine(c.id);
                let key = src.key();
                let live = view.live.iter().find(|l| l.key == key);
                let on = self.ui.source == src;
                let mut text = RichText::new(format!("📦 {} ({})", c.name, c.tracks.len()));
                if live.is_some() {
                    text = RichText::new(format!("📦 {} ({}) 📡", c.name, c.tracks.len()));
                }
                let mut r = ui.selectable_label(on, text);
                if live.is_some() {
                    r = r.on_hover_text(
                        "Shared live with the room: the other DJs see it and can load from it.",
                    );
                }
                follow(&r, on && knob);
                if r.clicked() {
                    *pick = Some(src.clone());
                }
                if let Some(d) = crate_drop(ui, &r) {
                    dropped = Some((Some(c.id), d));
                }
                r.context_menu(|ui| {
                    if ui.button("Rename…").clicked() {
                        self.ui.crate_dialog = Some(CrateDialog {
                            mode: CrateMode::Rename(c.id),
                            name: c.name.clone(),
                            add: Vec::new(),
                            focus: true,
                        });
                        ui.close_menu();
                    }
                    if live.is_some() {
                        if ui.button("Stop sharing live").clicked() {
                            unshare = Some(key.clone());
                            ui.close_menu();
                        }
                    } else {
                        let r =
                            ui.add_enabled(in_room, egui::Button::new("Share live with the room…"));
                        if r.on_disabled_hover_text("Host or join a room first")
                            .clicked()
                        {
                            share = Some(src.clone());
                            ui.close_menu();
                        }
                    }
                    if ui.button("Delete…").clicked() {
                        self.ui.crate_dialog = Some(CrateDialog {
                            mode: CrateMode::Delete(c.id),
                            name: c.name.clone(),
                            add: Vec::new(),
                            focus: false,
                        });
                        ui.close_menu();
                    }
                });
            }
            let r = ui.small_button("➕ New crate").on_hover_text(
                "Make a crate of your own tracks. Drop a track here (or a deck's waveform) to start one with it.",
            );
            if let Some(d) = crate_drop(ui, &r) {
                dropped = Some((None, d));
            } else if r.clicked() {
                self.ui.crate_dialog = Some(CrateDialog {
                    mode: CrateMode::New,
                    name: String::new(),
                    add: Vec::new(),
                    focus: true,
                });
            }
        });
        // The knob only moves through crates you can see.
        self.ui.mine_open = section.body_returned.is_some();
        if let Some((to, d)) = dropped {
            self.drop_track(to, d);
        }
        if let Some(key) = unshare {
            self.session.lock().unshare_live(&key);
        }
        if let Some(src) = share {
            // Show it, so the share window has its tracks.
            if self.ui.source != src {
                self.set_source(src.clone());
            }
            self.open_share(&src, view);
        }
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
        let (scanning, error, count) = {
            let lib = self.library.lock();
            match lib.lists.get(&key) {
                Some(l) => (l.scanning, l.error.clone(), l.tracks.len()),
                None => (false, None, 0),
            }
        };
        let live = view.live.iter().find(|l| l.key == key).cloned();
        let room_crate = src.room_crate(view).cloned();

        ui.horizontal(|ui| {
            ui.label(RichText::new(self.list_label(&src, view)).strong());
            self.search_box(ui);
            ui.label("Load to");
            let spacing = ui.spacing().item_spacing.x;
            ui.spacing_mut().item_spacing.x = 3.0;
            for (d, &color) in DECK_COLORS.iter().enumerate().take(shown) {
                let letter = RichText::new(deck_letter(d).to_string())
                    .strong()
                    .color(color);
                let r = ui
                    .add(egui::Button::new(letter).min_size(vec2(24.0, 0.0)))
                    .on_hover_text(format!(
                        "Load the selected track onto deck {}. Right-click (or SHIFT + LOAD): eject the deck's track.",
                        deck_letter(d)
                    ));
                if r.clicked() {
                    if let Some(item) = self.item_at(self.ui.sel_index, view) {
                        self.load(d, item, view);
                    }
                }
                if r.secondary_clicked() {
                    self.pending.push(Action::Eject { deck: d as u8 });
                }
            }
            ui.spacing_mut().item_spacing.x = spacing;
            let in_room = matches!(
                view.link.role,
                tandem_core::session::RoleView::Hosting { .. }
                    | tandem_core::session::RoleView::Joined { .. }
            );
            if let Some(l) = &live {
                let text = if l.preparing {
                    "📡 Sharing live (preparing…)".to_string()
                } else {
                    format!("📡 Shared live ({})", l.tracks)
                };
                ui.label(RichText::new(text).color(LIVE)).on_hover_text(
                    "The other DJs see this list and can load from it. Changes you make to it reach them.",
                );
                if ui.button("Stop sharing").clicked() {
                    self.session.lock().unshare_live(&key);
                }
            } else if let Some(c) = &room_crate {
                ui.label(
                    RichText::new(format!("📡 from {}'s PC when loaded", c.owner_name))
                        .color(LIVE),
                )
                .on_hover_text(format!(
                    "{} shares this list live. A track comes over from their PC only when someone loads it, and goes when Tandem closes.",
                    c.owner_name
                ));
            } else if src.local() {
                let live_ok = in_room && src.live_shareable();
                let can = (view.link.linked() || live_ok)
                    && (count > 0 || matches!(src, Source::Mine(_)))
                    && view.preparing_share.is_none();
                let r = ui.add_enabled(can, egui::Button::new("Share…"));
                let r = if !in_room {
                    r.on_disabled_hover_text("Host or join a room first")
                } else {
                    r.on_hover_text(
                        "Share it live (the others load from it, nothing is copied until they do) or send copies to keep.",
                    )
                };
                if r.clicked() {
                    self.open_share(&src, view);
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
                None if src.local() && !scanning && count > 0 => {
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
                Source::Live { .. } if room_crate.is_none() => "No longer shared.",
                _ if scanning => "Scanning…",
                _ if !self.ui.search.is_empty() => "No matches.",
                Source::All => "No music yet. Add a music folder on the left.",
                Source::Mine(_) => {
                    "Nothing in this crate yet: drag tracks onto it on the left, or right-click a track → Add to crate."
                }
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
        let mut to_crate: Option<(Option<u64>, PathBuf)> = None;
        let mut out_of_crate: Option<PathBuf> = None;
        let mine = lib.my_crates.clone();
        let room = src.room_tracks(view);
        // Tracks on this computer can be dragged onto a crate.
        let sense = if src.local() {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        scroll.show_rows(ui, row_h, n, |ui, range| {
            for i in range {
                let Some(&idx) = self.ui.filtered.get(i) else {
                    continue;
                };
                let file = list
                    .and_then(|l| l.tracks.get(idx))
                    .filter(|_| room.is_none());
                let row = match room {
                    Some(tracks) => tracks.get(idx).map(|t| Row {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        bpm: room_bpm(t),
                        key: t.key,
                        duration: t.duration,
                    }),
                    None => file.map(|t| Row {
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        bpm: lib_bpm(t),
                        key: t.key,
                        duration: t.duration,
                    }),
                };
                let Some(row) = row else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), sense);
                if let (Some(t), true) = (file, resp.drag_started()) {
                    resp.dnd_set_drag_payload(DragTracks::row(t.path.clone(), row.title.clone()));
                }
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
                    let Some(t) = file else { return };
                    ui.separator();
                    ui.menu_button("Add to crate", |ui| {
                        for c in mine.iter().filter(|c| src != Source::Mine(c.id)) {
                            if ui.button(format!("📦 {}", c.name)).clicked() {
                                to_crate = Some((Some(c.id), t.path.clone()));
                                ui.close_menu();
                            }
                        }
                        if ui.button("➕ New crate…").clicked() {
                            to_crate = Some((None, t.path.clone()));
                            ui.close_menu();
                        }
                    });
                    if let Source::Mine(_) = src {
                        if ui.button("Remove from this crate").clicked() {
                            out_of_crate = Some(t.path.clone());
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        drop(lib);
        match to_crate {
            Some((Some(id), path)) => self.add_to_crate(id, vec![path]),
            Some((None, path)) => {
                self.ui.crate_dialog = Some(CrateDialog {
                    mode: CrateMode::New,
                    name: String::new(),
                    add: vec![path],
                    focus: true,
                })
            }
            None => {}
        }
        if let (Some(path), Source::Mine(id)) = (out_of_crate, &src) {
            library::remove_from_crate(&self.library, *id, &[path]);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::NotYours;
    use std::sync::Arc;

    /// A track row dragged onto a crate on the left lands there when it's
    /// let go: the row hands over its file as it starts moving, and the
    /// crate takes it on release (egui clears it only after that frame).
    #[test]
    fn a_track_dragged_onto_a_crate_lands_in_it() {
        let ctx = egui::Context::default();
        let row_at = pos2(300.0, 18.0);
        let crate_at = pos2(40.0, 20.0);
        let mut got = None;
        let frame = |events: Vec<egui::Event>, got: &mut Option<DragTracks>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(600.0, 200.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let target = ui.put(
                        egui::Rect::from_center_size(crate_at, vec2(60.0, 20.0)),
                        egui::Button::new("📦 Gig"),
                    );
                    if let Some(d) = crate_drop(ui, &target) {
                        *got = Some(d);
                    }
                    // In a scrolling list, as the track rows are.
                    let list = egui::Rect::from_min_size(pos2(200.0, 10.0), vec2(200.0, 100.0));
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list), |ui| {
                        egui::ScrollArea::vertical().show_rows(ui, 20.0, 50, |ui, range| {
                            for i in range {
                                let resp = ui
                                    .allocate_exact_size(vec2(200.0, 20.0), Sense::click_and_drag())
                                    .1;
                                if i == 0 && resp.drag_started() {
                                    resp.dnd_set_drag_payload(DragTracks::row(
                                        PathBuf::from("a.mp3"),
                                        "A".into(),
                                    ));
                                }
                            }
                        });
                    });
                });
            });
        };
        let moved = |p: egui::Pos2| egui::Event::PointerMoved(p);
        let button = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(vec![moved(row_at)], &mut got);
        frame(vec![button(row_at, true)], &mut got);
        for x in [290.0, 250.0, 150.0, 60.0, 40.0] {
            frame(vec![moved(pos2(x, 20.0))], &mut got);
        }
        assert!(got.is_none(), "dropped before it was let go");
        frame(vec![button(crate_at, false)], &mut got);
        let d = got.take().expect("the crate didn't get the track");
        assert_eq!(d.file_for(&[]), Some(Ok(&PathBuf::from("a.mp3"))));
        // Nothing left to drop afterwards.
        frame(vec![moved(crate_at)], &mut got);
        assert!(egui::DragAndDrop::payload::<DragTracks>(&ctx).is_none());
    }

    const CRATE: egui::Pos2 = pos2(60.0, 350.0);
    const OVERVIEW: egui::Rect = egui::Rect::from_min_max(pos2(100.0, 20.0), pos2(500.0, 60.0));
    const WAVE: egui::Rect = egui::Rect::from_min_max(pos2(20.0, 150.0), pos2(580.0, 210.0));
    /// On the waveform, like the FX chip.
    const CHIP: egui::Rect = egui::Rect::from_min_max(pos2(530.0, 153.0), pos2(570.0, 170.0));

    /// A crate, and a deck's overview and scrolling waveform (with a chip on
    /// it), drawn in the app's order. The deck has a 100 s track.
    struct Booth {
        ctx: egui::Context,
        /// The DJ's own files of the deck's track: none, and it's Bob's.
        files: Vec<PathBuf>,
        /// The broadcast window's overview, which does nothing.
        seekable: bool,
        seeks: Vec<f64>,
        dropped: Vec<DragTracks>,
        chip_clicks: usize,
        /// The crate was outlined, and the pointer said no, last frame.
        lit: bool,
        no: bool,
    }

    impl Booth {
        fn new(files: &[&str]) -> Self {
            let mut b = Self {
                ctx: egui::Context::default(),
                files: files.iter().map(PathBuf::from).collect(),
                seekable: true,
                seeks: Vec::new(),
                dropped: Vec::new(),
                chip_clicks: 0,
                lit: false,
                no: false,
            };
            b.frame(vec![]);
            b
        }

        fn frame(&mut self, events: Vec<egui::Event>) {
            use crate::ui_decks::{overview, wave_dragged, OverviewUse};
            use tandem_core::model::{DeckState, TrackId};
            let dv = tandem_core::session::DeckView {
                state: DeckState {
                    track: Some(TrackId([1; 32])),
                    duration: 100.0,
                    ..Default::default()
                },
                meta: None,
                status: None,
                audio: None,
                loading: None,
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(600.0, 400.0),
                )),
                events,
                ..Default::default()
            };
            let ctx = self.ctx.clone();
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let gig = egui::Rect::from_center_size(CRATE, vec2(80.0, 20.0));
                    let gig = ui.put(gig, egui::Button::new("📦 Gig"));
                    self.dropped.extend(crate_drop(ui, &gig));
                    let mut carry = false;
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(OVERVIEW), |ui| {
                        match overview(ui, &dv, 0.0, LIVE, false, 40.0, self.seekable) {
                            Some(OverviewUse::Seek(t)) => self.seeks.push(t),
                            Some(OverviewUse::Carry) => carry = true,
                            None => {}
                        }
                    });
                    carry |= wave_dragged(ui, 0, WAVE);
                    if ui
                        .interact(CHIP, ui.id().with("chip"), Sense::click())
                        .clicked()
                    {
                        self.chip_clicks += 1;
                    }
                    if carry {
                        let drag = DragTracks {
                            title: "A".into(),
                            files: Arc::new(self.files.clone().into()),
                            owner: Some("Bob".into()),
                        };
                        egui::DragAndDrop::set_payload(ctx, drag);
                    }
                });
            });
            self.no = out.platform_output.cursor_icon == egui::CursorIcon::NotAllowed;
            self.lit = out.shapes.iter().any(|s| match &s.shape {
                egui::Shape::Rect(r) => r.stroke.color == LIVE,
                _ => false,
            });
        }

        fn carrying(&self) -> bool {
            egui::DragAndDrop::has_any_payload(&self.ctx)
        }

        /// Press at `from` and move to each of `to` in turn, a frame each.
        fn drag(&mut self, from: egui::Pos2, to: &[egui::Pos2]) {
            self.frame(vec![egui::Event::PointerMoved(from)]);
            self.frame(vec![button(from, true)]);
            for p in to {
                self.frame(vec![egui::Event::PointerMoved(*p)]);
            }
        }

        fn let_go(&mut self, at: egui::Pos2) {
            self.frame(vec![button(at, false)]);
            self.frame(vec![]);
        }

        fn escape(&mut self) {
            self.frame(vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }]);
            self.frame(vec![]);
        }

        /// What happened since the last call: seeks, and files dropped on
        /// the crate.
        fn took(&mut self) -> (Vec<f64>, Vec<PathBuf>) {
            let files = (std::mem::take(&mut self.dropped).iter())
                .map(|d| d.file_for(&[]).unwrap().unwrap().clone())
                .collect();
            (std::mem::take(&mut self.seeks), files)
        }
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    const NOTHING: (Vec<f64>, Vec<PathBuf>) = (Vec::new(), Vec::new());

    #[test]
    fn a_deck_overview_still_seeks() {
        let mut b = Booth::new(&["a.mp3"]);
        // A click.
        b.drag(pos2(200.0, 40.0), &[]);
        assert!(b.seeks.is_empty(), "seeked before it was let go");
        b.let_go(pos2(200.0, 40.0));
        assert_eq!(b.took(), (vec![25.0], vec![]));
        // A drag along it: once, where it's let go, with nothing carried.
        b.drag(
            pos2(200.0, 40.0),
            &[pos2(230.0, 40.0), pos2(280.0, 45.0), pos2(300.0, 50.0)],
        );
        assert!(b.seeks.is_empty() && !b.carrying());
        b.let_go(pos2(300.0, 50.0));
        assert_eq!(b.took(), (vec![50.0], vec![]));
        // A bit below it, or past its end, is still on it.
        b.drag(pos2(200.0, 40.0), &[pos2(300.0, 80.0)]);
        b.let_go(pos2(300.0, 80.0));
        assert_eq!(b.took(), (vec![50.0], vec![]));
        b.drag(pos2(200.0, 40.0), &[pos2(300.0, 40.0), pos2(540.0, 40.0)]);
        b.let_go(pos2(540.0, 40.0));
        assert_eq!(b.took(), (vec![100.0], vec![]));
        // The broadcast window's does nothing.
        b.seekable = false;
        b.drag(pos2(200.0, 40.0), &[pos2(200.0, 250.0), CRATE]);
        b.let_go(CRATE);
        b.drag(pos2(200.0, 40.0), &[]);
        b.let_go(pos2(200.0, 40.0));
        assert_eq!(b.took(), NOTHING);
    }

    /// Dragged out of its overview, a deck's track goes to the crate it's
    /// dropped on, or nowhere, and the deck never jumps.
    #[test]
    fn a_track_dragged_off_an_overview_lands_in_a_crate_and_never_seeks() {
        let mut b = Booth::new(&["a.mp3"]);
        let start = pos2(200.0, 40.0);
        let out = [pos2(195.0, 70.0), pos2(150.0, 200.0), pos2(80.0, 330.0)];
        b.drag(start, &out);
        assert!(b.carrying() && !b.lit);
        b.frame(vec![egui::Event::PointerMoved(CRATE)]);
        b.frame(vec![]);
        assert!(b.lit && !b.no, "the crate didn't light up");
        assert_eq!(b.took(), NOTHING);
        b.let_go(CRATE);
        assert_eq!(b.took(), (vec![], vec![PathBuf::from("a.mp3")]));
        assert!(!b.carrying() && !b.lit);

        // Let go anywhere else, nothing happens, nor the next time round.
        b.drag(start, &out);
        b.let_go(pos2(80.0, 300.0));
        assert_eq!(b.took(), NOTHING);
        assert!(!b.carrying());
        // Not even back where it came from.
        b.drag(start, &[pos2(200.0, 120.0), pos2(300.0, 40.0)]);
        assert!(b.carrying());
        b.let_go(pos2(300.0, 40.0));
        assert_eq!(b.took(), NOTHING);
        // Nor flicked out of it within a frame.
        b.drag(start, &[]);
        b.let_go(pos2(200.0, 250.0));
        assert_eq!(b.took(), NOTHING);
        // And the next click seeks.
        b.drag(start, &[]);
        b.let_go(start);
        assert_eq!(b.took(), (vec![25.0], vec![]));
    }

    #[test]
    fn escape_cancels_a_drag_on_a_deck() {
        let mut b = Booth::new(&["a.mp3"]);
        // One that would have been a seek.
        b.drag(pos2(200.0, 40.0), &[pos2(260.0, 40.0)]);
        b.escape();
        b.let_go(pos2(260.0, 40.0));
        assert_eq!(b.took(), NOTHING);
        // One carrying the track, from either waveform.
        for from in [pos2(200.0, 40.0), pos2(300.0, 180.0)] {
            b.drag(from, &[pos2(200.0, 250.0)]);
            assert!(b.carrying());
            b.escape();
            assert!(!b.carrying());
            b.frame(vec![egui::Event::PointerMoved(pos2(100.0, 300.0))]);
            b.frame(vec![egui::Event::PointerMoved(CRATE)]);
            assert!(!b.carrying() && !b.lit, "it was picked up again");
            b.let_go(CRATE);
            assert_eq!(b.took(), NOTHING);
        }
    }

    #[test]
    fn a_scrolling_waveform_carries_its_track_and_nothing_else() {
        let mut b = Booth::new(&["a.mp3"]);
        let on = pos2(300.0, 180.0);
        // A click, however slow or shaky, does nothing.
        b.drag(on, &[pos2(304.0, 183.0)]);
        for _ in 0..90 {
            b.frame(vec![]);
        }
        assert!(!b.carrying());
        b.let_go(pos2(304.0, 183.0));
        assert_eq!(b.took(), NOTHING);
        // Dragged along itself and let go: nothing.
        b.drag(on, &[pos2(340.0, 180.0)]);
        assert!(b.carrying());
        b.let_go(pos2(340.0, 180.0));
        assert_eq!(b.took(), NOTHING);
        // Onto a crate.
        b.drag(on, &[pos2(290.0, 220.0), pos2(200.0, 300.0), CRATE]);
        b.frame(vec![]);
        assert!(b.lit);
        b.let_go(CRATE);
        assert_eq!(b.took(), (vec![], vec![PathBuf::from("a.mp3")]));
        // The chip on it is still clicked, and beside it too, as before.
        for at in [CHIP.center(), pos2(CHIP.left() - 2.0, CHIP.center().y)] {
            b.chip_clicks = 0;
            b.drag(at, &[]);
            b.let_go(at);
            assert_eq!(b.chip_clicks, 1, "at {at:?}");
        }
        assert_eq!(b.took(), NOTHING);
    }

    /// Only a session copy here: the crate doesn't light up, the pointer
    /// says no, and dropping it adds nothing.
    #[test]
    fn a_track_that_isnt_the_djs_own_goes_in_no_crate() {
        let mut b = Booth::new(&[]);
        for from in [pos2(200.0, 40.0), pos2(300.0, 180.0)] {
            b.drag(from, &[pos2(200.0, 250.0), CRATE]);
            b.frame(vec![]);
            assert!(b.carrying() && b.no && !b.lit);
            b.let_go(CRATE);
            let d = b.dropped.pop().expect("the crate heard nothing");
            let no = d.file_for(&[]).unwrap().unwrap_err();
            assert_eq!(no, NotYours(Some("Bob".into())));
            assert_eq!(
                no.notice(),
                "That's Bob's track: ask them to send you a copy (Share…, then Send copies to keep)."
            );
            assert_eq!(b.took(), NOTHING);
        }
        assert!(NotYours(None)
            .notice()
            .starts_with("That's another DJ's track: ask"));
    }

    #[test]
    fn a_dropped_track_goes_in_as_the_file_the_crate_has() {
        let path = |p: &str| PathBuf::from(p);
        let d = |files: &[&str]| DragTracks {
            title: "A".into(),
            files: Arc::new(files.iter().map(PathBuf::from).collect::<Vec<_>>().into()),
            owner: None,
        };
        let two = d(&["best.mp3", "other.mp3"]);
        assert_eq!(two.file_for(&[]), Some(Ok(&path("best.mp3"))));
        // The crate has the same song from somewhere else: "already in it".
        let have = [path("x.mp3"), path("other.mp3")];
        assert_eq!(two.file_for(&have), Some(Ok(&path("other.mp3"))));
        let none = d(&[]);
        assert_eq!(none.file_for(&have), Some(Err(NotYours(None))));
        // Still being looked for.
        let files = Arc::default();
        let (title, owner) = ("A".into(), None);
        let waiting = DragTracks {
            title,
            files,
            owner,
        };
        assert_eq!(waiting.file_for(&have), None);
        waiting.files.set(vec![path("late.mp3")]).unwrap();
        assert_eq!(waiting.file_for(&[]), Some(Ok(&path("late.mp3"))));
    }

    #[test]
    fn my_crates_opens_while_a_track_is_dragged() {
        for default_open in [false, true] {
            let ctx = egui::Context::default();
            let (mut open, mut sprung) = (default_open, false);
            // Half a second, for the section to finish opening or closing.
            let mut run = |dragging: bool| {
                for _ in 0..30 {
                    let _ = ctx.run(egui::RawInput::default(), |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let section = egui::CollapsingHeader::new("MY CRATES")
                                .default_open(default_open)
                                .open(spring_open(dragging, open, &mut sprung))
                                .show(ui, |ui| ui.label("📦 Gig"));
                            open = section.body_returned.is_some();
                        });
                    });
                }
                open
            };
            // Open for the drag, and after it as it was before.
            for _ in 0..2 {
                assert_eq!(run(false), default_open);
                assert!(run(true));
            }
            assert_eq!(run(false), default_open);
        }
    }

    /// Holding a dragged track by the bottom of the source list brings the
    /// rest of it up, as far as it goes, and by the top brings it back.
    #[test]
    fn the_source_list_scrolls_under_a_dragged_track() {
        let ctx = egui::Context::default();
        let list = egui::Rect::from_min_size(pos2(20.0, 20.0), vec2(150.0, 200.0));
        let scroll_to = |p: egui::Pos2| {
            let mut offsets = vec![0.0f32];
            for _ in 0..120 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        pos2(0.0, 0.0),
                        vec2(400.0, 300.0),
                    )),
                    events: vec![egui::Event::PointerMoved(p)],
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list), |ui| {
                            let out = egui::ScrollArea::vertical()
                                .max_height(200.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.allocate_exact_size(vec2(100.0, 500.0), Sense::hover());
                                    drag_scroll(ui);
                                });
                            offsets.push(out.state.offset.y);
                        });
                    });
                });
            }
            offsets
        };
        assert!(scroll_to(list.center()).iter().all(|o| *o == 0.0));
        let down = scroll_to(pos2(60.0, 218.0));
        let end = *down.last().unwrap();
        assert!((end - 300.0).abs() < 1.0, "{end}");
        assert!(down.iter().all(|o| *o < 301.0), "past the end: {down:?}");
        let up = scroll_to(pos2(60.0, 22.0));
        assert_eq!(*up.last().unwrap(), 0.0);
    }
}
