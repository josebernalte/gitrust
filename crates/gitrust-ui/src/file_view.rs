//! One file, across the whole window.
//!
//! Two columns. On the left, everything that ever happened to this file, one
//! row per commit; on the right, whatever you want to know about the commit
//! picked over there. That shape is the point: the history of a file is not a
//! fourth question to switch to and lose your place in, it is the thing you
//! steer the other three with. Click a row and the diff, the file and the
//! blame all move to that commit together.
//!
//! The right-hand column is wide for a reason. Blame wants a column for the
//! commit, one for who, one for when and then the line itself, and on a panel
//! three hundred pixels wide that is four things fighting over nothing. So a
//! file opened this way takes the middle of the window, where the graph
//! usually is.
//!
//! Like every view here it touches no repository. It is handed what was read
//! and gives back what was asked for.

use chrono::{Local, TimeZone};
use eframe::egui::{self, RichText};
use gitrust_core::{Commit, Contents, FileDiff, HunkId, Line, Oid, Story};

use crate::{diff_view, syntax, theme};

/// How wide the list of commits is. Enough for a subject worth reading and
/// the hash beside it, and no wider: every pixel here is a pixel the diff
/// does not get.
const HISTORY: f32 = 420.0;

/// How tall one row in that list is. Two lines of text and room to breathe.
const ROW: f32 = 42.0;

/// Which of the three questions is being asked about the commit picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// What this commit did to it.
    Diff,
    /// What the whole of it said.
    Whole,
    /// Who is answerable for each line.
    Blame,
}

impl Look {
    pub fn title(self) -> &'static str {
        match self {
            Look::Diff => "What changed",
            Look::Whole => "The file",
            Look::Blame => "Who wrote it",
        }
    }
}

/// What has been asked for.
pub enum Act {
    /// Ask a different one of the three.
    Look(Look),
    /// Put it away and go back to the graph.
    Close,
    /// Go to that commit.
    Show(Oid),
    /// Look at this same file as it stood at that commit.
    At(Oid),
    /// Undo that piece of this commit, in the working tree.
    Revert(HunkId),
    Save(String),
}

/// Everything read for the file being looked at.
///
/// The three on the right are read the first time they are asked for and
/// kept. They are `None` until then rather than empty: a file with no blame
/// yet and a file with no lines are not the same thing, and only one of them
/// is worth drawing a message about.
///
/// `story` is not like the others. It belongs to the file rather than to the
/// commit, so it is read once when the file is opened and survives every
/// move between commits -- walking it again on each click would be the most
/// expensive thing in the window happening for no reason.
#[derive(Default)]
pub struct Read {
    pub diff: Option<FileDiff>,
    pub whole: Option<Contents>,
    pub blame: Option<Vec<Line>>,
    pub story: Option<Story>,
    /// The file's lines cut into coloured pieces, worked out once when its
    /// text first arrives. Both the file and the blame are the same lines at
    /// the same commit, so there is one of these and not two.
    pub painted: Option<Vec<Vec<syntax::Piece>>>,
}

/// The file being looked at, and what has been read about it.
pub struct Looking {
    pub path: String,
    /// The commit it is being looked at from: the row picked on the left.
    pub at: Oid,
    pub how: Look,
    pub read: Read,
    /// What went wrong, when something did. Blame on something that is not
    /// text refuses, and that refusal belongs on the screen rather than in a
    /// status bar that the next thing overwrites.
    pub trouble: Option<String>,
    pub editing: bool,
    pub draft: String,
}

pub fn show(ui: &mut egui::Ui, looking: &mut Looking, busy: bool, editable: bool) -> Option<Act> {
    let mut act = None;

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui
            .button(RichText::new("\u{2190} Back").font(theme::body(12.0)))
            .on_hover_text("back to the graph")
            .clicked()
        {
            act = Some(Act::Close);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(&looking.path)
                .font(theme::medium(13.5))
                .color(theme::TEXT),
        );
        ui.label(
            RichText::new(format!("at {}", short(looking.at)))
                .font(theme::mono(11.0))
                .color(theme::FAINT),
        );

        // The three questions sit over the column they answer, on the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            // Right to left, so the list reads the other way round.
            for how in [Look::Blame, Look::Whole, Look::Diff] {
                if ui.selectable_label(looking.how == how, how.title()).clicked() {
                    act = Some(Act::Look(how));
                }
            }
        });
    });
    theme::rule(ui);

    ui.add_space(4.0);
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(HISTORY);
            egui::ScrollArea::vertical()
                .id_salt("file-story")
                .auto_shrink([false, false])
                .show(ui, |ui| story(ui, looking, &mut act));
        });

        // A vertical line, not theme::rule: a rule takes the width it is
        // given, and inside a row that is every pixel the right-hand column
        // was going to be drawn in.
        ui.separator();

        ui.vertical(|ui| {
            if let Some(trouble) = &looking.trouble {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(trouble)
                        .font(theme::body(12.0))
                        .color(theme::REMOVED),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt(("file", looking.how.title()))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    match looking.how {
                        Look::Diff => diff(ui, looking, busy, &mut act),
                        Look::Whole => whole(ui, looking, editable, &mut act),
                        Look::Blame => blame(ui, looking, &mut act),
                    }
                    ui.add_space(20.0);
                });
        });
    });

    act
}

