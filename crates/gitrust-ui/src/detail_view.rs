//! The right-hand panel: the detail of the commit picked in the graph.
//!
//! It reads top to bottom in three stretches -- what was done, in which files,
//! and the change line by line -- parted by a hair instead of by boxes.

use chrono::{Local, TimeZone};
use eframe::egui::{self, Color32, PopupCloseBehavior, RichText};
use gitrust_core::{Block, ChangeKind, CommitDetail, FileDiff};

use crate::{diff_view, theme};

/// What has been asked about the commit being looked at.
pub enum Pick {
    /// Show that file's change.
    File(String),
    /// Open that file across the window, where there is room for more than
    /// the diff.
    Open(String),
    /// Put this tag on it.
    Tag { name: String, message: String },
}

/// The tag being written, which has to still be there on the next frame.
#[derive(Default)]
pub struct TagDraft {
    name: String,
    message: String,
}

/// How much of what is left each of the first two stretches takes, before
/// anybody has moved them.
///
/// Shares rather than heights, so that the panel still parts itself sensibly
/// when the window is resized or the panel is dragged wider -- and so that
/// moving a line means the same thing at any size.
const BODY: f32 = 0.34;
const FILES: f32 = 0.30;

/// Nothing may be dragged shut, and nothing may be dragged so far open that
/// the change underneath it is left with nowhere to go.
const LEAST: f32 = 0.06;
const MOST: f32 = 0.72;

/// Where the two lines sit, kept in egui's own memory rather than handed in
/// by the application.
///
/// That is a deliberate exception to this crate's rule that a view owns
/// nothing: where somebody dragged a line is the same kind of thing as how
/// far a list is scrolled or how wide the panel itself was pulled, and egui
/// is already keeping both of those in the same place. Nothing here is a
/// decision the application has to hear about, and none of it is worth
/// writing down anywhere.
#[derive(Clone, Copy)]
struct Shares {
    body: f32,
    files: f32,
}

impl Default for Shares {
    fn default() -> Self {
        Self {
            body: BODY,
            files: FILES,
        }
    }
}

fn kind_color(kind: ChangeKind) -> Color32 {
    match kind {
        ChangeKind::Added => theme::ADDED,
        ChangeKind::Deleted => theme::REMOVED,
        ChangeKind::Renamed => theme::lane(0),
        ChangeKind::TypeChange => theme::lane(6),
        ChangeKind::Conflicted => theme::REMOVED,
        _ => theme::CHANGED,
    }
}

/// Gives back whatever has been asked about this commit.
pub fn show(
    ui: &mut egui::Ui,
    detail: Option<&CommitDetail>,
    selected_file: Option<&str>,
    diff: Option<&FileDiff>,
    tag: &mut TagDraft,
    read_only: bool,
) -> Option<Pick> {
    let Some(detail) = detail else {
        theme::empty(ui, "Pick a commit in the graph to see it here");
        return None;
    };

    let mut picked = None;

    // Read once here and written once at the end, so that no stretch is laid
    // out against a line that a later one has already moved.
    let id = egui::Id::new("detail-shares");
    let was: Shares = ui.data(|data| data.get_temp(id)).unwrap_or_default();
    let mut shares = was;

    ui.add_space(12.0);

    // The subject rules: it is the only thing at headline size in the window.
    ui.label(
        RichText::new(&detail.summary)
            .font(theme::strong(15.0))
            .color(theme::TEXT),
    );

    ui.add_space(6.0);
    let when = Local
        .timestamp_opt(detail.time, 0)
        .single()
        .map(|dt| dt.format("%d/%m/%Y %H:%M").to_string())
        .unwrap_or_default();

    ui.horizontal(|ui| {
        // The email is kept for the hover: it identifies whoever signed, but
        // it repeats what the name already says and does not fit beside the
        // date.
        ui.label(
            RichText::new(&detail.author)
                .font(theme::medium(12.0))
                .color(theme::MUTED),
        )
        .on_hover_text(&detail.email);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(&detail.short_id)
                    .font(theme::mono(11.5))
                    .color(theme::FAINT),
            );
            if detail.is_merge() {
                ui.label(
                    RichText::new("merge")
                        .font(theme::medium(10.5))
                        .color(theme::lane(6))
                        .background_color(theme::tint(theme::lane(6), 38)),
                );
            }
            ui.label(
                RichText::new(when)
                    .font(theme::mono(11.0))
                    .color(theme::FAINT),
            );
        });
    });

    // The body is cut into blocks before it is painted: the prose is wrapped
    // again to the panel's width, and the lists, the code and the trailers are
    // left as they were, in mono, because there a line break means something.
    //
    // It is the one thing in this panel with no size of its own -- a commit
    // message is a line or it is two pages -- so it is given a ceiling and a
    // scroll inside it. Without one, a long message pushes the files and the
    // change itself off the bottom of a panel that does not scroll, and there
    // is no way left to reach them.
    //
    // The ceiling is a share of what is left below this point, and the line
    // under it can be dragged to change that share.
    let body_room = ui.available_height();
    if !detail.body.is_empty() {
        ui.add_space(10.0);
        let ceiling = body_room * shares.body;
        egui::ScrollArea::vertical()
            // Salted with the commit, so that a message you read to the end
            // does not leave the next one opened halfway down.
            .id_salt(("commit-body", detail.short_id.as_str()))
            // Shrinking down the page but not across it: a two-line message
            // takes two lines, and a long one stops at the ceiling.
            .auto_shrink([false, true])
            .max_height(ceiling)
            .show(ui, |ui| {
                for block in gitrust_core::blocks(&detail.body) {
                    let text = match &block {
                        Block::Prose(text) => RichText::new(text).font(theme::body(12.5)),
                        Block::Literal(text) => RichText::new(text).font(theme::mono(11.0)),
                    };
                    ui.add(egui::Label::new(text.color(theme::MUTED)).wrap());
                    ui.add_space(7.0);
                }
            });
    }

    // What can be done to this commit. One thing for now, but the row is set
    // up for the ones to come.
    ui.add_space(9.0);
    if !read_only {
        if let Some(action) = tag_menu(ui, tag) {
            picked = Some(action);
        }
    }

    // A line with nothing above it to resize is just a line.
    match detail.body.is_empty() {
        true => theme::rule(ui),
        false => {
            let moved = theme::grip(ui, "detail-body");
            shares.body = (shares.body + moved / body_room).clamp(LEAST, MOST);
        }
    }

    let (added, modified, deleted) = detail.counts();
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(
            RichText::new(theme::files_label(detail.files.len()))
                .font(theme::medium(11.5))
                .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (count, word, color) in [
                (deleted, "deleted", theme::REMOVED),
                (modified, "modified", theme::CHANGED),
                (added, "new", theme::ADDED),
            ] {
                if count > 0 {
                    ui.label(
                        RichText::new(format!("{count} {word}"))
                            .font(theme::body(11.0))
                            .color(color),
                    );
                }
            }
        });
    });
    ui.add_space(4.0);

    // The file list takes about a third of what is left; the rest is for the
    // diff, which is what actually gets read. A commit that touches forty
    // files is a commit where that is the wrong share, which is what the
    // line below it is for.
    let list_room = ui.available_height();
    let list_height = list_room * shares.files;
    egui::ScrollArea::vertical()
        .id_salt("commit-files")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for file in &detail.files {
                let is_selected = selected_file == Some(file.path.as_str());
                if file_row(ui, &file.path, file.kind, is_selected) {
                    picked = Some(Pick::File(file.path.clone()));
                }
            }
        });

    let moved = theme::grip(ui, "detail-files");
    shares.files = (shares.files + moved / list_room).clamp(LEAST, MOST);

    // The way to the other three questions about this file. It is a button
    // rather than a double click because a double click on a row that
    // already does something on one click is a thing nobody finds.
    if let Some(path) = selected_file {
        ui.horizontal(|ui| {
            if ui
                .button(RichText::new("Open it full width").font(theme::body(11.0)))
                .on_hover_text("the whole file, who wrote each line, and its history")
                .clicked()
            {
                picked = Some(Pick::Open(path.to_string()));
            }
        });
        ui.add_space(4.0);
    }

    match diff {
        None => theme::empty(ui, "Pick a file to see what changed"),
        Some(diff) if diff.binary => theme::empty(ui, "Binary file"),
        Some(diff) => {
            egui::ScrollArea::both()
                .id_salt("diff-commit")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for hunk in &diff.hunks {
                        diff_view::header(ui, hunk);
                        diff_view::lines(ui, hunk, &diff.path);
                        ui.add_space(8.0);
                    }
                });
        }
    }

    // Written only when a line actually moved: every commit in the graph
    // comes through here, and a write a frame for nothing is a write a frame.
    if shares.body != was.body || shares.files != was.files {
        ui.data_mut(|data| data.insert_temp(id, shares));
    }

    picked
}

