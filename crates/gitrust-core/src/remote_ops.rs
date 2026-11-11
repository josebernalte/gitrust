//! Mutating Git operations for a checkout reached through SSH.

use anyhow::{Context, Result};
use git2::Oid;

use crate::{Clash, Contents, Half, PullMode, RemoteRepo, StashEntry, Tracking};

impl RemoteRepo {
    pub fn half_done(&self) -> Half {
        for (file, half) in [("MERGE_HEAD", Half::Merge), ("CHERRY_PICK_HEAD", Half::CherryPick), ("REVERT_HEAD", Half::Revert)] {
            if self.git_path_exists(file) { return half; }
        }
        if self.git_path_exists("rebase-merge") || self.git_path_exists("rebase-apply") { Half::Rebase } else { Half::None }
    }

    /// Every unmerged path plus Git's three index stages, in the same shape
    /// as a local conflict so the UI remains transport-agnostic.
    pub fn clashes(&self) -> Result<Vec<Clash>> {
        let status = self.git(&["status", "--porcelain=v1", "-z"])?;
        let mut clashes = status.split('\0').filter_map(|row| {
            let bytes = row.as_bytes();
            (bytes.len() >= 4 && (bytes[0] == b'U' || bytes[1] == b'U')).then(|| row[3..].to_string())
        }).map(|path| {
            Ok(Clash {
                base: self.conflict_version(1, &path)?,
                ours: self.conflict_version(2, &path)?,
                theirs: self.conflict_version(3, &path)?,
                path,
            })
        }).collect::<Result<Vec<_>>>()?;
        clashes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(clashes)
    }

    fn conflict_version(&self, stage: u8, path: &str) -> Result<Contents> {
        let spec = format!(":{stage}:{path}");
        let size = match self.git(&["cat-file", "-s", &spec]) {
            Ok(size) => size.trim().parse::<usize>().context("remote Git returned an invalid blob size")?,
            Err(_) => return Ok(Contents::Absent),
        };
        match self.git(&["show", &spec]) {
            Ok(text) => Ok(Contents::Text(text)),
            // Channels are text based: preserve access to the paused state
            // even when an index stage is binary.
            Err(_) => Ok(Contents::Binary(size)),
        }
    }

    pub fn take_conflict_side(&self, path: &str, theirs: bool) -> Result<()> {
        let stage = if theirs { 3 } else { 2 };
        let spec = format!(":{stage}:{path}");
        if self.git(&["cat-file", "-e", &spec]).is_err() {
            // Taking a deleted side resolves the path by deleting it, rather
            // than asking `checkout --ours` for a blob that does not exist.
            return self.git(&["rm", "--", path]).map(|_| ());
        }
        self.git(&["checkout", if theirs { "--theirs" } else { "--ours" }, "--", path])?;
        self.git(&["add", "--", path]).map(|_| ())
    }

    pub fn continue_operation(&self) -> Result<String> {
        match self.half_done() {
            Half::Merge => { self.git(&["merge", "--continue"])?; Ok("merge completed".to_string()) }
            Half::Rebase => { self.git(&["rebase", "--continue"])?; Ok("rebase continued".to_string()) }
            Half::CherryPick => { self.git(&["cherry-pick", "--continue"])?; Ok("cherry-pick continued".to_string()) }
            Half::Revert => { self.git(&["revert", "--continue"])?; Ok("revert continued".to_string()) }
            Half::None | Half::Other => anyhow::bail!("there is no remote operation to continue"),
        }
    }

    pub fn abort_operation(&self) -> Result<String> {
        match self.half_done() {
            Half::Merge => { self.git(&["merge", "--abort"])?; Ok("merge aborted".to_string()) }
            Half::Rebase => { self.git(&["rebase", "--abort"])?; Ok("rebase aborted".to_string()) }
            Half::CherryPick => { self.git(&["cherry-pick", "--abort"])?; Ok("cherry-pick aborted".to_string()) }
            Half::Revert => { self.git(&["revert", "--abort"])?; Ok("revert aborted".to_string()) }
            Half::None | Half::Other => anyhow::bail!("there is no remote operation to abort"),
        }
    }

