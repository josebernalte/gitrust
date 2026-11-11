//! gitrust: a desktop Git client.
//!
//! The window follows the layout of the usual graphical clients: references
//! on the left, the graph in the middle and the detail of what is picked on
//! the right, so that clicking in the graph shows the diff without changing
//! screen.

// On Windows a program is either a console program or a window program, and
// the default is the console one: a black terminal opens behind the window
// and sits there for as long as the window is up. Released builds say they
// are a window; a debug build keeps the console, because that is where a
// panic goes and losing it is losing the one thing worth reading. Ignored
// everywhere else.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod action_bar;
mod board_view;
mod changes_view;
mod chooser;
mod conflict_view;
mod detail_view;
mod diff_view;
mod file_view;
mod graph_view;
mod picker;
mod refs_view;
mod rewrite_view;
mod ripple;
mod syntax;
mod theme;
mod toast;

use anyhow::Result;
use eframe::egui;
use gitrust_core::{
    compose, layout, layout_with_working, Backlog, Board, CommitDetail, FileDiff, Graph, HunkId,
    Join, Move, Oid, PullMode, PullRequests, Reader, RefEntry, RefKind, RemoteRepo, Repo, Settings,
    StashEntry, Tracking, WorkingState, Worktree, Writer,
};

/// How wide the repository menu is, measured inside its frame.
///
/// Told, not asked for, for the same reason the action bar's menus are: a
/// menu that works its own width out from the room it is given answers a
/// little differently every frame, and the answers drift. Wide enough for a
/// path you can read and for the folders listed under it.
const REPO_MENU: f32 = 520.0;

/// How wide the menu a drop opens is.
///
/// Set, not merely asked for: a minimum leaves egui free to remember a wider
/// rectangle from last time and hand it back a notch wider on every opening,
/// which is exactly what the repository menu above used to do.
const JOIN_MENU: f32 = 300.0;

/// How many commits are walked looking for the ones that touched a file.
///
/// Git keeps no history for a file, so this is found by reading commits one
/// by one. The number is a guess at where somebody stops caring, and the view
/// says out loud when it was reached rather than handing back a short answer
/// that looks complete.
const FILE_HISTORY: usize = 4_000;

/// The window's icon, carried inside the binary so that the program run
/// straight out of `target/release` looks like itself and not like a
/// nameless window. 256 is the largest any title bar or switcher asks for.
const ICON: &[u8] = include_bytes!("../../../assets/icon-256.png");

/// The line the project goes by, where somebody reads it before anything
/// else: the title bar, and whatever the desktop's switcher puts under the
/// icon.
///
/// It says what the two halves of the name are for. Trust is git's own
/// promise -- a hash that cannot be quietly changed -- and this program is
/// here to let somebody see it rather than take it on faith. The README
/// says the same at the length a README has.
const SLOGAN: &str = "Trust, protected. Code, eternal.";

/// What `--help` prints, and what `--version` prints the first line of. A
/// graphical program still gets run from a terminal once -- by whoever is
/// about to report a bug and is asked which version they have -- and a
/// window that opens on a repository called `--version` is the wrong answer
/// to that question.
fn tell_and_stop(arg: &str) -> bool {
    match arg {
        "-V" | "--version" => {
            println!("gitrust {}", env!("CARGO_PKG_VERSION"));
            true
        }
        "-h" | "--help" => {
            println!(
                "gitrust {} \u{2014} {}\n\n\
                 Usage: gitrust [REPOSITORY]\n\n\
                 With no argument it opens the repository in the current\n\
                 directory. A folder dragged onto the window opens too.\n\n\
                 \x20 -V, --version   print the version and stop\n\
                 \x20 -h, --help      print this and stop\n\n\
                 {}\n",
                env!("CARGO_PKG_VERSION"),
                SLOGAN,
                env!("CARGO_PKG_REPOSITORY"),
            );
            true
        }
        _ => false,
    }
}

fn main() -> eframe::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    if tell_and_stop(&path) {
        return Ok(());
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1500.0, 900.0])
        .with_min_inner_size([900.0, 480.0])
        // Capitalised here and nowhere else. This is the name read by a
        // person, in a title bar and a window switcher, among the names of
        // every other program they have open -- all of which are
        // capitalised. Everywhere a machine reads it instead -- the binary,
        // the crate, the app id below -- it stays lowercase, because those
        // are not names, they are identifiers that have to match.
        .with_title(format!("Gitrust \u{2014} {SLOGAN}"))
        // What the desktop files this window under. It has to be the name of
        // the .desktop file in packaging/ word for word: on Wayland the icon
        // comes from that file and never from the window, so the one below
        // is for X11 and for Windows, and this line is what gets an icon on
        // the dock everywhere else.
        .with_app_id("gitrust");

    // Reading it costs no new dependency: the decoder is already in here,
    // pulled in by eframe. If it ever fails to read, the window goes up
    // without an icon -- the wrong picture on a title bar is not a reason to
    // refuse to open a repository.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(ICON) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "gitrust",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, path)))),
    )
}

/// The window: the repositories open in it, and which one is being looked at.
///
/// Everything else lives in [`Open`], and that is not a judgement call -- when
/// the time came to split this in two, every single thing the window
/// remembered turned out to be about a repository: the graph, the working
/// tree, what is half-typed into the commit box, the operation running off to
/// one side. Nothing was left over to be shared. So tabs are a list of those
/// and a finger on one of them, and not a layer over something common.
struct App {
    open: Vec<Open>,
    /// Which one is on top. Kept inside the list by `close`, the only thing
    /// that can make the list shorter.
    at: usize,
    /// How many tabs have ever been opened. It is where each one's egui salt
    /// comes from, so it counts up and never counts down.
    opened: u64,
    /// The screen for choosing a repository, while it is up. The plus opens
    /// it; it is the one screen here that is not about a repository.
    picking: Option<picker::Picker>,
    /// The repositories this machine knows, read when the screen opens so the
    /// rows are not asking the disk sixty times a second.
    known: Vec<gitrust_core::Known>,
    /// What each of them was at, worked out at the same time.
    glance: std::collections::HashMap<std::path::PathBuf, String>,
    /// A clone on its way down, when one is.
    cloning: Option<Cloning>,
    /// A folder being chosen in the desktop's own dialog, and which box the
    /// answer belongs in.
    browsing: Option<(
        bool,
        std::sync::mpsc::Receiver<Result<Option<std::path::PathBuf>>>,
    )>,
    /// What somebody has said they prefer. It is about this computer, not
    /// about a repository, so it is held here and lent to whichever tab is
    /// on top rather than being a copy per tab that would drift apart.
    settings: Settings,
}

