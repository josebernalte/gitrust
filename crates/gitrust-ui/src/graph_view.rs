//! Painting the commit graph.
//!
//! It draws only the visible rows. In a repository of tens of thousands of
//! commits the cost per frame depends on the height of the window, not on the
//! size of the history.
//!
//! The row is the unit of reading: the branches and tags in their column at
//! the far left, then the lanes, then the message at full contrast, and
//! author, date and hash aligned right in fixed-width columns. It is those
//! columns that give the structure, so not one vertical line is needed. As
//! the window narrows the columns withdraw in order of importance instead of
//! colliding with each other.

use chrono::{Local, TimeZone};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Shape, Stroke,
};
use gitrust_core::{Graph, Oid, RefEntry, RefKind, Row, WorkingState};

use crate::refs_view::{self, Named, On, Pick};
use crate::ripple;

use crate::theme;

const LANE_W: f32 = 17.0;
const DOT_R: f32 = 4.5;
const LINE_W: f32 = 2.0;
const GUTTER: f32 = 12.0;

/// The right-hand columns, from the outside in.
const PAD_R: f32 = 14.0;
const HASH_W: f32 = 58.0;
const DATE_W: f32 = 100.0;
const AUTHOR_W: f32 = 116.0;
const COL_GAP: f32 = 16.0;
/// Below this the message is cut so short that the column gets in the way
/// more than it informs, and it withdraws.
const SUMMARY_MIN: f32 = 200.0;
/// The most lanes that are set aside before the message's column.
///
/// A repository can have twenty live branches at once somewhere in its
/// history, but setting room aside for all twenty leaves an empty strip in
/// the thousands of rows where there are only two. Eight covers what is seen
/// in practice; anything past that is clipped against the message's column.
const MAX_LANES: usize = 8;
/// The stroke of the line coming down from the uncommitted changes: dash and gap.
const DASH: (f32, f32) = (3.0, 3.5);

/// The references column: air against the border, a cap and a gap between
/// chips, and what is set aside for the count of the ones that do not fit.
const REFS_PAD: f32 = 10.0;
const REFS_MAX: f32 = 236.0;
const BADGE_PAD: f32 = 6.0;
const BADGE_GAP: f32 = 5.0;
const MORE_W: f32 = 26.0;

/// What has happened in the search bar.
pub enum Found {
    /// The needle has changed: everything has to be counted again.
    Changed,
    Next,
    Prev,
    Nothing,
}

/// The search bar, above the graph.
///
/// It does not filter: it leaves the whole history where it is and dims what
/// does not match. A graph with rows taken out stops being a graph -- the
/// lanes would lead nowhere -- and what you are looking for you nearly always
/// want to see in its place, with what was around it.
pub fn finder(ui: &mut egui::Ui, needle: &mut String, found: usize, at: usize) -> Found {
    let mut event = Found::Nothing;

    ui.horizontal(|ui| {
        ui.add_space(2.0);
        let entry = ui.add(
            egui::TextEdit::singleline(needle)
                .desired_width(260.0)
                .margin(egui::vec2(8.0, 5.0))
                .font(theme::body(12.0))
                .hint_text(RichText::new("Find: message, author or hash").color(theme::FAINT)),
        );

        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            entry.request_focus();
        }
        if entry.changed() {
            event = Found::Changed;
        }

        // Enter jumps to the next one; with Shift, to the one before. It is
        // what every search bar anyone has used does.
        if entry.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            event = if ui.input(|i| i.modifiers.shift) { Found::Prev } else { Found::Next };
        }

        if needle.trim().is_empty() {
            return;
        }

        ui.add_space(4.0);
        let (text, color) = match found {
            0 => ("no matches".to_string(), theme::MUTED),
            n => (format!("{} of {n}", at + 1), theme::FAINT),
        };
        ui.label(RichText::new(text).font(theme::mono(11.0)).color(color));

        if found > 0 {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (glyph, hint, what) in [
                ("↑", "Previous (Shift+Enter)", Found::Prev),
                ("↓", "Next (Enter)", Found::Next),
            ] {
                if ui
                    .add(
                        egui::Button::new(RichText::new(glyph).font(theme::body(12.0)))
                            .min_size(egui::vec2(24.0, 22.0)),
                    )
                    .on_hover_text(hint)
                    .clicked()
                {
                    event = what;
                }
            }
        }
    });

    event
}

