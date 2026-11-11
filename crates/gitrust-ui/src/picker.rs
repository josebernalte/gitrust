//! The screen for choosing a repository, cloning one, or starting one.
//!
//! It is what the plus in the tab strip opens, and the only screen in the
//! application that is not about a repository: everything else in the window
//! is a reading of one, and this is the list you reach before there is one to
//! read.
//!
//! Like every other view here it touches no repository. It is handed the list
//! and a line about each one, and hands back what was asked for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};
use gitrust_core::{ssh, Known};

use crate::theme;

/// What has been asked for from the screen.
pub enum Act {
    /// Open that repository in a tab.
    Open(PathBuf),
    /// Open the authenticated checkout without ever putting its path on this
    /// computer's known-repository list.
    OpenRemote { session: ssh::Session, path: String },
    /// Bring one down from an address into a folder.
    Clone { url: String, into: PathBuf },
    /// Make an empty one there.
    Start(PathBuf),
    /// Pin it to the top, or stop.
    Star { path: PathBuf, starred: bool },
    /// Take it off the list. The folder is not touched.
    Forget(PathBuf),
    /// Put the screen away without choosing anything.
    Close,
    /// Ask the desktop for a folder, and put what comes back in a box.
    /// `beside` picks which: the one a clone lands in, or the one above it.
    Browse { beside: bool },
}

/// Which box is open, when one is.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Asking {
    Open,
    Clone,
    Remote,
    Start,
}

impl Asking {
    fn hint(self) -> &'static str {
        match self {
            Asking::Open => "path to a repository",
            Asking::Clone => "address to clone from",
            Asking::Remote => "",
            Asking::Start => "path for the new repository",
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Asking::Open => "Open",
            Asking::Clone => "Clone",
            Asking::Remote => "",
            Asking::Start => "Start",
        }
    }
}

/// What is typed into the screen, which has to survive between frames.
pub struct Picker {
    needle: String,
    pub draft: String,
    /// Where a clone would land. Only used by the clone box, where it is
    /// filled in from the address as it is typed.
    pub beside: String,
    asking: Option<Asking>,
    remote: Remote,
}

struct Remote {
    hosts: Vec<ssh::Host>,
    host: String,
    user: String,
    port: String,
    password: String,
    path: String,
    folders: Vec<String>,
    session: Option<ssh::Session>,
    note: Option<(String, bool)>,
    trust_new_host: bool,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            needle: String::new(),
            draft: String::new(),
            beside: String::new(),
            asking: None,
            remote: Remote {
                hosts: ssh::configured_hosts().unwrap_or_default(),
                host: String::new(),
                user: String::new(),
                port: "22".to_string(),
                password: String::new(),
                path: "/".to_string(),
                folders: Vec::new(),
                session: None,
                note: None,
                trust_new_host: false,
            },
        }
    }
}

impl Picker {
    /// Whether what is being asked for is a place to make a new repository.
    ///
    /// It is the one case where a folder that was picked is not the answer
    /// but the start of one: a chooser only offers folders that exist, and
    /// the new one usually goes inside the one that was picked.
    pub fn starting(&self) -> bool {
        self.asking == Some(Asking::Start)
    }

    /// Where a clone typed in right now would go.
    fn landing(&self) -> Option<PathBuf> {
        gitrust_core::start::folder_for(&self.draft, Path::new(self.beside.trim()))
    }
}