/// A repository being brought down, and where it is going.
struct Cloning {
    into: std::path::PathBuf,
    done: std::sync::mpsc::Receiver<Result<String>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: String) -> Self {
        theme::install(&cc.egui_ctx);
        let mut app = Self {
            open: vec![Open::new(path, 0)],
            at: 0,
            opened: 1,
            picking: None,
            known: Vec::new(),
            glance: std::collections::HashMap::new(),
            cloning: None,
            browsing: None,
            settings: Settings::load(),
        };
        app.remember(0);
        app
    }

    /// Opens a folder in a tab of its own, or goes to the tab already on it.
    ///
    /// Two tabs onto one working tree would be two windows onto one set of
    /// files with nothing to tell them apart, and the second one would
    /// quietly disagree with the first as soon as anything was staged.
    fn open_tab(&mut self, path: std::path::PathBuf) {
        match self
            .open
            .iter()
            .position(|open| std::path::Path::new(&open.path) == path)
        {
            Some(which) => {
                self.at = which;
                self.picking = None;
            }
            None => self.add(path.display().to_string()),
        }
    }

    fn add(&mut self, path: String) {
        self.open.push(Open::new(path, self.opened));
        self.opened += 1;
        self.at = self.open.len() - 1;
        self.picking = None;
        self.remember(self.at);
    }

    fn add_remote(&mut self, session: gitrust_core::ssh::Session, path: String) {
        match RemoteRepo::new(session, path) {
            Ok(repo) => {
                self.open.push(Open::new_remote(repo, self.opened));
                self.opened += 1;
                self.at = self.open.len() - 1;
                self.picking = None;
            }
            Err(err) => self.complain(err),
        }
    }

    /// Writes down that a repository was opened, so the screen can offer it
    /// again next time.
    ///
    /// A path that is not a repository is not remembered and does not
    /// complain: the tab it was opened in will say what is wrong with it,
    /// and saying it twice in two places helps nobody.
    fn remember(&mut self, which: usize) {
        let Some(open) = self.open.get(which) else {
            return;
        };
        let path = std::path::PathBuf::from(&open.path);
        if Repo::open(&open.path).is_err() {
            return;
        }
        let now = chrono::Local::now().timestamp();
        let _ = gitrust_core::known::seen(&path, now);
    }

    /// Opens the screen, reading the list and a line about each one.
    ///
    /// Done here, once, rather than in the view: a row that asked the disk
    /// what branch it was on would ask it again every frame, for every
    /// repository on the list.
    fn pick(&mut self) {
        self.known = gitrust_core::known::all();
        self.glance = self
            .known
            .iter()
            .filter(|known| known.there())
            .filter_map(|known| {
                let repo = Repo::open(&known.path.display().to_string()).ok()?;
                let head = repo.head_name();
                let line = match repo.tracking() {
                    Some(t) if t.ahead > 0 && t.behind > 0 => {
                        format!("{head}  \u{2191}{}  \u{2193}{}", t.ahead, t.behind)
                    }
                    Some(t) if t.ahead > 0 => format!("{head}  \u{2191}{}", t.ahead),
                    Some(t) if t.behind > 0 => format!("{head}  \u{2193}{}", t.behind),
                    _ => head,
                };
                Some((known.path.clone(), line))
            })
            .collect();
        self.picking = Some(picker::Picker::default());
    }

    /// Acts on what the screen asked for.
    fn act_on_pick(&mut self, act: picker::Act) {
        match act {
            picker::Act::Open(path) => self.open_tab(path),
            picker::Act::OpenRemote { session, path } => self.add_remote(session, path),
            picker::Act::Start(path) => match gitrust_core::start::init(&path) {
                Ok(_) => self.add(path.display().to_string()),
                Err(err) => self.complain(err),
            },
            // The only slow one, and the only one that goes near a network.
            // It runs where it will not hold the window still, and the tab it
            // lands in is opened when it is there.
            picker::Act::Clone { url, into } => self.start_clone(url, into),
            picker::Act::Star { path, starred } => {
                let _ = gitrust_core::known::star(&path, starred);
                self.known = gitrust_core::known::all();
            }
            picker::Act::Forget(path) => {
                let _ = gitrust_core::known::forget(&path);
                self.known = gitrust_core::known::all();
            }
            picker::Act::Close => self.picking = None,
            picker::Act::Browse { beside } => self.browse(beside),
        }
    }

    /// Asks the desktop to put its own folder chooser up.
    ///
    /// On a thread, because it does not come back until somebody has looked
    /// at the dialog and decided, and that can be minutes.
    /// Whether the screen is asking where to start an empty repository.
    fn starting(&self) -> bool {
        self.picking.as_ref().is_some_and(|state| state.starting())
    }

    fn browse(&mut self, beside: bool) {
        if self.browsing.is_some() {
            return;
        }
        let (send, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(chooser::folder("Choose a repository folder"));
        });
        self.browsing = Some((beside, done));
    }

    fn poll_browse(&mut self, ctx: &egui::Context) {
        let Some((beside, done)) = &self.browsing else {
            return;
        };
        let beside = *beside;
        match done.try_recv() {
            Ok(found) => {
                self.browsing = None;
                match found {
                    // Cancelled. An ordinary answer, and nothing to say about it.
                    Ok(None) => {}
                    // Choosing a folder in a dialog called "choose a
                    // repository folder" is the decision. Putting it in a box
                    // and waiting for a second Open was a step that asked
                    // somebody to say yes to what they had just said yes to.
                    //
                    // The two boxes that are not that are left filled in
                    // instead: a clone still needs its address, and starting
                    // an empty repository usually means a folder inside the
                    // one that was picked, with a name yet to be typed.
                    Ok(Some(path)) => match (beside, self.starting()) {
                        (false, false) => self.act_on_pick(picker::Act::Open(path)),
                        _ => {
                            if let Some(state) = &mut self.picking {
                                let box_ = match beside {
                                    true => &mut state.beside,
                                    false => &mut state.draft,
                                };
                                *box_ = path.display().to_string();
                            }
                        }
                    },
                    Err(err) => self.complain(err),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.browsing = None,
        }
    }

    /// A folder dragged onto the window from a file manager.
    ///
    /// Free, and it works everywhere: eframe hands over whatever was dropped
    /// without anybody asking a desktop for anything. It is the shortest road
    /// from "that folder there" to a tab, and on a machine with no portal it
    /// is the only one that is not typing.
    fn poll_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });

        for path in dropped {
            // A file inside a repository is somebody aiming at the repository
            // and hitting one of its files, which is easy to do and plainly
            // meant. A file on its own is not a repository and says so.
            let folder = match path.is_dir() {
                true => path,
                false => match path.parent() {
                    Some(parent) => parent.to_path_buf(),
                    None => continue,
                },
            };
            match Repo::open(&folder.display().to_string()) {
                Ok(_) => self.act_on_pick(picker::Act::Open(folder)),
                Err(err) => self.complain(err),
            }
        }
    }

    fn complain(&mut self, err: anyhow::Error) {
        if let Some(open) = self.open.get_mut(self.at) {
            open.note_error(format!("{err:#}"));
        }
    }

    /// Takes a tab away.
    ///
    /// The last one does not close. A window with no repository in it would
    /// have nothing to draw and nothing to say, and "quit" is a thing the
    /// window manager already does better.
    fn close(&mut self, which: usize) {
        if self.open.len() < 2 {
            return;
        }
        self.open.remove(which);
        // Standing on a tab to the right of the one that went, you were
        // looking at a repository that has now shifted one place left.
        if self.at >= which {
            self.at = self.at.saturating_sub(1);
        }
        self.at = self.at.min(self.open.len() - 1);
    }

    /// The strip of tabs, and the box for opening another.
    fn strip(&mut self, ctx: &egui::Context) {
        let mut go = None;
        let mut shut = None;

        egui::TopBottomPanel::top("tabs")
            .frame(
                egui::Frame::NONE
                    .fill(theme::CANVAS)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 5,
                        bottom: 0,
                    }),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    for (which, open) in self.open.iter().enumerate() {
                        let here = which == self.at;
                        // The folder's own name. The whole path is too long
                        // for a tab and the part that differs is the end.
                        let name = std::path::Path::new(&open.path)
                            .file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_else(|| open.path.clone());

                        let label = ui
                            .selectable_label(
                                here,
                                egui::RichText::new(name)
                                    .font(match here {
                                        true => theme::medium(12.5),
                                        false => theme::body(12.5),
                                    })
                                    .color(match here {
                                        true => theme::TEXT,
                                        false => theme::MUTED,
                                    }),
                            )
                            .on_hover_text(&open.path);
                        if label.clicked() {
                            go = Some(which);
                        }

                        // The cross is on the tab you are on and on whatever
                        // the pointer is over, so the strip is not a row of
                        // crosses asking to be pressed by accident.
                        let shown = here || label.hovered();
                        if shown && self.open.len() > 1 {
                            let cross = ui.add(
                                egui::Button::new(
                                    egui::RichText::new("\u{00d7}")
                                        .font(theme::body(13.0))
                                        .color(theme::FAINT),
                                )
                                .frame(false)
                                .min_size(egui::vec2(16.0, 16.0)),
                            );
                            if cross.on_hover_text("Close this tab").clicked() {
                                shut = Some(which);
                            }
                        }
                        ui.add_space(6.0);
                    }

                    self.plus(ui);
                });
                ui.add_space(4.0);
            });

        if let Some(which) = shut {
            self.close(which);
            return;
        }
        if let Some(which) = go {
            self.at = which;
        }
    }

    /// The button that opens the screen for choosing a repository.
    fn plus(&mut self, ui: &mut egui::Ui) {
        let button = ui.add(
            egui::Button::new(
                egui::RichText::new("+")
                    .font(theme::body(14.0))
                    .color(theme::MUTED),
            )
            .frame(false)
            .min_size(egui::vec2(22.0, 20.0)),
        );
        if button.on_hover_text("Open another repository").clicked() {
            self.pick();
        }
    }

    /// Brings a repository down, off to one side.
    ///
    /// The only thing in this window that can take minutes, and the only one
    /// that goes near a network without a repository behind it. The tab is
    /// opened when the clone is there, not when it is asked for: a tab onto a
    /// folder that is still filling up has nothing to show and no way to say
    /// why.
    fn start_clone(&mut self, url: String, into: std::path::PathBuf) {
        let (send, done) = std::sync::mpsc::channel();
        let where_to = into.clone();
        std::thread::spawn(move || {
            let _ = send.send(gitrust_core::start::clone(&url, &where_to));
        });
        self.cloning = Some(Cloning { into, done });
    }

    /// Looks at whether the clone has finished.
    fn poll_clone(&mut self, ctx: &egui::Context) {
        let Some(cloning) = &self.cloning else {
            return;
        };
        match cloning.done.try_recv() {
            Ok(result) => {
                let into = cloning.into.clone();
                self.cloning = None;
                match result {
                    Ok(_) => self.add(into.display().to_string()),
                    Err(err) => self.complain(err),
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Without this egui goes to sleep as soon as nobody touches
                // the mouse, and a clone that finished would sit there unseen
                // until something else woke the window.
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.cloning = None,
        }
    }

    /// The screen, drawn over everything instead of a repository.
    fn picker(&mut self, ctx: &egui::Context) {
        let Some(state) = &mut self.picking else {
            return;
        };

        let open_now: Vec<std::path::PathBuf> = self
            .open
            .iter()
            .map(|open| std::path::PathBuf::from(&open.path))
            .collect();
        let known = std::mem::take(&mut self.known);
        let glance = std::mem::take(&mut self.glance);

        let mut act = None;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::NONE
                    .fill(theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(ctx, |ui| {
                act = picker::show(ui, state, &known, &glance, &open_now);

                if let Some(cloning) = &self.cloning {
                    ui.add_space(4.0);
                    theme::empty(
                        ui,
                        &format!("Cloning into {}\u{2026}", cloning.into.display()),
                    );
                }
            });

        self.known = known;
        self.glance = glance;

        if let Some(act) = act {
            self.act_on_pick(act);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Every tab is polled, not only the one on top. An operation left
        // running in another one has to finish whether or not you are looking
        // at it; otherwise coming back to it would find it still saying
        // "pushing…" with the push long since done.
        for open in &mut self.open {
            open.poll(ctx);
            open.poll_forge(ctx);
            open.poll_board(ctx);
        }

        // Always, even with one repository open: the plus lives in it, and a
        // strip that appears only once you have two tabs is a way of opening
        // a second tab that you can only reach by having one.
        self.strip(ctx);
        self.poll_clone(ctx);
        self.poll_browse(ctx);
        self.poll_dropped(ctx);

        // One or the other, never both: the screen is not a dialog over a
        // repository, it is what is there when you have not picked one.
        match self.picking.is_some() {
            true => self.picker(ctx),
            false => {
                let mut elsewhere = None;
                if let Some(open) = self.open.get_mut(self.at) {
                    open.draw(ctx, &mut self.settings);
                    elsewhere = open.elsewhere.take();
                }
                if let Some(path) = elsewhere {
                    self.open_tab(path);
                }
            }
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum View {
    History,
    Changes,
    Board,
}

struct Open {
    /// What keeps this tab's egui state apart from the other tabs'. Scroll
    /// positions, folded sections and open popups are remembered by widget
    /// id, and without something to tell two tabs apart they would all
    /// remember one window between them.
    ///
    /// It is handed out on creation and never reused, rather than being the
    /// tab's position: closing a tab would otherwise shift every tab after it
    /// onto somebody else's remembered state.
    salt: u64,
    path: String,
    /// A remote tab owns the authenticated session for its whole lifetime.
    remote: Option<RemoteRepo>,
    /// A folder this tab has asked for a tab of its own.
    ///
    /// A tab cannot make a tab: the list of them belongs to the window, and
    /// this is the whole of what one tab can say about it. Taken by the
    /// window on the frame it is set and never read twice.
    elsewhere: Option<std::path::PathBuf>,
    head: String,
    /// What is in the repository: it stays put in the bottom bar.
    summary: String,
    /// The last thing that happened. It is cleared on a reload.
    status: String,
    status_failed: bool,
    view: View,

    graph: Graph,
    limit: usize,
    /// What is being looked for in the history, and where it is.
    needle: String,
    found: Vec<usize>,
    at: usize,
    /// The row to bring into view on the next frame.
    bring: Option<usize>,
    refs: Vec<RefEntry>,
    ref_filter: String,

    /// The work laid out in columns, and which project it is filtered by.
    board: Option<Board>,
    project: Option<String>,
    /// Where you are on the board: the trunk, then every branch you went
    /// into. The last one is what the columns are measured against.
    trail: Vec<String>,
    /// Every folder this repository is checked out in, the main one first.
    worktrees: Vec<Worktree>,
    /// The one column that is stored rather than worked out, and what is being
    /// typed into it.
    backlog: Backlog,
    backlog_draft: String,
    /// What the forge last said, and the asking of it.
    pulls: Option<PullRequests>,
    forge: Option<Forge>,
    /// The board being worked out, when one is.
    laying: Option<Laying>,
    /// Why the forge could not answer, if it could not. It is kept so the
    /// board can say why it is guessing instead of quietly guessing.
    forge_note: Option<String>,
    /// The fetch button's position: it repeats the last thing asked for.
    pull_mode: PullMode,

    // The action bar and what it needs to know.
    tracking: Option<Tracking>,
    stashes: Vec<StashEntry>,
    draft: action_bar::Draft,
    job: Option<Job>,

    // The commit picked in the graph.
    selected_commit: Option<Oid>,
    detail: Option<CommitDetail>,
    detail_file: Option<String>,
    detail_diff: Option<FileDiff>,

    /// The file being looked at across the window, when one is. It takes the
    /// middle of the window from the graph while it is up.
    looking: Option<file_view::Looking>,
    /// What is half-done here, read on every load. It is the one thing that
    /// changes what the whole window is for, so it is not asked for: it is
    /// known.
    half: gitrust_core::Half,
    clashes: Vec<gitrust_core::Clash>,
    /// The conflict screen, while it is up.
    deciding: Option<conflict_view::Deciding>,
    /// The plan for rewriting a run of commits, while one is being arranged.
    /// Nothing in it has happened: it is a list of intentions until the
    /// button at the top of that screen is pressed.
    rewriting: Option<gitrust_core::Plan>,

    // The working tree.
    work: WorkingState,
    selected: Option<(String, bool)>,
    diff: Option<FileDiff>,
    subject: String,
    body: String,
    /// Whether what is written will redo the last commit.
    amend: bool,
    /// What was being written before the switch went on, so it can be given
    /// back if the switch goes off.
    saved: Option<(String, String)>,
    /// The tag being written in the detail panel, if any.
    tag: detail_view::TagDraft,
    /// What is waiting for a yes before anything is destroyed.
    confirm: Option<Confirm>,
    /// The note being written, when one is.
    describing: Option<Describing>,
    /// The name being asked for, when one is.
    naming: Option<Naming>,
    /// The two branches one was dropped on the other, waiting to be told
    /// which of the three things that meant.
    joining: Option<Joining>,
    /// The last thing that happened, said where it will be seen.
    toast: Option<toast::Toast>,
}

/// A slow operation, running off to one side.
///
/// Fetching or pushing takes as long as the network takes, and doing it on
/// the painting thread would leave the window hung with no word of why. The
/// thread opens its own repository -- `git2::Repository` does not travel
/// between threads -- and sends back one single thing: the sentence to put in
/// the bottom bar.
struct Job {
    done: std::sync::mpsc::Receiver<Result<String>>,
}

/// The asking of the forge, which is a job of its own.
///
/// It is kept apart from [`Job`] on purpose. `gh` goes out to the network, and
/// it can sit there for as long as the network lets it; hanging the toolbar on
/// that would mean a board nobody asked to open could stop you pushing. So
/// this one darkens nothing and holds nothing up: the board is already drawn
/// from the graph, and the answer only ever improves it.
struct Forge {
    done: std::sync::mpsc::Receiver<Result<PullRequests>>,
}

/// The board, worked out off to one side.
///
/// Laying a board out means weighing every branch against every other, and on
/// a repository with thirty of them that is well over a second. Spent on the
/// painting thread it is a window that does not move, after every operation.
/// So it goes the way fetching goes: its own thread, its own repository, and
/// the board that was already there stays up until the new one arrives.
///
/// Only the newest one is ever listened to. Starting another drops this one,
/// and an answer nobody is holding a receiver for is an answer nobody reads,
/// so a slow board can never land on top of a quick one.
struct Laying {
    done: std::sync::mpsc::Receiver<Result<Board>>,
}

/// A note being written about a branch.
///
/// It is held here and not in the view for the same reason the confirm is:
/// a view that drew it would have to remember it between frames, and the
/// views here remember nothing.
struct Describing {
    branch: String,
    title: String,
    body: String,
    /// Whether there was one already, which is the difference between
    /// cancelling and being able to take it away.
    had: bool,
}

/// A name being asked for before something is made or renamed.
///
/// The two that need one are the same shape -- a box, a verb, and what to do
/// with what was typed -- so they share a dialog rather than having one each.
struct Naming {
    title: String,
    hint: String,
    verb: String,
    draft: String,
    deed: Named,
}

/// What the name will be used for.
enum Named {
    BranchAt(Oid),
    Rename(String),
}

/// One branch dropped on another, with the question not yet put.
///
/// The gesture says which two branches and which way round. It does not say
/// whether that meant a merge, a rebase or a move up, and guessing would be
/// guessing at the one part that cannot be taken back.
struct Joining {
    /// The branch dragged: where the commits come from.
    from: String,
    /// The branch dropped on: the one that ends up somewhere else.
    onto: String,
    /// Whether `onto` could simply move up. Worked out once, when the gesture
    /// lands, rather than every frame the menu is on screen.
    room: bool,
    /// Where it was dropped, which is where the menu goes. A menu that opens
    /// in the middle of the window makes you look away from the two branches
    /// you were just aiming at.
    at: egui::Pos2,
    /// Whether it has been on screen for a frame. Until it has, a click
    /// cannot be meant for anything outside it -- and the release that
    /// dropped the branch is still in the same frame.
    armed: bool,
}

/// Something destructive that is waiting for a yes.
///
/// Everything that cannot be undone comes through here. The view asks for the
/// action like any other; it is the app that stops to ask, so that no view
/// has to remember to.
struct Confirm {
    title: String,
    detail: String,
    verb: String,
    deed: Deed,
}

/// What will be done if the answer is yes. It is deliberately a different
/// type from the views' actions: that way there is no looping back to asking.
enum Deed {
    Discard(String),
    DiscardHunk {
        path: String,
        hunk: HunkId,
    },
    DiscardAll(Vec<String>),
    DeleteBranch {
        name: String,
        remote: bool,
    },
    CloseWorktree {
        name: String,
    },
    /// Throw away whatever is half-done.
    Abandon,
    MoveTo {
        oid: Oid,
        how: Move,
    },
    Land {
        base: String,
        branch: String,
    },
    Review(String),
}

impl Open {
    fn new(path: String, salt: u64) -> Self {
        Self::with_backend(path, salt, None)
    }

    fn new_remote(remote: RemoteRepo, salt: u64) -> Self {
        let path = remote.location();
        Self::with_backend(path, salt, Some(remote))
    }

    fn with_backend(path: String, salt: u64, remote: Option<RemoteRepo>) -> Self {
        let mut app = Self {
            salt,
            path,
            remote,
            elsewhere: None,
            head: String::new(),
            summary: String::new(),
            status: String::new(),
            status_failed: false,
            view: View::History,
            graph: Graph::default(),
            limit: 5_000,
            needle: String::new(),
            found: Vec::new(),
            at: 0,
            bring: None,
            refs: Vec::new(),
            ref_filter: String::new(),
            board: None,
            project: None,
            trail: Vec::new(),
            worktrees: Vec::new(),
            backlog: Backlog::default(),
            backlog_draft: String::new(),
            pulls: None,
            forge: None,
            laying: None,
            forge_note: None,
            pull_mode: PullMode::Merge,
            tracking: None,
            stashes: Vec::new(),
            draft: action_bar::Draft::default(),
            job: None,
            looking: None,
            half: gitrust_core::Half::None,
            clashes: Vec::new(),
            deciding: None,
            rewriting: None,
            selected_commit: None,
            detail: None,
            detail_file: None,
            detail_diff: None,
            work: WorkingState::default(),
            selected: None,
            diff: None,
            subject: String::new(),
            body: String::new(),
            amend: false,
            saved: None,
            tag: detail_view::TagDraft::default(),
            confirm: None,
            describing: None,
            naming: None,
            joining: None,
            toast: None,
        };
        app.load();
        if let Ok(shot) = std::env::var("GITRUST_SHOT") {
            app.pose(&shot);
        }
        app
    }

    fn is_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Opens the window straight onto one screen, for photographing it.
    ///
    /// `GITRUST_SHOT=board`, `changes:<path>`, `diff:<path>`, `whole:<path>`,
    /// `blame:<path>` or `clash`.
    ///
    /// It is here because the pictures in the README are the first thing
    /// anybody sees of a program with a window, and they go stale every time
    /// the window changes. Taking them again means getting to each screen,
    /// and the machines that take them have no way to move a pointer: a
    /// screen you can only reach by clicking is a screen that cannot be
    /// photographed. So each one can be opened by name instead.
    ///
    /// It puts the window where a click would have put it and does nothing
    /// else -- no state that clicking could not reach, and nothing written.
    fn pose(&mut self, shot: &str) {
        let (what, rest) = shot.split_once(':').unwrap_or((shot, ""));
        match what {
            "board" => self.view = View::Board,
            "changes" => {
                self.view = View::Changes;
                if !rest.is_empty() {
                    self.handle(changes_view::Action::Select {
                        path: rest.to_string(),
                        staged: false,
                    });
                }
            }
            "diff" | "whole" | "blame" => {
                self.look_at(rest.to_string());
                if let Some(looking) = self.looking.as_mut() {
                    looking.how = match what {
                        "whole" => file_view::Look::Whole,
                        "blame" => file_view::Look::Blame,
                        _ => file_view::Look::Diff,
                    };
                }
                self.read_file();
            }
            "clash" => self.decide(),
            _ => {}
        }
    }

    fn repo(&self) -> Result<Repo> {
        Repo::open(&self.path)
    }

    /// Reloads everything: history, references and the working tree's state.
    fn load(&mut self) {
        if self.is_remote() {
            self.load_remote();
            return;
        }
        let started = std::time::Instant::now();
        // A reload changes which commit is the last one, so a message brought
        // in from it is no longer good for anything.
        self.drop_amend();

        let mut repo = match self.repo() {
            Ok(repo) => repo,
            Err(err) => return self.fail(err),
        };

        self.head = repo.head_name();
        self.path = repo.workdir().display().to_string();
        // Before anything else that reads the tree: what is half-done here
        // changes what every other reading means.
        self.half = repo.half_done();
        self.clashes = repo.clashes().unwrap_or_default();
        if self.half == gitrust_core::Half::None {
            self.deciding = None;
        }
        self.refs = repo.references().unwrap_or_default();
        self.tracking = repo.tracking();
        self.lay_out();
        self.backlog = repo.backlog().unwrap_or_default();
        self.worktrees = repo.worktrees().unwrap_or_default();
        self.stashes = repo.stashes().unwrap_or_default();

        // The tree's state goes before the graph: whether there is a row of
        // uncommitted changes above the history depends on it.
        self.read_work(&repo);
        if let Err(err) = self.rebuild_graph(&repo) {
            return self.fail(err);
        }
        self.preselect();

        self.recount();
        self.status = format!("read in {} ms", started.elapsed().as_millis());
        self.status_failed = false;
        self.reload_detail(&repo);
    }

    fn load_remote(&mut self) {
        let started = std::time::Instant::now();
        let Some(repo) = self.remote.as_ref() else {
            return;
        };
        self.head = repo.head_name();
        self.path = repo.location();
        self.half = gitrust_core::Half::None;
        self.clashes.clear();
        self.tracking = None;
        self.worktrees.clear();
        self.stashes.clear();
        self.refs = match repo.references() {
            Ok(value) => value,
            Err(err) => return self.fail(err),
        };
        self.work = match repo.status() {
            Ok(value) => value,
            Err(err) => return self.fail(err),
        };
        if let Some((path, staged)) = self.selected.clone() {
            self.diff = repo.file_diff(&path, staged).ok();
        }
        let commits = match repo.commits(self.limit) {
            Ok(value) => value,
            Err(err) => return self.fail(err),
        };
        let total = commits.len();
        let head = self
            .refs
            .iter()
            .find(|reference| reference.is_head)
            .map(|reference| reference.target);
        self.graph = match head.filter(|_| !self.work.is_clean()) {
            Some(head) => layout_with_working(commits, head),
            None => layout(commits),
        };
        let refs = self.refs.len();
        self.summary = format!(
            "{total} {} across {refs} {} · remote read-only",
            if total == 1 { "commit" } else { "commits" },
            if refs == 1 { "ref" } else { "refs" }
        );
        self.preselect();
        self.status = format!("read remotely in {} ms", started.elapsed().as_millis());
        self.status_failed = false;
        self.reload_remote_detail();
    }

    fn reload_remote_detail(&mut self) {
        let Some(oid) = self.selected_commit.filter(|oid| !oid.is_zero()) else {
            self.detail = None;
            self.detail_diff = None;
            return;
        };
        let Some(repo) = self.remote.as_ref() else {
            return;
        };
        self.detail = repo.commit_detail(oid).ok();
        if self.detail_file.is_none() {
            self.detail_file = self
                .detail
                .as_ref()
                .and_then(|detail| detail.files.first())
                .map(|file| file.path.clone());
        }
        self.detail_diff = self
            .detail_file
            .as_deref()
            .and_then(|path| repo.commit_file_diff(oid, path).ok());
    }

    /// Lays out the graph, with the uncommitted-changes row if the working
    /// tree has something to say.
    fn rebuild_graph(&mut self, repo: &Repo) -> Result<()> {
        let commits = repo.commits(self.limit)?;
        let total = commits.len();

        self.graph = match repo.head_id() {
            Some(head) if !self.work.is_clean() => layout_with_working(commits, head),
            _ => layout(commits),
        };

        let refs = self.refs.len();
        self.summary = format!(
            "{total} {} across {refs} {}",
            if total == 1 { "commit" } else { "commits" },
            if refs == 1 { "ref" } else { "refs" },
        );
        Ok(())
    }

    /// On opening, the right panel shows where HEAD is instead of greeting you
    /// with a gap: the first thing anyone wants to know about a repository is
    /// what last happened in it.
    fn preselect(&mut self) {
        if self.selected_commit.is_some() {
            return;
        }
        self.selected_commit = self
            .graph
            .rows
            .iter()
            .find(|row| row.commit.refs.iter().any(|r| r.kind == RefKind::Head))
            .or_else(|| self.graph.rows.first())
            .map(|row| row.commit.id);
    }

    /// Counts again which rows match what is being looked for.
    ///
    /// It is redone on typing and on reloading: the indices are positions in
    /// the graph, and a new graph leaves them meaningless.
    fn recount(&mut self) {
        let needle = self.needle.trim().to_lowercase();
        self.found = if needle.is_empty() {
            Vec::new()
        } else {
            self.graph
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.commit.matches(&needle))
                .map(|(index, _)| index)
                .collect()
        };
        self.at = 0;
    }

    /// Jumps to the next or previous match, wrapping round at the end.
    fn jump(&mut self, forward: bool) {
        if self.found.is_empty() {
            return;
        }
        let last = self.found.len() - 1;
        self.at = match (forward, self.at) {
            (true, at) if at >= last => 0,
            (true, at) => at + 1,
            (false, 0) => last,
            (false, at) => at - 1,
        };
        self.land();
    }

    /// Brings the current match into view and picks it.
    fn land(&mut self) {
        let Some(&index) = self.found.get(self.at) else {
            return;
        };
        self.bring = Some(index);
        if let Some(row) = self.graph.rows.get(index) {
            let oid = row.commit.id;
            self.select_commit(oid);
        }
    }

    fn has_working_row(&self) -> bool {
        self.graph
            .rows
            .first()
            .is_some_and(|row| row.commit.is_working())
    }

    /// Takes amend mode off and gives back whatever was being written.
    fn drop_amend(&mut self) {
        self.amend = false;
        if let Some((subject, body)) = self.saved.take() {
            self.subject = subject;
            self.body = body;
        }
    }

    /// Whether there is a commit to amend, and the remote that already has it.
    ///
    /// The remote's name comes back owned on purpose. Borrowed from `&self`,
    /// the borrow would last as long as the value, and the commit box could
    /// not take hold of what is being written at the same time.
    fn amend_state(&self) -> (bool, Option<String>) {
        let amendable = self.graph.rows.iter().any(|row| !row.commit.is_working());

        // With nothing ahead of the remote, the last commit is already there.
        let pushed = self
            .tracking
            .as_ref()
            .filter(|tracking| tracking.ahead == 0)
            .map(|tracking| tracking.remote.clone());

        (amendable, pushed)
    }

    /// Reloads only what changes when the index is touched.
    fn refresh(&mut self) {
        let mut repo = match self.repo() {
            Ok(repo) => repo,
            Err(err) => return self.fail(err),
        };

        self.head = repo.head_name();
        self.stashes = repo.stashes().unwrap_or_default();
        let had_working = self.has_working_row();
        self.read_work(&repo);

        // The changes row comes and goes from the graph with the working tree:
        // if the tree has just got dirty, or just got clean, it has to be laid
        // out again so the row appears or leaves.
        let wants_working = !self.work.is_clean();
        if had_working != wants_working {
            let _ = self.rebuild_graph(&repo);
            self.recount();
        }

        // And if what was picked was that row and it no longer exists, the
        // cursor goes to HEAD instead of staying on a gap.
        if !wants_working && self.selected_commit.is_some_and(|oid| oid.is_zero()) {
            self.selected_commit = None;
            self.preselect();
            self.reload_detail(&repo);
        }
    }

    fn read_work(&mut self, repo: &Repo) {
        match repo.status() {
            Ok(work) => self.work = work,
            Err(err) => return self.note_error(format!("could not read the status: {err}")),
        }

        // If the picked file is no longer on its side, we let the pick go.
        if let Some((path, staged)) = self.selected.clone() {
            let list = if staged {
                &self.work.staged
            } else {
                &self.work.unstaged
            };
            if list.iter().any(|file| file.path == path) {
                self.diff = repo.file_diff(&path, staged).ok();
            } else {
                self.selected = None;
                self.diff = None;
            }
        }
    }

    fn reload_detail(&mut self, repo: &Repo) {
        // The uncommitted-changes row has no detail to read: the right panel
        // shows it the working tree instead.
        let Some(oid) = self.selected_commit.filter(|oid| !oid.is_zero()) else {
            self.detail = None;
            self.detail_diff = None;
            return;
        };

        self.detail = repo.commit_detail(oid).ok();

        // With no file picked, the commit's first one is opened. A commit of
        // one single file should not force one more click to see the only
        // thing it carries.
        if self.detail_file.is_none() {
            self.detail_file = self
                .detail
                .as_ref()
                .and_then(|detail| detail.files.first())
                .map(|file| file.path.clone());
        }

        self.detail_diff = match &self.detail_file {
            Some(path) if self.detail.is_some() => repo.commit_file_diff(oid, path).ok(),
            _ => None,
        };
    }

    /// Picks a commit and brings it into the graph's view.
    ///
    /// Pressing a branch in the sidebar or a card on the board means you want
    /// to see it. If it falls outside the window, picking it goes unnoticed.
    fn reveal_commit(&mut self, oid: Oid) {
        self.bring = self.graph.rows.iter().position(|row| row.commit.id == oid);
        self.select_commit(oid);
    }

    fn select_commit(&mut self, oid: Oid) {
        self.selected_commit = Some(oid);
        self.detail_file = None;
        self.detail_diff = None;

        if self.is_remote() {
            self.reload_remote_detail();
            return;
        }
        match self.repo() {
            Ok(repo) => self.reload_detail(&repo),
            Err(err) => self.fail(err),
        }
    }

    fn select_detail_file(&mut self, path: String) {
        let Some(oid) = self.selected_commit else {
            return;
        };
        self.detail_diff = match self.remote.as_ref() {
            Some(repo) => repo.commit_file_diff(oid, &path).ok(),
            None => self
                .repo()
                .and_then(|repo| repo.commit_file_diff(oid, &path))
                .ok(),
        };
        self.detail_file = Some(path);
    }

    /// Opens a file across the window.
    fn look_at(&mut self, path: String) {
        let Some(at) = self.selected_commit else {
            return;
        };
        self.looking = Some(file_view::Looking {
            path,
            at,
            how: file_view::Look::Diff,
            read: file_view::Read::default(),
            trouble: None,
            editing: false,
            draft: String::new(),
        });
        self.read_file();
    }

    /// Reads whatever is needed and not already read.
    ///
    /// Each of the three questions is read the first time it is asked for and
    /// then kept, so going back and forth between them costs nothing. The
    /// history of the file is different: it is the column on the left, so it
    /// is always needed, and it belongs to the file rather than to the commit
    /// picked -- which is why moving between commits does not throw it away.
    fn read_file(&mut self) {
        let Some(looking) = &self.looking else {
            return;
        };
        let (path, at, how) = (looking.path.clone(), looking.at, looking.how);

        let needed = match how {
            file_view::Look::Diff => looking.read.diff.is_none(),
            file_view::Look::Whole => looking.read.whole.is_none(),
            file_view::Look::Blame => looking.read.blame.is_none(),
        };
        let walking = looking.read.story.is_none();
        if !needed && !walking {
            return;
        }

        if let Some(remote) = self.remote.as_ref() {
            if how == file_view::Look::Whole {
                let read = remote.read_working_file(&path).map(|bytes| match String::from_utf8(bytes) { Ok(text) => gitrust_core::Contents::Text(text), Err(err) => gitrust_core::Contents::Binary(err.into_bytes().len()) });
                if let Some(looking) = &mut self.looking { match read { Ok(whole) => looking.read.whole = Some(whole), Err(err) => looking.trouble = Some(format!("{err:#}")) } }
            }
            return;
        }
        let Ok(repo) = self.repo() else {
            return;
        };

        // On this thread. Reading a file out of a commit is instant, blame is
        // libgit2's own work, and the history of one file is the only slow
        // one -- bounded by the limit it is given. If that limit ever has to
        // go up, this is the line that has to move off here first.
        let mut trouble = None;

        if walking {
            let read = repo.file_history(&path, FILE_HISTORY);
            if let Some(looking) = &mut self.looking {
                match read {
                    Ok(story) => looking.read.story = Some(story),
                    Err(err) => trouble = Some(format!("{err:#}")),
                }
            }
        }

        if !needed {
            if let Some(looking) = &mut self.looking {
                looking.trouble = trouble;
            }
            return;
        }

        match how {
            file_view::Look::Diff => {
                let read = repo.commit_file_diff(at, &path);
                if let Some(looking) = &mut self.looking {
                    match read {
                        Ok(diff) => looking.read.diff = Some(diff),
                        Err(err) => trouble = Some(format!("{err:#}")),
                    }
                }
            }
            file_view::Look::Whole => {
                let read = repo.file_at(at, &path);
                if let Some(looking) = &mut self.looking {
                    match read {
                        Ok(whole) => {
                            if let gitrust_core::Contents::Text(text) = &whole {
                                looking.read.painted = Some(syntax::read(text, &path));
                            }
                            looking.read.whole = Some(whole);
                        }
                        Err(err) => trouble = Some(format!("{err:#}")),
                    }
                }
            }
            file_view::Look::Blame => {
                let read = repo.blame(&path, Some(at));
                if let Some(looking) = &mut self.looking {
                    match read {
                        Ok(lines) => {
                            // The same lines at the same commit as the file
                            // itself, so whichever of the two is asked for
                            // first is the one that pays for the colour.
                            if looking.read.painted.is_none() {
                                let text: Vec<&str> =
                                    lines.iter().map(|line| line.text.as_str()).collect();
                                looking.read.painted = Some(syntax::read_lines(&text, &path));
                            }
                            looking.read.blame = Some(lines);
                        }
                        Err(err) => trouble = Some(format!("{err:#}")),
                    }
                }
            }
        }

        if let Some(looking) = &mut self.looking {
            looking.trouble = trouble;
        }
    }

    /// Opens the screen for arranging what becomes of a run of commits.
    ///
    /// Reading the range can refuse -- a merge in the way, a commit that is
    /// not on this branch -- and that refusal is the answer, so it goes where
    /// every other refusal goes rather than opening an empty screen.
    fn start_rewrite(&mut self, oid: gitrust_core::Oid) {
        match self.repo().and_then(|repo| repo.plan_from(oid)) {
            Ok(plan) => {
                self.looking = None;
                self.deciding = None;
                self.rewriting = Some(plan);
            }
            Err(err) => self.note_error(format!("{err:#}")),
        }
    }

    fn act_on_rewrite(&mut self, act: rewrite_view::Act) {
        match act {
            rewrite_view::Act::Close => self.rewriting = None,
            rewrite_view::Act::Step { at, step } => {
                if let Some(plan) = &mut self.rewriting {
                    if let Some(line) = plan.lines.get_mut(at) {
                        line.step = step;
                    }
                }
            }
            rewrite_view::Act::Typed { at, text } => {
                if let Some(plan) = &mut self.rewriting {
                    if let Some(line) = plan.lines.get_mut(at) {
                        line.message = text;
                    }
                }
            }
            rewrite_view::Act::Move { at, up } => {
                if let Some(plan) = &mut self.rewriting {
                    let with = match up {
                        true => at.checked_sub(1),
                        false => (at + 1 < plan.lines.len()).then_some(at + 1),
                    };
                    if let Some(with) = with {
                        plan.lines.swap(at, with);
                    }
                }
            }
            rewrite_view::Act::Go => {
                let Some(plan) = self.rewriting.take() else {
                    return;
                };
                // Put back if it would not run, so the arranging is not lost
                // along with the attempt: a plan that clashes is a line or
                // two away from a plan that does not.
                match self.repo().and_then(|repo| repo.rewrite(&plan)) {
                    Ok(note) => {
                        self.load();
                        self.said(note);
                    }
                    Err(err) => {
                        self.rewriting = Some(plan);
                        self.note_error(format!("{err:#}"));
                    }
                }
            }
        }
    }

    /// Opens the screen for deciding what a merge or a rebase could not.
    fn decide(&mut self) {
        self.looking = None;
        let first = self.clashes.first().map(|clash| clash.path.clone());
        self.deciding = Some(conflict_view::Deciding {
            path: first,
            draft: None,
        });
    }

    fn act_on_conflict(&mut self, act: conflict_view::Act) {
        match act {
            conflict_view::Act::Close => self.deciding = None,
            conflict_view::Act::Pick(path) => {
                if let Some(state) = &mut self.deciding {
                    state.path = Some(path);
                    // Moving to another file puts away whatever was being
                    // typed into this one. It belonged to that file.
                    state.draft = None;
                }
            }
            conflict_view::Act::Typed(text) => {
                if let Some(state) = &mut self.deciding {
                    state.draft = Some(text);
                }
            }
            conflict_view::Act::Drop => {
                if let Some(state) = &mut self.deciding {
                    state.draft = None;
                }
            }
            conflict_view::Act::Edit(path) => {
                let read = self.repo().and_then(|repo| repo.clashing_text(&path));
                match read {
                    Ok(text) => {
                        if let Some(state) = &mut self.deciding {
                            state.draft = Some(text);
                        }
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
            conflict_view::Act::Take { path, side } => {
                self.note_down(move |repo| repo.take_side(&path, side));
                self.after_deciding();
            }
            conflict_view::Act::Write { path, text } => {
                self.note_down(move |repo| repo.take_text(&path, &text));
                if let Some(state) = &mut self.deciding {
                    state.draft = None;
                }
                self.after_deciding();
            }
            conflict_view::Act::Finish => {
                // One button, four things behind it, and which one is a fact
                // about the repository rather than about this screen -- so
                // the core picks. A rebase writes one commit and walks on
                // into the next, which is why the screen is pointed at
                // whatever is clashing now rather than put away.
                self.note_down(|repo| repo.see_it_through());
                self.after_deciding();
            }
            conflict_view::Act::Abandon => self.ask_abandon(),
        }
    }

    /// Moves on to the next file still waiting, once one is settled.
    ///
    /// Staying on a file that is decided would show "that one is decided" and
    /// leave somebody to find the next one themselves, which on a merge with
    /// twenty clashing files is twenty pieces of pointless work.
    fn after_deciding(&mut self) {
        let next = self.clashes.first().map(|clash| clash.path.clone());
        if let Some(state) = &mut self.deciding {
            state.path = next;
        }
    }

    /// Throwing a half-done merge or rebase away loses work nothing gives
    /// back, so it is asked about by name.
    fn ask_abandon(&mut self) {
        let half = self.half;
        let files = self.clashes.len();
        // A rebase that has already replayed some commits loses those too,
        // and that is the part somebody would not think of: the files in
        // front of them are one commit of several.
        let also = match half {
            gitrust_core::Half::Rebase => {
                " Any commits this rebase had already replayed go back where they were."
            }
            gitrust_core::Half::CherryPick | gitrust_core::Half::Revert => {
                " The commit it came from is untouched either way."
            }
            _ => "",
        };
        self.ask(
            format!("Throw away {}?", half.says()),
            &format!(
                "Everything goes: the markers, both sides, and anything typed into \
                 those {files} files since it started. The branch goes back to where \
                 it was before, and nothing else is touched.{also}"
            ),
            "Throw it away",
            Deed::Abandon,
        );
    }

    fn act_on_file(&mut self, act: file_view::Act) {
        match act {
            file_view::Act::Close => self.looking = None,
            file_view::Act::Look(how) => {
                if let Some(looking) = &mut self.looking {
                    looking.how = how;
                    looking.trouble = None;
                }
                self.read_file();
            }
            // Another commit is another diff, another file and another
            // blame, so all three are thrown away rather than shown beside
            // the wrong hash. The history is not: it is about the file, it
            // is the list being clicked in, and walking it again on every
            // click would be the most expensive thing in the window
            // happening for no reason at all.
            file_view::Act::At(oid) => {
                if let Some(looking) = &mut self.looking {
                    looking.at = oid;
                    let story = looking.read.story.take();
                    looking.read = file_view::Read {
                        story,
                        ..Default::default()
                    };
                    looking.trouble = None;
                }
                self.read_file();
            }
            file_view::Act::Show(oid) => {
                self.looking = None;
                self.reveal_commit(oid);
                self.view = View::History;
            }
            file_view::Act::Revert(hunk) => {
                let Some(looking) = &self.looking else {
                    return;
                };
                let (path, at) = (looking.path.clone(), looking.at);
                self.note_down(move |repo| repo.revert_hunk(at, &path, hunk));
            }
            file_view::Act::Save(text) => {
                let Some(path) = self.looking.as_ref().map(|looking| looking.path.clone()) else { return };
                match self.remote.as_ref().ok_or_else(|| anyhow::anyhow!("not a remote checkout")).and_then(|repo| repo.write_working_file(&path, text.as_bytes())) {
                    Ok(()) => { if let Some(looking) = &mut self.looking { looking.editing = false; looking.read.whole = Some(gitrust_core::Contents::Text(text)); } self.load(); self.said(format!("{path} saved remotely")); }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
        }
    }

    /// Runs an operation on the repository and reloads the state.
    fn run(&mut self, op: impl FnOnce(&mut Repo) -> Result<()>) -> bool {
        match self.repo().and_then(|mut repo| op(&mut repo)) {
            Ok(()) => {
                self.refresh();
                true
            }
            Err(err) => {
                self.note_error(format!("{err:#}"));
                false
            }
        }
    }

    fn run_remote(&mut self, op: impl FnOnce(&RemoteRepo) -> Result<()>) -> bool {
        match self.remote.as_ref().ok_or_else(|| anyhow::anyhow!("no remote checkout")).and_then(op) {
            Ok(()) => { self.load(); true }
            Err(err) => { self.note_error(format!("{err:#}")); false }
        }
    }

    /// Sends a slow operation off to a thread of its own.
    ///
    /// While it lasts the action bar goes dark altogether: queueing a push
    /// behind a pull when neither has finished is asking for a mess somebody
    /// then has to explain.
    fn spawn(
        &mut self,
        doing: &str,
        work: impl FnOnce(&mut Repo) -> Result<String> + Send + 'static,
    ) {
        let (send, done) = std::sync::mpsc::channel();
        let path = self.path.clone();

        std::thread::spawn(move || {
            let _ = send.send(Repo::open(&path).and_then(|mut repo| work(&mut repo)));
        });

        self.status = format!("{doing}…");
        self.status_failed = false;
        self.job = Some(Job { done });
    }

    /// Looks at whether the background operation has finished.
    ///
    /// While it has not, it asks to be repainted: without that egui falls
    /// asleep as soon as nobody touches the mouse, and the result would sit
    /// there waiting for somebody to move the window.
    fn poll(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;

        let Some(job) = &self.job else {
            return;
        };

        match job.done.try_recv() {
            Ok(result) => {
                self.job = None;
                match result {
                    Ok(note) => {
                        self.load();
                        self.said(note);
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
            }
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.note_error("the operation was cut short".to_string());
            }
        }
    }

    /// Leaves something waiting for a yes instead of doing it.
    fn ask(&mut self, title: String, detail: &str, verb: &str, deed: Deed) {
        self.confirm = Some(Confirm {
            title,
            detail: detail.to_string(),
            verb: verb.to_string(),
            deed,
        });
    }

    /// Asks before deleting a branch, saying what it costs.
    fn ask_delete_branch(&mut self, name: String, remote: bool) {
        if remote {
            let server = name
                .split_once('/')
                .map(|(server, _)| server)
                .unwrap_or("the remote");
            let detail = format!(
                "It is deleted on {server}, not just here. Anyone who has it keeps seeing it until they fetch."
            );
            return self.ask(
                format!("Delete {name} on the remote?"),
                &detail,
                "Delete",
                Deed::DeleteBranch { name, remote },
            );
        }

        // How many commits go with it is the only question that matters.
        let alone = self
            .repo()
            .and_then(|repo| repo.unmerged(&name))
            .unwrap_or(0);
        let detail = match alone {
            0 => "Everything it has is somewhere else too, so nothing is lost.".to_string(),
            1 => "It has one commit that is on no other branch, remote or tag. That goes with it."
                .to_string(),
            n => format!(
                "It has {n} commits that are on no other branch, remote or tag. Those go with it."
            ),
        };

        self.ask(
            format!("Delete the branch {name}?"),
            &detail,
            "Delete",
            Deed::DeleteBranch { name, remote },
        );
    }

    /// Goes and asks the forge which pull requests there are.
    ///
    /// It runs at most once at a time, and it never says anything in the
    /// bottom bar while it runs: nobody asked for it out loud, so it should
    /// not talk over whatever did.
    fn ask_forge(&mut self) {
        if self.forge.is_some() {
            return;
        }

        let (send, done) = std::sync::mpsc::channel();
        let path = self.path.clone();

        std::thread::spawn(move || {
            let _ = send.send(Repo::open(&path).and_then(|repo| repo.pull_requests()));
        });

        self.forge = Some(Forge { done });
    }

    /// Looks at whether the forge has answered, and redraws the board if so.
    fn poll_forge(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;

        let Some(forge) = &self.forge else {
            return;
        };

        match forge.done.try_recv() {
            Ok(Ok(pulls)) => {
                self.forge = None;
                self.forge_note = None;
                self.pulls = Some(pulls);
                self.rebuild_board();
            }
            // A forge that cannot answer is not a failure of the board; it is
            // the board going back to what the graph alone can tell. The
            // reason is kept for the header to show, not shouted in the bar.
            Ok(Err(err)) => {
                self.forge = None;
                self.forge_note = Some(format!("{err:#}"));
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
            Err(TryRecvError::Disconnected) => {
                self.forge = None;
                self.forge_note = Some("the forge was not reached".to_string());
            }
        }
    }

    /// Hands an address to whatever the desktop opens addresses with.
    ///
    /// It is spawned and forgotten rather than waited on: the browser takes a
    /// second or two to appear and there is nothing to report when it does.
    /// What there is to report is it not being there at all.
    fn open_in_browser(&mut self, url: &str) {
        match std::process::Command::new("xdg-open").arg(url).spawn() {
            Ok(_) => {
                self.said(format!("opening {url}"));
            }
            Err(err) => self.note_error(format!("could not open a browser: {err}")),
        }
    }

    /// Writes something in the backlog, or takes something out of it.
    ///
    /// All of it is local and finishes before anybody notices: the sharing is
    /// a separate thing, asked for out loud. Everything is read again
    /// afterwards because starting an item makes a branch and moves HEAD,
    /// which changes the graph, the sidebar and the columns at once.
    fn note_down(&mut self, op: impl FnOnce(&Repo) -> Result<String>) {
        match self.repo().and_then(|repo| op(&repo)) {
            Ok(note) => {
                self.load();
                self.said(note);
            }
            Err(err) => self.note_error(format!("{err:#}")),
        }
    }

    /// Lays the columns out again over whatever the forge last said.
    fn rebuild_board(&mut self) {
        self.lay_out();
    }

    /// Sends the board off to be laid out against wherever the trail ends.
    fn lay_out(&mut self) {
        if self.trail.is_empty() {
            let trunk = self.repo().ok().and_then(|repo| repo.trunk().ok());
            match trunk {
                Some(trunk) => self.trail.push(trunk),
                None => {
                    self.board = None;
                    self.laying = None;
                    return;
                }
            }
        }

        let base = self.trail.last().cloned().unwrap_or_default();
        let path = self.path.clone();
        let pulls = self.pulls.clone();
        let (send, done) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let _ = send
                .send(Repo::open(&path).and_then(|repo| repo.board_against(&base, pulls.as_ref())));
        });

        self.laying = Some(Laying { done });
    }

    /// Looks at whether the board has been worked out, and puts it up if so.
    ///
    /// A branch you went into can stop existing while you are standing in it:
    /// deleted, or merged and tidied away. Stepping back out is better than
    /// showing nothing and better than an error about a branch you did not
    /// ask about.
    fn poll_board(&mut self, ctx: &egui::Context) {
        use std::sync::mpsc::TryRecvError;

        let Some(laying) = &self.laying else {
            return;
        };

        match laying.done.try_recv() {
            Ok(Ok(board)) => {
                self.laying = None;
                self.board = Some(board);
            }
            Ok(Err(_)) => {
                self.laying = None;

                // A step you went into has gone: step back out of it and lay
                // the one below out instead.
                if self.trail.len() > 1 {
                    self.trail.pop();
                    self.project = None;
                    self.lay_out();
                    return;
                }

                // The bottom step was never this repository's trunk, which is
                // what opening another repository leaves behind. Work it out
                // again -- but only if it comes back different, or this would
                // ask the same question forever.
                let trunk = self.repo().ok().and_then(|repo| repo.trunk().ok());
                match trunk {
                    Some(trunk) if self.trail.first() != Some(&trunk) => {
                        self.trail = vec![trunk];
                        self.project = None;
                        self.lay_out();
                    }
                    _ => {
                        self.trail.clear();
                        self.board = None;
                    }
                }
            }
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
            }
            Err(TryRecvError::Disconnected) => {
                self.laying = None;
            }
        }
    }

    /// Looks at one branch's own board, or at the one it came from.
    ///
    /// The project filter is dropped on the way: it was made of the names on
    /// the board you were looking at, and carrying it in would quietly empty
    /// the one you are arriving at.
    fn walk_to(&mut self, trail: Vec<String>) {
        self.trail = trail;
        self.project = None;
        self.rebuild_board();
    }

    /// Asks before opening a pull request, which everyone on the repository
    /// is going to see.
    fn ask_review(&mut self, branch: String) {
        let trunk = match &self.board {
            Some(board) => board.trunk.clone(),
            None => return,
        };

        self.ask(
            format!("Open a pull request for {branch}?"),
            &format!(
                "{branch} is pushed first, and then a pull request is opened against \
                 {trunk} with the title and body taken from its own commits. \
                 Everyone on the repository sees it."
            ),
            "Open",
            Deed::Review(branch),
        );
    }

    /// Looks at another checkout of the same repository.
    ///
    /// Same history, same branches, a different HEAD and a different working
    /// tree. Nothing is written and nothing is fetched: it is the window
    /// moving, not the repository.
    fn visit(&mut self, path: std::path::PathBuf) {
        self.path = path.display().to_string();
        // What was picked belonged to the other folder's working tree.
        self.selected_commit = None;
        self.detail = None;
        self.detail_file = None;
        self.detail_diff = None;
        self.selected = None;
        self.diff = None;

        self.load();
        self.said(format!("looking at {}", self.path));
    }

    /// Asks before taking a checkout away, saying what goes with it.
    fn ask_release(&mut self, name: String) {
        let where_it_is = self
            .worktrees
            .iter()
            .find(|tree| tree.name == name)
            .map(|tree| tree.path.display().to_string())
            .unwrap_or_else(|| name.clone());

        // What is uncommitted there is the only thing that cannot be had
        // back, so it is the number that goes in the question.
        let loose = self
            .repo()
            .and_then(|repo| repo.worktree_changes(&name))
            .unwrap_or(0);
        let detail = match loose {
            0 => format!(
                "{where_it_is} is deleted. The branch stays where it is, and \
                 nothing is lost: there is nothing uncommitted in there."
            ),
            1 => format!(
                "{where_it_is} is deleted, and 1 uncommitted file in it goes too. \
                 The branch itself stays."
            ),
            n => format!(
                "{where_it_is} is deleted, and {n} uncommitted files in it go too. \
                 The branch itself stays."
            ),
        };

        self.ask(
            format!("Close the folder {name}?"),
            &detail,
            "Close",
            Deed::CloseWorktree { name },
        );
    }

    /// Moves the branch you are on to a commit, asking first when the move
    /// would take your files with it.
    ///
    /// Only the one that discards asks. The other two keep everything you had
    /// not committed -- staged or not -- so there is nothing to warn about
    /// and nothing a question would protect.
    fn ask_move(&mut self, oid: Oid, how: Move) {
        if how != Move::Discard {
            let doing = format!("moving to {}", &oid.to_string()[..7]);
            return self.spawn(&doing, move |repo| repo.move_branch(oid, how));
        }

        let loose = self.work.staged.len() + self.work.unstaged.len();
        let detail = match loose {
            0 => format!(
                "{} moves back to {}. The commits after it stop being shown, and \
                 nothing of yours goes with them: there is nothing uncommitted.",
                self.head,
                &oid.to_string()[..7]
            ),
            1 => format!(
                "{} moves back to {}, and the one file you have not committed is \
                 thrown away with it. No stash holds it and nothing gives it back.",
                self.head,
                &oid.to_string()[..7]
            ),
            n => format!(
                "{} moves back to {}, and the {n} files you have not committed are \
                 thrown away with it. No stash holds them and nothing gives them back.",
                self.head,
                &oid.to_string()[..7]
            ),
        };

        self.ask(
            format!("Move {} and throw the rest away?", self.head),
            &detail,
            "Move and discard",
            Deed::MoveTo { oid, how },
        );
    }

    /// Asks before bringing a branch into the one whose board you are on,
    /// which may start by moving you somewhere else.
    ///
    /// Not always the main branch. Inside a card it is that card's branch,
    /// because a subtask is finished when it goes into its parent, and
    /// merging it into the trunk instead would be doing something else
    /// entirely while looking like the same gesture.
    fn ask_land(&mut self, branch: String) {
        let base = match &self.board {
            Some(board) => board.trunk.clone(),
            None => return,
        };

        let detail = if self.head == base {
            format!("{branch} is merged into {base}. If they clash, nothing is touched and it says where.")
        } else {
            format!(
                "You move to {base} first, and then {branch} is merged into it. \
                 If they clash, nothing is merged and you stay on {base}."
            )
        };

        self.ask(
            format!("Bring {branch} into {base}?"),
            &detail,
            "Merge",
            Deed::Land { base, branch },
        );
    }

    /// Does what has already been answered yes to.
    fn commit_deed(&mut self, deed: Deed) {
        let discarded = match deed {
            Deed::Discard(path) if self.is_remote() => self.run_remote(|repo| repo.discard(&path)),
            Deed::Discard(path) => self.run(|repo| repo.discard(&path)),
            Deed::DiscardHunk { path, hunk } if self.is_remote() => self.run_remote(|repo| repo.discard_hunk(&path, hunk)),
            Deed::DiscardHunk { path, hunk } => self.run(|repo| repo.discard_hunk(&path, hunk)),
            Deed::DiscardAll(paths) if self.is_remote() => self.run_remote(|repo| repo.discard_all(&paths)),
            Deed::DiscardAll(paths) => self.run(|repo| repo.discard_all(&paths)),

            // The remote one goes out on the network; the one here is instant.
            Deed::DeleteBranch { name, remote } if remote => {
                let doing = format!("deleting {name}");
                return self.spawn(&doing, move |repo| repo.delete_remote_branch(&name));
            }
            Deed::MoveTo { oid, how } => {
                let doing = format!("moving to {}", &oid.to_string()[..7]);
                return self.spawn(&doing, move |repo| repo.move_branch(oid, how));
            }
            Deed::Abandon => {
                return self.spawn("throwing it away", |repo| repo.abandon());
            }
            Deed::CloseWorktree { name } => {
                let doing = format!("closing {name}");
                return self.spawn(&doing, move |repo| repo.remove_worktree(&name));
            }
            Deed::Land { base, branch } => {
                let doing = format!("merging {branch} into {base}");
                return self.spawn(&doing, move |repo| repo.land_into(&base, &branch));
            }
            Deed::Review(branch) => {
                // What the forge last said no longer holds once there is a new
                // pull request in it, so it gets asked again when this lands.
                self.pulls = None;
                let doing = format!("opening a pull request for {branch}");
                return self.spawn(&doing, move |repo| repo.open_pull_request(&branch));
            }
            Deed::DeleteBranch { name, .. } => {
                return match self.repo().and_then(|repo| repo.delete_branch(&name)) {
                    Ok(()) => {
                        // The references change, so the graph and the sidebar
                        // have to be read again.
                        self.load();
                        self.said(format!("branch {name} deleted"));
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                };
            }
        };

        if discarded {
            self.said("changes discarded".to_string());
        }
    }

    /// The warning about what cannot be undone.
    ///
    /// What destroys goes on the right and in red, and what does nothing on
    /// the left: the button that is easy to reach without reading has to be
    /// the one that breaks nothing.
    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let Some(confirm) = &self.confirm else {
            return;
        };

        let (title, detail, verb) = (
            confirm.title.clone(),
            confirm.detail.clone(),
            confirm.verb.clone(),
        );
        let mut go = false;
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let modal = egui::Modal::new(egui::Id::new("confirm"))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::same(18))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS))
                    .stroke(egui::Stroke::new(1.0, theme::EDGE)),
            )
            .show(ctx, |ui| {
                ui.set_max_width(372.0);
                ui.label(
                    egui::RichText::new(&title)
                        .font(theme::strong(14.0))
                        .color(theme::TEXT),
                );
                ui.add_space(8.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&detail)
                            .font(theme::body(12.0))
                            .color(theme::MUTED),
                    )
                    .wrap(),
                );
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(88.0, 28.0)))
                        .clicked()
                    {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button = egui::Button::new(
                            egui::RichText::new(&verb)
                                .font(theme::medium(13.0))
                                .color(theme::REMOVED),
                        )
                        .fill(theme::tint(theme::REMOVED, 38))
                        .min_size(egui::vec2(104.0, 28.0));

                        if ui.add(button).clicked() {
                            go = true;
                        }
                    });
                });
            });

        if modal.backdrop_response.clicked() {
            close = true;
        }

        if go {
            if let Some(confirm) = self.confirm.take() {
                self.commit_deed(confirm.deed);
            }
        } else if close {
            self.confirm = None;
        }
    }

    /// Opens the note for a branch, with whatever is already written in it.
    fn describe(&mut self, branch: String) {
        let found = self
            .repo()
            .ok()
            .and_then(|repo| repo.note(&branch).ok().flatten());
        self.describing = Some(Describing {
            had: found.is_some(),
            title: found
                .as_ref()
                .map(|note| note.title.clone())
                .unwrap_or_default(),
            body: found
                .as_ref()
                .map(|note| note.body.clone())
                .unwrap_or_default(),
            branch,
        });
    }

    /// What a branch is for, in the words somebody would use out loud.
    ///
    /// Two boxes and not one: a line that has to be read at a glance on a
    /// card, and underneath it as much as anybody wants to write, which the
    /// card never shows.
    fn note_dialog(&mut self, ctx: &egui::Context) {
        let Some(note) = &mut self.describing else {
            return;
        };

        let branch = note.branch.clone();
        let had = note.had;
        let (mut save, mut clear) = (false, false);
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let modal = egui::Modal::new(egui::Id::new("note"))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::same(18))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS))
                    .stroke(egui::Stroke::new(1.0, theme::EDGE)),
            )
            .show(ctx, |ui| {
                ui.set_width(452.0);
                ui.label(
                    egui::RichText::new("What is this branch for?")
                        .font(theme::strong(14.0))
                        .color(theme::TEXT),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(&branch)
                        .font(theme::mono(11.0))
                        .color(theme::MUTED),
                );
                ui.add_space(12.0);

                ui.add(
                    egui::TextEdit::singleline(&mut note.title)
                        .desired_width(f32::INFINITY)
                        .margin(egui::vec2(8.0, 6.0))
                        .font(theme::body(12.5))
                        .hint_text(
                            egui::RichText::new("in the words you would use out loud")
                                .color(theme::FAINT),
                        ),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::multiline(&mut note.body)
                        .desired_width(f32::INFINITY)
                        .desired_rows(5)
                        .margin(egui::vec2(8.0, 6.0))
                        .font(theme::body(12.0))
                        .hint_text(
                            egui::RichText::new("anything else worth saying, or nothing")
                                .color(theme::FAINT),
                        ),
                );
                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(88.0, 28.0)))
                        .clicked()
                    {
                        close = true;
                    }
                    // Only offered when there is one to take away, so the
                    // dialog never shows a button that would do nothing.
                    if had
                        && ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Take it away").color(theme::MUTED),
                                )
                                .min_size(egui::vec2(104.0, 28.0)),
                            )
                            .on_hover_text(
                                "the reference keeps its own history, so the words are still in it",
                            )
                            .clicked()
                    {
                        clear = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button = egui::Button::new(
                            egui::RichText::new("Save")
                                .font(theme::medium(13.0))
                                .color(theme::TEXT),
                        )
                        .fill(theme::CHOSEN)
                        .min_size(egui::vec2(104.0, 28.0));

                        if ui.add(button).clicked() {
                            save = true;
                        }
                    });
                });
            });

        if modal.backdrop_response.clicked() {
            close = true;
        }

        if save || clear {
            let Some(note) = self.describing.take() else {
                return;
            };
            match clear {
                true => self.note_down(|repo| repo.note_clear(&note.branch)),
                false => {
                    self.note_down(|repo| repo.note_set(&note.branch, &note.title, &note.body))
                }
            }
        } else if close {
            self.describing = None;
        }
    }

    /// Asks for a name, for whatever is about to be given one.
    /// Puts the question a drop asks but cannot answer.
    ///
    /// Dropping one branch on another is one gesture with three meanings, and
    /// they are not near-misses of each other: a merge keeps both histories,
    /// a rebase writes one of them again, and a move up writes nothing at
    /// all. Nothing in the gesture says which, so nothing here guesses.
    fn ask_join(&mut self, from: String, onto: String, ctx: &egui::Context) {
        // Asked now rather than while the menu is up: it is one question
        // about two branches that cannot change while somebody is reading.
        let room = self
            .repo()
            .and_then(|repo| repo.has_room(&onto, &from))
            .unwrap_or(false);
        let at = ctx.pointer_latest_pos().unwrap_or_default();
        self.joining = Some(Joining {
            from,
            onto,
            room,
            at,
            armed: false,
        });
    }

    /// The menu the drop opens, where the drop happened.
    ///
    /// It is the same menu as the one on a right click, and deliberately so:
    /// a drop and a right click are two ways of asking the same question, and
    /// two different-looking answers to it would be two things to learn.
    fn join_menu(&mut self, ctx: &egui::Context) {
        let Some(joining) = &self.joining else {
            return;
        };
        let (from, onto, room, at, armed) = (
            joining.from.clone(),
            joining.onto.clone(),
            joining.room,
            joining.at,
            joining.armed,
        );

        // What each one would really do. It is not on the face of the menu --
        // three sentences would not be a menu -- but it is one hover away,
        // including the reason the greyed-out one is greyed out.
        let options: Vec<(Join, String, bool)> = vec![
            (
                Join::Merge,
                format!(
                    "One commit holding both sides. Nothing already written changes, \
                     and you are left on {onto}."
                ),
                true,
            ),
            (
                Join::Rebase,
                format!(
                    "{onto}'s own commits are written again on top of {from}, so the \
                     history comes out as one line. They are new commits: anyone who \
                     already has the old ones ends up with both."
                ),
                true,
            ),
            (
                Join::FastForward,
                match room {
                    true => format!(
                        "{onto} has nothing of its own since the two parted, so it just \
                         moves. No commit is written."
                    ),
                    false => format!(
                        "{onto} has commits that {from} does not, so there is no moving \
                         up to be done here."
                    ),
                },
                room,
            ),
        ];

        let mut chosen = None;
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let area = egui::Area::new(egui::Id::new("joining"))
            .order(egui::Order::Foreground)
            .fixed_pos(at)
            // Dropped near the bottom of the window, the menu would otherwise
            // open below it and be unreachable.
            .constrain(true)
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.set_width(JOIN_MENU);

                    // A row in a menu is not a button with a box round it.
                    // Inside an area of our own the ordinary widget style
                    // applies, so the two lines egui's own menus set are set
                    // here: no fill and no outline until the pointer is on it.
                    let widgets = &mut ui.style_mut().visuals.widgets;
                    widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                    widgets.inactive.bg_stroke = egui::Stroke::NONE;

                    for (how, why, can) in options {
                        // The one that writes nothing goes under a rule: it
                        // is a different kind of answer from the two above,
                        // which both leave a commit behind.
                        if how == Join::FastForward {
                            theme::rule(ui);
                        }

                        let button = egui::Button::new(
                            egui::RichText::new(how.says(&from, &onto)).font(theme::body(12.5)),
                        )
                        .min_size(egui::vec2(ui.available_width(), 24.0));

                        let button = ui
                            .add_enabled(can, button)
                            .on_hover_text(&why)
                            .on_disabled_hover_text(&why);
                        if button.clicked() {
                            chosen = Some(how);
                        }
                    }
                });
            });

        // Clicking anywhere else puts it away, the way a menu does. Not on
        // the frame it opened: the pointer release that dropped the branch
        // is still that frame's, and it would close before it was seen.
        if armed {
            let outside = ctx.input(|i| i.pointer.any_click())
                && !ctx
                    .pointer_latest_pos()
                    .is_some_and(|pos| area.response.rect.contains(pos));
            close |= outside;
        }

        if let Some(how) = chosen {
            self.joining = None;
            let doing = how.doing(&from, &onto);
            self.spawn(&doing, move |repo| repo.join(&from, &onto, how));
            return;
        }

        match close {
            true => self.joining = None,
            false => {
                if let Some(joining) = &mut self.joining {
                    joining.armed = true;
                }
            }
        }
    }

    fn name_dialog(&mut self, ctx: &egui::Context) {
        let Some(naming) = &mut self.naming else {
            return;
        };

        let (title, hint, verb) = (
            naming.title.clone(),
            naming.hint.clone(),
            naming.verb.clone(),
        );
        let mut go = false;
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        let modal = egui::Modal::new(egui::Id::new("naming"))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::same(18))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS))
                    .stroke(egui::Stroke::new(1.0, theme::EDGE)),
            )
            .show(ctx, |ui| {
                ui.set_width(372.0);
                ui.label(
                    egui::RichText::new(&title)
                        .font(theme::strong(14.0))
                        .color(theme::TEXT),
                );
                ui.add_space(10.0);

                let entry = ui.add(
                    egui::TextEdit::singleline(&mut naming.draft)
                        .desired_width(f32::INFINITY)
                        .margin(egui::vec2(8.0, 6.0))
                        .font(theme::mono(12.0))
                        .hint_text(egui::RichText::new(&hint).color(theme::FAINT)),
                );
                entry.request_focus();
                if entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(88.0, 28.0)))
                        .clicked()
                    {
                        close = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let button = egui::Button::new(
                            egui::RichText::new(&verb)
                                .font(theme::medium(13.0))
                                .color(theme::TEXT),
                        )
                        .fill(theme::CHOSEN)
                        .min_size(egui::vec2(104.0, 28.0));
                        if ui.add(button).clicked() {
                            go = true;
                        }
                    });
                });
            });

        if modal.backdrop_response.clicked() {
            close = true;
        }

        if go {
            let Some(naming) = self.naming.take() else {
                return;
            };
            let name = naming.draft.trim().to_string();
            if name.is_empty() {
                return self.note_error("it needs a name".to_string());
            }
            match naming.deed {
                Named::BranchAt(oid) => self.note_down(move |repo| repo.branch_at(&name, oid)),
                Named::Rename(old) => self.note_down(move |repo| repo.rename_branch(&old, &name)),
            }
        } else if close {
            self.naming = None;
        }
    }

    /// Says that something finished, in the bar and in a toast at once.
    ///
    /// Both, not one or the other: the bar is the record of the last thing
    /// that happened and stays until the next, and the toast is the part that
    /// gets seen. A sentence said only in the bar is a sentence nobody reads
    /// while they are looking at the work they just asked about.
    fn said(&mut self, text: String) {
        self.toast = Some(toast::Toast::new(text.clone(), false));
        self.status = text;
        self.status_failed = false;
    }

    /// The same, for something that went wrong: the bar marks it as a failure
    /// and the toast comes up in red and stays up for longer.
    fn note_error(&mut self, message: String) {
        self.toast = Some(toast::Toast::new(message.clone(), true));
        self.status = message;
        self.status_failed = true;
    }

    fn fail(&mut self, err: anyhow::Error) {
        self.graph = Graph::default();
        self.refs.clear();
        self.board = None;
        self.tracking = None;
        self.stashes.clear();
        self.work = WorkingState::default();
        self.selected = None;
        self.diff = None;
        self.detail = None;
        self.detail_diff = None;
        self.head.clear();
        self.summary.clear();
        self.note_error(format!("{err:#}"));
    }

    fn handle(&mut self, action: changes_view::Action) {
        use changes_view::Action;

        match action {
            Action::Select { path, staged } => {
                self.selected = Some((path.clone(), staged));
                self.diff = match self.remote.as_ref() {
                    Some(repo) => repo.file_diff(&path, staged).ok(),
                    None => self
                        .repo()
                        .and_then(|repo| repo.file_diff(&path, staged))
                        .ok(),
                };
            }
            Action::Stage(path) if self.is_remote() => { self.run_remote(|repo| repo.stage(&path)); }
            Action::Stage(path) => {
                self.run(|repo| repo.stage(&path));
            }
            Action::Unstage(path) if self.is_remote() => { self.run_remote(|repo| repo.unstage(&path)); }
            Action::Unstage(path) => {
                self.run(|repo| repo.unstage(&path));
            }
            Action::StageHunk { path, hunk } if self.is_remote() => {
                self.run_remote(|repo| repo.stage_hunk(&path, hunk));
            }
            Action::StageHunk { path, hunk } => {
                self.run(|repo| repo.stage_hunk(&path, hunk));
            }
            Action::UnstageHunk { path, hunk } if self.is_remote() => {
                self.run_remote(|repo| repo.unstage_hunk(&path, hunk));
            }
            Action::UnstageHunk { path, hunk } => {
                self.run(|repo| repo.unstage_hunk(&path, hunk));
            }
            Action::Discard { path, untracked } => {
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let detail = if untracked {
                    "Git knows nothing about that file yet, so it goes from the disk and is left nowhere."
                } else {
                    "It goes back to what the index says. Whatever you have written since is saved nowhere."
                };
                self.ask(
                    format!("Discard the changes to {name}?"),
                    detail,
                    "Discard",
                    Deed::Discard(path),
                );
            }
            Action::DiscardHunk { path, hunk } => {
                self.ask(
                    "Discard this hunk?".to_string(),
                    "That hunk goes back to what the index says. The rest of the file stays as you have it.",
                    "Discard",
                    Deed::DiscardHunk { path, hunk },
                );
            }
            Action::DiscardAll => {
                let paths: Vec<String> = self
                    .work
                    .unstaged
                    .iter()
                    .map(|file| file.path.clone())
                    .collect();
                self.ask(
                    format!("Discard {}?", theme::files_label(paths.len())),
                    "The ones Git knows go back to the index; the new ones go from the disk. None of it can be got back.",
                    "Discard",
                    Deed::DiscardAll(paths),
                );
            }
            Action::StageAll if self.is_remote() => {
                let paths: Vec<String> = self.work.unstaged.iter().map(|file| file.path.clone()).collect();
                self.run_remote(|repo| repo.stage_all(&paths));
            }
            Action::StageAll => {
                let paths: Vec<String> =
                    self.work.unstaged.iter().map(|f| f.path.clone()).collect();
                self.run(|repo| repo.stage_all(&paths));
            }
            Action::UnstageAll if self.is_remote() => {
                let paths: Vec<String> = self.work.staged.iter().map(|file| file.path.clone()).collect();
                self.run_remote(|repo| repo.unstage_all(&paths));
            }
            Action::UnstageAll => {
                let paths: Vec<String> = self.work.staged.iter().map(|f| f.path.clone()).collect();
                self.run(|repo| repo.unstage_all(&paths));
            }
            Action::AmendToggled => {
                if self.amend {
                    // The last commit's message comes in, and whatever was
                    // written is kept: taking the switch off has to give it
                    // back, not have lost it along the way.
                    self.saved = Some((self.subject.clone(), self.body.clone()));
                    let message = match self.remote.as_ref() {
                        Some(repo) => repo.head_message().ok(),
                        None => self.repo().ok().and_then(|repo| repo.head_message()),
                    };
                    if let Some((subject, body)) = message {
                        self.subject = subject;
                        self.body = body;
                    }
                } else {
                    self.drop_amend();
                }
            }
            Action::Amend if self.is_remote() => {
                let message = compose(&self.subject, &self.body);
                let result = self.remote.as_ref().ok_or_else(|| anyhow::anyhow!("no remote checkout")).and_then(|repo| repo.amend(&message));
                match result {
                    Ok(oid) => { self.saved = None; self.subject.clear(); self.body.clear(); self.selected_commit = Some(oid); self.detail_file = None; self.load(); self.said("last remote commit redone".to_string()); }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
            Action::Amend => {
                let message = compose(&self.subject, &self.body);
                match self.repo().and_then(|repo| repo.amend(&message)) {
                    Ok(oid) => {
                        self.saved = None;
                        self.subject.clear();
                        self.body.clear();
                        // The old one no longer exists: the cursor goes to the
                        // one replacing it, not to a gap.
                        self.selected_commit = Some(oid);
                        self.detail_file = None;
                        self.load();
                        self.said("last commit redone".to_string());
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
            Action::Commit if self.is_remote() => {
                let message = compose(&self.subject, &self.body);
                let result = self.remote.as_ref().ok_or_else(|| anyhow::anyhow!("no remote checkout")).and_then(|repo| repo.commit(&message));
                match result {
                    Ok(oid) => { self.subject.clear(); self.body.clear(); self.selected_commit = Some(oid); self.detail_file = None; self.load(); self.said(format!("commit created on {}", self.head)); }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
            Action::Commit => {
                let message = compose(&self.subject, &self.body);
                match self.repo().and_then(|repo| repo.commit(&message)) {
                    Ok(oid) => {
                        self.subject.clear();
                        self.body.clear();
                        // The cursor stays on what has just been written: the
                        // changes row it was on has just stopped existing.
                        self.selected_commit = Some(oid);
                        self.detail_file = None;
                        // The commit changes the history: redraw the graph.
                        self.load();
                        self.said(format!("commit created on {}", self.head));
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
        }
    }
}

impl Open {
    fn act(&mut self, action: action_bar::Action) {
        use action_bar::Action;

        match action {
            // The two that go out on the network leave for a thread of their
            // own. The rest are local and finish before anyone notices.
            Action::Pull(mode) => self.spawn("pulling", move |repo| repo.pull(mode)),
            Action::Fetch => self.spawn("fetching", |repo| repo.fetch_all()),
            Action::Push => self.spawn("pushing", |repo| repo.push()),
            Action::PushForce => self.spawn("forcing", |repo| repo.push_force()),
            Action::PushTags => self.spawn("sending tags", |repo| repo.push_tags()),

            Action::Branch(name) => match self.repo().and_then(|repo| repo.branch_here(&name)) {
                Ok(()) => {
                    self.load();
                    self.said(format!("you are now on {name}"));
                }
                Err(err) => self.note_error(format!("{err:#}")),
            },

            Action::StashSave(message) => self.local(|repo| repo.stash_save(&message)),
            Action::StashPop(index) => self.local(|repo| repo.stash_pop(index)),
            Action::StashApply(index) => self.local(|repo| repo.stash_apply(index)),
            Action::StashDrop(index) => self.local(|repo| repo.stash_drop(index)),
        }
    }

    /// A local operation that leaves word of how it went.
    fn local(&mut self, op: impl FnOnce(&mut Repo) -> Result<String>) {
        match self.repo().and_then(|mut repo| op(&mut repo)) {
            Ok(note) => {
                self.refresh();
                self.said(note);
            }
            Err(err) => self.note_error(format!("{err:#}")),
        }
    }
}

impl Open {
    /// Everything this repository draws, in the order it is drawn.
    fn draw(&mut self, ctx: &egui::Context, settings: &mut Settings) {
        self.top_bar(ctx, settings);
        self.confirm_dialog(ctx);
        self.note_dialog(ctx);
        self.name_dialog(ctx);
        self.join_menu(ctx);
        self.status_bar(ctx);
        self.refs_panel(ctx);

        if self.view == View::History
            && self.looking.is_none()
            && self.deciding.is_none()
            && self.rewriting.is_none()
        {
            self.detail_panel(ctx);
        }

        // Something half-done is the one thing in this window that cannot
        // wait. It is said in a band under the bar, on every view, until it
        // is seen through or thrown away.
        self.half_bar(ctx);

        if self.deciding.is_some() {
            let mut act = None;
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::CANVAS)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ctx, |ui| {
                    ui.push_id(self.salt, |ui| {
                        if let Some(state) = &self.deciding {
                            act = conflict_view::show(
                                ui,
                                self.half,
                                &self.clashes,
                                state,
                                self.job.is_some(),
                            );
                        }
                    });
                });
            if let Some(act) = act {
                self.act_on_conflict(act);
            }
            self.toasting(ctx);
            return;
        }

        // Arranging a rewrite takes the middle of the window too, and for
        // the same reason: it is a list you read before you run it.
        if self.rewriting.is_some() {
            let mut act = None;
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::CANVAS)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ctx, |ui| {
                    ui.push_id(self.salt, |ui| {
                        if let Some(plan) = &self.rewriting {
                            act = rewrite_view::show(ui, plan, self.job.is_some());
                        }
                    });
                });
            if let Some(act) = act {
                self.act_on_rewrite(act);
            }
            self.toasting(ctx);
            return;
        }

        // A file open across the window takes the middle of it. The views
        // above stay where they are: pressing one is a way out of here, and
        // taking them away would leave only the Back button.
        if self.looking.is_some() {
            let mut act = None;
            let busy = self.job.is_some();
            let editable = self.is_remote();
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::NONE
                        .fill(theme::CANVAS)
                        .inner_margin(egui::Margin::symmetric(14, 0)),
                )
                .show(ctx, |ui| {
                    ui.push_id(self.salt, |ui| {
                        if let Some(looking) = &mut self.looking {
                            act = file_view::show(ui, looking, busy, editable);
                        }
                    });
                });
            if let Some(act) = act {
                self.act_on_file(act);
            }
            self.toasting(ctx);
            return;
        }

        let frame = match self.view {
            // The graph is painted on the deepest background in the window so
            // that the lanes are the brightest thing on screen.
            View::History => egui::Frame::NONE.fill(theme::CANVAS),
            View::Changes => egui::Frame::NONE.fill(theme::PANEL),
            // The board is painted on the canvas so that the cards, which wear
            // the panels' color, lift off the background.
            View::Board => egui::Frame::NONE
                .fill(theme::CANVAS)
                .inner_margin(egui::Margin::symmetric(12, 0)),
        };

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // Each tab keeps its own scroll positions and folds. Without
            // this they are all one window as far as egui is concerned,
            // and scrolling one repository's history scrolls the rest.
            ui.push_id(self.salt, |ui| match self.view {
                View::History => {
                    // With a detached HEAD there is no branch for the menu to
                    // merge into or move up, the same as in the sidebar.
                    let on_branch = self.refs.iter().any(|entry| entry.is_head);
                    match graph_view::finder(ui, &mut self.needle, self.found.len(), self.at) {
                        graph_view::Found::Changed => {
                            self.recount();
                            self.land();
                        }
                        graph_view::Found::Next => self.jump(true),
                        graph_view::Found::Prev => self.jump(false),
                        graph_view::Found::Nothing => {}
                    }

                    let needle = self.needle.trim().to_lowercase();
                    let asked = graph_view::show(
                        ui,
                        &self.graph,
                        &self.refs,
                        self.selected_commit,
                        &self.work,
                        &needle,
                        self.bring.take(),
                        on_branch.then_some(self.head.as_str()),
                        self.job.is_some(),
                        settings.waves,
                    );
                    if let Some(oid) = asked.show {
                        self.select_commit(oid);
                    }
                    if let Some(picked) = asked.pick {
                        if self.is_remote() {
                            if let refs_view::Pick::Show(oid) = picked {
                                self.reveal_commit(oid);
                            }
                        } else {
                            self.act_on_ref(picked, ctx);
                        }
                    }
                }
                View::Board => {
                    // The forge is asked the first time the board is looked
                    // at, not when the window opens: somebody who never leaves
                    // the history should not be making network calls.
                    if self.pulls.is_none() && self.forge_note.is_none() {
                        self.ask_forge();
                    }

                    let state = board_view::Forge {
                        asking: self.forge.is_some(),
                        note: self.forge_note.as_deref(),
                    };

                    // The checkout you are looking at is not somewhere else,
                    // so a card for a branch open here says nothing about
                    // folders: you are standing in it.
                    let here = std::path::Path::new(&self.path);
                    let elsewhere: Vec<Worktree> = self
                        .worktrees
                        .iter()
                        .filter(|tree| tree.path != here)
                        .cloned()
                        .collect();
                    let picked = match &self.board {
                        Some(board) => board_view::show(
                            ui,
                            board,
                            &mut self.project,
                            self.job.is_some(),
                            state,
                            board_view::Waiting {
                                backlog: &self.backlog,
                                draft: &mut self.backlog_draft,
                            },
                            &self.trail,
                            &elsewhere,
                        ),
                        None if self.laying.is_some() => {
                            theme::empty(ui, "Working the board out…");
                            None
                        }
                        None => {
                            theme::empty(ui, "Open a repository to see its work");
                            None
                        }
                    };

                    match picked {
                        Some(board_view::Act::Pick(refs_view::Pick::Show(oid))) => {
                            self.reveal_commit(oid);
                            self.view = View::History;
                        }
                        Some(board_view::Act::Pick(refs_view::Pick::Switch { name, remote })) => {
                            self.switch(name, remote)
                        }
                        // Pushing is what moves a card into review only while
                        // nobody has asked the forge. Once it has answered,
                        // what moves it is a pull request, so that is what the
                        // drop does.
                        Some(board_view::Act::Publish(branch)) => {
                            let doing = format!("pushing {branch}");
                            self.spawn(&doing, move |repo| repo.push_branch(&branch));
                        }
                        Some(board_view::Act::Review(branch)) => self.ask_review(branch),
                        Some(board_view::Act::Land(branch)) => self.ask_land(branch),
                        Some(board_view::Act::Reload) => {
                            self.pulls = None;
                            self.forge_note = None;
                            self.ask_forge();
                        }
                        Some(board_view::Act::Open(url)) => self.open_in_browser(&url),

                        // Going in and coming back out. Neither touches the
                        // repository: they measure the same board against a
                        // different branch.
                        Some(board_view::Act::Enter(branch)) => {
                            let mut trail = self.trail.clone();
                            trail.push(branch);
                            self.walk_to(trail);
                        }
                        Some(board_view::Act::Up(depth)) => {
                            let mut trail = self.trail.clone();
                            trail.truncate(depth + 1);
                            self.walk_to(trail);
                        }

                        // Opening a branch somewhere else writes a whole
                        // working tree, so it goes off to a thread like the
                        // rest of what takes a moment.
                        Some(board_view::Act::Describe(branch)) => self.describe(branch),
                        Some(board_view::Act::Isolate(branch)) => {
                            let doing = format!("opening {branch} in a folder of its own");
                            self.spawn(&doing, move |repo| repo.add_worktree(&branch));
                        }
                        Some(board_view::Act::Visit(path)) => self.visit(path),
                        Some(board_view::Act::Release(name)) => self.ask_release(name),

                        // The backlog. Writing is instant and local; sending
                        // it out is the only part that goes near a network.
                        Some(board_view::Act::Note(title)) => {
                            let project = self.project.clone();
                            self.note_down(move |repo| {
                                repo.backlog_add(&title, project.as_deref())
                            });
                        }
                        Some(board_view::Act::Start(id)) => {
                            self.note_down(move |repo| repo.backlog_start(&id))
                        }
                        Some(board_view::Act::Drop(id)) => {
                            self.note_down(move |repo| repo.backlog_remove(&id))
                        }
                        Some(board_view::Act::Share) => {
                            self.spawn("sharing the backlog", |repo| repo.backlog_share())
                        }
                        _ => {}
                    }
                }
                View::Changes => {
                    let read_only = false;
                    let (amendable, pushed) = if self.is_remote() {
                        (!self.graph.rows.is_empty(), None)
                    } else {
                        self.amend_state()
                    };
                    let selected = self
                        .selected
                        .as_ref()
                        .map(|(path, staged)| (path.as_str(), *staged));

                    let mut draft = changes_view::Draft {
                        subject: &mut self.subject,
                        body: &mut self.body,
                        amend: &mut self.amend,
                        amendable,
                        pushed: pushed.as_deref(),
                    };

                    if read_only {
                        ui.label(
                            egui::RichText::new(
                                "Remote checkout — staging and commits arrive in deliverable 3.",
                            )
                            .color(theme::MUTED),
                        );
                    }
                    let action = changes_view::show(
                        ui,
                        &self.work,
                        selected,
                        self.diff.as_ref(),
                        &mut draft,
                        read_only,
                    );

                    if let Some(action) = action {
                        if !read_only || matches!(action, changes_view::Action::Select { .. }) {
                            self.handle(action);
                        }
                    }
                }
            })
        });

        self.toasting(ctx);
    }

    /// The band that says something is half-done.
    ///
    /// It is not a toast and it does not go away on its own. A repository in
    /// the middle of a merge behaves differently from one that is not --
    /// committing writes a merge, switching branches refuses -- and anything
    /// that changes what the window means has to stay on the screen while it
    /// is true.
    fn half_bar(&mut self, ctx: &egui::Context) {
        if self.half == gitrust_core::Half::None || self.deciding.is_some() {
            return;
        }

        let mut decide = false;
        egui::TopBottomPanel::top(egui::Id::new(("half", self.salt)))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::symmetric(12, 7)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} is half-done here.",
                            match self.half {
                                gitrust_core::Half::Merge => "A merge",
                                gitrust_core::Half::Rebase => "A rebase",
                                gitrust_core::Half::CherryPick => "A commit being taken along",
                                gitrust_core::Half::Revert => "A commit being undone",
                                _ => "Something",
                            }
                        ))
                        .font(theme::medium(12.5))
                        .color(theme::HEAD),
                    );
                    ui.label(
                        egui::RichText::new(match self.clashes.len() {
                            0 => "Nothing left to decide.".to_string(),
                            1 => "1 file still has both answers in it.".to_string(),
                            n => format!("{n} files still have both answers in them."),
                        })
                        .font(theme::body(12.0))
                        .color(theme::MUTED),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("See it through").font(theme::medium(12.0)),
                                )
                                .fill(theme::CHOSEN),
                            )
                            .clicked()
                        {
                            decide = true;
                        }
                    });
                });
            });

        if decide {
            self.decide();
        }
    }

    /// Last, over the panels and after whatever has just been pressed, so
    /// that a toast asked for this very frame is up before the frame ends.
    /// It takes itself away when its time is up.
    fn toasting(&mut self, ctx: &egui::Context) {
        if let Some(toast) = &self.toast {
            if !toast::show(ctx, toast) {
                self.toast = None;
            }
        }
    }
}