/// The tagging popup, with the name and what it is for.
///
/// Writing the message or not is what decides the kind of tag, just as in
/// `git tag`. There is no switch because none is needed: whoever wants an
/// annotated tag is exactly whoever has something to say about it.
fn tag_menu(ui: &mut egui::Ui, draft: &mut TagDraft) -> Option<Pick> {
    let button = ui.add(egui::Button::new(
        RichText::new("Tag          ")
            .font(theme::body(11.5))
            .color(theme::MUTED),
    ));
    theme::caret(ui, &button);
    let id = ui.make_persistent_id("menu-tag");

    if button.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(id));
    }

    egui::popup::popup_below_widget(
        ui,
        id,
        &button,
        PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(266.0);
            ui.label(
                RichText::new("Tag on this commit")
                    .font(theme::medium(12.0))
                    .color(theme::TEXT),
            );
            ui.add_space(5.0);

            let entry = ui.add(
                egui::TextEdit::singleline(&mut draft.name)
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 5.0))
                    .font(theme::mono(12.0))
                    .hint_text(RichText::new("v1.0").color(theme::FAINT)),
            );
            ui.add_space(5.0);
            ui.add(
                egui::TextEdit::multiline(&mut draft.message)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .margin(egui::vec2(8.0, 5.0))
                    .font(theme::body(11.5))
                    .hint_text(RichText::new("what this version brings").color(theme::FAINT)),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("With a message it is annotated; without one, lightweight")
                    .font(theme::body(10.5))
                    .color(theme::FAINT),
            );
            ui.add_space(6.0);

            let entered = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let pressed = ui
                .add_enabled(!draft.name.trim().is_empty(), egui::Button::new("Create"))
                .clicked();

            (entered || pressed).then(|| {
                ui.memory_mut(|memory| memory.close_popup());
                Pick::Tag {
                    name: std::mem::take(&mut draft.name),
                    message: std::mem::take(&mut draft.message),
                }
            })
        },
    )
    .flatten()
}

/// A row of the file list: the status letter in its gutter, and the path with
/// the folder muted so that the file's name stands out.
fn file_row(ui: &mut egui::Ui, path: &str, kind: ChangeKind, selected: bool) -> bool {
    let mut clicked = false;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(kind.tag())
                .font(theme::mono(11.0))
                .color(kind_color(kind)),
        );

        if ui
            .selectable_label(selected, theme::path_text(path, 11.5, selected))
            .on_hover_text(path)
            .clicked()
        {
            clicked = true;
        }
    });

    clicked
}