/// Gives back the commit that was clicked, if any.
///
/// The working tree's state is needed to label the uncommitted-changes row,
/// when the graph carries it. `needle` dims what does not match, and `bring`
/// brings a row into view in one jump.
/// What the graph was asked for: a commit to show, and whatever its menu
/// wanted done.
#[derive(Default)]
pub struct Asked {
    pub show: Option<Oid>,
    pub pick: Option<Pick>,
}

/// A chip as it was actually drawn.
///
/// The graph is painted, not built out of widgets: one rectangle takes the
/// clicks and the row is worked out from where in it the pointer was. That
/// is right for rows, which are a stack of equal bands, and no use at all
/// for chips, which sit at whatever width their name needs. So each one
/// says where it ended up, and a click is matched against the list.
///
/// Only the ones that were drawn are in here. A chip scrolled off the top
/// or elided down to "+3" cannot be double-clicked, which is correct: you
/// cannot double-click what you cannot see.
struct Chip {
    rect: Rect,
    name: String,
    kind: RefKind,
    /// The branch HEAD is on. There is nowhere to switch to from it.
    current: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    graph: &Graph,
    refs: &[RefEntry],
    selected: Option<Oid>,
    work: &WorkingState,
    needle: &str,
    bring: Option<usize>,
    head: Option<&str>,
    busy: bool,
    remote_checkout: bool,
    // Whether the pointer leaves a wave behind. Off, the graph is drawn the
    // way it was before there was one: straight lanes, a flat band under the
    // row, and no frames asked for.
    waves: bool,
) -> Asked {
    if graph.rows.is_empty() {
        theme::empty(ui, "Open a repository to see its history");
        return Asked::default();
    }

    let mut picked = None;
    let on_remotes: Vec<&str> = refs
        .iter()
        .filter(|entry| entry.kind == RefKind::RemoteBranch)
        .map(|entry| entry.name.as_str())
        .collect();

    // How far the uncommitted changes' line comes down: from its row to
    // HEAD's, which is its parent. Whatever passes through that lane before
    // it gets there is still that same line, and is drawn dashed.
    let pending = graph.rows.first().filter(|row| row.commit.is_working()).map(|row| {
        let head = row.commit.parents[0];
        let until = graph.rows.iter().position(|r| r.commit.id == head).unwrap_or(0);
        (row.lane, until)
    });
    let pending_label = theme::pending_label(work.staged.len(), work.unstaged.len());

    // With a heavily branched history the lanes would eat the message's
    // column, so past a certain point they are allowed to overlap.
    let lanes = graph.lane_count.clamp(1, MAX_LANES);
    let graph_w = GUTTER * 2.0 + lanes as f32 * LANE_W;
    let mut clicked = None;
    let mut chips: Vec<Chip> = Vec::new();

    egui::ScrollArea::vertical()
        .id_salt("graph")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let row_h = theme::ROW_H;
            let total_h = graph.rows.len() as f32 * row_h;
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), total_h),
                egui::Sense::click(),
            );
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, theme::CANVAS);

            // The branch column is measured before anything else: where
            // everything else starts depends on its width.
            let refs_w = refs_width(ui, graph, rect.width());
            let lanes_left = rect.left() + refs_w;

            // The lanes are painted against a strip of their own: that way a
            // history wider than the cap is cut off sharply instead of running
            // underneath the messages.
            let lanes_painter = painter.with_clip_rect(Rect::from_min_max(
                egui::pos2(lanes_left, rect.min.y),
                egui::pos2(lanes_left + graph_w, rect.max.y),
            ));

            // Only the rows that fall inside the visible clip.
            let clip = ui.clip_rect();
            let first = (((clip.top() - rect.top()) / row_h).floor() as isize).max(0) as usize;
            let last = ((((clip.bottom() - rect.top()) / row_h).ceil() as usize) + 1)
                .min(graph.rows.len());

            let row_at = |y: f32| {
                let index = ((y - rect.top()) / row_h).floor();
                (index >= 0.0 && (index as usize) < graph.rows.len()).then(|| index as usize)
            };

            let hovered = response.hover_pos().and_then(|pos| row_at(pos.y));

            // Every tap still ringing, which is usually none and at most a
            // handful. Asked for once a frame, here, because it is also what
            // lands the new one. Turned off, nothing is asked for and nothing
            // is remembered, so the graph costs what it always did.
            let ringing = match waves {
                true => ripple::waves(ui, egui::Id::new("graph-waves"), hovered),
                false => Vec::new(),
            };

            // The menu is about the row the pointer was on when it opened,
            // not wherever it has wandered to since, so which row that was is
            // remembered rather than asked again each frame.
            let held = egui::Id::new("graph-menu-row");
            if response.secondary_clicked() {
                if let Some(index) = response.interact_pointer_pos().and_then(|p| row_at(p.y)) {
                    ui.data_mut(|data| data.insert_temp(held, index));
                }
            }
            if let Some(index) = ui
                .data(|data| data.get_temp::<usize>(held))
                .filter(|index| *index < graph.rows.len())
            {
                let commit = &graph.rows[index].commit;
                // The uncommitted row is not a commit and nothing in the menu
                // means anything for it.
                if !commit.is_working() {
                    let reference = named_of(&graph.rows[index], head);
                    let upstream = reference.as_ref().and_then(|named| {
                        on_remotes
                            .iter()
                            .find(|remote| {
                                remote
                                    .rsplit_once('/')
                                    .is_some_and(|(_, leaf)| leaf == named.name)
                            })
                            .map(|remote| remote.to_string())
                    });
                    let on = On { target: commit.id, reference };

                    response.context_menu(|ui| {
                        refs_view::menu(ui, &on, upstream.as_deref(), head, busy, remote_checkout, &mut picked);
                    });
                }
            }
            if response.clicked() {
                if let Some(index) = response.interact_pointer_pos().and_then(|p| row_at(p.y)) {
                    clicked = Some(graph.rows[index].commit.id);
                }
            }

            let lane_x = |lane: usize| lanes_left + GUTTER + lane as f32 * LANE_W + LANE_W / 2.0;
            let row_y = |row: usize| rect.top() + row as f32 * row_h + row_h / 2.0;

            // Where each string was plucked: the lane that bends and the
            // height its bend runs away from. A tap on a row that is no
            // longer there -- the repository was read again under us -- is
            // dropped rather than carried as a lane it never belonged to.
            let struck: Vec<(usize, usize, f32, f32)> = ringing
                .iter()
                .filter(|(row, _)| *row < graph.rows.len())
                .map(|(row, age)| (*row, graph.rows[*row].lane, row_y(*row), *age))
                .collect();

            // The waves add up. Two taps a few rows apart leave a lane
            // carrying both, which is the whole point: it is one string, and
            // a string does not forget the first tap when it gets a second.
            let sway = |lane: usize, y: f32| {
                ripple::bound(
                    struck
                        .iter()
                        .filter(|(_, hit, _, _)| *hit == lane)
                        .map(|(_, _, from, age)| ripple::sway(*age, (y - from).abs()))
                        .sum(),
                )
            };
            let bending = |lane: usize| struck.iter().any(|(_, hit, _, _)| *hit == lane);
            let lane_at = |lane: usize, y: f32| Pos2::new(lane_x(lane) + sway(lane, y), y);

            // And so does the shake of the dots, each tap reaching a few
            // rows either side of where it landed.
            let swell = |index: usize| {
                ripple::bound_swell(
                    struck
                        .iter()
                        .map(|(row, _, _, age)| match index.abs_diff(*row) {
                            away if away <= ripple::CARRIES => ripple::swell(*age, away),
                            _ => 0.0,
                        })
                        .sum(),
                )
            };

            let text_left = lanes_left + graph_w;
            let columns = Columns::for_width(text_left, rect.right());

            // Bringing a row into view is asked for in here, which is where
            // we know which pixel it falls on.
            if let Some(index) = bring.filter(|index| *index < graph.rows.len()) {
                let band = Rect::from_min_size(
                    egui::pos2(rect.left(), rect.top() + index as f32 * row_h),
                    egui::vec2(rect.width(), row_h),
                );
                ui.scroll_to_rect(band, Some(Align::Center));
            }

            let band_of = |index: usize| {
                Rect::from_min_size(
                    egui::pos2(rect.left(), rect.top() + index as f32 * row_h),
                    egui::vec2(rect.width(), row_h),
                )
            };

            // The backgrounds all go first. If each row painted its own just
            // before its lines, the band of the chosen row would cover half
            // the line coming down to it from the one above.
            for index in first..last {
                let row = &graph.rows[index];

                // The chosen row is marked with its own lane's color on the
                // left edge, not with a blue brought in from outside: that way
                // you see at a glance which branch what you are looking at is on.
                if selected == Some(row.commit.id) {
                    let band = band_of(index);
                    let edge = if row.commit.is_working() {
                        theme::MUTED
                    } else {
                        theme::lane(row.color)
                    };
                    painter.rect_filled(band, 0.0, theme::CHOSEN);
                    painter.rect_filled(
                        Rect::from_min_size(band.min, egui::vec2(2.0, band.height())),
                        0.0,
                        edge,
                    );
                } else if hovered == Some(index) {
                    let band = band_of(index);
                    let color = match row.commit.is_working() {
                        true => theme::MUTED,
                        false => theme::lane(row.color),
                    };
                    // The wash stays with the pointer rather than adding up:
                    // it is the background saying where you are, and two lit
                    // rows would be two answers to that. The newest tap is
                    // the one on this row; no tap at all means its wave is
                    // over, which is what an age past its life says.
                    let age = struck
                        .iter()
                        .rev()
                        .find(|(hit, _, _, _)| *hit == index)
                        .map_or(ripple::LIFE, |(_, _, _, age)| *age);
                    ripple::wash(&painter, band, lane_x(row.lane), age, color);
                }
            }

            for index in first..last {
                let row = &graph.rows[index];
                let band = band_of(index);

                // This row's lines come down to the next one.
                for link in &row.links {
                    // The one joining the changes to HEAD is dashed and has no
                    // color: it is not history yet, and must not read as a
                    // branch. Only the one running straight down that lane: a
                    // branch merging into HEAD also ends there, and that is history.
                    let dashed = pending.is_some_and(|(lane, until)| {
                        index < until && link.from == lane && link.to == lane
                    });
                    let stroke = if dashed {
                        Stroke::new(LINE_W, theme::MUTED)
                    } else {
                        Stroke::new(LINE_W, theme::lane(link.color))
                    };
                    let top = lane_at(link.from, row_y(index));
                    let bottom = lane_at(link.to, row_y(index + 1));

                    if dashed {
                        lanes_painter
                            .extend(Shape::dashed_line(&[top, bottom], stroke, DASH.0, DASH.1));
                    } else if link.from == link.to && bending(link.from) {
                        // The bend is sampled down the row instead of being
                        // one segment: a snake drawn a row at a time is a
                        // zigzag, because the swing between two rows is most
                        // of a wave and the straight line cuts the corner.
                        // The ends are the same points the rows above and
                        // below start from, so it joins up.
                        let joints = (0..=ripple::JOINTS).map(|joint| {
                            let share = joint as f32 / ripple::JOINTS as f32;
                            lane_at(link.from, top.y + (bottom.y - top.y) * share)
                        });
                        lanes_painter.add(Shape::line(joints.collect(), stroke));
                    } else if link.from == link.to {
                        lanes_painter.line_segment([top, bottom], stroke);
                    } else {
                        // An S-curve so that the jump between lanes reads well.
                        let c1 = Pos2::new(top.x, top.y + row_h * 0.5);
                        let c2 = Pos2::new(bottom.x, bottom.y - row_h * 0.5);
                        lanes_painter.add(egui::Shape::CubicBezier(
                            egui::epaint::CubicBezierShape::from_points_stroke(
                                [top, c1, c2, bottom],
                                false,
                                Color32::TRANSPARENT,
                                stroke,
                            ),
                        ));
                    }
                }

                // The dot rides the bend: it is a bead on the string, not a
                // mark pinned to the canvas behind it.
                let center = lane_at(row.lane, row_y(index));
                draw_commit(&lanes_painter, row, center, swell(index));

                // One set of rings per tap this row has had, so that running
                // the pointer down the graph leaves a trail of them going
                // out rather than one that keeps starting over. They go on
                // the unclipped painter: the lane strip is narrower than
                // their reach, and a ring cut off square at the edge of a
                // strip is not a ring.
                for (_, _, _, age) in struck.iter().filter(|(hit, _, _, _)| *hit == index) {
                    let color = match row.commit.is_working() {
                        true => theme::MUTED,
                        false => theme::lane(row.color),
                    };
                    ripple::rings(&painter, center, *age, color, DOT_R + 3.0);
                }

                if row.commit.is_working() {
                    draw_working(&painter, text_left, row_y(index), &pending_label);
                    // A hair makes clear where what is already written starts.
                    painter.hline(
                        band.x_range(),
                        band.bottom(),
                        Stroke::new(1.0, theme::HAIRLINE),
                    );
                } else {
                    // What does not match the search is dimmed, but stays: its
                    // lane is still needed in order to read the rest.
                    let dim = !needle.is_empty() && !row.commit.matches(needle);
                    draw_refs(
                        &painter,
                        ui,
                        row,
                        lanes_left,
                        rect.left() + 8.0,
                        row_y(index),
                        &mut chips,
                    );
                    draw_row(&painter, ui, row, text_left, row_y(index), &columns, dim);
                }
            }

            // After the rows, because until they are drawn there are no
            // chips to hit. The single click that came with the double has
            // already selected the commit by then, which is what you want:
            // you end up on the branch, looking at its newest commit.
            let under = |pos: Pos2| {
                chips.iter().find(|chip| chip.rect.contains(pos)).filter(|chip| {
                    !chip.current
                        && matches!(chip.kind, RefKind::LocalBranch | RefKind::RemoteBranch)
                })
            };

            // The pointer says so before anybody double-clicks anything.
            // Nothing else in the graph has a hand under it, so a hand here
            // means this one thing is more than a drawing.
            if !busy {
                if let Some(pos) = response.hover_pos() {
                    if under(pos).is_some() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            }

            if response.double_clicked() && !busy {
                if let Some(chip) = response.interact_pointer_pos().and_then(under) {
                    picked = Some(Pick::Switch {
                        name: chip.name.clone(),
                        remote: chip.kind == RefKind::RemoteBranch,
                    });
                }
            }
        });

    Asked { show: clicked, pick: picked }
}

