//! A read-only Git checkout whose commands run through an SSH session.

use std::collections::HashMap;

use anyhow::{Context, Result};
use git2::Oid;

use crate::{
    ssh::Session, ChangeKind, Commit, CommitDetail, CommitFile, DiffLine, FileChange, FileDiff,
    Hunk, HunkId, Reader, RefEntry, RefKind, RefLabel, WorkingState,
};

/// A remote checkout. Its session remains open for as long as this value does.
pub struct RemoteRepo {
    session: Session,
    path: String,
}

impl RemoteRepo {
    pub fn new(session: Session, path: String) -> Result<Self> {
        session.verify_checkout(&path)?;
        Ok(Self { session, path })
    }

    fn git(&self, arguments: &[&str]) -> Result<String> {
        let mut words = vec!["git", "-C", &self.path];
        words.extend(arguments);
        self.session.command(&words)
    }

    fn ref_labels(&self) -> Result<HashMap<Oid, Vec<RefLabel>>> {
        let mut labels = HashMap::new();
        let output = self.git(&["for-each-ref", "--format=%(objectname)%00%(refname)%00"])?;
        let fields: Vec<&str> = output.split('\0').collect();
        for pair in fields.chunks_exact(2) {
            let Ok(target) = Oid::from_str(pair[0]) else {
                continue;
            };
            let Some((kind, name)) = label(pair[1]) else {
                continue;
            };
            labels
                .entry(target)
                .or_insert_with(Vec::new)
                .push(RefLabel { name, kind });
        }
        Ok(labels)
    }
}

impl Reader for RemoteRepo {
    fn location(&self) -> String {
        format!(
            "{}@{}:{}",
            self.session.login().user,
            self.session.login().host,
            self.path
        )
    }

    fn head_name(&self) -> String {
        self.git(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|name| name.trim().to_string())
            .unwrap_or_else(|| "detached HEAD".to_string())
    }

    fn commits(&self, limit: usize) -> Result<Vec<Commit>> {
        let labels = self.ref_labels()?;
        let output = self.git(&[
            "log",
            "--all",
            "--topo-order",
            "--date-order",
            "--format=%H%x1f%h%x1f%an%x1f%at%x1f%P%x1f%s%x1e",
            &format!("-{limit}"),
        ])?;
        let head = self.head_name();
        output
            .split('\x1e')
            .filter(|row| !row.trim().is_empty())
            .map(|row| remote_commit(row, &labels, &head))
            .collect()
    }

    fn references(&self) -> Result<Vec<RefEntry>> {
        let head = self.head_name();
        let mut refs = self
            .ref_labels()?
            .into_iter()
            .flat_map(|(target, labels)| labels.into_iter().map(move |label| (target, label)))
            .map(|(target, label)| RefEntry {
                is_head: label.kind == RefKind::LocalBranch && label.name == head,
                name: label.name,
                kind: label.kind,
                target,
            })
            .collect::<Vec<_>>();
        refs.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(refs)
    }

    fn status(&self) -> Result<WorkingState> {
        let output = self.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
        Ok(remote_status(&output))
    }

    fn file_diff(&self, path: &str, staged: bool) -> Result<FileDiff> {
        let mut arguments = vec!["diff", "--no-ext-diff", "--unified=3"];
        if staged {
            arguments.push("--cached");
        }
        arguments.extend(["--", path]);
        parse_diff(path, &self.git(&arguments)?)
    }

    fn commit_detail(&self, oid: Oid) -> Result<CommitDetail> {
        let id = oid.to_string();
        let output = self.git(&[
            "show",
            "--no-patch",
            "--format=%H%x1f%h%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%at%x1f%P",
            &id,
        ])?;
        let fields: Vec<&str> = output.trim_end_matches('\n').split('\x1f').collect();
        anyhow::ensure!(
            fields.len() == 8,
            "remote Git returned an incomplete commit detail"
        );
        let files = self
            .git(&[
                "diff-tree",
                "--root",
                "--no-commit-id",
                "--name-status",
                "-z",
                "-r",
                &id,
            ])?;
        let files = commit_files(&files);
        Ok(CommitDetail {
            id: Oid::from_str(fields[0]).context("remote Git returned an invalid commit id")?,
            short_id: fields[1].to_string(),
            summary: fields[2].to_string(),
            body: fields[3].to_string(),
            author: fields[4].to_string(),
            email: fields[5].to_string(),
            time: fields[6]
                .parse()
                .context("remote Git returned an invalid commit time")?,
            parents: fields[7]
                .split_whitespace()
                .filter_map(|parent| Oid::from_str(parent).ok())
                .collect(),
            files,
        })
    }

