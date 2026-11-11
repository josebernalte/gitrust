//! The sidebar of references: locals, remotes and tags.
//!
//! Branches are grouped by their folder (`feat/`, `fix/`...) the way the
//! branch tree of the graphical clients does, because a naming convention
//! with slashes is the norm as soon as a repository grows.

use std::collections::BTreeMap;

use eframe::egui::{self, RichText};
use gitrust_core::{Move, Oid, RefEntry, RefKind, Worktree};

use crate::theme;

/// What has been asked for from the sidebar.
pub enum Pick {
    /// Show in the graph the commit the reference points at.
    Show(Oid),
    /// Switch to that branch.
    Switch { name: String, remote: bool },
    /// Take that tag off here.
    DeleteTag(String),
    /// Delete that branch: the one here, or the one on the remote.
    DeleteBranch { name: String, remote: bool },
    /// Bring that branch into the one you are on.
    Merge(String),
    /// Move the branch you are on up to that one, refusing to write a merge.
    FastForward(String),
    /// Put the branch you are on back on top of that one.
    Rebase(String),
    /// Bring down whatever the remote has for the branch you are on.
    Pull,
    /// Send that branch to the remote.
    Push(String),
    /// Say which remote branch a local one follows, or that it follows none.
    Follow { branch: String, upstream: Option<String> },
    /// Open that branch in a folder of its own.
    Isolate(String),
    /// Go to that commit, off any branch.
    GoTo(Oid),
    /// Ask for a name, then make a branch at that commit.
    BranchAt(Oid),
    /// Write what that commit did onto the branch you are on, again.
    CherryPick(Oid),
    /// Write a commit that undoes that one.
    Revert(Oid),
    /// Arrange what should become of every commit from that one to the tip.
    Rewrite(Oid),
    /// Move the branch you are on to that commit.
    MoveTo(Oid, Move),
    /// Ask for a name, then rename that branch.
    Rename(String),
    /// Put that text on the clipboard.
    Copy(String),
    /// Look at another folder this same repository is checked out in.
    Visit(std::path::PathBuf),
    /// The same folder, in a tab of its own, leaving this one where it is.
    VisitTab(std::path::PathBuf),
    /// Take that checkout away, leaving the branch where it is.
    Release(String),
    /// One branch was dropped on another. Which of the three things that
    /// could mean is not decided here: the gesture asks the question and
    /// something else puts it to somebody.
    Join { from: String, onto: String },
}

/// The branch being dragged.
///
/// It travels as a type of its own rather than as a bare `String` so that
/// nothing else dropped anywhere in the window can be mistaken for it: egui
/// hands a payload back by type, and the board is dragging cards around at
/// the same time.
#[derive(Clone)]
struct Carried {
    name: String,
}

/// Gives back whatever has been asked about a reference.
///
/// With `busy` set there is an operation under way and branches stop being
/// clickable: switching branches while something is coming down from the
/// remote is asking two things at once of the same files.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    refs: &[RefEntry],
    trees: &[Worktree],
    here: &std::path::Path,
    filter: &mut String,
    selected: Option<Oid>,
    head: Option<&str>,
    busy: bool,
    remote_checkout: bool,
) -> Option<Pick> {
    let mut picked = None;

    ui.add_space(8.0);
    ui.add(
        egui::TextEdit::singleline(filter)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(8.0, 5.0))
            .font(theme::body(12.5))
            .hint_text(RichText::new("Find a branch").color(theme::FAINT)),
    );
    ui.add_space(8.0);

    // Both sides of every branch, worked out once here rather than per row.
    //
    // A branch is in one of three states and the sidebar never said which:
    // on this disk only, on a remote only, or on both. All three look the
    // same in a list of names, and which one it is decides whether there is
    // anything to push, anything to pull, and whether deleting it here
    // throws the work away.
    let remote_branches: Vec<&RefEntry> = refs
        .iter()
        .filter(|entry| entry.kind == RefKind::RemoteBranch)
        .collect();
    let local_branches: Vec<&RefEntry> = refs
        .iter()
        .filter(|entry| entry.kind == RefKind::LocalBranch)
        .collect();

    let needle = filter.trim().to_lowercase();
    let matching: Vec<&RefEntry> = refs
        .iter()
        .filter(|entry| needle.is_empty() || entry.name.to_lowercase().contains(&needle))
        .collect();

    egui::ScrollArea::vertical()
        .id_salt("refs")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if refs.is_empty() {
                theme::empty(ui, "No references");
            } else if matching.is_empty() {
                theme::empty(ui, "No branch by that name");
            } else {
                for (title, mark, kind) in [
                    ("Branches", theme::Glyph::Folder, RefKind::LocalBranch),
                    ("Remotes", theme::Glyph::Cloud, RefKind::RemoteBranch),
                    ("Tags", theme::Glyph::Tag, RefKind::Tag),
                ] {
                    section(
                        ui,
                        title,
                        mark,
                        kind,
                        &matching,
                        &remote_branches,
                        &local_branches,
                        selected,
                        head,
                        busy,
                        remote_checkout,
                        !needle.is_empty(),
                        &mut picked,
                    );
                }
            }

            // The other folders this same repository is checked out in. They
            // are not references, but they are the other half of "where am
            // I", and that question is answered here.
            if !remote_checkout && trees.len() > 1 {
                checkouts(ui, trees, here, busy, &mut picked);
            }
            ui.add_space(12.0);
        });

    carried_chip(ui);

    picked
}