/// `swell` is what the wave is doing to this dot right now, as a share of its
/// radius: zero when nothing is passing through it.
fn draw_commit(painter: &egui::Painter, row: &Row, center: Pos2, swell: f32) {
    let color = theme::lane(row.color);
    let dot_r = DOT_R * (1.0 + swell);

    // The uncommitted changes go hollow and grey: they are not a commit, and
    // giving them the branch's color would be saying they are already part of
    // it. The ring sits flush with the dot so as not to eat the dashes of its
    // line, which is short and has few to spare.
    if row.commit.is_working() {
        painter.circle_filled(center, dot_r + 1.0, theme::CANVAS);
        painter.circle_stroke(center, dot_r - 0.5, Stroke::new(1.6, theme::MUTED));
        return;
    }

    // A ring in the canvas's color lifts the dot off the lines passing behind
    // it; without one the graph smears at the junctions.
    painter.circle_filled(center, dot_r + 2.5, theme::CANVAS);

    // A merge is marked with a hollow dot; an ordinary commit, solid.
    if row.commit.parents.len() > 1 {
        painter.circle_filled(center, dot_r, theme::CANVAS);
        painter.circle_stroke(center, dot_r - 0.5, Stroke::new(2.0, color));
    } else {
        painter.circle_filled(center, dot_r, color);
    }

    // Where HEAD is can be seen from across the room.
    if row.commit.refs.iter().any(|r| r.kind == RefKind::Head) {
        painter.circle_stroke(center, dot_r + 3.5, Stroke::new(1.5, theme::HEAD));
    }
}

