# Differ

## Install
pls don't

```bash
cargo install --path .
```

## Run

Requires Rust (edition 2024; Rust 1.88 or newer), Git, and a working desktop graphics driver. On Linux, install your distribution's development packages for X11, Wayland, xkbcommon and clipboard support. On Debian/Ubuntu these include `pkg-config libx11-dev libxi-dev libxrandr-dev libxcursor-dev libxkbcommon-dev libwayland-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev`.

```sh
cargo run -- --repo /path/to/repo
cargo run -- --repo /path/to/repo --branch feature/my-change
cargo run -- --repo /path/to/repo --commit HEAD
cargo run -- --commit abc123
cargo run -- --help
```

The repository defaults to the current directory. Subdirectories within a repository are accepted. With neither `--branch` nor `--commit`, the app shows staged and unstaged changes and untracked, non-ignored files in the current checkout. Ignored files are excluded. `--branch` and `--commit` are mutually exclusive. Local branches use their short name; remote branches can use a full ref such as `refs/remotes/origin/feature`.

- **Branch:** checked-out `HEAD` → selected branch tip. Choosing the checked-out branch shows no changes.
- **Commit:** first parent → selected commit. A root commit compares with an empty tree; a merge commit compares with its first parent.

Revisions and working-tree changes are loaded at startup. While the app is focused, changes refresh automatically about once a second and immediately when focus returns. The selected file and scroll position are preserved. Use **Reload** for an immediate refresh, or enter another repository path in the **Project** field and choose **Open** to switch projects. Double-clicking a file stages or unstages it. The app never checks out branches, commits, or discards working-tree content.

## Interface

Select a changed file in the collapsible tree. Working-tree files are grouped into **Staged** and **Unstaged** sections. Single-click previews that section’s diff; double-click stages the entire unstaged file or unstages a staged file while preserving its working-tree contents. Double-click a directory row to stage or unstage all changed files beneath it, including nested directories. Single-click still expands or collapses the directory. Partially staged files appear in both sections. Conflicted files must be resolved before using these actions. Drag the sidebar divider to resize it. Edit the project path or browse with the native folder dialog (starting at the entered path) to open a different repository without restarting. Relative paths in the project field are resolved from `$HOME`.

- **Branch / Commit:** select a local branch to view its latest commit, or choose an older commit by hash and subject. Commits compare with their first parent; root commits compare with an empty tree. History includes merged commits, with **Load more…** after each 100 entries. Selection never checks out a branch or fetches.
- **Branch / Commit filters:** open either dropdown and type to filter by branch name or commit hash/subject. Matching is case-insensitive and literal, including Unicode lowercase characters; regex is not supported. Use **×** to clear. Commit filtering covers loaded history only: **Load more…** remains available even when nothing matches, and the working-tree entry is always shown. Typing does not change the selection. Filters persist across closing the dropdown, selecting revisions, refreshing, and loading more; opening a repository clears both.
- **Working-tree changes (current checkout):** return to staged and unstaged changes regardless of the branch being browsed. Opening a new project starts here; selecting a folder opens it immediately.
- **Auto-refresh / Reload:** refresh working-tree changes, branches, history, and branch comparisons while preserving the current view. Selected historical commits stay fixed. History follows external checkouts unless you explicitly choose a branch. Refresh failures keep the last view visible and temporarily disable staging until recovery. Detached HEAD and repositories without commits are supported. CLI startup targets remain supported, including explicitly supplied remote refs.
- **Inline / Side by side:** one combined diff or aligned old/new columns with shared scrolling.
- **Changes / Full file:** three context lines around each change, or every line including removed content.
- Drag to select visible code text and use the system copy shortcut. **Copy old file / Copy new file** copies the complete side, without line numbers or diff markers.
- Horizontal scrolling preserves long lines. Tabs display as four spaces; copied file text retains tabs.

Added/removed lines use both colors and `+`/`−` markers. Binary files and submodules show metadata. Untracked symlinks are listed without following their targets. Non-UTF-8 text is displayed with replacement characters. Files load in a background worker and are cached during the session; only visible rows are rendered. Extremely large files still require memory for their full diff and presentation indices.

## Checks

```sh
cargo fmt --check
cargo check
cargo test
```

The CLI, Git backend, and diff-layout tests use only the Rust standard library and can also run without downloading desktop dependencies:

```sh
rustc --edition=2024 --test src/lib.rs -o /tmp/differ-tests
/tmp/differ-tests
```

Manual desktop checks: edit files and stage/unstage externally while the app is focused; switch away and return; check selection and scroll preservation after file ordering changes. Open the default working-tree view, a changed branch, and a commit; select nested files; try all four layout/context combinations; resize the sidebar/window; scroll long files and long lines; select/copy text; check empty, invalid-revision, binary, rename, deleted-file, untracked-file, and symlink states.

## Current validation limits

Compilation and automated tests run locally. Desktop interaction, including focus events and visual scroll preservation, still requires the manual checks above.

Branch/commit filter checks cover literal and Unicode matching and state preservation across refresh and history loading. Remaining manual desktop checks: dropdown focus, clear buttons, selection with active filters, empty results, and Load more.