    fn commit_file_diff(&self, oid: Oid, path: &str) -> Result<FileDiff> {
        let id = oid.to_string();
        parse_diff(
            path,
            &self.git(&[
                "show",
                "--format=",
                "--no-ext-diff",
                "--unified=3",
                &id,
                "--",
                path,
            ])?,
        )
    }
}

fn commit_files(output: &str) -> Vec<CommitFile> {
    let mut fields = output.split('\0');
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let Some(code) = status.chars().next() else {
            continue;
        };
        let path = match code {
            'R' | 'C' => {
                let _old = fields.next();
                fields.next()
            }
            _ => fields.next(),
        };
        let Some(path) = path else {
            break;
        };
        let kind = match code {
            'A' => ChangeKind::Added,
            'D' => ChangeKind::Deleted,
            'R' => ChangeKind::Renamed,
            'T' => ChangeKind::TypeChange,
            _ => ChangeKind::Modified,
        };
        files.push(CommitFile {
            path: path.to_string(),
            kind,
        });
    }
    files
}

fn parse_diff(path: &str, text: &str) -> Result<FileDiff> {
    let mut diff = FileDiff {
        path: path.to_string(),
        ..Default::default()
    };
    if text.contains("Binary files ") || text.contains("GIT binary patch") {
        diff.binary = true;
        return Ok(diff);
    }
    let mut current: Option<Hunk> = None;
    let mut old = 0;
    let mut new = 0;
    for line in text.lines() {
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                diff.hunks.push(hunk);
            }
            let id = hunk_id(line)?;
            old = id.old_start;
            new = id.new_start;
            current = Some(Hunk {
                header: line.to_string(),
                id,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            continue;
        };
        let Some(origin) = line.chars().next() else {
            continue;
        };
        if !matches!(origin, ' ' | '+' | '-') {
            continue;
        }
        let (old_lineno, new_lineno) = match origin {
            ' ' => {
                let pair = (Some(old), Some(new));
                old += 1;
                new += 1;
                pair
            }
            '-' => {
                let number = Some(old);
                old += 1;
                (number, None)
            }
            '+' => {
                let number = Some(new);
                new += 1;
                (None, number)
            }
            _ => unreachable!(),
        };
        hunk.lines.push(DiffLine {
            origin,
            content: line[1..].to_string(),
            old_lineno,
            new_lineno,
        });
    }
    if let Some(hunk) = current {
        diff.hunks.push(hunk);
    }
    Ok(diff)
}

fn hunk_id(header: &str) -> Result<HunkId> {
    let mut pieces = header.split_whitespace();
    let old = pieces
        .nth(1)
        .context("remote Git returned a malformed hunk")?;
    let new = pieces
        .next()
        .context("remote Git returned a malformed hunk")?;
    let (old_start, old_lines) = hunk_range(old.trim_start_matches('-'))?;
    let (new_start, new_lines) = hunk_range(new.trim_start_matches('+'))?;
    Ok(HunkId {
        old_start,
        old_lines,
        new_start,
        new_lines,
    })
}

fn hunk_range(range: &str) -> Result<(u32, u32)> {
    let (start, lines) = range.split_once(',').unwrap_or((range, "1"));
    Ok((
        start
            .parse()
            .context("remote Git returned an invalid hunk range")?,
        lines
            .parse()
            .context("remote Git returned an invalid hunk range")?,
    ))
}

fn remote_status(output: &str) -> WorkingState {
    let mut state = WorkingState::default();
    let mut records = output.split('\0');
    while let Some(record) = records.next() {
        if record.len() < 4 {
            continue;
        }
        let bytes = record.as_bytes();
        let staged = (bytes[0] as char != '?')
            .then(|| change_kind(bytes[0] as char, bytes[1] as char))
            .flatten();
        let unstaged = (bytes[1] as char != '?')
            .then(|| change_kind(bytes[1] as char, bytes[0] as char))
            .flatten()
            .or_else(|| {
                (bytes[0] as char == '?' && bytes[1] as char == '?')
                    .then_some(ChangeKind::Untracked)
            });
        let path = record[3..].to_string();
        if let Some(kind) = staged {
            state.staged.push(FileChange {
                path: path.clone(),
                kind,
                staged: true,
            });
        }
        if let Some(kind) = unstaged {
            state.unstaged.push(FileChange {
                path: path.clone(),
                kind,
                staged: false,
            });
        }
        // `-z` puts the source name of a rename/copy in the following record.
        if matches!(bytes[0] as char, 'R' | 'C') || matches!(bytes[1] as char, 'R' | 'C') {
            let _ = records.next();
        }
    }
    state.staged.sort_by(|a, b| a.path.cmp(&b.path));
    state.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
    state
}