/// The uncommitted-changes row.
///
/// It carries no author, no date and no hash, because it has none of the
/// three yet. Instead it says how much there is on each side of the index,
/// which is the only thing that can be known about it.
fn draw_working(painter: &egui::Painter, left: f32, y: f32, pending: &str) {
    let title = painter.text(
        Pos2::new(left + 2.0, y),
        Align2::LEFT_CENTER,
        "Uncommitted changes",
        theme::medium(13.0),
        theme::TEXT,
    );

    if !pending.is_empty() {
        painter.text(
            Pos2::new(title.right() + 10.0, y),
            Align2::LEFT_CENTER,
            pending,
            theme::body(11.5),
            theme::MUTED,
        );
    }
}

/// Where each right-hand column falls at the width there is.
struct Columns {
    author: Option<f32>,
    date: Option<f32>,
    hash: Option<f32>,
    summary_right: f32,
}

impl Columns {
    fn for_width(left: f32, right: f32) -> Self {
        let mut edge = right - PAD_R;
        let mut columns = Self {
            author: None,
            date: None,
            hash: None,
            summary_right: right - PAD_R,
        };

        // From the outside in, and each one only if the message has room left.
        for (width, slot) in [
            (HASH_W, &mut columns.hash),
            (DATE_W, &mut columns.date),
            (AUTHOR_W, &mut columns.author),
        ] {
            if edge - width - COL_GAP - left < SUMMARY_MIN {
                break;
            }
            *slot = Some(edge);
            edge -= width + COL_GAP;
        }

        columns.summary_right = edge - 8.0;
        columns
    }
}

