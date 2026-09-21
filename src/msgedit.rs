//! Editing the commit message box: a string with a cursor in it.
//!
//! The cursor is a byte offset into the text and always sits on a character
//! boundary. Lines are the text's own `\n` lines; how the box wraps them on
//! screen is the renderer's business, so up and down move between these.

/// Put the cursor back inside `text` and on a character boundary, after the
/// text was replaced from outside.
pub fn clamp(text: &str, cur: &mut usize) {
    *cur = (*cur).min(text.len());
    while !text.is_char_boundary(*cur) {
        *cur -= 1;
    }
}

pub fn insert(text: &mut String, cur: &mut usize, s: &str) {
    clamp(text, cur);
    text.insert_str(*cur, s);
    *cur += s.len();
}

/// Remove the character before the cursor.
pub fn backspace(text: &mut String, cur: &mut usize) {
    clamp(text, cur);
    if let Some(c) = text[..*cur].chars().next_back() {
        *cur -= c.len_utf8();
        text.remove(*cur);
    }
}

/// Remove the character under the cursor.
pub fn delete(text: &mut String, cur: &mut usize) {
    clamp(text, cur);
    if *cur < text.len() {
        text.remove(*cur);
    }
}

/// Remove the word before the cursor, and the blanks between it and the
/// cursor, the way every other text box does it.
pub fn delete_word_back(text: &mut String, cur: &mut usize) {
    let end = *cur;
    word_left(text, cur);
    text.replace_range(*cur..end, "");
}

pub fn left(text: &str, cur: &mut usize) {
    clamp(text, cur);
    if let Some(c) = text[..*cur].chars().next_back() {
        *cur -= c.len_utf8();
    }
}

pub fn right(text: &str, cur: &mut usize) {
    clamp(text, cur);
    if let Some(c) = text[*cur..].chars().next() {
        *cur += c.len_utf8();
    }
}

pub fn word_left(text: &str, cur: &mut usize) {
    clamp(text, cur);
    let before = &text[..*cur];
    let trimmed = before.trim_end();
    *cur = trimmed
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
}

pub fn word_right(text: &str, cur: &mut usize) {
    clamp(text, cur);
    let after = &text[*cur..];
    let skip_blank = after.len() - after.trim_start().len();
    let word = after[skip_blank..]
        .find(char::is_whitespace)
        .unwrap_or(after.len() - skip_blank);
    *cur += skip_blank + word;
}

/// Where the cursor's line starts.
fn line_start(text: &str, cur: usize) -> usize {
    text[..cur].rfind('\n').map_or(0, |i| i + 1)
}

/// Where the cursor's line ends, before its `\n`.
fn line_end(text: &str, cur: usize) -> usize {
    text[cur..].find('\n').map_or(text.len(), |i| cur + i)
}

pub fn home(text: &str, cur: &mut usize) {
    clamp(text, cur);
    *cur = line_start(text, *cur);
}

pub fn end(text: &str, cur: &mut usize) {
    clamp(text, cur);
    *cur = line_end(text, *cur);
}

/// The byte offset `column` characters into the line starting at `start`, or
/// the line's end when it is shorter.
fn at_column(text: &str, start: usize, column: usize) -> usize {
    let stop = line_end(text, start);
    text[start..stop]
        .char_indices()
        .nth(column)
        .map_or(stop, |(i, _)| start + i)
}

/// Up a line, keeping the column. Returns false on the first line, where
/// there is nowhere to go.
pub fn up(text: &str, cur: &mut usize) -> bool {
    clamp(text, cur);
    let start = line_start(text, *cur);
    if start == 0 {
        return false;
    }
    let column = text[start..*cur].chars().count();
    let prev = line_start(text, start - 1);
    *cur = at_column(text, prev, column);
    true
}

/// Down a line, keeping the column. Returns false on the last line.
pub fn down(text: &str, cur: &mut usize) -> bool {
    clamp(text, cur);
    let stop = line_end(text, *cur);
    if stop == text.len() {
        return false;
    }
    let column = text[line_start(text, *cur)..*cur].chars().count();
    *cur = at_column(text, stop + 1, column);
    true
}

/// The row and column the cursor sits at once every line is broken at
/// `width` characters, as the box shows it.
pub fn screen_pos(text: &str, cur: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut cur = cur;
    clamp(text, &mut cur);
    let start = line_start(text, cur);
    let rows_above: usize = text[..start]
        .split_terminator('\n')
        .map(|l| l.chars().count().div_ceil(width).max(1))
        .sum();
    let column = text[start..cur].chars().count();
    (rows_above + column / width, column % width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_goes_in_at_the_cursor() {
        let mut t = String::from("fix bug");
        let mut c = 3;
        insert(&mut t, &mut c, " the");
        assert_eq!((t.as_str(), c), ("fix the bug", 7));
        backspace(&mut t, &mut c);
        assert_eq!((t.as_str(), c), ("fix th bug", 6));
        delete(&mut t, &mut c);
        assert_eq!((t.as_str(), c), ("fix thbug", 6));
    }

    #[test]
    fn arrows_step_over_whole_characters() {
        let t = "zażółć";
        let mut c = t.len();
        left(t, &mut c);
        assert_eq!(&t[c..], "ć");
        right(t, &mut c);
        assert_eq!(c, t.len());
    }

    #[test]
    fn words_are_skipped_and_deleted_whole() {
        let mut t = String::from("add the  thing");
        let mut c = t.len();
        word_left(&t, &mut c);
        assert_eq!(&t[c..], "thing");
        word_left(&t, &mut c);
        assert_eq!(&t[c..], "the  thing");
        word_right(&t, &mut c);
        assert_eq!(&t[c..], "  thing");
        let mut c = t.len();
        delete_word_back(&mut t, &mut c);
        assert_eq!(t, "add the  ");
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let t = "subject line\n\nbody text";
        let mut c = 4;
        assert!(!up(t, &mut c));
        assert!(down(t, &mut c));
        assert_eq!(c, 13);
        // The blank line in between has no column 4 to keep.
        assert!(down(t, &mut c));
        assert_eq!(&t[c..], "body text");
        assert!(!down(t, &mut c));
        let mut c = t.len() - 5;
        assert!(up(t, &mut c));
        assert_eq!(c, 13);
        assert!(up(t, &mut c));
        assert_eq!(c, 0);
        let mut c = 4;
        assert!(down(t, &mut c));
        assert!(up(t, &mut c));
        assert_eq!(c, 0);
    }

    #[test]
    fn home_and_end_stay_on_the_line() {
        let t = "one\ntwo";
        let mut c = 5;
        home(t, &mut c);
        assert_eq!(c, 4);
        end(t, &mut c);
        assert_eq!(c, 7);
    }

    #[test]
    fn the_cursor_is_found_in_wrapped_rows() {
        assert_eq!(screen_pos("abcdef", 6, 4), (1, 2));
        assert_eq!(screen_pos("abcd", 4, 4), (1, 0));
        assert_eq!(screen_pos("ab\n\ncd", 5, 4), (2, 1));
        assert_eq!(screen_pos("abcdefgh\nx", 10, 4), (2, 1));
    }
}