/// Everything that ever happened to this file, newest first.
fn story(ui: &mut egui::Ui, looking: &Looking, act: &mut Option<Act>) {
    let Some(story) = &looking.read.story else {
        return theme::empty(ui, "Walking the history\u{2026}");
    };
    if story.commits.is_empty() {
        return theme::empty(ui, "Nothing in this history touched this file");
    }

    for commit in &story.commits {
        if row(ui, commit, commit.id == looking.at) {
            *act = Some(Act::At(commit.id));
        }
    }

    ui.add_space(8.0);
    match story.cut_short {
        // A list that might be missing its oldest entries has to say so, or
        // it is a different answer wearing the same clothes.
        true => {
            ui.label(
                RichText::new(
                    "The walk stopped before the beginning. Git keeps no history \
                     for a file, so this is found by reading commits one by one, \
                     and there is a limit on how many.",
                )
                .font(theme::body(11.0))
                .color(theme::FAINT),
            );
        }
        false => {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("End of history")
                        .font(theme::body(11.0))
                        .color(theme::FAINT),
                );
            });
        }
    }
    ui.add_space(8.0);
}

/// One commit in that list: what it said, who said it, when, and its hash.
///
/// The whole row is the target rather than the hash alone. Picking which
/// version of a file you are looking at is the only thing this list is for,
/// and a four-character target for the only thing on offer is a target you
/// miss.
fn row(ui: &mut egui::Ui, commit: &Commit, here: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW),
        egui::Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        match (here, response.hovered()) {
            (true, _) => {
                ui.painter().rect_filled(rect, 4.0, theme::CHOSEN);
            }
            (false, true) => {
                ui.painter().rect_filled(rect, 4.0, theme::RAISED);
            }
            _ => {}
        }

        let inner = rect.shrink2(egui::vec2(8.0, 5.0));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));

        // A label can be selected with the mouse, and a label that can be
        // selected senses clicks -- so every word written in here lands on
        // top of the row and swallows the click meant for it, leaving only
        // the gaps between the words working. Nothing in a row is text
        // anybody wants to copy; the row is one target and says so.
        ui.style_mut().interaction.selectable_labels = false;

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            // A row is at least `interact_size` tall, and that is a size for
            // buttons. Two of them stacked came to more than the band drawn
            // behind them, so the second line -- the date and who wrote it --
            // sat below its own row and outside the highlight. Here a line is
            // as tall as the words in it.
            ui.spacing_mut().interact_size.y = 0.0;

            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(&commit.short_id)
                            .font(theme::mono(10.5))
                            .color(match here {
                                true => theme::TEXT,
                                false => theme::lane(0),
                            }),
                    );
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&commit.summary)
                                    .font(theme::body(12.0))
                                    .color(theme::TEXT),
                            )
                            .truncate(),
                        );
                    });
                });
            });

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(
                    RichText::new(when(commit.time))
                        .font(theme::mono(10.0))
                        .color(theme::FAINT),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("by {}", commit.author))
                            .font(theme::body(10.5))
                            .color(theme::MUTED),
                    )
                    .truncate(),
                );
            });
        });
    }

    response
        .on_hover_text("look at the file as it stood here")
        .clicked()
}

fn diff(ui: &mut egui::Ui, looking: &Looking, busy: bool, act: &mut Option<Act>) {
    let Some(diff) = &looking.read.diff else {
        return theme::empty(ui, "Working it out\u{2026}");
    };
    if diff.hunks.is_empty() {
        return theme::empty(ui, "This commit did not change this file");
    }

    for hunk in &diff.hunks {
        ui.horizontal(|ui| {
            diff_view::header(ui, hunk);
        });
        // Undoing one piece writes nothing: it puts the change back in the
        // files in front of you and leaves it there to look at.
        if !busy
            && ui
                .button(RichText::new("Undo this piece").font(theme::body(11.0)))
                .on_hover_text(
                    "puts this piece back in the working tree. \
                     Nothing in the history moves",
                )
                .clicked()
        {
            *act = Some(Act::Revert(hunk.id));
        }
        diff_view::lines(ui, hunk, &looking.path);
        ui.add_space(10.0);
    }
}