/// The name of the branch being dragged, following the cursor.
///
/// egui's own drag source paints the widget itself under the pointer. This
/// one wraps nothing, so there would be nothing to see: you would be dragging
/// across a window that looks exactly as it did, with only the cursor to say
/// anything was happening.
fn carried_chip(ui: &egui::Ui) {
    let ctx = ui.ctx();
    let Some(carried) = egui::DragAndDrop::payload::<Carried>(ctx) else {
        return;
    };
    let Some(pointer) = ctx.pointer_latest_pos() else {
        return;
    };

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("carried-branch"),
    ));
    let text = painter.layout_no_wrap(carried.name.clone(), theme::body(12.0), theme::TEXT);

    // Below and to the right of the pointer, so the chip never covers the row
    // being aimed at.
    let corner = pointer + egui::vec2(14.0, 10.0);
    let rect = egui::Rect::from_min_size(corner, text.size() + egui::vec2(14.0, 7.0));
    let radius = egui::CornerRadius::same(theme::RADIUS);
    painter.rect_filled(rect, radius, theme::PANEL);
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, theme::HEAD),
        egui::StrokeKind::Inside,
    );
    painter.galley(corner + egui::vec2(7.0, 3.5), text, theme::TEXT);
}

#[allow(clippy::too_many_arguments)]
fn section(
    ui: &mut egui::Ui,
    title: &str,
    mark: theme::Glyph,
    kind: RefKind,
    refs: &[&RefEntry],
    remote_branches: &[&RefEntry],
    local_branches: &[&RefEntry],
    selected: Option<Oid>,
    head: Option<&str>,
    busy: bool,
    remote_checkout: bool,
    searching: bool,
    picked: &mut Option<Pick>,
) {
    let entries: Vec<&RefEntry> = refs.iter().copied().filter(|e| e.kind == kind).collect();
    if entries.is_empty() {
        return;
    }

    theme::section(ui, title, mark, entries.len(), searching, |ui| {
        // The gesture cannot be seen, so it is said. And only where it is any
        // use: with one local branch, the one you are on, there is nowhere to go.
        if kind == RefKind::LocalBranch && entries.len() > 1 {
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Double-click to switch branch")
                        .font(theme::body(10.5))
                        .color(theme::FAINT),
                );
            });
        }

        ui.add_space(2.0);

        // The ones with no folder go on top; the rest, in their group.
        let mut folders: BTreeMap<&str, Vec<&RefEntry>> = BTreeMap::new();
        for entry in &entries {
            match entry.folder() {
                Some(folder) => folders.entry(folder).or_default().push(entry),
                None => leaf(ui, entry, remote_branches, local_branches, selected, head, busy, remote_checkout, picked),
            }
        }

        for (folder, group) in folders {
            // While something is being looked for, a folded folder is a match
            // nobody can see.
            theme::group(ui, folder, searching, |ui| {
                for entry in group {
                    leaf(ui, entry, remote_branches, local_branches, selected, head, busy, remote_checkout, picked);
                }
            });
        }
    });
}

