//! From a selection in the text overlay to the address the app understands: the text layer's
//! lines carry a byte offset per character, so a selection becomes a story (or cell, or
//! footnote), a byte range and the quoted text. No DOM in here, so it is tested natively.

use crate::api::{Cell, Line};

/// How much text around a quote is kept, so the app can find it again after edits.
const CONTEXT: usize = 32;

/// The character index at a UTF-16 offset into `text` (what a DOM `Range` gives), clamped.
pub fn char_at_utf16(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.chars().enumerate() {
        if units >= utf16 {
            return i;
        }
        units += c.len_utf16();
    }
    text.chars().count()
}

/// The byte offset where character `i` of the line starts; past the last character, the line's
/// end.
pub fn byte_at(line: &Line, i: usize) -> u32 {
    line.bytes.get(i).copied().unwrap_or(line.end)
}

/// The selection's address and quote.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchor {
    pub story: u64,
    pub cell: Option<Cell>,
    pub footnote: Option<u64>,
    pub start: u32,
    pub end: u32,
    pub exact: String,
    pub prefix: String,
    pub suffix: String,
}

/// The anchor of a selection running from character `from` of `lines[0]` to character `to` of
/// the last line (exclusive). The lines are the selected lines in order; they must share one
/// story, cell or footnote, and the range must not be empty.
pub fn anchor(lines: &[Line], from: usize, to: usize) -> Option<Anchor> {
    let (first, last) = (lines.first()?, lines.last()?);
    if lines.iter().any(|l| l.story != first.story || l.cell != first.cell || l.footnote != first.footnote) {
        return None;
    }
    let (start, end) = (byte_at(first, from), byte_at(last, to));
    if start >= end {
        return None;
    }
    let mut exact = String::new();
    for (i, l) in lines.iter().enumerate() {
        let chars: Vec<char> = l.text.chars().collect();
        let a = if i == 0 { from.min(chars.len()) } else { 0 };
        let b = if i + 1 == lines.len() { to.min(chars.len()) } else { chars.len() };
        if i > 0 {
            exact.push(' ');
        }
        exact.extend(&chars[a.min(b)..b]);
    }
    let prefix: String =
        first.text.chars().take(from).collect::<Vec<_>>().into_iter().rev().take(CONTEXT).collect::<Vec<_>>().into_iter().rev().collect();
    let suffix: String = last.text.chars().skip(to).take(CONTEXT).collect();
    Some(Anchor { story: first.story, cell: first.cell.clone(), footnote: first.footnote, start, end, exact, prefix, suffix })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn line(story: u64, start: u32, text: &str) -> Line {
        let mut bytes = Vec::new();
        let mut b = start;
        for c in text.chars() {
            bytes.push(b);
            b += c.len_utf8() as u32;
        }
        Line { story, cell: None, footnote: None, frame: 1, start, end: b, text: text.into(), bytes, rect: [0.0, 0.0, 10.0, 1.0], rtl: false }
    }

    #[test]
    fn utf16_offsets_become_character_indexes() {
        let t = "a😀b";
        assert_eq!(char_at_utf16(t, 0), 0);
        assert_eq!(char_at_utf16(t, 1), 1);
        assert_eq!(char_at_utf16(t, 3), 2, "the emoji is two units");
        assert_eq!(char_at_utf16(t, 4), 3);
        assert_eq!(char_at_utf16(t, 99), 3, "clamped");
    }

    #[test]
    fn bytes_follow_the_line_and_the_end_is_past_the_last_character() {
        let l = line(3, 10, "héllo");
        assert_eq!((byte_at(&l, 0), byte_at(&l, 1), byte_at(&l, 2)), (10, 11, 13));
        assert_eq!(byte_at(&l, 5), l.end);
        assert_eq!(byte_at(&l, 50), l.end);
    }

    #[test]
    fn a_selection_across_two_lines_quotes_both_with_context() {
        let a = line(3, 0, "Every page begins as an empty");
        let b = line(3, 30, "field of possibility.");
        let an = anchor(&[a.clone(), b.clone()], 6, 5).unwrap();
        assert_eq!((an.story, an.start, an.end), (3, 6, 35));
        assert_eq!(an.exact, "page begins as an empty field");
        assert_eq!((an.prefix.as_str(), an.suffix.as_str()), ("Every ", " of possibility."));
        let one = anchor(&[a], 6, 10).unwrap();
        assert_eq!((one.exact.as_str(), one.start, one.end), ("page", 6, 10));
    }

    #[test]
    fn empty_or_mixed_selections_have_no_anchor() {
        let a = line(3, 0, "one");
        let other = line(4, 0, "two");
        assert_eq!(anchor(std::slice::from_ref(&a), 2, 2), None);
        assert_eq!(anchor(&[a, other], 0, 3), None);
        assert_eq!(anchor(&[], 0, 1), None);
    }
}