fn change_kind(code: char, other: char) -> Option<ChangeKind> {
    Some(match code {
        ' ' => return None,
        '?' => ChangeKind::Untracked,
        'A' => ChangeKind::Added,
        'M' => ChangeKind::Modified,
        'D' => ChangeKind::Deleted,
        'R' => ChangeKind::Renamed,
        'C' | 'T' => ChangeKind::TypeChange,
        'U' | _ if other == 'U' => ChangeKind::Conflicted,
        _ => return None,
    })
}

fn remote_commit(row: &str, labels: &HashMap<Oid, Vec<RefLabel>>, head: &str) -> Result<Commit> {
    let fields: Vec<&str> = row.split('\x1f').collect();
    anyhow::ensure!(
        fields.len() == 6,
        "remote Git returned an incomplete history row"
    );
    let id = Oid::from_str(fields[0]).context("remote Git returned an invalid commit id")?;
    let mut refs = labels.get(&id).cloned().unwrap_or_default();
    if refs
        .iter()
        .any(|label| label.kind == RefKind::LocalBranch && label.name == head)
    {
        refs.insert(
            0,
            RefLabel {
                name: head.to_string(),
                kind: RefKind::Head,
            },
        );
    }
    Ok(Commit {
        id,
        short_id: fields[1].to_string(),
        author: fields[2].to_string(),
        time: fields[3]
            .parse()
            .context("remote Git returned an invalid commit time")?,
        parents: fields[4]
            .split_whitespace()
            .filter_map(|id| Oid::from_str(id).ok())
            .collect(),
        summary: fields[5].trim_end_matches('\n').to_string(),
        refs,
    })
}

fn label(reference: &str) -> Option<(RefKind, String)> {
    reference
        .strip_prefix("refs/heads/")
        .map(|name| (RefKind::LocalBranch, name.to_string()))
        .or_else(|| {
            reference
                .strip_prefix("refs/remotes/")
                .map(|name| (RefKind::RemoteBranch, name.to_string()))
        })
        .or_else(|| {
            reference
                .strip_prefix("refs/tags/")
                .map(|name| (RefKind::Tag, name.to_string()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names_keep_their_kind() {
        assert!(matches!(
            label("refs/heads/main"),
            Some((RefKind::LocalBranch, _))
        ));
        assert!(matches!(
            label("refs/remotes/origin/main"),
            Some((RefKind::RemoteBranch, _))
        ));
        assert!(matches!(label("refs/tags/v1"), Some((RefKind::Tag, _))));
    }

    #[test]
    fn status_keeps_staged_unstaged_and_renamed_files_apart() {
        let state =
            remote_status("M  staged.rs\0 M unstaged.rs\0RM renamed.rs\0before.rs\0?? new file\0");
        assert_eq!(state.staged.len(), 2);
        assert_eq!(state.unstaged.len(), 3);
        assert!(state
            .unstaged
            .iter()
            .any(|change| change.path == "renamed.rs"));
        assert!(state
            .unstaged
            .iter()
            .any(|change| change.path == "unstaged.rs"));
        assert!(state
            .unstaged
            .iter()
            .any(|change| change.path == "new file"));
    }

    #[test]
    fn unified_diff_keeps_line_numbers() {
        let diff = parse_diff(
            "note.txt",
            "diff --git a/note.txt b/note.txt\n@@ -2,2 +2,3 @@\n old\n-gone\n+new\n+last\n",
        )
        .unwrap();
        assert_eq!(
            diff.hunks[0].id,
            HunkId {
                old_start: 2,
                old_lines: 2,
                new_start: 2,
                new_lines: 3
            }
        );
        assert_eq!(diff.hunks[0].lines[1].old_lineno, Some(3));
        assert_eq!(diff.hunks[0].lines[2].new_lineno, Some(3));
    }

    #[test]
    fn commit_files_use_nul_records_and_the_new_name_after_a_rename() {
        let file = commit_files("R100\0old\tname.txt\0new name.txt\0").pop().unwrap();
        assert_eq!(file.path, "new name.txt");
        assert_eq!(file.kind, ChangeKind::Renamed);
    }

    #[test]
    fn binary_patches_are_not_read_as_empty_text_diffs() {
        let diff = parse_diff("logo.png", "diff --git a/logo.png b/logo.png\nGIT binary patch\nliteral 4\n")
            .unwrap();
        assert!(diff.binary);
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn detached_history_does_not_invent_a_head_label() {
        let id = "0123456789012345678901234567890123456789";
        let row = format!("{id}\x1f0123456\x1fAna\x1f42\x1f\x1fdetached commit");
        let commit = remote_commit(&row, &HashMap::new(), "detached HEAD").unwrap();
        assert_eq!(commit.id.to_string(), id);
        assert!(commit.refs.is_empty());
        assert_eq!(commit.summary, "detached commit");
    }
}