/// The other folders this repository is checked out in.
///
/// They used to live inside the repository menu, which was defensible -- that
/// is the control that answers "where am I" -- and wrong in practice: you had
/// to open a menu meant for changing repository in order to move about inside
/// this one. One list in one place, and it is this one.
///
/// Going to one writes nothing. The window looks somewhere else, which is the
/// same move as opening a repository and nothing like switching branch.
fn checkouts(
    ui: &mut egui::Ui,
    trees: &[Worktree],
    here: &std::path::Path,
    busy: bool,
    picked: &mut Option<Pick>,
) {
    // A click and the first half of a double click are the same event, and
    // egui cannot tell them apart any more than anybody could: it has not
    // seen the second click yet. So the single click is held for as long as
    // a double click is allowed to take, and let go only if no second one
    // arrives. Without this, double-clicking would first send this tab to
    // the other folder and only then open a tab for it, which is both of the
    // two things at once and neither of the ones anybody asked for.
    //
    // The wait is egui's own, not a number picked here, so it matches every
    // other double click on the machine.
    let delay = ui.ctx().options(|options| options.input_options.max_double_click_delay);
    let now = ui.input(|input| input.time);
    let held = egui::Id::new("worktree-held");
    let before: Option<(std::path::PathBuf, f64)> = ui.data(|data| data.get_temp(held));
    let mut waiting = before.clone();

    theme::section(ui, "Worktrees", theme::Glyph::Trees, trees.len(), false, |ui| {
        ui.add_space(2.0);
        for tree in trees {
            let current = tree.path == here;
            let color = match (current, tree.missing) {
                (_, true) => theme::REMOVED,
                (true, _) => theme::HEAD,
                _ => theme::TEXT,
            };

            let label = match tree.main {
                true => format!("{} (main)", tree.name),
                false => tree.name.clone(),
            };
            let response = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    theme::glyph(ui, theme::Glyph::Folder, color.gamma_multiply(0.65));
                    ui.selectable_label(
                        current,
                        RichText::new(label)
                            .font(match current {
                                true => theme::medium(12.5),
                                false => theme::body(12.5),
                            })
                            .color(color),
                    )
                })
                .inner;

            // What is open there, and what is wrong with it, said on hover
            // rather than in a second column: the sidebar is 222 pixels wide
            // and the branch names are longer than that on their own.
            let mut hint = tree.path.display().to_string();
            match (&tree.branch, tree.missing) {
                (_, true) => hint.push_str("\n\nThe folder is gone: Git kept the \
                                            bookkeeping, somebody deleted the folder"),
                (Some(branch), _) => hint.push_str(&format!("\n\n{branch} is open there")),
                (None, _) => hint.push_str("\n\nOff any branch there"),
            }
            if let Some(why) = &tree.locked {
                hint.push_str(&format!("\nLocked: {why}"));
            }
            if !current && !tree.missing {
                hint.push_str(
                    "\n\nClick to look at it here, double-click to open it in \
                     a tab of its own. Nothing is written either way",
                );
            }
            let response = response.on_hover_text(hint);

            if !busy && !tree.main {
                response.context_menu(|ui| {
                    ui.set_width(220.0);
                    if ui
                        .button("Close this checkout")
                        .on_hover_text("takes the folder away and leaves the branch alone")
                        .clicked()
                    {
                        *picked = Some(Pick::Release(tree.name.clone()));
                        ui.close_menu();
                    }
                });
            }

            if !current && !tree.missing {
                if response.double_clicked() {
                    // Whatever was being held was the first half of this.
                    waiting = None;
                    *picked = Some(Pick::VisitTab(tree.path.clone()));
                } else if response.clicked() {
                    waiting = Some((tree.path.clone(), now));
                }
            }
        }
    });

    match &waiting {
        // Long enough: nobody is coming with a second click.
        // Unless something else already spoke this frame: one ask goes back
        // at a time, and a click dropped on the floor is a click the person
        // made and nothing came of.
        Some((path, since)) if now - since >= delay && picked.is_none() => {
            *picked = Some(Pick::Visit(path.clone()));
            waiting = None;
        }
        // Still waiting, and nothing else is going to ask for a frame while
        // the pointer sits still -- so this asks for the one that lets the
        // click through.
        Some((_, since)) => {
            let left = delay - (now - since);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(left.max(0.0)));
        }
        None => {}
    }
    // Only when it moved. The sidebar is drawn every frame and a write a
    // frame to say nothing changed is still a write a frame.
    if waiting != before {
        ui.data_mut(|data| data.insert_temp(held, waiting));
    }
}

