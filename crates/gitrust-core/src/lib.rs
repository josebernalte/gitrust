//! gitrust's core: reading the repository and working out the commit graph.
//!
//! This crate knows nothing about the user interface. Everything it exposes is
//! plain data, so the UI could be replaced without touching the Git logic.

pub mod backlog;
pub mod board;
pub mod conflict;
pub mod branch;
pub mod detail;
pub mod diff;
pub mod file;
pub mod forge;
pub mod graph;
pub mod history;
pub mod known;
pub mod merge;
pub mod message;
pub mod model;
pub mod note;
pub mod remote;
pub mod reader;
pub mod remote_repo;
pub mod remote_ops;
pub mod repo;
pub mod rewrite;
pub mod settings;
pub mod ssh;
pub mod stash;
pub mod start;
pub mod status;
pub mod tag;
pub mod worktree;
pub mod writer;

pub use backlog::{Backlog, Item};
pub use board::{Board, Card, Lane};
pub use conflict::{Clash, Half, Side};
pub use detail::{CommitDetail, CommitFile, RefEntry};
pub use git2::Oid;
pub use diff::{DiffLine, FileDiff, Hunk, HunkId};
pub use file::{Contents, Line, Story};
pub use forge::{PullRequest, PullRequestState, PullRequests};
pub use graph::{layout, layout_with_working};
pub use history::Move;
pub use known::Known;
pub use merge::Join;
pub use message::{blocks, compose, Block, SUBJECT_LIMIT};
pub use model::{Commit, Graph, Link, RefKind, RefLabel, Row};
pub use note::Note;
pub use remote::{PullMode, Tracking};
pub use reader::Reader;
pub use remote_repo::RemoteRepo;
pub use rewrite::{Plan, Step};
pub use repo::Repo;
pub use settings::Settings;
pub use stash::StashEntry;
pub use status::{ChangeKind, FileChange, WorkingState};
pub use worktree::Worktree;
pub use writer::Writer;
