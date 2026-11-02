use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Branch(String),
    Commit(String),
    Worktree,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitSummary {
    pub id: String,
    pub short_id: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branches {
    pub local: Vec<String>,
    pub current: Option<String>,
}

pub fn branches(repo: &Path) -> Result<Branches, String> {
    let raw = checked(
        repo,
        &[
            arg("for-each-ref"),
            arg("--sort=refname"),
            arg("--format=%(refname:short)"),
            arg("refs/heads/"),
        ],
    )?;
    let current = git(
        repo,
        &[
            arg("symbolic-ref"),
            arg("--quiet"),
            arg("--short"),
            arg("HEAD"),
        ],
    )?;
    Ok(Branches {
        local: String::from_utf8_lossy(&raw)
            .lines()
            .map(str::to_owned)
            .collect(),
        current: current
            .status
            .success()
            .then(|| String::from_utf8_lossy(&current.stdout).trim().to_owned()),
    })
}

/// Ref identities used to avoid reloading unchanged history during polling.
pub fn refs_snapshot(repo: &Path) -> Result<Vec<u8>, String> {
    let mut refs = checked(
        repo,
        &[
            arg("for-each-ref"),
            arg("--sort=refname"),
            arg("--format=%(refname) %(objectname)"),
            arg("refs/heads/"),
            arg("refs/remotes/"),
        ],
    )?;
    refs.extend(checked(repo, &[arg("rev-parse"), arg("--git-dir")])?);
    let head = git(repo, &[arg("rev-parse"), arg("--verify"), arg("HEAD")])?;
    refs.extend(head.stdout);
    let symbolic = git(repo, &[arg("symbolic-ref"), arg("--quiet"), arg("HEAD")])?;
    refs.extend(symbolic.stdout);
    Ok(refs)
}

pub fn commit_history(
    repo: &Path,
    source: &str,
    skip: usize,
    limit: usize,
) -> Result<Vec<CommitSummary>, String> {
    let tip = match resolve(repo, source) {
        Ok(tip) => tip,
        Err(error) => {
            if source == "HEAD" {
                let branches = branches(repo)?;
                if branches
                    .current
                    .is_some_and(|name| !branches.local.contains(&name))
                {
                    return Ok(Vec::new());
                }
            }
            return Err(error);
        }
    };
    let raw = checked(
        repo,
        &[
            arg("log"),
            arg("--date-order"),
            arg("--no-show-signature"),
            arg("--format=%H%x00%h%x00%s"),
            arg(&format!("--skip={skip}")),
            arg(&format!("--max-count={limit}")),
            arg(&tip),
            arg("--"),
        ],
    )?;
    String::from_utf8_lossy(&raw)
        .lines()
        .map(|line| {
            let parts: Vec<_> = line.splitn(3, '\0').collect();
            if parts.len() != 3 {
                return Err("Malformed commit history".into());
            }
            Ok(CommitSummary {
                id: parts[0].into(),
                short_id: parts[1].into(),
                subject: parts[2].into(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    pub repo: PathBuf,
    pub old: String,
    pub new: String,
    pub label: String,
    pub working_tree: bool,
    pub files: Vec<ChangedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub old_path: PathBuf,
    pub status: String,
    pub untracked: bool,
    pub staged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub rows: Vec<DiffLine>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
    pub kind: LineKind,
}

#[derive(Copy, Eq, PartialEq, Clone, Debug)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Note,
}

fn git(repo: &Path, args: &[OsString]) -> Result<Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_COUNT", "0")
        .output()
        .map_err(|e| format!("could not run git: {e}"))
}

fn checked(repo: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
    let out = git(repo, args)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            display_args(args),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|s| s.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn arg(s: &str) -> OsString {
    OsString::from(s)
}

fn resolve(repo: &Path, name: &str) -> Result<String, String> {
    let expr = format!("{name}^{{commit}}");
    let bytes = checked(
        repo,
        &[
            arg("rev-parse"),
            arg("--verify"),
            arg("--end-of-options"),
            arg(&expr),
        ],
    )?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

fn normalize_repo(repo: &Path) -> Result<PathBuf, String> {
    let bytes = checked(repo, &[arg("rev-parse"), arg("--show-toplevel")])?;
    let root = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    Ok(path_from_bytes(root))
}

fn short_hash(repo: &Path, hash: &str) -> Result<String, String> {
    let bytes = checked(repo, &[arg("rev-parse"), arg("--short=12"), arg(hash)])?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

fn diff_flags() -> Vec<OsString> {
    vec![
        arg("--no-ext-diff"),
        arg("--no-textconv"),
        arg("--no-color"),
        arg("--submodule=short"),
        arg("--no-relative"),
        arg("--output-indicator-new=+"),
        arg("--output-indicator-old=-"),
        arg("--output-indicator-context= "),
    ]
}

fn nul_fields(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.split(|b| *b == 0).filter(|part| !part.is_empty())
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn changed_files(
    repo: &Path,
    old: Option<&str>,
    new: Option<&str>,
    staged: bool,
) -> Result<Vec<ChangedFile>, String> {
    let mut args = vec![
        arg("diff"),
        arg("--name-status"),
        arg("-z"),
        arg("--find-renames"),
    ];
    args.extend(diff_flags());
    if staged {
        args.push(arg("--cached"));
    }
    if let (Some(old), Some(new)) = (old, new) {
        args.extend([arg(old), arg(new)]);
    }
    args.push(arg("--"));
    let raw = checked(repo, &args)?;
    let fields: Vec<&[u8]> = nul_fields(&raw).collect();
    let mut i = 0;
    let mut files = Vec::new();
    while i < fields.len() {
        let status = String::from_utf8_lossy(fields[i]).into_owned();
        i += 1;
        let code = status.as_bytes().first().copied().unwrap_or(b'?');
        if code == b'R' || code == b'C' {
            if i + 1 >= fields.len() {
                return Err("malformed NUL-delimited rename status from git".into());
            }
            let old_path = path_from_bytes(fields[i]);
            let path = path_from_bytes(fields[i + 1]);
            i += 2;
            files.push(ChangedFile {
                path,
                old_path,
                status,
                untracked: false,
                staged,
            });
        } else {
            if i >= fields.len() {
                return Err("malformed NUL-delimited status from git".into());
            }
            let path = path_from_bytes(fields[i]);
            i += 1;
            files.push(ChangedFile {
                path: path.clone(),
                old_path: path,
                status,
                untracked: false,
                staged,
            });
        }
    }
    Ok(files)
}

fn untracked_files(repo: &Path) -> Result<Vec<ChangedFile>, String> {
    let raw = checked(
        repo,
        &[
            arg("ls-files"),
            arg("--others"),
            arg("--exclude-standard"),
            arg("-z"),
        ],
    )?;
    Ok(nul_fields(&raw)
        .map(|path| {
            let path = path_from_bytes(path);
            ChangedFile {
                path: path.clone(),
                old_path: path,
                status: "A".to_owned(),
                untracked: true,
                staged: false,
            }
        })
        .collect())
}

pub fn compare(repo: &Path, target: Target) -> Result<Comparison, String> {
    let repo = normalize_repo(repo)?;
    let (old, new, label, working_tree) = match target {
        Target::Worktree => {
            let branch = checked(
                &repo,
                &[arg("symbolic-ref"), arg("--short"), arg("-q"), arg("HEAD")],
            )
            .map(|name| String::from_utf8_lossy(&name).trim().to_owned())
            .unwrap_or_else(|_| "detached HEAD".to_owned());
            (
                "INDEX".to_owned(),
                "WORKTREE".to_owned(),
                format!("Working-tree changes · {branch}"),
                true,
            )
        }
        Target::Branch(branch) => {
            let head = resolve(&repo, "HEAD")?;
            let refname = if branch.starts_with("refs/") {
                branch.clone()
            } else {
                format!("refs/heads/{branch}")
            };
            let target = resolve(&repo, &refname)?;
            (head, target, branch, false)
        }
        Target::Commit(commit) => {
            let new = resolve(&repo, &commit)?;
            let parents = checked(
                &repo,
                &[
                    arg("rev-list"),
                    arg("--parents"),
                    arg("-n"),
                    arg("1"),
                    arg(&new),
                ],
            )?;
            let parent = String::from_utf8_lossy(&parents)
                .split_whitespace()
                .nth(1)
                .map(str::to_owned);
            let old = match parent {
                Some(p) => p,
                None => String::from_utf8_lossy(&checked(
                    &repo,
                    &[arg("hash-object"), arg("-t"), arg("tree"), arg("--stdin")],
                )?)
                .trim()
                .to_owned(),
            };
            (old, new, commit, false)
        }
    };
    let files = if working_tree {
        let mut files = changed_files(&repo, None, None, false)?;
        files.extend(untracked_files(&repo)?);
        files.extend(changed_files(&repo, None, None, true)?);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    } else {
        changed_files(&repo, Some(&old), Some(&new), false)?
    };
    let label = if working_tree {
        label
    } else {
        format!(
            "{} → {} ({label})",
            short_hash(&repo, &old)?,
            short_hash(&repo, &new)?
        )
    };
    Ok(Comparison {
        repo,
        old,
        new,
        label,
        working_tree,
        files,
    })
}

fn literal_pathspec(path: &Path) -> OsString {
    let mut s = OsString::from(":(literal)");
    s.push(path.as_os_str());
    s
}

/// Change only the index entries represented by this file; never modify the worktree.
pub fn toggle_staging(repo: &Path, file: &ChangedFile) -> Result<(), String> {
    toggle_staging_files(repo, std::slice::from_ref(file))
}

/// Stage or unstage a group in one Git operation, including both paths of renames.
pub fn toggle_staging_files(repo: &Path, files: &[ChangedFile]) -> Result<(), String> {
    let Some(first) = files.first() else {
        return Ok(());
    };
    if files.iter().any(|file| file.staged != first.staged) {
        return Err("Choose files from a single staging section".into());
    }
    let mut paths = Vec::new();
    for file in files {
        paths.push(literal_pathspec(&file.path));
        if file.old_path != file.path {
            paths.push(literal_pathspec(&file.old_path));
        }
    }
    paths.sort();
    paths.dedup();
    let mut conflict_args = vec![arg("ls-files"), arg("--unmerged"), arg("-z"), arg("--")];
    conflict_args.extend(paths.clone());
    if !checked(repo, &conflict_args)?.is_empty() {
        return Err(
            "Resolve merge conflicts in the selected files before staging or unstaging them."
                .into(),
        );
    }
    let mut args = if first.staged {
        let source = match resolve(repo, "HEAD") {
            Ok(head) => head,
            Err(error) => {
                let b = branches(repo)?;
                if !b.current.is_some_and(|name| !b.local.contains(&name)) {
                    return Err(error);
                }
                String::from_utf8_lossy(&checked(
                    repo,
                    &[arg("hash-object"), arg("-t"), arg("tree"), arg("--stdin")],
                )?)
                .trim()
                .to_owned()
            }
        };
        vec![
            arg("restore"),
            arg("--staged"),
            arg(&format!("--source={source}")),
            arg("--"),
        ]
    } else {
        vec![arg("add"), arg("--all"), arg("--")]
    };
    args.extend(paths);
    checked(repo, &args).map(|_| ())
}

pub fn load_file(c: &Comparison, index: usize) -> Result<FileDiff, String> {
    let file = c
        .files
        .get(index)
        .ok_or_else(|| format!("file index {index} is out of range"))?;
    if file.untracked {
        let path = c.repo.join(&file.path);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("could not inspect {}: {e}", file.path.display()))?;
        if metadata.file_type().is_symlink() {
            return Ok(FileDiff {
                rows: Vec::new(),
                summary: Some("Untracked symbolic link (contents not followed)".into()),
            });
        }
        let content = std::fs::read(&path)
            .map_err(|e| format!("could not read {}: {e}", file.path.display()))?;
        if content.contains(&0) {
            return Ok(FileDiff {
                rows: Vec::new(),
                summary: Some("Untracked binary file contents omitted".into()),
            });
        }
        let mut rows = full_file_rows(&content);
        for row in &mut rows {
            if row.kind == LineKind::Context {
                row.kind = LineKind::Added;
                row.old = None;
            }
        }
        let summary = if rows.is_empty() {
            Some("Empty untracked file".into())
        } else {
            None
        };
        return Ok(FileDiff { rows, summary });
    }
    let mut args = vec![arg("diff")];
    args.extend(diff_flags());
    args.extend([
        arg("--find-renames"),
        arg("--full-index"),
        arg("--unified=2147483647"),
    ]);
    if file.staged {
        args.push(arg("--cached"));
    }
    if !c.working_tree {
        args.extend([arg(&c.old), arg(&c.new)]);
    }
    args.push(arg("--"));
    if file.old_path != file.path {
        args.push(literal_pathspec(&file.old_path));
    }
    args.push(literal_pathspec(&file.path));
    let bytes = checked(&c.repo, &args)?;
    let mut diff = parse_patch(&String::from_utf8_lossy(&bytes))?;
    let mode_only = diff
        .summary
        .as_deref()
        .is_some_and(|s| s.contains(" mode "));
    let is_non_text_summary = diff.summary.as_deref().is_some_and(|s| {
        s.lines().any(|line| {
            line.starts_with("Binary files ")
                || line.starts_with("GIT binary patch")
                || line.starts_with("Submodule ")
                || line.starts_with("Subproject commit ")
        })
    });
    if diff.rows.is_empty()
        && !is_non_text_summary
        && file.status != "D"
        && (file.status.starts_with('R') || mode_only)
    {
        let content = if c.working_tree && !file.staged {
            std::fs::read(c.repo.join(&file.path)).map_err(|e| e.to_string())?
        } else {
            let mut spec = OsString::from(if file.staged { "" } else { &c.new });
            spec.push(":");
            spec.push(file.path.as_os_str());
            checked(&c.repo, &[arg("show"), spec])?
        };
        if content.contains(&0) {
            diff.summary = Some("Binary file contents omitted".to_owned());
        } else {
            diff.rows = full_file_rows(&content);
        }
    }
    Ok(diff)
}

fn full_file_rows(content: &[u8]) -> Vec<DiffLine> {
    let text = String::from_utf8_lossy(content);
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let mut rows: Vec<DiffLine> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| DiffLine {
            old: Some(i + 1),
            new: Some(i + 1),
            text: (*line).to_owned(),
            kind: LineKind::Context,
        })
        .collect();
    if !content.is_empty() && !content.ends_with(b"\n") {
        rows.push(DiffLine {
            old: None,
            new: None,
            text: "\\ No newline at end of file".to_owned(),
            kind: LineKind::Note,
        });
    }
    rows
}

fn parse_patch(patch: &str) -> Result<FileDiff, String> {
    let mut rows = Vec::new();
    let mut summary = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    let mut is_submodule = false;
    for line in patch.split_terminator('\n') {
        if line.starts_with("index ") && line.ends_with(" 160000") {
            is_submodule = true;
            summary.push("Submodule change (gitlink)".to_owned());
            continue;
        }
        if line.starts_with("@@") {
            let (old, new) = parse_hunk_header(line)?;
            old_line = old;
            new_line = new;
            in_hunk = true;
            continue;
        }
        if in_hunk {
            if is_submodule {
                if line.starts_with('+') || line.starts_with('-') || line.starts_with(' ') {
                    summary.push(line.to_owned());
                } else {
                    in_hunk = false;
                    collect_summary(line, &mut summary);
                }
            } else if line == "\\ No newline at end of file" {
                rows.push(DiffLine {
                    old: None,
                    new: None,
                    text: line.to_owned(),
                    kind: LineKind::Note,
                });
            } else if let Some(text) = line.strip_prefix('+') {
                rows.push(DiffLine {
                    old: None,
                    new: Some(new_line),
                    text: text.to_owned(),
                    kind: LineKind::Added,
                });
                new_line += 1;
            } else if let Some(text) = line.strip_prefix('-') {
                rows.push(DiffLine {
                    old: Some(old_line),
                    new: None,
                    text: text.to_owned(),
                    kind: LineKind::Removed,
                });
                old_line += 1;
            } else if let Some(text) = line.strip_prefix(' ') {
                rows.push(DiffLine {
                    old: Some(old_line),
                    new: Some(new_line),
                    text: text.to_owned(),
                    kind: LineKind::Context,
                });
                old_line += 1;
                new_line += 1;
            } else if line.starts_with("\\") {
                rows.push(DiffLine {
                    old: None,
                    new: None,
                    text: line.to_owned(),
                    kind: LineKind::Note,
                });
            } else {
                in_hunk = false;
                collect_summary(line, &mut summary);
            }
        } else {
            collect_summary(line, &mut summary);
        }
    }
    Ok(FileDiff {
        rows,
        summary: if summary.is_empty() {
            None
        } else {
            Some(summary.join("\n"))
        },
    })
}

fn collect_summary(line: &str, out: &mut Vec<String>) {
    if line.starts_with("Binary files ")
        || line.starts_with("GIT binary patch")
        || line.starts_with("Subproject commit ")
        || line.starts_with("Submodule ")
        || line.starts_with("old mode ")
        || line.starts_with("new mode ")
        || line.starts_with("new file mode ")
        || line.starts_with("deleted file mode ")
    {
        out.push(line.to_owned());
    }
}

fn parse_hunk_header(line: &str) -> Result<(usize, usize), String> {
    let rest = line
        .strip_prefix("@@ -")
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let (old, rest) = rest
        .split_once(" +")
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    let (new, _) = rest
        .split_once(" @@")
        .ok_or_else(|| format!("invalid hunk header: {line}"))?;
    fn start(range: &str) -> Result<usize, String> {
        let n = range.split(',').next().unwrap_or(range);
        n.parse()
            .map_err(|_| format!("invalid hunk line number: {range}"))
    }
    Ok((start(old)?, start(new)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Repo(PathBuf);
    impl Repo {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("git-diff-test-{}-{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            let repo = Self(path);
            repo.run(&["init", "-q"]);
            repo.run(&["config", "user.email", "test@example.com"]);
            repo.run(&["config", "user.name", "Test"]);
            repo
        }
        fn run(&self, args: &[&str]) -> String {
            let output = Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }
        fn commit(&self, message: &str) -> String {
            self.run(&["add", "-A"]);
            self.run(&["commit", "-qm", message]);
            self.run(&["rev-parse", "HEAD"])
        }
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn history_lists_local_branches_and_pages_only_reachable_commits() {
        let r = Repo::new();
        fs::write(r.0.join("file"), "one").unwrap();
        let first = r.commit("first subject");
        let main = r.run(&["symbolic-ref", "--short", "HEAD"]);
        r.run(&["checkout", "-qb", "side"]);
        fs::write(r.0.join("side"), "side").unwrap();
        let side = r.commit("side subject");
        r.run(&["checkout", "-q", &main]);
        fs::write(r.0.join("file"), "two").unwrap();
        let second = r.commit("second subject");
        let b = branches(&r.0).unwrap();
        assert_eq!(b.current.as_deref(), Some(main.as_str()));
        assert!(b.local.contains(&"side".into()));
        let page = commit_history(&r.0, "HEAD", 0, 1).unwrap();
        assert_eq!(page[0].id, second);
        assert_eq!(page[0].subject, "second subject");
        assert!(second.starts_with(&page[0].short_id));
        assert_eq!(commit_history(&r.0, "HEAD", 1, 1).unwrap()[0].id, first);
        assert!(commit_history(&r.0, "HEAD", 2, 1).unwrap().is_empty());
        assert!(
            !commit_history(&r.0, "HEAD", 0, 100)
                .unwrap()
                .iter()
                .any(|c| c.id == side)
        );
        assert_eq!(
            commit_history(&r.0, "refs/heads/side", 0, 1).unwrap()[0].id,
            side
        );
        r.run(&["checkout", "-q", "--detach"]);
        assert!(branches(&r.0).unwrap().current.is_none());
        assert_eq!(commit_history(&r.0, "HEAD", 0, 1).unwrap()[0].id, second);
        r.run(&["checkout", "-q", "--orphan", "empty"]);
        assert!(commit_history(&r.0, "HEAD", 0, 100).unwrap().is_empty());
    }

    #[test]
    fn empty_repository_history_and_invalid_source() {
        let r = Repo::new();
        assert!(branches(&r.0).unwrap().local.is_empty());
        assert!(commit_history(&r.0, "HEAD", 0, 100).unwrap().is_empty());
        assert!(commit_history(&r.0, "refs/heads/missing", 0, 100).is_err());
    }

    #[test]
    fn compares_branch_and_commit_with_nul_safe_rename_paths() {
        let r = Repo::new();
        fs::write(r.0.join("before name.txt"), "one\n").unwrap();
        let root_commit = r.commit("initial");
        r.run(&["branch", "base"]);
        fs::rename(r.0.join("before name.txt"), r.0.join("after name.txt")).unwrap();
        fs::write(r.0.join("after name.txt"), "one\ntwo\n").unwrap();
        let commit = r.commit("rename and edit");
        let comparison = compare(&r.0, Target::Commit(commit.clone())).unwrap();
        assert_eq!(comparison.files.len(), 1);
        assert_eq!(comparison.files[0].path, PathBuf::from("after name.txt"));
        assert_eq!(
            comparison.files[0].old_path,
            PathBuf::from("before name.txt")
        );
        let diff = load_file(&comparison, 0).unwrap();
        assert!(
            diff.rows
                .iter()
                .any(|row| row.kind == LineKind::Added && row.text == "two")
        );
        let base = compare(&r.0, Target::Branch("base".into())).unwrap();
        assert_eq!(base.files.len(), 1);
        assert!(base.label.contains("→"));

        let root = compare(&r.0, Target::Commit(root_commit)).unwrap();
        assert_eq!(root.files.len(), 1);
        assert!(
            load_file(&root, 0)
                .unwrap()
                .rows
                .iter()
                .any(|row| row.kind == LineKind::Added && row.text == "one")
        );
    }

    #[test]
    fn normalizes_subdirectory_and_empty_current_branch_comparison() {
        let r = Repo::new();
        fs::create_dir_all(r.0.join("nested/dir")).unwrap();
        fs::write(r.0.join("nested/dir/file"), "x\n").unwrap();
        r.commit("initial");
        let branch = r.run(&["symbolic-ref", "--short", "HEAD"]);
        let comparison = compare(&r.0.join("nested/dir"), Target::Branch(branch)).unwrap();
        assert_eq!(comparison.repo, r.0);
        assert!(comparison.files.is_empty());
    }

    #[test]
    fn worktree_includes_separate_staged_unstaged_and_untracked_changes() {
        let r = Repo::new();
        fs::write(r.0.join("tracked.txt"), "committed\n").unwrap();
        fs::write(r.0.join(".gitignore"), "ignored.txt\n").unwrap();
        r.commit("initial");
        fs::write(r.0.join("tracked.txt"), "staged\n").unwrap();
        r.run(&["add", "tracked.txt"]);
        fs::write(r.0.join("tracked.txt"), "working\n").unwrap();
        fs::write(r.0.join("untracked.txt"), "new\n").unwrap();
        fs::write(r.0.join("ignored.txt"), "ignored\n").unwrap();

        let comparison = compare(&r.0, Target::Worktree).unwrap();
        assert!(comparison.working_tree);
        assert!(comparison.label.contains("Working-tree changes"));
        assert_eq!(comparison.files.len(), 3);
        assert_eq!(comparison.files[0].path, PathBuf::from("tracked.txt"));
        assert!(!comparison.files[0].staged);
        assert!(comparison.files[1].staged);
        let staged_diff = load_file(&comparison, 1).unwrap();
        assert!(
            staged_diff
                .rows
                .iter()
                .any(|l| l.kind == LineKind::Removed && l.text == "committed")
        );
        assert!(
            staged_diff
                .rows
                .iter()
                .any(|l| l.kind == LineKind::Added && l.text == "staged")
        );
        assert!(!staged_diff.rows.iter().any(|l| l.text == "working"));
        assert_eq!(comparison.files[2].path, PathBuf::from("untracked.txt"));
        assert!(comparison.files[2].untracked);
        let diff = load_file(&comparison, 0).unwrap();
        assert!(
            diff.rows
                .iter()
                .any(|line| line.kind == LineKind::Removed && line.text == "staged")
        );
        assert!(
            diff.rows
                .iter()
                .any(|line| line.kind == LineKind::Added && line.text == "working")
        );
        assert!(!diff.rows.iter().any(|line| line.text == "committed"));
        assert!(!diff.rows.iter().any(|line| line.text == "new"));
        let new_file = load_file(&comparison, 2).unwrap();
        assert_eq!(new_file.rows.len(), 1);
        assert_eq!(new_file.rows[0].kind, LineKind::Added);
        assert_eq!(new_file.rows[0].text, "new");
        assert_eq!(new_file.rows[0].new, Some(1));
        assert_eq!(r.run(&["diff", "--cached", "--name-only"]), "tracked.txt");
    }

    fn entry(r: &Repo, path: &str, staged: bool) -> ChangedFile {
        compare(&r.0, Target::Worktree)
            .unwrap()
            .files
            .into_iter()
            .find(|f| f.path == PathBuf::from(path) && f.staged == staged)
            .unwrap()
    }

    #[test]
    fn stage_and_unstage_preserve_worktree_and_unrelated_index_entries() {
        let r = Repo::new();
        fs::write(r.0.join("file"), "base\n").unwrap();
        fs::write(r.0.join("other"), "base\n").unwrap();
        r.commit("base");
        fs::write(r.0.join("file"), "partial\n").unwrap();
        fs::write(r.0.join("other"), "unrelated\n").unwrap();
        r.run(&["add", "file", "other"]);
        fs::write(r.0.join("file"), "working\n").unwrap();
        toggle_staging(&r.0, &entry(&r, "file", false)).unwrap();
        assert_eq!(r.run(&["show", ":file"]), "working");
        toggle_staging(&r.0, &entry(&r, "file", true)).unwrap();
        assert_eq!(r.run(&["show", ":file"]), "base");
        assert_eq!(fs::read_to_string(r.0.join("file")).unwrap(), "working\n");
        assert_eq!(r.run(&["show", ":other"]), "unrelated");
        fs::remove_file(r.0.join("file")).unwrap();
        toggle_staging(&r.0, &entry(&r, "file", false)).unwrap();
        assert_eq!(entry(&r, "file", true).status, "D");
        toggle_staging(&r.0, &entry(&r, "file", true)).unwrap();
        assert!(!r.0.join("file").exists());
        assert_eq!(r.run(&["show", ":file"]), "base");
    }

    #[test]
    fn stage_untracked_and_unstage_before_first_commit_with_literal_paths() {
        let r = Repo::new();
        let path = "-literal[*]: name\nfile";
        fs::write(r.0.join(path), "new\n").unwrap();
        fs::write(r.0.join("other"), "other\n").unwrap();
        toggle_staging(&r.0, &entry(&r, path, false)).unwrap();
        let c = compare(&r.0, Target::Worktree).unwrap();
        let index = c.files.iter().position(|f| f.staged).unwrap();
        assert!(
            load_file(&c, index)
                .unwrap()
                .rows
                .iter()
                .any(|l| l.text == "new" && l.kind == LineKind::Added)
        );
        toggle_staging(&r.0, &entry(&r, path, true)).unwrap();
        assert!(r.run(&["ls-files"]).is_empty());
        assert_eq!(fs::read_to_string(r.0.join(path)).unwrap(), "new\n");
        assert!(entry(&r, path, false).untracked);
    }

    #[test]
    fn unstage_rename_restores_both_index_paths_and_can_restage() {
        let r = Repo::new();
        fs::write(r.0.join("before"), "same\n").unwrap();
        r.commit("base");
        r.run(&["mv", "before", "after"]);
        let renamed = entry(&r, "after", true);
        assert!(renamed.status.starts_with('R'));
        toggle_staging(&r.0, &renamed).unwrap();
        assert_eq!(r.run(&["ls-files"]), "before");
        assert!(!r.0.join("before").exists());
        assert_eq!(fs::read_to_string(r.0.join("after")).unwrap(), "same\n");
        let mut unstaged_rename = renamed;
        unstaged_rename.staged = false;
        toggle_staging(&r.0, &unstaged_rename).unwrap();
        assert_eq!(r.run(&["ls-files"]), "after");
    }

    #[test]
    fn directory_batch_handles_nested_files_renames_and_preserves_other_entries() {
        let r = Repo::new();
        fs::create_dir_all(r.0.join("dir/nested")).unwrap();
        fs::write(r.0.join("dir/edit"), "base\n").unwrap();
        fs::write(r.0.join("dir/nested/delete"), "deleted\n").unwrap();
        fs::write(r.0.join("outside"), "base\n").unwrap();
        fs::write(r.0.join("rename-source"), "rename\n").unwrap();
        r.commit("base");
        fs::write(r.0.join("dir/edit"), "edited\n").unwrap();
        fs::remove_file(r.0.join("dir/nested/delete")).unwrap();
        fs::write(r.0.join("dir/nested/new[*]"), "new\n").unwrap();
        fs::write(r.0.join("outside"), "staged outside\n").unwrap();
        r.run(&["add", "outside"]);
        let files: Vec<_> = compare(&r.0, Target::Worktree)
            .unwrap()
            .files
            .into_iter()
            .filter(|f| !f.staged && f.path.starts_with("dir"))
            .collect();
        assert_eq!(files.len(), 3);
        toggle_staging_files(&r.0, &files).unwrap();
        assert_eq!(r.run(&["show", ":dir/edit"]), "edited");
        assert_eq!(r.run(&["show", ":dir/nested/new[*]"]), "new");
        r.run(&["mv", "rename-source", "dir/renamed"]);
        let files: Vec<_> = compare(&r.0, Target::Worktree)
            .unwrap()
            .files
            .into_iter()
            .filter(|f| f.staged && f.path.starts_with("dir"))
            .collect();
        assert_eq!(files.len(), 4);
        toggle_staging_files(&r.0, &files).unwrap();
        assert_eq!(r.run(&["diff", "--cached", "--name-only"]), "outside");
        assert_eq!(
            fs::read_to_string(r.0.join("dir/edit")).unwrap(),
            "edited\n"
        );
        assert!(!r.0.join("dir/nested/delete").exists());
        assert!(r.0.join("dir/nested/new[*]").exists());
        assert!(r.0.join("dir/renamed").exists());
        assert!(!r.0.join("rename-source").exists());
    }

    #[test]
    fn staging_failures_leave_index_untouched() {
        let r = Repo::new();
        fs::write(r.0.join("file"), "base\n").unwrap();
        r.commit("base");
        fs::write(r.0.join("file"), "changed\n").unwrap();
        let file = entry(&r, "file", false);
        fs::write(r.0.join(".git/index.lock"), "locked").unwrap();
        assert!(toggle_staging(&r.0, &file).is_err());
        assert_eq!(r.run(&["show", ":file"]), "base");
    }

    #[test]
    fn unresolved_conflicts_cannot_be_toggled() {
        let r = Repo::new();
        fs::write(r.0.join("file"), "base\n").unwrap();
        r.commit("base");
        let main = r.run(&["symbolic-ref", "--short", "HEAD"]);
        r.run(&["checkout", "-qb", "side"]);
        fs::write(r.0.join("file"), "side\n").unwrap();
        r.commit("side");
        r.run(&["checkout", "-q", &main]);
        fs::write(r.0.join("file"), "main\n").unwrap();
        r.commit("main");
        let output = git(&r.0, &[arg("merge"), arg("side")]).unwrap();
        assert!(!output.status.success());
        let before = r.run(&["ls-files", "--unmerged"]);
        let file = entry(&r, "file", true);
        assert!(
            toggle_staging(&r.0, &file)
                .unwrap_err()
                .contains("conflicts")
        );
        assert_eq!(r.run(&["ls-files", "--unmerged"]), before);
        fs::write(r.0.join("clean"), "new\n").unwrap();
        let mut conflict = file;
        conflict.staged = false;
        assert!(toggle_staging_files(&r.0, &[entry(&r, "clean", false), conflict]).is_err());
        assert!(r.run(&["ls-files", "clean"]).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn untracked_symlinks_are_shown_without_following_them() {
        use std::os::unix::fs::symlink;
        let r = Repo::new();
        fs::write(r.0.join("tracked"), "base\n").unwrap();
        r.commit("initial");
        let outside = r.0.with_extension("outside");
        fs::write(&outside, "outside content\n").unwrap();
        symlink(&outside, r.0.join("link")).unwrap();

        let comparison = compare(&r.0, Target::Worktree).unwrap();
        let link_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from("link"))
            .unwrap();
        let diff = load_file(&comparison, link_index).unwrap();
        assert!(diff.rows.is_empty());
        assert!(diff.summary.unwrap().contains("symbolic link"));
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn merge_commit_compares_against_first_parent() {
        let r = Repo::new();
        fs::write(r.0.join("base"), "base\n").unwrap();
        r.commit("base");
        let main_branch = r.run(&["symbolic-ref", "--short", "HEAD"]);
        r.run(&["branch", "side"]);
        fs::write(r.0.join("main"), "main\n").unwrap();
        let first_parent = r.commit("main change");
        r.run(&["checkout", "-q", "side"]);
        fs::write(r.0.join("side"), "side\n").unwrap();
        r.commit("side change");
        r.run(&["checkout", "-q", &main_branch]);
        r.run(&["merge", "-q", "--no-ff", "side", "-m", "merge"]);
        let merge = r.run(&["rev-parse", "HEAD"]);
        let history = commit_history(&r.0, "HEAD", 0, 100).unwrap();
        assert_eq!(history[0].id, merge);
        assert!(history.iter().any(|c| c.subject == "side change"));
        let comparison = compare(&r.0, Target::Commit(merge)).unwrap();
        assert_eq!(comparison.old, first_parent);
        assert!(
            comparison
                .files
                .iter()
                .any(|f| f.path == PathBuf::from("side"))
        );
    }

    #[test]
    fn handles_mode_change_pure_rename_binary_deletion_and_odd_paths() {
        let r = Repo::new();
        let odd = "colon:\tline\nbreak.txt";
        fs::write(r.0.join(odd), "old\n").unwrap();
        fs::write(r.0.join("binary"), [0u8, 1, 2, 255]).unwrap();
        fs::write(r.0.join("binary rename old"), [0u8, 1, 2, 255]).unwrap();
        fs::write(r.0.join("binary same old"), [0u8, 4, 5, 255]).unwrap();
        fs::write(r.0.join("rename old"), "complete file\nsecond line").unwrap();
        r.commit("initial");
        fs::rename(r.0.join("rename old"), r.0.join("rename new")).unwrap();
        fs::rename(r.0.join("binary rename old"), r.0.join("binary rename new")).unwrap();
        fs::write(r.0.join("binary rename new"), [0u8, 1, 3, 255]).unwrap();
        fs::rename(r.0.join("binary same old"), r.0.join("binary same new")).unwrap();
        fs::set_permissions(r.0.join(odd), {
            use std::os::unix::fs::PermissionsExt;
            fs::Permissions::from_mode(0o755)
        })
        .unwrap();
        fs::remove_file(r.0.join("binary")).unwrap();
        let commit = r.commit("mode rename delete");
        let comparison = compare(&r.0, Target::Commit(commit)).unwrap();
        let rename_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from("rename new"))
            .unwrap();
        let rename = load_file(&comparison, rename_index).unwrap();
        assert_eq!(rename.rows.len(), 3);
        assert_eq!(rename.rows[0].text, "complete file");
        assert_eq!(rename.rows[0].old, Some(1));
        assert_eq!(rename.rows[0].new, Some(1));
        assert_eq!(rename.rows[2].kind, LineKind::Note);
        let mode_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from(odd))
            .unwrap();
        let mode = load_file(&comparison, mode_index).unwrap();
        assert!(mode.summary.as_deref().unwrap().contains("mode"));
        assert!(!mode.rows.is_empty());
        let binary_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from("binary"))
            .unwrap();
        let binary = load_file(&comparison, binary_index).unwrap();
        assert!(binary.summary.as_deref().unwrap().contains("Binary files"));
        let binary_rename_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from("binary rename new"))
            .unwrap();
        let binary_rename = load_file(&comparison, binary_rename_index).unwrap();
        assert!(
            binary_rename
                .summary
                .as_deref()
                .unwrap()
                .contains("Binary files")
        );
        assert!(binary_rename.rows.is_empty());
        let binary_same_index = comparison
            .files
            .iter()
            .position(|f| f.path == PathBuf::from("binary same new"))
            .unwrap();
        let binary_same = load_file(&comparison, binary_same_index).unwrap();
        assert!(
            binary_same
                .summary
                .as_deref()
                .unwrap()
                .contains("Binary file contents")
        );
        assert!(binary_same.rows.is_empty());
    }

    #[test]
    fn parses_hunks_and_no_newline_marker() {
        let diff =
            parse_patch("@@ -1,2 +1,2 @@\n keep\n-old\n+new\n\\ No newline at end of file\n")
                .unwrap();
        assert_eq!(diff.rows.len(), 4);
        assert_eq!(diff.rows[0].old, Some(1));
        assert_eq!(diff.rows[0].new, Some(1));
        assert_eq!(diff.rows[3].kind, LineKind::Note);
        assert_eq!(diff.rows[3].text, "\\ No newline at end of file");
    }

    #[test]
    fn preserves_carriage_return_in_patch_content() {
        let diff = parse_patch("@@ -1 +1 @@\n-old\r\n+new\r\n").unwrap();
        assert_eq!(diff.rows[0].text, "old\r");
        assert_eq!(diff.rows[1].text, "new\r");
    }

    #[test]
    fn summarizes_submodule_patch_without_treating_it_as_file_text() {
        let diff = parse_patch(
            "diff --git a/dependency b/dependency\nindex aaaaaaa..bbbbbbb 160000\n--- a/dependency\n+++ b/dependency\n@@ -1 +1 @@\n-Subproject commit aaaaaaa\n+Subproject commit bbbbbbb\n",
        )
        .unwrap();
        assert!(diff.rows.is_empty());
        assert!(diff.summary.unwrap().contains("Submodule change"));
    }

    #[test]
    fn rejects_out_of_range_file_index() {
        let c = Comparison {
            repo: PathBuf::new(),
            old: String::new(),
            new: String::new(),
            label: String::new(),
            working_tree: false,
            files: vec![],
        };
        assert!(load_file(&c, 0).unwrap_err().contains("out of range"));
    }

    #[test]
    fn reports_invalid_repository_and_revision() {
        let path = std::env::temp_dir().join(format!("not-a-git-repo-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        assert!(compare(&path, Target::Branch("main".into())).is_err());
        let repo = Repo::new();
        assert!(compare(&repo.0, Target::Commit("no-such-revision".into())).is_err());
    }
}