#[allow(clippy::too_many_arguments)]
fn leaf(
    ui: &mut egui::Ui,
    entry: &RefEntry,
    remote_branches: &[&RefEntry],
    local_branches: &[&RefEntry],
    selected: Option<Oid>,
    head: Option<&str>,
    busy: bool,
    remote_checkout: bool,
    picked: &mut Option<Pick>,
) {
    // The color says what kind of reference it is: locals in the graph's
    // azure, remotes muted because they usually duplicate a local one, and
    // tags in the violet of their chip.
    let color = match (entry.is_head, entry.kind) {
        (true, _) | (_, RefKind::Head) => theme::HEAD,
        (_, RefKind::LocalBranch) => theme::lane(0),
        (_, RefKind::RemoteBranch) => theme::MUTED,
        (_, RefKind::Tag) => theme::lane(6),
    };

    let font = if entry.is_head {
        theme::medium(12.5)
    } else {
        theme::body(12.5)
    };

    // Where this branch is, which is three states and not two. The sections
    // say two of them -- a row under Branches is on this disk, a row under
    // Remotes is on a remote -- and the graph's chips say it again, `master`
    // beside `origin/master`. What none of them says is whether the other
    // side has it too, so it is said in the hover and drawn nowhere: a mark
    // on every row costs every row to answer a question asked of one.
    let is_branch = matches!(entry.kind, RefKind::LocalBranch | RefKind::RemoteBranch);
    let mine = entry.branch_name();
    let on_disk = entry.kind == RefKind::LocalBranch
        || (is_branch && local_branches.iter().any(|other| other.branch_name() == mine));
    let on_remote = entry.kind == RefKind::RemoteBranch
        || (is_branch && remote_branches.iter().any(|other| other.branch_name() == mine));

    // The glyph and the name are one row, and the name alone is the thing you
    // press: a hit area that swallowed the glyph would start a drag from a
    // drawing, which is a strange place for a branch to come from.
    let is_selected = selected == Some(entry.target);
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            theme::glyph(
                ui,
                match entry.kind {
                    RefKind::Tag => theme::Glyph::Tag,
                    _ => theme::Glyph::Branch,
                },
                // Dimmer than the name beside it. It says what kind of thing
                // the row is, which you need once and then stop reading.
                color.gamma_multiply(0.65),
            );
            ui.selectable_label(
                is_selected,
                RichText::new(entry.leaf()).font(font).color(color),
            )
        })
        .inner;

    // You can go to a branch. To the one you are already on there is nowhere
    // to go, and to a tag not yet: that leaves HEAD detached, which is a
    // place you have to know how to get out of and we do not show how here.
    let branch = matches!(entry.kind, RefKind::LocalBranch | RefKind::RemoteBranch);
    let switchable = branch && !entry.is_head && !busy;

    // The row has to become a drag source and a drop target without ceasing
    // to be a button. Wrapping it in `dnd_drag_source` would have taken the
    // click, the double click and the menu with it: that call registers its
    // own interaction on top of everything it drew, and sits over the lot.
    // Raising the sense of the label already there costs none of them.
    let draggable = branch && !busy && !remote_checkout;
    let response = response.interact(egui::Sense::click_and_drag());
    if draggable {
        response.dnd_set_drag_payload(Carried { name: entry.name.clone() });
    }

    // Only a local branch can be dropped on. Whichever of the three things
    // the answer turns out to be, it is the branch underneath that ends up
    // somewhere else, and a remote branch or a tag is not one to move here.
    let takes_drops = entry.kind == RefKind::LocalBranch && !busy && !remote_checkout;
    let arriving = match takes_drops {
        true => response.dnd_hover_payload::<Carried>(),
        false => None,
    };
    if arriving.is_some_and(|carried| carried.name != entry.name) {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            egui::CornerRadius::same(theme::RADIUS),
            egui::Stroke::new(1.0, theme::HEAD),
            egui::StrokeKind::Outside,
        );
    }
    if takes_drops {
        if let Some(carried) = response.dnd_release_payload::<Carried>() {
            if carried.name != entry.name {
                *picked = Some(Pick::Join {
                    from: carried.name.clone(),
                    onto: entry.name.clone(),
                });
                return;
            }
        }
    }

    let mut hint = entry.name.clone();
    if is_branch {
        hint.push_str(match (on_disk, on_remote) {
            (true, true) => "\nOn this machine and on a remote",
            (true, false) => "\nOn this machine only: no remote has it",
            (false, true) => "\nOn a remote only: not on this machine yet",
            // Neither, which cannot happen: a branch came from one of the
            // two lists this was worked out from.
            (false, false) => "",
        });
    }
    if switchable {
        hint.push_str("\nDouble-click to switch to it");
    }
    if draggable {
        hint.push_str("\nDrag it onto a branch to merge, rebase or move it up");
    }
    let response = response.on_hover_text(hint);

    let remote = entry.kind == RefKind::RemoteBranch;
    // Which remote branch this local one would follow, by the only
    // convention there is: the same branch name on a remote. Matched on the
    // name without the remote in front, so a branch in a folder finds its
    // own counterpart and not somebody else's with the same last segment.
    let upstream: Option<String> = match entry.kind {
        RefKind::LocalBranch => remote_branches
            .iter()
            .find(|other| other.branch_name() == entry.branch_name())
            .map(|other| other.name.clone()),
        _ => None,
    };

    let on = On {
        target: entry.target,
        reference: Some(Named {
            name: entry.name.clone(),
            kind: entry.kind,
            is_head: entry.is_head,
        }),
    };
    response.context_menu(|ui| {
        menu(ui, &on, upstream.as_deref(), head, busy, remote_checkout, picked);
    });

    if switchable && response.double_clicked() {
        *picked = Some(Pick::Switch { name: entry.name.clone(), remote });
        return;
    }

    if response.clicked() {
        *picked = Some(Pick::Show(entry.target));
    }
}

