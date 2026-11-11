//! The changes panel: the index and working tree lists, the diff and the commit box.

use eframe::egui::{self, Color32, RichText};
use gitrust_core::{ChangeKind, FileChange, FileDiff, HunkId, WorkingState, SUBJECT_LIMIT};

use crate::{diff_view, theme};

/// What the user has asked for in this frame. The view does not touch the
/// repository: it only describes the intention and lets `main` carry it out.
pub enum Action {
    Select {
        path: String,
        staged: bool,
    },
    Stage(String),
    Unstage(String),
    StageHunk {
        path: String,
        hunk: HunkId,
    },
    UnstageHunk {
        path: String,
        hunk: HunkId,
    },
    StageAll,
    UnstageAll,
    /// Throw away a file's changes. `untracked` changes what is lost: a new
    /// file is deleted whole, a known one goes back to the index.
    Discard {
        path: String,
        untracked: bool,
    },
    DiscardHunk {
        path: String,
        hunk: HunkId,
    },
    DiscardAll,
    Commit,
    Amend,
    /// The amend switch has changed position.
    AmendToggled,
}

/// The message being written, and which commit it will end up in.
pub struct Draft<'a> {
    pub subject: &'a mut String,
    pub body: &'a mut String,
    /// Whether what is written will redo the last commit instead of making one.
    pub amend: &'a mut bool,
    /// There is a commit to amend. Without one, the switch is not shown.
    pub amendable: bool,
    /// The remote that already has the last commit, if it has it. Amending it
    /// then forces the next push, and that has to be said beforehand, not
    /// after.
    pub pushed: Option<&'a str>,
}

fn kind_color(kind: ChangeKind) -> Color32 {
    match kind {
        ChangeKind::Added => theme::ADDED,
        ChangeKind::Modified => theme::CHANGED,
        ChangeKind::Deleted => theme::REMOVED,
        ChangeKind::Renamed => theme::lane(0),
        ChangeKind::TypeChange => theme::lane(6),
        ChangeKind::Untracked => theme::MUTED,
        ChangeKind::Conflicted => theme::REMOVED,
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &WorkingState,
    selected: Option<(&str, bool)>,
    diff: Option<&FileDiff>,
    draft: &mut Draft<'_>,
    read_only: bool,
) -> Option<Action> {
    let mut action = None;

    egui::SidePanel::left("changes")
        .resizable(true)
        .default_width(330.0)
        .frame(
            egui::Frame::NONE
                .fill(theme::PANEL)
                .inner_margin(egui::Margin::symmetric(10, 0)),
        )
        .show_inside(ui, |ui| {
            // The commit box goes right at the bottom, pinned.
            egui::TopBottomPanel::bottom("commit")
                .resizable(false)
                .frame(
                    egui::Frame::NONE
                        .fill(theme::PANEL)
                        .inner_margin(egui::Margin::symmetric(0, 10)),
                )
                .show_inside(ui, |ui| {
                    ui.add_enabled_ui(!read_only, |ui| commit_box(ui, state, draft, &mut action))
                });

            egui::ScrollArea::vertical()
                .id_salt("lists")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add_space(10.0);
                    section(
                        ui,
                        "Staged",
                        &state.staged,
                        selected,
                        true,
                        &mut action,
                        read_only,
                    );
                    ui.add_space(14.0);
                    section(
                        ui,
                        "Unstaged",
                        &state.unstaged,
                        selected,
                        false,
                        &mut action,
                        read_only,
                    );
                    ui.add_space(10.0);
                });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(theme::CANVAS)
                .inner_margin(egui::Margin::symmetric(12, 8)),
        )
        .show_inside(ui, |ui| {
            show_diff(ui, selected, diff, &mut action, read_only);
        });

    action
}

/// The same panel in a single column, for the right-hand side of the history.
///
/// It is what comes up when you pick the uncommitted-changes row in the graph:
/// the two lists, the commit box and the diff, one below the other, so that
/// you can stage and commit without leaving the graph.
pub fn compact(
    ui: &mut egui::Ui,
    state: &WorkingState,
    selected: Option<(&str, bool)>,
    diff: Option<&FileDiff>,
    draft: &mut Draft<'_>,
) -> Option<Action> {
    let mut action = None;

    ui.add_space(12.0);
    ui.label(
        RichText::new("Uncommitted changes")
            .font(theme::strong(15.0))
            .color(theme::TEXT),
    );
    ui.add_space(3.0);
    let note = theme::pending_label(state.staged.len(), state.unstaged.len());
    ui.label(
        RichText::new(if note.is_empty() {
            "The working tree is clean".to_string()
        } else {
            note
        })
        .font(theme::body(11.5))
        .color(theme::MUTED),
    );

    theme::rule(ui);

    // The lists stay within a third of the panel; the rest is for the commit
    // box and the diff, which is where the work happens.
    let list_height = (ui.available_height() * 0.34).clamp(90.0, 300.0);
    egui::ScrollArea::vertical()
        .id_salt("narrow-lists")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            section(
                ui,
                "Staged",
                &state.staged,
                selected,
                true,
                &mut action,
                false,
            );
            ui.add_space(12.0);
            section(
                ui,
                "Unstaged",
                &state.unstaged,
                selected,
                false,
                &mut action,
                false,
            );
        });

    theme::rule(ui);
    commit_box(ui, state, draft, &mut action);
    theme::rule(ui);
    show_diff(ui, selected, diff, &mut action, false);

    action
}