fn draw_row(
    painter: &egui::Painter,
    ui: &egui::Ui,
    row: &Row,
    left: f32,
    y: f32,
    columns: &Columns,
    dim: bool,
) {
    let x = left + 2.0;
    let summary_font = theme::body(13.0);
    let summary = elide(ui, &row.commit.summary, columns.summary_right - x, &summary_font);
    painter.text(
        Pos2::new(x, y),
        Align2::LEFT_CENTER,
        summary,
        summary_font,
        if dim { theme::FAINT } else { theme::TEXT },
    );

    if let Some(edge) = columns.author {
        let font = theme::body(11.5);
        painter.text(
            Pos2::new(edge, y),
            Align2::RIGHT_CENTER,
            elide(ui, &row.commit.author, AUTHOR_W, &font),
            font,
            if dim { theme::FAINT } else { theme::MUTED },
        );
    }

    if let Some(edge) = columns.date {
        let when = Local
            .timestamp_opt(row.commit.time, 0)
            .single()
            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        painter.text(
            Pos2::new(edge, y),
            Align2::RIGHT_CENTER,
            when,
            theme::mono(11.0),
            theme::FAINT,
        );
    }

    if let Some(edge) = columns.hash {
        painter.text(
            Pos2::new(edge, y),
            Align2::RIGHT_CENTER,
            &row.commit.short_id,
            theme::mono(11.0),
            theme::FAINT,
        );
    }
}