fn whole(ui: &mut egui::Ui, looking: &mut Looking, editable: bool, act: &mut Option<Act>) {
    match &looking.read.whole {
        None => theme::empty(ui, "Reading it\u{2026}"),
        Some(Contents::Absent) => {
            theme::empty(ui, "This file was not there yet at this commit")
        }
        Some(Contents::Binary(size)) => theme::empty(
            ui,
            &format!("Not text: {size} bytes of something else"),
        ),
        Some(Contents::Text(text)) => {
            if editable {
                if !looking.editing && ui.button("Edit remote file").clicked() {
                    looking.editing = true;
                    looking.draft = text.clone();
                }
                if looking.editing {
                    ui.add(egui::TextEdit::multiline(&mut looking.draft).desired_width(f32::INFINITY).desired_rows(24).font(theme::mono(11.5)));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() { looking.editing = false; }
                        if ui.button("Save remote file").clicked() { *act = Some(Act::Save(looking.draft.clone())); }
                    });
                    return;
                }
            }
            // Painted as numbered lines rather than put in a text box: a box
            // would let somebody type into a file as it was in 2023, which
            // is not a thing that can be done.
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.spacing_mut().interact_size.y = 13.0;
            let painted = looking.read.painted.as_deref();
            for (index, line) in text.lines().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    ui.add_sized(
                        [44.0, 15.0],
                        egui::Label::new(
                            RichText::new((index + 1).to_string())
                                .font(theme::mono(11.0))
                                .color(theme::FAINT),
                        ),
                    );
                    code(ui, line, painted.and_then(|all| all.get(index)));
                });
            }
        }
    }
}

fn blame(ui: &mut egui::Ui, looking: &Looking, act: &mut Option<Act>) {
    let Some(lines) = &looking.read.blame else {
        return theme::empty(ui, "Working out who wrote what\u{2026}");
    };
    if lines.is_empty() {
        return theme::empty(ui, "There is nothing in this file");
    }

    ui.spacing_mut().item_spacing.y = 1.0;
    ui.spacing_mut().interact_size.y = 13.0;
    for (index, line) in lines.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            // Only the first line of a run says whose it is. Twenty lines
            // from one commit want saying once; repeating it turns the
            // left-hand side into noise you have to read past.
            let said = line.first;
            let hash = ui.add_sized(
                [52.0, 15.0],
                egui::Label::new(
                    RichText::new(match said {
                        true => line.short.clone(),
                        false => String::new(),
                    })
                    .font(theme::mono(10.5))
                    .color(theme::lane(0)),
                )
                .sense(egui::Sense::click()),
            );
            if said && !line.commit.is_zero() {
                let hash = hash.on_hover_text(format!(
                    "{}\n{}\n{}",
                    line.summary,
                    line.author,
                    when(line.time)
                ));
                if hash.clicked() {
                    *act = Some(Act::Show(line.commit));
                }
            }

            ui.add_sized(
                [120.0, 15.0],
                egui::Label::new(
                    RichText::new(match said {
                        // Nobody's yet: written since the last commit.
                        true if line.commit.is_zero() => "not committed".to_string(),
                        true => line.author.clone(),
                        false => String::new(),
                    })
                    .font(theme::body(10.5))
                    .color(match line.commit.is_zero() {
                        true => theme::ADDED,
                        false => theme::MUTED,
                    }),
                )
                .truncate(),
            );

            ui.add_sized(
                [40.0, 15.0],
                egui::Label::new(
                    RichText::new(line.number.to_string())
                        .font(theme::mono(10.5))
                        .color(theme::FAINT),
                ),
            );
            code(
                ui,
                &line.text,
                looking.read.painted.as_deref().and_then(|all| all.get(index)),
            );
        });
    }
}

/// One line of a file, coloured if it has been read for colour.
fn code(ui: &mut egui::Ui, line: &str, pieces: Option<&Vec<syntax::Piece>>) {
    match pieces {
        Some(pieces) => {
            ui.label(syntax::laid(pieces, theme::mono(11.5)));
        }
        None => {
            ui.label(RichText::new(line).font(theme::mono(11.5)).color(theme::TEXT));
        }
    }
}

fn when(seconds: i64) -> String {
    match Local.timestamp_opt(seconds, 0).single() {
        Some(time) => time.format("%d/%m/%Y").to_string(),
        None => String::new(),
    }
}

fn short(oid: Oid) -> String {
    oid.to_string().chars().take(7).collect()
}