/// The commit box: subject and description apart, as Git expects them.
fn commit_box(
    ui: &mut egui::Ui,
    state: &WorkingState,
    draft: &mut Draft<'_>,
    action: &mut Option<Action>,
) {
    // The subject goes apart from the body: Git expects a blank line between
    // the two, and this way nobody has to remember it.
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Title")
                .font(theme::medium(11.5))
                .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let length = draft.subject.chars().count();
            let color = if length > SUBJECT_LIMIT {
                theme::CHANGED
            } else {
                theme::FAINT
            };
            ui.label(
                RichText::new(format!("{length}/{SUBJECT_LIMIT}"))
                    .font(theme::mono(10.5))
                    .color(color),
            );
        });
    });
    ui.add_space(3.0);
    ui.add(
        egui::TextEdit::singleline(draft.subject)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(8.0, 6.0))
            .font(theme::body(12.5))
            .hint_text(RichText::new("feat: one-line summary").color(theme::FAINT)),
    );

    ui.add_space(8.0);
    ui.label(
        RichText::new("Description")
            .font(theme::medium(11.5))
            .color(theme::MUTED),
    );
    ui.add_space(3.0);
    ui.add(
        egui::TextEdit::multiline(draft.body)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(8.0, 6.0))
            .font(theme::mono(11.5))
            .hint_text(RichText::new("What changes, and why").color(theme::FAINT)),
    );
    ui.add_space(8.0);

    // When amending, nothing needs to be staged: changing only the message of
    // the last commit is half the times anyone amends.
    let amending = *draft.amend;
    let ready = !draft.subject.trim().is_empty() && (amending || !state.staged.is_empty());
    let shortcut = ready && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));

    ui.horizontal(|ui| {
        let label = if amending { "Amend" } else { "Commit" };
        let button = egui::Button::new(RichText::new(label).font(theme::medium(13.0)))
            .fill(if ready {
                theme::tint(theme::lane(0), 54)
            } else {
                theme::RAISED
            })
            .min_size(egui::vec2(88.0, 28.0));

        if ui
            .add_enabled(ready, button)
            .on_hover_text("Ctrl+Enter")
            .clicked()
            || shortcut
        {
            *action = Some(if amending {
                Action::Amend
            } else {
                Action::Commit
            });
        }

        // If there is nothing to commit, the hint says what is missing instead
        // of merely describing the state.
        let note = if !amending && state.staged.is_empty() {
            "Stage something first".to_string()
        } else if draft.subject.trim().is_empty() {
            "The title is missing".to_string()
        } else if state.staged.is_empty() {
            "Only the message will change".to_string()
        } else {
            theme::files_label(state.staged.len())
        };
        ui.label(
            RichText::new(note)
                .font(theme::body(11.5))
                .color(theme::FAINT),
        );

        if draft.amendable {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let text = RichText::new("Amend the last one")
                    .font(theme::body(11.5))
                    .color(if amending { theme::TEXT } else { theme::MUTED });

                if ui
                    .checkbox(draft.amend, text)
                    .on_hover_text("Redoes the last commit instead of writing another")
                    .changed()
                {
                    *action = Some(Action::AmendToggled);
                }
            });
        }
    });

    // Amending a commit that already went out leaves two different histories
    // with the same name. It can be done, but not blindly.
    if let (true, Some(remote)) = (amending, draft.pushed) {
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "That commit is already on {remote}: the next push will have to be forced"
            ))
            .font(theme::body(11.0))
            .color(theme::CHANGED),
        );
    }
}