/// Draws the screen and gives back whatever was asked of it.
///
/// `glance` is a line about each repository -- its branch and how far it is
/// from its remote -- worked out once by whoever opened this screen. Reading
/// it here, per row and per frame, would be opening twenty repositories sixty
/// times a second.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut Picker,
    known: &[Known],
    glance: &HashMap<PathBuf, String>,
    open_now: &[PathBuf],
    saved_remotes: &[ssh::RemoteDescriptor],
) -> Option<Act> {
    let mut act = None;

    ui.add_space(16.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.label(
            RichText::new("Repositories")
                .font(theme::strong(17.0))
                .color(theme::TEXT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("Close").min_size(egui::vec2(72.0, 26.0)))
                .clicked()
            {
                act = Some(Act::Close);
            }
        });
    });

    ui.add_space(12.0);
    if let Some(asked) = buttons(ui, state, saved_remotes) {
        act = Some(asked);
    }

    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::singleline(&mut state.needle)
                .desired_width(ui.available_width() - 8.0)
                .margin(egui::vec2(8.0, 6.0))
                .font(theme::body(12.5))
                .hint_text(RichText::new("Find a repository").color(theme::FAINT)),
        );
    });
    ui.add_space(12.0);

    let needle = state.needle.trim().to_lowercase();
    let matching: Vec<&Known> = known
        .iter()
        .filter(|known| {
            needle.is_empty()
                || known.name().to_lowercase().contains(&needle)
                || known
                    .path
                    .display()
                    .to_string()
                    .to_lowercase()
                    .contains(&needle)
        })
        .collect();

    if known.is_empty() {
        theme::empty(
            ui,
            "No repositories yet. Open one, clone one, or start one.",
        );
        return act;
    }
    if matching.is_empty() {
        theme::empty(ui, "No repository by that name");
        return act;
    }

    // Three lists out of one, and a repository can be in two of them: the one
    // you are looking at can also be a favourite. Saying it twice is better
    // than making somebody look in both places to find out.
    let here: Vec<&Known> = matching
        .iter()
        .copied()
        .filter(|k| open_now.contains(&k.path))
        .collect();
    let starred: Vec<&Known> = matching.iter().copied().filter(|k| k.starred).collect();
    let rest: Vec<&Known> = matching
        .iter()
        .copied()
        .filter(|k| !k.starred && !open_now.contains(&k.path))
        .collect();

    egui::ScrollArea::vertical()
        .id_salt("picker")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (title, mark, group) in [
                ("Open now", theme::Glyph::Trees, here),
                ("Favourites", theme::Glyph::Tag, starred),
                ("Everything else", theme::Glyph::Folder, rest),
            ] {
                if group.is_empty() {
                    continue;
                }
                theme::section(ui, title, mark, group.len(), !needle.is_empty(), |ui| {
                    for known in group {
                        if let Some(asked) = row(ui, known, glance, open_now) {
                            act = Some(asked);
                        }
                    }
                });
            }
            ui.add_space(16.0);
        });

    act
}

/// The three ways a repository gets here, and the box behind whichever was
/// pressed.
fn buttons(
    ui: &mut egui::Ui,
    state: &mut Picker,
    saved_remotes: &[ssh::RemoteDescriptor],
) -> Option<Act> {
    let mut act = None;

    ui.horizontal(|ui| {
        ui.add_space(4.0);
        for asking in [Asking::Open, Asking::Clone, Asking::Remote, Asking::Start] {
            let on = state.asking == Some(asking);
            let label = match asking {
                Asking::Open => "Open a folder",
                Asking::Clone => "Clone from an address",
                Asking::Remote => "Open remote checkout",
                Asking::Start => "Start an empty one",
            };
            if ui
                .selectable_label(on, RichText::new(label).font(theme::body(12.5)))
                .clicked()
            {
                // Pressing the one already open puts it away, which is what
                // anybody expects of a control that looks pressed in.
                state.asking = (!on).then_some(asking);
                state.draft.clear();
                if asking == Asking::Clone && state.beside.is_empty() {
                    // Somewhere to start from, since a clone needs a folder
                    // to land beside and typing a whole path to say "here" is
                    // a lot of typing.
                    state.beside = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                }
            }
        }
    });

    let Some(asking) = state.asking else {
        return None;
    };

    if asking == Asking::Remote {
        return remote_picker(ui, &mut state.remote, saved_remotes);
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let room = ui.available_width() - 90.0;
        let entry = ui.add(
            egui::TextEdit::singleline(&mut state.draft)
                .desired_width(room)
                .margin(egui::vec2(8.0, 6.0))
                .font(theme::mono(12.0))
                .hint_text(RichText::new(asking.hint()).color(theme::FAINT)),
        );
        // Only while nothing else has it: asking every frame outright would
        // take it back off the button the moment the pointer put it there.
        if ui.memory(|memory| memory.focused().is_none()) {
            entry.request_focus();
        }

        // Only where a path is being asked for. There is no folder on disk
        // to point at when what is wanted is an address to clone from.
        if asking != Asking::Clone && ui.button("Browse\u{2026}").clicked() {
            act = Some(Act::Browse { beside: false });
        }

        let typed = !state.draft.trim().is_empty();
        let go = ui.add_enabled(
            typed,
            egui::Button::new(RichText::new(asking.verb()).font(theme::medium(12.5)))
                .fill(theme::CHOSEN)
                .min_size(egui::vec2(76.0, 28.0)),
        );
        let entered = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

        if typed && (go.clicked() || entered) {
            let path = PathBuf::from(state.draft.trim());
            act = match asking {
                Asking::Open => Some(Act::Open(path)),
                Asking::Start => Some(Act::Start(path)),
                Asking::Clone => state.landing().map(|into| Act::Clone {
                    url: state.draft.trim().to_string(),
                    into,
                }),
                Asking::Remote => None,
            };
        }
    });

    // Where a clone will land, said before it lands rather than after.
    if asking == Asking::Clone {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(
                RichText::new("into")
                    .font(theme::body(11.5))
                    .color(theme::FAINT),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.beside)
                    .desired_width(240.0)
                    .margin(egui::vec2(6.0, 4.0))
                    .font(theme::mono(11.0)),
            );
            if ui
                .small_button("\u{2026}")
                .on_hover_text("Pick the folder")
                .clicked()
            {
                act = Some(Act::Browse { beside: true });
            }
            let landing = state
                .landing()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "nowhere yet".to_string());
            ui.label(
                RichText::new(landing)
                    .font(theme::mono(11.0))
                    .color(theme::MUTED),
            );
        });
    }

    // Browse is the one answer that leaves the box where it is: what comes
    // back goes into it, and nothing has been chosen yet.
    if act.is_some() && !matches!(act, Some(Act::Browse { .. })) {
        state.asking = None;
        state.draft.clear();
    }
    act
}