/// What a menu is being opened on: a commit, and the reference sitting on it
/// when it is clear which one that is.
///
/// A sidebar row is always a reference. A row in the graph is always a
/// commit, and may have no reference at all or several, so the branch part is
/// optional and everything that needs one is simply not offered without it.
pub struct On {
    pub target: Oid,
    pub reference: Option<Named>,
}

/// The reference the menu is about, flattened out of wherever it came from.
pub struct Named {
    pub name: String,
    pub kind: RefKind,
    pub is_head: bool,
}

/// Everything that can be done to a commit, and to the branch on it.
///
/// Its own function rather than a closure inside the row: it is the longest
/// thing in this file by far, a row that also drew it was unreadable for the
/// one thing a row is about, and the graph wants the same menu.
pub fn menu(
    ui: &mut egui::Ui,
    on: &On,
    upstream: Option<&str>,
    head: Option<&str>,
    busy: bool,
    remote_checkout: bool,
    picked: &mut Option<Pick>,
) {
    let kind = on.reference.as_ref().map(|named| named.kind);
    let local = kind == Some(RefKind::LocalBranch);
    let remote = kind == Some(RefKind::RemoteBranch);
    let is_head = on.reference.as_ref().is_some_and(|named| named.is_head);

    // You can go to a branch. To the one you are already on there is nowhere
    // to go, and to a tag not here: that leaves HEAD off any branch, which
    // the commit items below say outright and this would not.
    let switchable = (local || remote) && !is_head && !busy;
    let removable = kind == Some(RefKind::Tag) && !busy;

    let target = on.target;
    let name = on.reference.as_ref().map(|named| named.name.clone()).unwrap_or_default();

    ui.set_width(286.0);

    // Where you can go. First, because it is what the menu is opened for
    // nine times out of ten.
    if switchable {
        let button = ui.button("Switch to this branch");
        let button = match remote {
            true => button.on_hover_text("makes the local branch here if it is not there yet"),
            false => button,
        };
        if button.clicked() {
            *picked = Some(Pick::Switch { name: name.clone(), remote });
            ui.close_menu();
        }
    }
    if !remote_checkout && local && !is_head && !busy && ui.button("Open it in its own folder").clicked() {
        *picked = Some(Pick::Isolate(name.clone()));
        ui.close_menu();
    }
    if !busy
        && ui
            .button("Go to this commit")
            .on_hover_text("off any branch: a commit written there belongs to nothing")
            .clicked()
    {
        *picked = Some(Pick::GoTo(target));
        ui.close_menu();
    }

    // What it can be joined with. Only against a branch you are on, and
    // never against itself.
    if let Some(head) = head.filter(|_| switchable) {
        theme::rule(ui);
        if !remote_checkout && ui
            .button(format!("Move {head} up to this"))
            .on_hover_text("and nothing else: if a merge would be needed it says so instead")
            .clicked()
        {
            *picked = Some(Pick::FastForward(name.clone()));
            ui.close_menu();
        }
        if ui
            .button(format!("Merge it into {head}"))
            .on_hover_text("if they clash, nothing is touched and it says where")
            .clicked()
        {
            *picked = Some(Pick::Merge(name.clone()));
            ui.close_menu();
        }
        if ui
            .button(format!("Rebase {head} onto this"))
            .on_hover_text("rewrites your commits: do not do it to anything others have")
            .clicked()
        {
            *picked = Some(Pick::Rebase(name.clone()));
            ui.close_menu();
        }
    }

    // The remote.
    if !busy && (local || remote) {
        theme::rule(ui);
        if is_head && ui.button("Pull").clicked() {
            *picked = Some(Pick::Pull);
            ui.close_menu();
        }
        if local && ui.button("Push it").clicked() {
            *picked = Some(Pick::Push(name.clone()));
            ui.close_menu();
        }
        if local {
            if let Some(candidate) = &upstream {
                if ui
                    .button(format!("Make it follow {candidate}"))
                    .on_hover_text("following is what makes 'two ahead' mean anything")
                    .clicked()
                {
                    *picked = Some(Pick::Follow {
                        branch: name.clone(),
                        upstream: Some(candidate.to_string()),
                    });
                    ui.close_menu();
                }
            }
            if ui.button("Make it follow nothing").clicked() {
                *picked = Some(Pick::Follow { branch: name.clone(), upstream: None });
                ui.close_menu();
            }
        }
    }

    // What can be done to this commit from where you are.
    if !busy {
        theme::rule(ui);
        if ui.button("Branch from this commit…").clicked() {
            *picked = Some(Pick::BranchAt(target));
            ui.close_menu();
        }
        if head.is_some() {
            if ui
                .button("Take this commit along")
                .on_hover_text("writes what it did here too; it stays where it is")
                .clicked()
            {
                *picked = Some(Pick::CherryPick(target));
                ui.close_menu();
            }
            if ui
                .button("Undo this commit")
                .on_hover_text("writes one that says it backwards; nothing is rewritten")
                .clicked()
            {
                *picked = Some(Pick::Revert(target));
                ui.close_menu();
            }
            if !remote_checkout && ui
                .button("Rewrite from this commit\u{2026}")
                .on_hover_text(
                    "squash, reword, drop or reorder everything from here to the tip. \
                     Nothing happens until you say so",
                )
                .clicked()
            {
                *picked = Some(Pick::Rewrite(target));
                ui.close_menu();
            }
        }
        if !remote_checkout { if let Some(head) = head {
            ui.menu_button(format!("Move {head} to this commit"), |ui| {
                ui.set_width(250.0);
                for how in [Move::Keep, Move::Unstage, Move::Discard] {
                    let label = match how {
                        Move::Keep => "Keep everything staged",
                        Move::Unstage => "Keep the files, unstage them",
                        Move::Discard => "Throw away what is not committed",
                    };
                    let text = match how {
                        Move::Discard => RichText::new(label).color(theme::REMOVED),
                        _ => RichText::new(label),
                    };
                    if ui.button(text).clicked() {
                        *picked = Some(Pick::MoveTo(target, how));
                        ui.close_menu();
                    }
                }
            });
        } }
    }

    // Naming, and unnaming.
    if !busy && (local || remote || removable) {
        theme::rule(ui);
        if local && ui.button("Rename it…").clicked() {
            *picked = Some(Pick::Rename(name.clone()));
            ui.close_menu();
        }
        if switchable {
            let (label, hint) = match remote {
                true => (
                    "Delete the branch on the remote",
                    "deleted on the server: everyone will see it",
                ),
                false => (
                    "Delete the branch",
                    "only the one here; anything on a remote stays",
                ),
            };
            if ui
                .button(RichText::new(label).color(theme::REMOVED))
                .on_hover_text(hint)
                .clicked()
            {
                *picked = Some(Pick::DeleteBranch { name: name.clone(), remote });
                ui.close_menu();
            }
        }
        if removable
            && ui
                .button(RichText::new("Delete the tag").color(theme::REMOVED))
                .on_hover_text("only the one here; the remote's stays put")
                .clicked()
        {
            *picked = Some(Pick::DeleteTag(name.clone()));
            ui.close_menu();
        }
    }

    // Things to take elsewhere.
    theme::rule(ui);
    // A commit with no reference on it has no name to copy, and offering it
    // would put an empty string on the clipboard.
    if !name.is_empty() && ui.button("Copy its name").clicked() {
        *picked = Some(Pick::Copy(name.clone()));
        ui.close_menu();
    }
    if ui.button("Copy the commit hash").clicked() {
        *picked = Some(Pick::Copy(target.to_string()));
        ui.close_menu();
    }
}