fn section(
    ui: &mut egui::Ui,
    title: &str,
    files: &[FileChange],
    selected: Option<(&str, bool)>,
    staged: bool,
    action: &mut Option<Action>,
    read_only: bool,
) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(
            RichText::new(title)
                .font(theme::medium(11.5))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(files.len().to_string())
                .font(theme::mono(10.5))
                .color(theme::FAINT),
        );

        if !files.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if staged { "Unstage all" } else { "Stage all" };
                if ui
                    .add_enabled(
                        !read_only,
                        egui::Button::new(
                            RichText::new(label)
                                .font(theme::body(11.0))
                                .color(theme::MUTED),
                        ),
                    )
                    .clicked()
                {
                    *action = Some(if staged {
                        Action::UnstageAll
                    } else {
                        Action::StageAll
                    });
                }

                // Throwing away a whole side cannot be undone, so it wears the
                // color of what is being deleted and does not go first.
                if !staged
                    && ui
                        .add_enabled(
                            !read_only,
                            egui::Button::new(
                                RichText::new("Discard all")
                                    .font(theme::body(11.0))
                                    .color(theme::REMOVED),
                            ),
                        )
                        .clicked()
                {
                    *action = Some(Action::DiscardAll);
                }
            });
        }
    });
    ui.add_space(4.0);

    if files.is_empty() {
        let note = if staged {
            "The index is empty"
        } else {
            "Nothing unstaged"
        };
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new(note)
                    .font(theme::body(11.5))
                    .color(theme::FAINT),
            );
        });
        return;
    }

    for file in files {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            let button = if staged { "−" } else { "+" };
            let hint = if staged { "Unstage" } else { "Stage" };
            if ui
                .add_enabled(
                    !read_only,
                    egui::Button::new(RichText::new(button).font(theme::mono(12.0)))
                        .min_size(egui::vec2(22.0, 20.0)),
                )
                .on_hover_text(hint)
                .clicked()
            {
                *action = Some(if staged {
                    Action::Unstage(file.path.clone())
                } else {
                    Action::Stage(file.path.clone())
                });
            }

            ui.label(
                RichText::new(file.kind.tag())
                    .font(theme::mono(11.0))
                    .color(kind_color(file.kind)),
            );

            let is_selected = selected == Some((file.path.as_str(), staged));
            if ui
                .selectable_label(is_selected, theme::path_text(&file.path, 11.5, is_selected))
                .on_hover_text(&file.path)
                .clicked()
            {
                *action = Some(Action::Select {
                    path: file.path.clone(),
                    staged,
                });
            }

            // The throw-away one goes to the far end of the row, away from
            // the staging one: between two buttons side by side, the one that
            // cannot be undone is the one you should not press by accident.
            if !staged {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(
                            !read_only,
                            egui::Button::new(
                                RichText::new("×")
                                    .font(theme::mono(12.0))
                                    .color(theme::MUTED),
                            )
                            .min_size(egui::vec2(20.0, 20.0)),
                        )
                        .on_hover_text("Discard this file's changes")
                        .clicked()
                    {
                        *action = Some(Action::Discard {
                            path: file.path.clone(),
                            untracked: file.kind == ChangeKind::Untracked,
                        });
                    }
                });
            }
        });
    }
}

fn show_diff(
    ui: &mut egui::Ui,
    selected: Option<(&str, bool)>,
    diff: Option<&FileDiff>,
    action: &mut Option<Action>,
    read_only: bool,
) {
    let Some((path, staged)) = selected else {
        theme::empty(ui, "Pick a file to see what changed");
        return;
    };

    ui.horizontal(|ui| {
        ui.label(theme::path_text(path, 12.5, false));
        ui.label(
            RichText::new(if staged {
                "in the index"
            } else {
                "in the working tree"
            })
            .font(theme::body(11.0))
            .color(theme::FAINT),
        );
        if let Some(diff) = diff {
            let (added, removed) = diff.counts();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                diff_view::counts(ui, added, removed);
            });
        }
    });
    theme::rule(ui);

    let Some(diff) = diff else {
        theme::empty(ui, "Nothing to show");
        return;
    };

    if diff.binary {
        theme::empty(ui, "Binary file");
        return;
    }

    egui::ScrollArea::both()
        .id_salt("diff")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;

            for hunk in &diff.hunks {
                // Each hunk can be moved on its own, which is what `git add -p`
                // does without making you answer questions in the terminal.
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    let label = if staged { "− Unstage" } else { "+ Stage" };
                    let hint = if staged {
                        "Send just this hunk back to the working tree"
                    } else {
                        "Stage just this hunk"
                    };

                    if ui
                        .add_enabled(
                            !read_only,
                            egui::Button::new(RichText::new(label).font(theme::body(11.0))),
                        )
                        .on_hover_text(hint)
                        .clicked()
                    {
                        let path = path.to_string();
                        *action = Some(if staged {
                            Action::UnstageHunk {
                                path,
                                hunk: hunk.id,
                            }
                        } else {
                            Action::StageHunk {
                                path,
                                hunk: hunk.id,
                            }
                        });
                    }

                    ui.label(
                        RichText::new(hunk.header.trim())
                            .font(theme::mono(11.0))
                            .color(theme::FAINT),
                    );

                    let (added, removed) = hunk.counts();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        diff_view::counts(ui, added, removed);

                        if !staged
                            && ui
                                .add_enabled(
                                    !read_only,
                                    egui::Button::new(
                                        RichText::new("Discard")
                                            .font(theme::body(11.0))
                                            .color(theme::REMOVED),
                                    ),
                                )
                                .on_hover_text("Undo just this hunk in the working tree")
                                .clicked()
                        {
                            *action = Some(Action::DiscardHunk {
                                path: path.to_string(),
                                hunk: hunk.id,
                            });
                        }
                    });
                });

                diff_view::lines(ui, hunk, path);
                ui.add_space(8.0);
            }
        });
}