/// The first half of opening a remote checkout: authenticate and choose a
/// folder. A verified session remains here until the remote-tab backend is
/// ready to take ownership of it.
fn remote_picker(
    ui: &mut egui::Ui,
    remote: &mut Remote,
    saved_remotes: &[ssh::RemoteDescriptor],
) -> Option<Act> {
    let mut open = None;
    ui.add_space(8.0);
    if !saved_remotes.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("Saved checkouts")
                    .font(theme::body(11.5))
                    .color(theme::FAINT),
            );
            for saved in saved_remotes {
                let label = format!(
                    "{}@{}:{}{}",
                    saved.login.user, saved.login.host, saved.login.port, saved.path
                );
                if ui.small_button(label).clicked() {
                    remote.host = saved.login.host.clone();
                    remote.user = saved.login.user.clone();
                    remote.port = saved.login.port.to_string();
                    remote.path = saved.path.clone();
                    remote.session = None;
                    remote.note = Some((
                        "Press Connect to authenticate again; no password was saved.".to_string(),
                        false,
                    ));
                }
            }
        });
        ui.add_space(5.0);
    }
    if !remote.hosts.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("SSH hosts")
                    .font(theme::body(11.5))
                    .color(theme::FAINT),
            );
            for saved in &remote.hosts {
                if ui.small_button(&saved.alias).clicked() {
                    remote.host = saved
                        .hostname
                        .clone()
                        .unwrap_or_else(|| saved.alias.clone());
                    remote.user = saved.user.clone().unwrap_or_default();
                    remote.port = saved.port.unwrap_or(22).to_string();
                    remote.session = None;
                    remote.note = None;
                }
            }
        });
        ui.add_space(5.0);
    }
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut remote.user)
                .hint_text("user")
                .desired_width(120.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut remote.host)
                .hint_text("host")
                .desired_width(190.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut remote.port)
                .hint_text("port")
                .desired_width(62.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut remote.password)
                .password(true)
                .hint_text("password")
                .desired_width(150.0),
        );
        let ready = !remote.user.trim().is_empty() && !remote.host.trim().is_empty();
        if ui
            .add_enabled(ready, egui::Button::new("Connect").fill(theme::CHOSEN))
            .clicked()
        {
            let port = remote.port.trim().parse().unwrap_or(22);
            let password = std::mem::take(&mut remote.password);
            // Trust is a confirmation for this one connection attempt, never
            // a broad setting that could accept a later, unrelated host.
            let trust_new_host = std::mem::replace(&mut remote.trust_new_host, false);
            match ssh::Login::new(
                remote.host.trim().to_string(),
                remote.user.trim().to_string(),
                port,
            )
            .and_then(|login| {
                ssh::Session::connect(
                    login,
                    (!password.is_empty()).then_some(password.as_str()),
                    trust_new_host,
                )
            }) {
                Ok(session) => match session.folders("/") {
                    Ok(folders) => {
                        remote.session = Some(session);
                        remote.folders = folders;
                        remote.note = Some((
                            "Connected. Choose the remote checkout folder.".to_string(),
                            false,
                        ));
                    }
                    Err(err) => remote.note = Some((format!("SFTP: {err:#}"), true)),
                },
                Err(err) => remote.note = Some((format!("SSH: {err:#}"), true)),
            }
        }
    });
    ui.checkbox(
        &mut remote.trust_new_host,
        "I verified this host key; trust and save it for this attempt",
    );
    if let Some((note, bad)) = &remote.note {
        ui.add_space(4.0);
        ui.label(RichText::new(note).font(theme::body(11.5)).color(if *bad {
            theme::REMOVED
        } else {
            theme::MUTED
        }));
    }
    let Some(session) = remote.session.as_ref() else {
        return None;
    };
    let mut use_folder = false;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&remote.path)
                .font(theme::mono(11.5))
                .color(theme::TEXT),
        );
        if remote.path != "/" && ui.small_button("Up").clicked() {
            let parent = Path::new(&remote.path)
                .parent()
                .unwrap_or(Path::new("/"))
                .display()
                .to_string();
            match session.folders(&parent) {
                Ok(folders) => {
                    remote.path = parent;
                    remote.folders = folders;
                }
                Err(err) => remote.note = Some((format!("SFTP: {err:#}"), true)),
            }
        }
        if ui.button("Use this folder").clicked() {
            remote.note = Some(match session.verify_checkout(&remote.path) {
                Ok(_) => {
                    use_folder = true;
                    (format!("Opening {}…", remote.path), false)
                }
                Err(err) => (format!("Git: {err:#}"), true),
            });
        }
    });
    egui::ScrollArea::vertical()
        .max_height(180.0)
        .show(ui, |ui| {
            for folder in remote.folders.clone() {
                if ui
                    .selectable_label(false, RichText::new(&folder).font(theme::mono(11.5)))
                    .clicked()
                {
                    match session.folders(&folder) {
                        Ok(folders) => {
                            remote.path = folder;
                            remote.folders = folders;
                        }
                        Err(err) => remote.note = Some((format!("SFTP: {err:#}"), true)),
                    }
                }
            }
        });
    if use_folder {
        let session = remote.session.take().expect("session checked above");
        open = Some(Act::OpenRemote {
            session,
            path: remote.path.clone(),
        });
    }
    open
}