impl Open {
    fn top_bar(&mut self, ctx: &egui::Context, settings: &mut Settings) {
        if self.is_remote() {
            return self.remote_top_bar(ctx);
        }
        egui::TopBottomPanel::top(egui::Id::new(("bar", self.salt)))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    self.repo_menu(ui);
                    theme::vrule(ui);
                    self.views(ui);
                    theme::vrule(ui);

                    // Where HEAD is, in the window's one and only amber. It
                    // sits by the actions because they all end up there.
                    if !self.head.is_empty() {
                        ui.label(
                            egui::RichText::new(&self.head)
                                .font(theme::medium(12.0))
                                .color(theme::HEAD)
                                .background_color(theme::tint(theme::HEAD, 28)),
                        );
                        // A breath before the actions: HEAD's amber and the
                        // counts' amber are almost the same, and side by side
                        // they would read as one single stain of color.
                        ui.add_space(11.0);
                    }

                    let action = action_bar::show(
                        ui,
                        self.tracking.as_ref(),
                        &self.stashes,
                        &mut self.pull_mode,
                        self.job.is_some(),
                        &mut self.draft,
                    );

                    let mut reload = false;
                    // Taken out and put back rather than lent to the closure:
                    // the closure already holds the rest of us.
                    let mut waves = settings.waves;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        reload = ui
                            .add_enabled(self.job.is_none(), egui::Button::new("↻"))
                            .on_hover_text("Reload")
                            .clicked();
                        ui.add_space(6.0);
                        ui.toggle_value(&mut waves, "Waves")
                            .on_hover_text("The wave the pointer leaves in the graph");
                    });

                    if waves != settings.waves {
                        settings.waves = waves;
                        // Said out loud only when it fails. A preference that
                        // cannot be written down is one that will be back on
                        // tomorrow, and finding that out tomorrow is worse
                        // than being told now.
                        if let Err(err) = settings.save() {
                            self.note_error(format!("could not remember that: {err:#}"));
                        }
                    }

                    if let Some(action) = action {
                        self.act(action);
                    } else if reload {
                        self.load();
                    }
                });
            });
    }

    fn remote_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top(egui::Id::new(("bar", self.salt)))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Remote checkout · read-only")
                            .font(theme::medium(12.5))
                            .color(theme::HEAD),
                    );
                    theme::vrule(ui);
                    ui.selectable_value(&mut self.view, View::History, "History");
                    ui.selectable_value(&mut self.view, View::Changes, "Changes");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("↻")
                            .on_hover_text("Reload remote checkout")
                            .clicked()
                        {
                            self.load();
                        }
                    });
                })
            });
    }

    /// Which repository is open, and how to change it.
    ///
    /// Day to day the folder's name is enough. The whole path, which ate a
    /// third of the bar to say almost nothing, has gone to the popup where it
    /// really is used: when opening another repository.
    fn repo_menu(&mut self, ui: &mut egui::Ui) {
        let name = std::path::Path::new(self.path.trim_end_matches('/'))
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string());

        let button =
            ui.button(egui::RichText::new(format!("{name}     ")).font(theme::medium(12.5)));
        theme::caret(ui, &button);
        let hint = self.path.clone();
        let button = button.on_hover_text(hint);
        let id = ui.make_persistent_id("menu-repo");

        if button.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(id));
        }

        let open = egui::popup::popup_below_widget(
            ui,
            id,
            &button,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_width(REPO_MENU);
                let mut go = false;

                ui.horizontal(|ui| {
                    let entry = ui.add(
                        egui::TextEdit::singleline(&mut self.path)
                            .desired_width(ui.available_width() - 66.0)
                            .margin(egui::vec2(8.0, 5.0))
                            .font(theme::mono(11.5))
                            .hint_text(
                                egui::RichText::new("path to a repository").color(theme::FAINT),
                            ),
                    );
                    go = ui.button("Open").clicked()
                        || (entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                });

                // The other folders this same repository is checked out in.
                // They belong here rather than anywhere else, because this is
                // the control that answers "where am I", and going to one of
                // them is the same move as opening a repository: the window
                // looks somewhere else and nothing is written.
                if go {
                    ui.memory_mut(|memory| memory.close_popup());
                }
                go
            },
        )
        .unwrap_or(false);

        if open {
            self.selected = None;
            self.diff = None;
            self.selected_commit = None;
            self.detail = None;
            // Another repository is somewhere you have not gone into yet,
            // even when it happens to have a branch of the same name.
            self.trail.clear();
            self.project = None;
            self.load();
        }
    }

    /// The two views, flush together like a two-position control.
    fn views(&mut self, ui: &mut egui::Ui) {
        let pending = self.work.staged.len() + self.work.unstaged.len();

        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.selectable_value(&mut self.view, View::History, "History");

            let changes = ui.selectable_value(&mut self.view, View::Changes, "Changes");
            ui.add_space(2.0);
            ui.selectable_value(&mut self.view, View::Board, "Board");
            ui.add_space(2.0);
            if pending > 0 {
                ui.label(
                    egui::RichText::new(pending.to_string())
                        .font(theme::mono(10.5))
                        .color(theme::CHANGED)
                        .background_color(theme::tint(theme::CHANGED, 32)),
                )
                .on_hover_text("changes not committed");
            }
            let _ = changes;
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom(egui::Id::new(("status", self.salt)))
            .frame(
                egui::Frame::NONE
                    .fill(theme::RAISED)
                    .inner_margin(egui::Margin::symmetric(12, 5)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&self.summary)
                            .font(theme::body(11.5))
                            .color(theme::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let color = if self.status_failed {
                            theme::REMOVED
                        } else {
                            theme::FAINT
                        };
                        ui.label(
                            egui::RichText::new(&self.status)
                                .font(theme::body(11.5))
                                .color(color),
                        );
                    });
                });
            });
    }

    /// The references come along always, whether you look at the history or
    /// at the changes.
    fn refs_panel(&mut self, ctx: &egui::Context) {
        let mut picked = None;
        // With a detached HEAD there is no branch to bring anything into.
        let on_branch = self.refs.iter().any(|entry| entry.is_head);

        egui::SidePanel::left(egui::Id::new(("refs-panel", self.salt)))
            .resizable(true)
            .default_width(222.0)
            .width_range(170.0..=360.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(ctx, |ui| {
                ui.add_enabled_ui(!self.is_remote(), |ui| {
                    picked = refs_view::show(
                        ui,
                        &self.refs,
                        &self.worktrees,
                        std::path::Path::new(&self.path),
                        &mut self.ref_filter,
                        self.selected_commit,
                        on_branch.then_some(self.head.as_str()),
                        self.job.is_some(),
                    );
                });
            });

        if let Some(picked) = picked {
            self.act_on_ref(picked, ctx);
        }
    }

    /// Carries out whatever was asked of a reference.
    ///
    /// The sidebar and the graph offer the same menu, so they answer to the
    /// same place: anything added here is in both at once, which is the point
    /// of them sharing it.
    fn act_on_ref(&mut self, picked: refs_view::Pick, ctx: &egui::Context) {
        match picked {
            refs_view::Pick::Show(oid) => {
                self.reveal_commit(oid);
                self.view = View::History;
            }
            refs_view::Pick::Switch { name, remote } => self.switch(name, remote),
            refs_view::Pick::Merge(name) => {
                // It can rewrite the whole tree, so it goes apart.
                let doing = format!("merging {name}");
                self.spawn(&doing, move |repo| repo.merge(&name));
            }
            refs_view::Pick::DeleteBranch { name, remote } => self.ask_delete_branch(name, remote),
            refs_view::Pick::FastForward(name) => {
                self.note_down(move |repo| repo.fast_forward_to(&name))
            }
            refs_view::Pick::Rebase(name) => {
                let doing = format!("rebasing onto {name}");
                self.spawn(&doing, move |repo| repo.rebase_onto(&name));
            }
            refs_view::Pick::Pull => {
                self.spawn("pulling", move |repo| repo.pull(PullMode::Merge));
            }
            refs_view::Pick::Push(name) => {
                let doing = format!("pushing {name}");
                self.spawn(&doing, move |repo| repo.push_branch(&name));
            }
            refs_view::Pick::Follow { branch, upstream } => {
                self.note_down(move |repo| repo.set_upstream(&branch, upstream.as_deref()))
            }
            refs_view::Pick::Isolate(name) => {
                let doing = format!("opening {name} in a folder of its own");
                self.spawn(&doing, move |repo| repo.add_worktree(&name));
            }
            refs_view::Pick::GoTo(oid) => {
                let doing = format!("going to {}", &oid.to_string()[..7]);
                self.spawn(&doing, move |repo| repo.checkout_commit(oid));
            }
            refs_view::Pick::BranchAt(oid) => {
                self.naming = Some(Naming {
                    title: format!("A branch at {}", &oid.to_string()[..7]),
                    hint: "feat/something".to_string(),
                    verb: "Make it".to_string(),
                    draft: String::new(),
                    deed: Named::BranchAt(oid),
                });
            }
            refs_view::Pick::Rename(name) => {
                self.naming = Some(Naming {
                    title: format!("Rename {name}"),
                    hint: name.clone(),
                    verb: "Rename".to_string(),
                    draft: name.clone(),
                    deed: Named::Rename(name),
                });
            }
            refs_view::Pick::CherryPick(oid) => {
                let doing = format!("taking {} along", &oid.to_string()[..7]);
                self.spawn(&doing, move |repo| repo.cherry_pick(oid));
            }
            refs_view::Pick::Revert(oid) => {
                let doing = format!("undoing {}", &oid.to_string()[..7]);
                self.spawn(&doing, move |repo| repo.revert(oid));
            }
            refs_view::Pick::MoveTo(oid, how) => self.ask_move(oid, how),
            refs_view::Pick::Rewrite(oid) => self.start_rewrite(oid),
            refs_view::Pick::Visit(path) => self.visit(path),
            refs_view::Pick::VisitTab(path) => self.elsewhere = Some(path),
            refs_view::Pick::Release(name) => self.ask_release(name),
            refs_view::Pick::Join { from, onto } => self.ask_join(from, onto, ctx),
            refs_view::Pick::Copy(text) => {
                ctx.copy_text(text.clone());
                self.said(format!("{text} copied"));
            }
            refs_view::Pick::DeleteTag(name) => {
                match self.repo().and_then(|repo| repo.delete_tag(&name)) {
                    Ok(()) => {
                        self.load();
                        self.said(format!("tag {name} deleted"));
                    }
                    Err(err) => self.note_error(format!("{err:#}")),
                }
            }
        }
    }

    /// Switches branch on a thread of its own.
    ///
    /// It is local, but it is the operation that can write the most files of
    /// all the ones here: between two distant branches half the tree is
    /// rewritten, and with the window frozen that reads as the app having died.
    fn switch(&mut self, name: String, remote: bool) {
        // The tab stays where it is: if you were in the changes you were
        // working on files, and switching branch is no reason to send you to
        // another screen.
        //
        // The cursor is let go before leaving, though: on reaching the new
        // branch what you want to see is its last commit, not the place in the
        // old branch where you had been looking.
        self.selected_commit = None;
        self.detail_file = None;

        let doing = format!("switching to {name}");
        self.spawn(&doing, move |repo| {
            if remote {
                repo.checkout_remote(&name)
            } else {
                repo.checkout_branch(&name)
            }
        });
    }

    fn detail_panel(&mut self, ctx: &egui::Context) {
        // With the changes row picked, the right panel becomes the whole
        // working tree: lists, commit box and diff. That way you stage and
        // commit without leaving the graph, which is what makes the two views
        // feel like one.
        let working = self.selected_commit.is_some_and(|oid| oid.is_zero());
        let (amendable, pushed) = self.amend_state();
        let mut clicked = None;
        let mut action = None;

        // How far it can be dragged open. There is a cap because without one
        // a commit with a long message widens the panel until the graph's
        // column is nothing, but the cap was a fixed 520 pixels, which is a
        // third of a laptop and a quarter of a desk monitor: on a wide screen
        // it stopped the panel while there was still room to spare, and a
        // diff is read at whatever width there is. Half the window is as far
        // as it can go and still leave a graph behind it. The floor is the
        // old number, so a narrow window is no worse off than before.
        let widest = (ctx.screen_rect().width() * 0.5).max(520.0);

        egui::SidePanel::right(egui::Id::new(("detail", self.salt)))
            .resizable(true)
            .default_width(420.0)
            .width_range(300.0..=widest)
            .frame(
                egui::Frame::NONE
                    .fill(theme::PANEL)
                    .inner_margin(egui::Margin::symmetric(14, 0)),
            )
            .show(ctx, |ui| {
                if working {
                    let selected = self
                        .selected
                        .as_ref()
                        .map(|(path, staged)| (path.as_str(), *staged));

                    let mut draft = changes_view::Draft {
                        subject: &mut self.subject,
                        body: &mut self.body,
                        amend: &mut self.amend,
                        amendable,
                        pushed: pushed.as_deref(),
                    };

                    action = changes_view::compact(
                        ui,
                        &self.work,
                        selected,
                        self.diff.as_ref(),
                        &mut draft,
                    );
                } else {
                    let read_only = self.is_remote();
                    clicked = detail_view::show(
                        ui,
                        self.detail.as_ref(),
                        self.detail_file.as_deref(),
                        self.detail_diff.as_ref(),
                        &mut self.tag,
                        read_only,
                    );
                }
            });

        if let Some(action) = action {
            self.handle(action);
        }
        match clicked {
            Some(detail_view::Pick::File(path)) => self.select_detail_file(path),
            Some(detail_view::Pick::Open(path)) if !self.is_remote() => self.look_at(path),
            Some(detail_view::Pick::Tag { name, message }) if !self.is_remote() => {
                self.create_tag(name, message)
            }
            Some(detail_view::Pick::Open(_)) | Some(detail_view::Pick::Tag { .. }) => {}
            None => {}
        }
    }

    /// Tags the commit being looked at.
    fn create_tag(&mut self, name: String, message: String) {
        let Some(oid) = self.selected_commit.filter(|oid| !oid.is_zero()) else {
            return self.note_error("no commit is selected".to_string());
        };

        match self.repo().and_then(|repo| repo.tag(&name, oid, &message)) {
            Ok(()) => {
                // The new tag has to show up in the graph and in the sidebar,
                // and that only happens by reading the references again.
                self.load();
                self.said(format!("tag {name} created"));
            }
            Err(err) => self.note_error(format!("{err:#}")),
        }
    }
}