    fn git_path_exists(&self, path: &str) -> bool {
        self.git(&["rev-parse", "--path-format=absolute", "--git-path", path])
            .ok().is_some_and(|path| self.session.exists(path.trim()))
    }
    pub fn branch_at(&self, name: &str, oid: Oid) -> Result<String> {
        self.git(&["branch", name, &oid.to_string()])?;
        Ok(format!("{name} created"))
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<String> {
        self.git(&["branch", "-m", old, new])?;
        Ok(format!("{old} is now {new}"))
    }

    pub fn delete_branch(&self, name: &str) -> Result<()> {
        self.git(&["branch", "-d", name]).map(|_| ())
    }

    pub fn checkout_branch(&self, name: &str) -> Result<String> {
        self.git(&["switch", name])?;
        Ok(format!("now on {name}"))
    }

    pub fn branch_here(&self, name: &str) -> Result<String> {
        self.git(&["switch", "-c", name])?;
        Ok(format!("you are now on {name}"))
    }

    pub fn checkout_commit(&self, oid: Oid) -> Result<String> {
        self.git(&["switch", "--detach", &oid.to_string()])?;
        Ok(format!("now at {}", &oid.to_string()[..7]))
    }

    pub fn checkout_remote(&self, name: &str) -> Result<String> {
        let (_, branch) = name.split_once('/').context("a remote branch is named remote/branch")?;
        self.git(&["switch", "--track", "-c", branch, name])?;
        Ok(format!("now on {branch}, following {name}"))
    }

    pub fn fetch_all(&self) -> Result<String> {
        self.git(&["fetch", "--all", "--prune"])?;
        Ok("fetched every remote".to_string())
    }

    pub fn pull(&self, mode: PullMode) -> Result<String> {
        let mut args = vec!["pull"];
        match mode { PullMode::Merge => {}, PullMode::FastForward => args.push("--ff-only"), PullMode::Rebase => args.push("--rebase") }
        self.git(&args)?;
        Ok("pulled the current branch".to_string())
    }

    pub fn push(&self) -> Result<String> {
        self.git(&["push"])?;
        Ok("pushed the current branch".to_string())
    }

    pub fn push_force(&self) -> Result<String> {
        self.git(&["push", "--force-with-lease"])?;
        Ok("force-pushed the current branch".to_string())
    }

    pub fn push_tags(&self) -> Result<String> {
        self.git(&["push", "--tags"])?;
        Ok("sent tags".to_string())
    }

    pub fn push_branch(&self, branch: &str) -> Result<String> {
        self.git(&["push", "-u", "origin", branch])?;
        Ok(format!("pushed {branch}"))
    }

    pub fn delete_remote_branch(&self, name: &str) -> Result<String> {
        let (remote, branch) = name.split_once('/').context("a remote branch is named remote/branch")?;
        self.git(&["push", remote, "--delete", branch])?;
        Ok(format!("deleted {name}"))
    }

    pub fn tracking(&self) -> Option<Tracking> {
        let remote = self.git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).ok()?;
        let remote = remote.trim();
        let counts = self.git(&["rev-list", "--left-right", "--count", "@{upstream}...HEAD"]).ok()?;
        let mut counts = counts.split_whitespace();
        let behind = counts.next()?.parse().ok()?;
        let ahead = counts.next()?.parse().ok()?;
        Some(Tracking { remote: remote.to_string(), ahead, behind })
    }

    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<String> {
        match upstream {
            Some(upstream) => self.git(&["branch", "--set-upstream-to", upstream, branch])?,
            None => self.git(&["branch", "--unset-upstream", branch])?,
        };
        Ok(format!("tracking updated for {branch}"))
    }

    pub fn tag(&self, name: &str, target: Oid, message: &str) -> Result<()> {
        anyhow::ensure!(!name.trim().is_empty(), "the tag needs a name");
        if message.trim().is_empty() { self.git(&["tag", name, &target.to_string()])?; }
        else { self.git(&["tag", "-a", name, &target.to_string(), "-m", message.trim()])?; }
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> { self.git(&["tag", "-d", name]).map(|_| ()) }

    pub fn stashes(&self) -> Result<Vec<StashEntry>> {
        let text = self.git(&["stash", "list", "--format=%gd%x1f%H%x1f%ct%x1f%gs"])?;
        Ok(text.lines().filter_map(|line| {
            let fields: Vec<_> = line.split('\x1f').collect();
            let index = fields.first()?.trim_start_matches("stash@{").trim_end_matches('}').parse().ok()?;
            Some(StashEntry { index, id: Oid::from_str(fields.get(1)?).ok()?, time: fields.get(2)?.parse().ok()?, message: fields.get(3)?.to_string() })
        }).collect())
    }

    pub fn stash_save(&self, message: &str) -> Result<String> { self.git(&["stash", "push", "-m", message])?; Ok("changes stashed".to_string()) }
    pub fn stash_pop(&self, index: usize) -> Result<String> { self.git(&["stash", "pop", &format!("stash@{{{index}}}")])?; Ok("changes restored".to_string()) }
    pub fn stash_apply(&self, index: usize) -> Result<String> { self.git(&["stash", "apply", &format!("stash@{{{index}}}")])?; Ok("changes applied, and still stashed".to_string()) }
    pub fn stash_drop(&self, index: usize) -> Result<String> { self.git(&["stash", "drop", &format!("stash@{{{index}}}")])?; Ok("stash dropped".to_string()) }

    pub fn merge(&self, name: &str) -> Result<String> {
        self.git(&["merge", name])?;
        Ok(format!("merged {name}"))
    }

    pub fn rebase_onto(&self, name: &str) -> Result<String> {
        self.git(&["rebase", name])?;
        Ok(format!("rebased onto {name}"))
    }

    pub fn cherry_pick(&self, oid: Oid) -> Result<String> {
        self.git(&["cherry-pick", &oid.to_string()])?;
        Ok(format!("took {} along", &oid.to_string()[..7]))
    }

    pub fn revert(&self, oid: Oid) -> Result<String> {
        self.git(&["revert", "--no-edit", &oid.to_string()])?;
        Ok(format!("undid {}", &oid.to_string()[..7]))
    }
}