/// One repository: what it is called, where it is, and what it was at.
fn row(
    ui: &mut egui::Ui,
    known: &Known,
    glance: &HashMap<PathBuf, String>,
    open_now: &[PathBuf],
) -> Option<Act> {
    let mut act = None;
    let there = known.there();
    let open = open_now.contains(&known.path);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        // The star is a button whether or not it is on, so the row does not
        // change shape when you press it.
        let star = ui
            .add(
                egui::Button::new(
                    RichText::new(match known.starred {
                        true => "\u{2605}",
                        false => "\u{2606}",
                    })
                    .font(theme::body(13.0))
                    .color(match known.starred {
                        true => theme::HEAD,
                        false => theme::FAINT,
                    }),
                )
                .frame(false)
                .min_size(egui::vec2(18.0, 18.0)),
            )
            .on_hover_text(match known.starred {
                true => "Stop keeping it at the top",
                false => "Keep it at the top",
            });
        if star.clicked() {
            act = Some(Act::Star {
                path: known.path.clone(),
                starred: !known.starred,
            });
        }

        let name = ui.selectable_label(
            open,
            RichText::new(known.name())
                .font(theme::medium(12.5))
                .color(match there {
                    true => theme::TEXT,
                    false => theme::FAINT,
                }),
        );

        ui.label(
            RichText::new(known.parent())
                .font(theme::mono(11.0))
                .color(theme::FAINT),
        );

        // A folder that is gone is not the same as a repository that is gone:
        // a disk comes back. It is said and left on the list.
        if !there {
            ui.label(
                RichText::new("not where it was")
                    .font(theme::body(11.0))
                    .color(theme::REMOVED),
            );
        } else if let Some(line) = glance.get(&known.path) {
            ui.label(
                RichText::new(line)
                    .font(theme::mono(11.0))
                    .color(theme::MUTED),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new("\u{00d7}")
                            .font(theme::body(13.0))
                            .color(theme::FAINT),
                    )
                    .frame(false)
                    .min_size(egui::vec2(18.0, 18.0)),
                )
                .on_hover_text("Forget it. The folder is not touched")
                .clicked()
            {
                act = Some(Act::Forget(known.path.clone()));
            }
        });

        let name = name.on_hover_text(match (there, open) {
            (false, _) => format!("{}\n\nNothing there right now", known.path.display()),
            (_, true) => format!("{}\n\nAlready open in a tab", known.path.display()),
            _ => format!("{}\n\nClick to open it in a tab", known.path.display()),
        });
        if name.clicked() && there {
            act = Some(Act::Open(known.path.clone()));
        }
    });
    ui.add_space(2.0);

    act
}
