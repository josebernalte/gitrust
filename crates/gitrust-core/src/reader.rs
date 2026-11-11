//! The read-only boundary shared by local and remote checkouts.
//!
//! Views ask this boundary for plain Git models.  They never need to know
//! whether those models came from libgit2 on this computer or Git over SSH.

use anyhow::Result;

use crate::{Commit, CommitDetail, FileDiff, Oid, RefEntry, Repo, WorkingState};

/// The part of a checkout that the history and refs views may read.
pub trait Reader {
    /// A human-readable location, for a tab title or an error.
    fn location(&self) -> String;
    fn head_name(&self) -> String;
    fn commits(&self, limit: usize) -> Result<Vec<Commit>>;
    fn references(&self) -> Result<Vec<RefEntry>>;
    fn status(&self) -> Result<WorkingState>;
    fn file_diff(&self, path: &str, staged: bool) -> Result<FileDiff>;
    fn commit_file_diff(&self, oid: Oid, path: &str) -> Result<FileDiff>;
    fn commit_detail(&self, oid: Oid) -> Result<CommitDetail>;
}

impl Reader for Repo {
    fn location(&self) -> String {
        self.workdir().display().to_string()
    }

    fn head_name(&self) -> String {
        Repo::head_name(self)
    }

    fn commits(&self, limit: usize) -> Result<Vec<Commit>> {
        Repo::commits(self, limit)
    }

    fn references(&self) -> Result<Vec<RefEntry>> {
        Repo::references(self)
    }

    fn status(&self) -> Result<WorkingState> {
        Repo::status(self)
    }

    fn file_diff(&self, path: &str, staged: bool) -> Result<FileDiff> {
        Repo::file_diff(self, path, staged)
    }

    fn commit_detail(&self, oid: Oid) -> Result<CommitDetail> {
        Repo::commit_detail(self, oid)
    }

    fn commit_file_diff(&self, oid: Oid, path: &str) -> Result<FileDiff> {
        Repo::commit_file_diff(self, oid, path)
    }
}