/// The width of the references column: whatever the fullest row asks for,
/// capped.
///
/// It is measured every frame, but only over the rows that carry any, which
/// in any repository are a few among thousands. The cap against the width
/// available is so that in a narrow window the column gives way before the
/// message does: the message is what people come to read.
fn refs_width(ui: &egui::Ui, graph: &Graph, available: f32) -> f32 {
    let font = theme::medium(11.0);

    let widest = ui.fonts(|fonts| {
        graph
            .rows
            .iter()
            .filter(|row| !row.commit.refs.is_empty())
            .map(|row| {
                row.commit
                    .ordered_refs()
                    .iter()
                    .map(|(label, _)| badge_w(fonts, &label.name, &font))
                    .sum::<f32>()
                    + REFS_PAD
            })
            .fold(0.0_f32, f32::max)
    });

    if widest <= 0.0 {
        return 0.0;
    }
    (widest + REFS_PAD).min(REFS_MAX).min(available * 0.28)
}

/// A row's branches and tags, flush against the graph and growing outwards.
///
/// What matters most stays next to the lanes, which is where the eye goes,
/// and what does not fit falls off the outer side leaving word of how much it
/// is keeping quiet about. The names line up against the graph rather than
/// against the window's border: that way they form a column even though they
/// measure different things.
#[allow(clippy::too_many_arguments)]
fn draw_refs(
    painter: &egui::Painter,
    ui: &egui::Ui,
    row: &Row,
    right: f32,
    left: f32,
    y: f32,
    chips: &mut Vec<Chip>,
) {
    let refs = row.commit.ordered_refs();
    if refs.is_empty() {
        return;
    }

    let font = theme::medium(11.0);
    let widths: Vec<f32> =
        ui.fonts(|fonts| refs.iter().map(|(l, _)| badge_w(fonts, &l.name, &font)).collect());

    // How many fit. If any are left out the count has to be done again,
    // setting room aside to say so, which takes room too.
    let fitting = |reserve: f32| {
        let mut x = right;
        let mut count = 0;
        for width in &widths {
            if x - width < left + reserve {
                break;
            }
            x -= width + BADGE_GAP;
            count += 1;
        }
        count
    };

    let mut shown = fitting(0.0);
    if shown < refs.len() {
        shown = fitting(MORE_W);
    }

    let lane = theme::lane(row.color);
    let mut x = right;
    let mut from = 0;

    // If not even the first one fits, it is clipped rather than dropped: half
    // a branch says what this row is about, and a bare count says nothing.
    if shown == 0 {
        let reserve = if refs.len() > 1 { MORE_W } else { 0.0 };
        let room = right - left - reserve - BADGE_PAD * 2.0;
        if room > 24.0 {
            let (label, current) = refs[0];
            let text = elide(ui, &label.name, room, &font);
            let width = ui.fonts(|fonts| badge_w(fonts, &text, &font));
            x -= width;
            let rect = draw_badge(painter, &text, label.kind, current, lane, x, y, width);
            // The name as it is, not as it was shortened to fit: what gets
            // checked out is a branch, and `feat/jury-p…` is not one.
            chips.push(Chip { rect, name: label.name.clone(), kind: label.kind, current });
            x -= BADGE_GAP;
            shown = 1;
            from = 1;
        }
    }

    for index in from..shown {
        let (label, current) = refs[index];
        x -= widths[index];
        let rect =
            draw_badge(painter, &label.name, label.kind, current, lane, x, y, widths[index]);
        chips.push(Chip { rect, name: label.name.clone(), kind: label.kind, current });
        x -= BADGE_GAP;
    }

    if shown < refs.len() {
        painter.text(
            Pos2::new(x, y),
            Align2::RIGHT_CENTER,
            format!("+{}", refs.len() - shown),
            theme::mono(10.5),
            theme::FAINT,
        );
    }
}

