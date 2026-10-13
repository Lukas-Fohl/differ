//! Presentation helpers for rendering unified diffs.

use crate::git::{DiffLine, LineKind};

/// A row in the presentation model. Indices refer to the original diff lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayRow {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub gap: Option<usize>,
}

fn line_row(index: usize, split: bool, kind: LineKind) -> DisplayRow {
    if !split {
        return DisplayRow {
            left: Some(index),
            right: None,
            gap: None,
        };
    }
    match kind {
        LineKind::Context => DisplayRow {
            left: Some(index),
            right: Some(index),
            gap: None,
        },
        LineKind::Added => DisplayRow {
            left: None,
            right: Some(index),
            gap: None,
        },
        LineKind::Removed | LineKind::Note => DisplayRow {
            left: Some(index),
            right: None,
            gap: None,
        },
    }
}

fn note_row(index: usize, split: bool, preceding: Option<LineKind>) -> DisplayRow {
    if !split {
        return DisplayRow {
            left: Some(index),
            right: None,
            gap: None,
        };
    }
    match preceding {
        Some(LineKind::Added) => DisplayRow {
            left: None,
            right: Some(index),
            gap: None,
        },
        Some(LineKind::Context) => DisplayRow {
            left: Some(index),
            right: Some(index),
            gap: None,
        },
        _ => DisplayRow {
            left: Some(index),
            right: None,
            gap: None,
        },
    }
}

/// Build inline or side-by-side rows, optionally collapsing distant context.
pub fn display_rows(lines: &[DiffLine], split: bool, full: bool) -> Vec<DisplayRow> {
    let mut expanded = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if split && lines[i].kind == LineKind::Removed {
            // Notes belong to the immediately preceding changed line. Keep
            // them attached to their side while pairing deletion/addition runs.
            let mut removed = Vec::new();
            while i < lines.len() && lines[i].kind == LineKind::Removed {
                let line = i;
                i += 1;
                let note = if i < lines.len() && lines[i].kind == LineKind::Note {
                    let note = i;
                    i += 1;
                    Some(note)
                } else {
                    None
                };
                removed.push((line, note));
            }
            let mut added = Vec::new();
            while i < lines.len() && lines[i].kind == LineKind::Added {
                let line = i;
                i += 1;
                let note = if i < lines.len() && lines[i].kind == LineKind::Note {
                    let note = i;
                    i += 1;
                    Some(note)
                } else {
                    None
                };
                added.push((line, note));
            }
            for n in 0..removed.len().max(added.len()) {
                let old = removed.get(n).copied();
                let new = added.get(n).copied();
                expanded.push(DisplayRow {
                    left: old.map(|(line, _)| line),
                    right: new.map(|(line, _)| line),
                    gap: None,
                });
                let old_note = old.and_then(|(_, note)| note);
                let new_note = new.and_then(|(_, note)| note);
                if old_note.is_some() || new_note.is_some() {
                    expanded.push(DisplayRow {
                        left: old_note,
                        right: new_note,
                        gap: None,
                    });
                }
            }
            continue;
        }
        if lines[i].kind == LineKind::Note {
            let preceding = i.checked_sub(1).map(|prev| lines[prev].kind);
            expanded.push(note_row(i, split, preceding));
        } else {
            expanded.push(line_row(i, split, lines[i].kind));
        }
        i += 1;
    }

    if full {
        return expanded;
    }

    // Keep three context lines on either side of every non-context line.
    let mut keep = vec![false; lines.len()];
    for (index, line) in lines.iter().enumerate() {
        if line.kind != LineKind::Context {
            keep[index] = true;
            let mut before = index;
            for _ in 0..3 {
                if before == 0 || lines[before - 1].kind != LineKind::Context {
                    break;
                }
                before -= 1;
                keep[before] = true;
            }
            let mut after = index + 1;
            for _ in 0..3 {
                if after >= lines.len() || lines[after].kind != LineKind::Context {
                    break;
                }
                keep[after] = true;
                after += 1;
            }
        }
    }
    let mut rows = Vec::new();
    let mut k = 0;
    while k < expanded.len() {
        let row = expanded[k];
        let idx = row.left.or(row.right);
        if idx.is_some_and(|n| keep[n]) {
            rows.push(row);
            k += 1;
        } else {
            let start = k;
            while k < expanded.len() {
                let idx = expanded[k].left.or(expanded[k].right);
                if idx.is_some_and(|n| keep[n]) {
                    break;
                }
                k += 1;
            }
            let count = k - start;
            if count > 0 {
                rows.push(DisplayRow {
                    left: None,
                    right: None,
                    gap: Some(count),
                });
            }
        }
    }
    rows
}

