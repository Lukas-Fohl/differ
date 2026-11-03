# Feature roadmap

Suggested priorities for upcoming improvements:

1. **Auto-refresh — implemented** — Refresh working-tree and index changes while focused, and refresh on focus return, preserving file selection and scroll position. Branch/history updates and failure recovery are covered by automated tests; desktop interaction still needs manual validation.
2. **Stage selected hunks** — Stage or unstage individual change blocks instead of entire files, making it easier to split unrelated edits into separate commits.
3. **Search and filter** — Filter the sidebar by file path and search within the displayed diff.
4. **Keyboard navigation** — Move between files and change blocks, with shortcuts for staging and unstaging.
5. **Branch and commit filtering — implemented** — Filter local branches by name and loaded commits by hash or subject. Filters persist across refresh and Load more. Dropdown interaction still needs manual desktop validation. Author/date display and searching unloaded history remain future enhancements.
6. **Commit creation** — Enter a commit message and commit staged changes, with a clear summary of what is included.

Next priority: hunk staging.