/// A reference chip. Gives back where it was drawn, so that a click can be
/// matched to it afterwards.
///
/// The branch you are on goes solid, and it is the only one in the whole
/// window that does. Weight does what color cannot: one of the eight lanes is
/// amber, so any old branch landing in it would dress the same as HEAD and
/// say you are where you are not.
///
/// The other locals are tinted with their lane, so that the name and its line
/// read as one thread. The remotes go hollow because they nearly always
/// duplicate a local one, and what matters is that they do not compete.
fn draw_badge(
    painter: &egui::Painter,
    name: &str,
    kind: RefKind,
    current: bool,
    lane: Color32,
    x: f32,
    y: f32,
    width: f32,
) -> Rect {
    let chip = Rect::from_min_size(Pos2::new(x, y - 9.0), egui::vec2(width, 18.0));
    let radius = CornerRadius::same(3);
    let text = Pos2::new(chip.left() + BADGE_PAD, y);
    let font = theme::medium(11.0);

    if current {
        painter.rect_filled(chip, radius, theme::HEAD);
        painter.text(text, Align2::LEFT_CENTER, name, font, theme::CANVAS);
        return chip;
    }

    let (color, filled) = match kind {
        RefKind::Head => (theme::HEAD, true),
        RefKind::LocalBranch => (lane, true),
        RefKind::RemoteBranch => (theme::MUTED, false),
        RefKind::Tag => (theme::lane(6), true),
    };

    if filled {
        painter.rect_filled(chip, radius, theme::tint(color, 38));
    } else {
        painter.rect_stroke(
            chip,
            radius,
            Stroke::new(1.0, theme::HAIRLINE),
            egui::StrokeKind::Inside,
        );
    }
    painter.text(text, Align2::LEFT_CENTER, name, font, color);
    chip
}

fn badge_w(fonts: &egui::epaint::Fonts, text: &str, font: &FontId) -> f32 {
    text_w(fonts, text, font) + BADGE_PAD * 2.0
}

fn text_w(fonts: &egui::epaint::Fonts, text: &str, font: &FontId) -> f32 {
    text.chars().map(|c| fonts.glyph_width(font, c)).sum()
}

/// Clips the text with an ellipsis if it does not fit in `max_w`.
fn elide(ui: &egui::Ui, text: &str, max_w: f32, font: &FontId) -> String {
    let width = |s: &str| ui.fonts(|fonts| text_w(fonts, s, font));

    if width(text) <= max_w {
        return text.to_string();
    }

    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        if width(&out) > max_w - 12.0 {
            out.pop();
            break;
        }
    }
    out.push('…');
    out
}

/// Which reference a row in the graph is about, when it is clear which.
///
/// Exactly one local branch on the row is clear: that is the branch, even
/// with its remote counterpart sitting beside it. Several of them is not, and
/// then the menu is about the commit alone rather than guessing at one.
fn named_of(row: &Row, head: Option<&str>) -> Option<Named> {
    let mut locals = row
        .commit
        .refs
        .iter()
        .filter(|label| label.kind == RefKind::LocalBranch);

    let first = locals.next()?;
    if locals.next().is_some() {
        return None;
    }

    Some(Named {
        is_head: head == Some(first.name.as_str()),
        name: first.name.clone(),
        kind: first.kind,
    })
}
