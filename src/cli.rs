use crate::git::Target;
use std::{ffi::OsString, path::PathBuf};

pub const HELP: &str = "differ — read-only desktop Git diff viewer

USAGE:
    differ [OPTIONS]

OPTIONS:
    --repo PATH       Git repository to open (default: current directory)
    --branch REF      Compare checked-out HEAD with a local branch tip
    --commit REF      Compare a commit with its first parent
    -h, --help        Print this help

DEFAULT VIEW:
    With no --branch or --commit, show unstaged tracked changes and untracked,
    non-ignored files in the current checkout. Staged changes are excluded.

REVISION DETAILS:
    --branch accepts a local branch name or a full ref such as
    refs/remotes/origin/feature. Comparing the checked-out branch with itself
    shows no changes.

    --commit accepts a SHA or Git revision such as HEAD~2. Root commits are
    compared with an empty tree; merge commits are compared with their first
    parent. --branch and --commit cannot be used together.

EXAMPLES:
    differ
    differ --repo /path/to/repo
    differ --branch feature/my-change
    differ --repo /path/to/repo --branch refs/remotes/origin/feature
    differ --commit HEAD
    differ --commit abc123

The viewer does not check out branches or modify repository content.";

pub struct Options {
    pub repo: PathBuf,
    pub target: Target,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut args = args.into_iter();
    let mut repo = PathBuf::from(".");
    let mut target = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(None),
            Some("--repo") => repo = args.next().ok_or("--repo requires a path")?.into(),
            Some(flag @ ("--branch" | "--commit")) => {
                if target.is_some() {
                    return Err("Choose exactly one of --branch and --commit".into());
                }
                let value = args
                    .next()
                    .ok_or_else(|| format!("{flag} requires a revision"))?
                    .into_string()
                    .map_err(|_| "Revision must be valid UTF-8")?;
                if value.is_empty() || value.starts_with('-') {
                    return Err("Revision must be nonempty and must not begin with '-'".into());
                }
                target = Some(if flag == "--branch" {
                    Target::Branch(value)
                } else {
                    Target::Commit(value)
                });
            }
            _ => return Err(format!("Unknown argument: {}", arg.to_string_lossy())),
        }
    }
    Ok(Some(Options {
        repo,
        target: target.unwrap_or(Target::Worktree),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(a: &[&str]) -> Result<Option<Options>, String> {
        parse(a.iter().map(OsString::from))
    }
    #[test]
    fn cli_contract() {
        assert!(matches!(
            args(&[]).unwrap().unwrap().target,
            Target::Worktree
        ));
        assert!(args(&["--help"]).unwrap().is_none());
        assert!(args(&["--branch", "main", "--commit", "HEAD"]).is_err());
        assert!(args(&["--commit"]).is_err());
        assert!(args(&["--branch", "--help"]).is_err());
        assert!(args(&["--unknown"]).is_err());
        let o = args(&["--repo", "/tmp/a b", "--commit", "HEAD~2"])
            .unwrap()
            .unwrap();
        assert_eq!(o.repo, PathBuf::from("/tmp/a b"));
        assert!(matches!(o.target, Target::Commit(s) if s == "HEAD~2"));
    }
}
