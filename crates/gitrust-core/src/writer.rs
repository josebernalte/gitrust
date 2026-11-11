//! The working-tree writes shared by local and SSH-backed checkouts.

use anyhow::Result;
use git2::Oid;

use crate::{HunkId, Repo};

/// The safe whole-file writes used by the changes view.
pub trait Writer {
    fn stage(&self, path: &str) -> Result<()>;
    fn unstage(&self, path: &str) -> Result<()>;
    fn stage_hunk(&self, path: &str, hunk: HunkId) -> Result<()>;
    fn unstage_hunk(&self, path: &str, hunk: HunkId) -> Result<()>;
    fn discard(&self, path: &str) -> Result<()>;
    fn discard_hunk(&self, path: &str, hunk: HunkId) -> Result<()>;
    fn stage_all(&self, paths: &[String]) -> Result<()>;
    fn unstage_all(&self, paths: &[String]) -> Result<()>;
    fn discard_all(&self, paths: &[String]) -> Result<()>;
    fn commit(&self, message: &str) -> Result<Oid>;
    fn amend(&self, message: &str) -> Result<Oid>;
}

impl Writer for Repo {
    fn stage(&self, path: &str) -> Result<()> { Repo::stage(self, path) }
    fn unstage(&self, path: &str) -> Result<()> { Repo::unstage(self, path) }
    fn stage_hunk(&self, path: &str, hunk: HunkId) -> Result<()> { Repo::stage_hunk(self, path, hunk) }
    fn unstage_hunk(&self, path: &str, hunk: HunkId) -> Result<()> { Repo::unstage_hunk(self, path, hunk) }
    fn discard(&self, path: &str) -> Result<()> { Repo::discard(self, path) }
    fn discard_hunk(&self, path: &str, hunk: HunkId) -> Result<()> { Repo::discard_hunk(self, path, hunk) }
    fn stage_all(&self, paths: &[String]) -> Result<()> { Repo::stage_all(self, paths) }
    fn unstage_all(&self, paths: &[String]) -> Result<()> { Repo::unstage_all(self, paths) }
    fn discard_all(&self, paths: &[String]) -> Result<()> { Repo::discard_all(self, paths) }
    fn commit(&self, message: &str) -> Result<Oid> { Repo::commit(self, message) }
    fn amend(&self, message: &str) -> Result<Oid> { Repo::amend(self, message) }
}
