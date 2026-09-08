//! Terminal-cell layout primitives shared by input, transcript and overlays.
//!
//! Text is split on grapheme clusters and measured in terminal cells.  Keeping
//! this logic in one module prevents the input cursor, markdown wrapping and
//! diff rows from slowly acquiring different ideas about what a character is.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Width of one Unicode scalar value in terminal cells.
pub(crate) fn char_cells(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// Width of a grapheme cluster in terminal cells.
///
/// `UnicodeWidthStr` handles combining marks and common emoji sequences.  A
/// cluster made entirely of zero-width code points stays zero-width; this is
/// important for cursor placement after combining marks.
pub(crate) fn grapheme_cells(grapheme: &str) -> usize {
    UnicodeWidthStr::width(grapheme)
}

pub(crate) fn str_cells(text: &str) -> usize {
    text.graphemes(true).map(grapheme_cells).sum()
}

/// Wrap text without splitting a grapheme or placing half of a wide glyph on
/// the next row. Explicit newlines always start a new logical row.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for source in text.split('\n') {
        let mut row = String::new();
        let mut cells = 0usize;
        for grapheme in source.graphemes(true) {
            let used = grapheme_cells(grapheme);
            if cells > 0 && cells.saturating_add(used) > width {
                rows.push(std::mem::take(&mut row));
                cells = 0;
            }
            row.push_str(grapheme);
            cells = cells.saturating_add(used);
        }
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

pub(crate) fn clip(text: &str, width: usize, ellipsis: &str) -> String {
    if width == 0 {
        return String::new();
    }
    if str_cells(text) <= width {
        return text.to_owned();
    }
    let marker_width = str_cells(ellipsis);
    if marker_width >= width {
        return ellipsis
            .graphemes(true)
            .scan(0usize, |used, g| {
                let next = used.saturating_add(grapheme_cells(g));
                (next <= width).then(|| {
                    *used = next;
                    g
                })
            })
            .collect();
    }
    let mut result = String::new();
    let mut used = 0usize;
    for grapheme in text.graphemes(true) {
        let width_with_marker = used
            .saturating_add(grapheme_cells(grapheme))
            .saturating_add(marker_width);
        if width_with_marker > width {
            break;
        }
        result.push_str(grapheme);
        used = used.saturating_add(grapheme_cells(grapheme));
    }
    result.push_str(ellipsis);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_and_emoji_use_terminal_cells() {
        assert_eq!(str_cells("你好"), 4);
        assert_eq!(str_cells("a你b"), 4);
        assert_eq!(str_cells("e\u{301}"), 1);
        assert!(str_cells("🚀") >= 1);
    }

    #[test]
    fn wrapping_never_splits_wide_graphemes() {
        assert_eq!(wrap("你好世界", 3), vec!["你", "好", "世", "界"]);
        assert_eq!(wrap("ab你好", 4), vec!["ab你", "好"]);
        assert_eq!(wrap("a\nb", 8), vec!["a", "b"]);
    }

    #[test]
    fn clipping_reserves_marker_cells() {
        assert_eq!(clip("你好世界", 5, "…"), "你好…");
        assert_eq!(clip("abcdef", 3, "…"), "ab…");
    }
}
