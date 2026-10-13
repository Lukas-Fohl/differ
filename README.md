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

The repository defaults to the current directory. Subdirectories within a repository are accepted. With neither `--branch` nor `--commit`, the app shows unstaged tracked-file changes and untracked, non-ignored files in the current checkout. Staged changes and ignored files are excluded. `--branch` and `--commit` are mutually exclusive. Local branches use their short name; remote branches can use a full ref such as `refs/remotes/origin/feature`.

- **Branch:** checked-out `HEAD` → selected branch tip. Choosing the checked-out branch shows no changes.
- **Commit:** first parent → selected commit. A root commit compares with an empty tree; a merge commit compares with its first parent.

Revisions and unstaged changes are captured at startup. The app never checks out branches, stages files, or writes repository content. Restart to load updated revisions.

## Interface

Select a changed file in the collapsible tree. Drag the sidebar divider to resize it.

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

Manual desktop checks: open the default unstaged view, a changed branch, and a commit; select nested files; try all four layout/context combinations; resize the sidebar/window; scroll long files and long lines; select/copy text; check empty, invalid-revision, binary, rename, deleted-file, untracked-file, and symlink states.

## Current validation limits

The implementation environment cannot resolve crates.io and has no cached Bevy/egui packages. Desktop compilation, runtime checks, and Cargo.lock generation therefore remain pending. Dependency versions are pinned in Cargo.toml; run `cargo check` with registry access to generate Cargo.lock, then retain it for reproducible builds.