/// Reconstruct one side of a diff, including its final newline when present.
pub fn file_text(lines: &[DiffLine], new: bool) -> String {
    let mut out = String::new();
    let mut missing_newline = false;
    let mut previous_was_present = false;
    for line in lines {
        if line.kind == LineKind::Note {
            // The marker applies to the immediately preceding side's line.
            if previous_was_present {
                missing_newline = true;
            }
            previous_was_present = false;
            continue;
        }
        let present = if new {
            line.kind != LineKind::Removed
        } else {
            line.kind != LineKind::Added
        };
        if present {
            out.push_str(&line.text);
            out.push('\n');
            missing_newline = false;
        }
        previous_was_present = present;
    }
    if missing_newline {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: LineKind, text: &str) -> DiffLine {
        DiffLine {
            old: None,
            new: None,
            text: text.into(),
            kind,
        }
    }

    #[test]
    fn split_pairs_change_runs_and_keeps_context_aligned() {
        let lines = [
            line(LineKind::Context, "same"),
            line(LineKind::Removed, "old1"),
            line(LineKind::Removed, "old2"),
            line(LineKind::Added, "new"),
            line(LineKind::Context, "tail"),
        ];
        assert_eq!(
            display_rows(&lines, true, true),
            vec![
                DisplayRow {
                    left: Some(0),
                    right: Some(0),
                    gap: None
                },
                DisplayRow {
                    left: Some(1),
                    right: Some(3),
                    gap: None
                },
                DisplayRow {
                    left: Some(2),
                    right: None,
                    gap: None
                },
                DisplayRow {
                    left: Some(4),
                    right: Some(4),
                    gap: None
                },
            ]
        );
    }

    #[test]
    fn changes_mode_collapses_long_context_with_count() {
        let lines = [
            line(LineKind::Context, "a"),
            line(LineKind::Context, "b"),
            line(LineKind::Context, "c"),
            line(LineKind::Context, "d"),
            line(LineKind::Context, "e"),
            line(LineKind::Removed, "gone"),
            line(LineKind::Context, "f"),
            line(LineKind::Context, "g"),
            line(LineKind::Context, "h"),
            line(LineKind::Context, "i"),
        ];
        let rows = display_rows(&lines, false, false);
        assert_eq!(rows.first().unwrap().gap, Some(2));
        assert_eq!(rows.last().unwrap().gap, Some(1));
        assert!(rows.iter().any(|r| r.left == Some(2)));
        assert!(rows.iter().any(|r| r.left == Some(5)));
    }

    #[test]
    fn file_text_reconstructs_each_side_and_missing_final_newline() {
        let lines = [
            line(LineKind::Context, "first"),
            line(LineKind::Removed, "old"),
            line(LineKind::Added, "new"),
            line(LineKind::Note, "\\ No newline at end of file"),
        ];
        assert_eq!(file_text(&lines, false), "first\nold\n");
        assert_eq!(file_text(&lines, true), "first\nnew");
    }

    #[test]
    fn split_keeps_missing_newline_notes_on_their_respective_sides() {
        let lines = [
            line(LineKind::Removed, "old"),
            line(LineKind::Note, "no newline"),
            line(LineKind::Added, "new"),
            line(LineKind::Note, "no newline"),
        ];
        assert_eq!(
            display_rows(&lines, true, true),
            vec![
                DisplayRow {
                    left: Some(0),
                    right: Some(2),
                    gap: None
                },
                DisplayRow {
                    left: Some(1),
                    right: Some(3),
                    gap: None
                },
            ]
        );
        assert_eq!(file_text(&lines, false), "old");
        assert_eq!(file_text(&lines, true), "new");
    }

    #[test]
    fn standalone_split_notes_follow_the_preceding_line_side() {
        let added = [
            line(LineKind::Added, "new"),
            line(LineKind::Note, "no newline"),
        ];
        assert_eq!(
            display_rows(&added, true, true)[1],
            DisplayRow {
                left: None,
                right: Some(1),
                gap: None
            }
        );

        let context = [
            line(LineKind::Context, "same"),
            line(LineKind::Note, "no newline"),
        ];
        assert_eq!(
            display_rows(&context, true, true)[1],
            DisplayRow {
                left: Some(1),
                right: Some(1),
                gap: None
            }
        );
    }

    #[test]
    fn changes_mode_keeps_separate_large_unchanged_gaps() {
        let mut lines = vec![line(LineKind::Context, "before"); 20];
        lines[5] = line(LineKind::Added, "one");
        lines[15] = line(LineKind::Removed, "two");
        let rows = display_rows(&lines, false, false);
        assert_eq!(
            rows.iter().filter_map(|r| r.gap).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
        assert!(rows.iter().any(|r| r.left == Some(5)));
        assert!(rows.iter().any(|r| r.left == Some(15)));
    }
}
