//! Explains an empty answer to a position query: says what is at the position,
//! so a column that landed on whitespace is not mistaken for "no definition".

pub(crate) fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The identifiers on a line, as (0-based character index, text).
pub fn identifiers(line: &str) -> Vec<(usize, String)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if is_ident(chars[i]) {
            let start = i;
            while i < chars.len() && is_ident(chars[i]) {
                i += 1;
            }
            out.push((start, chars[start..i].iter().collect()));
        } else {
            i += 1;
        }
    }
    out
}

/// What is at 1-based `line`:`col` of `text`, for a query that found nothing.
/// Columns count characters.
pub fn describe(text: &str, line: u32, col: u32) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(src) = lines.get((line as usize).wrapping_sub(1)) else {
        return format!("line {line} is past the end of the file ({} lines)", lines.len());
    };
    let chars: Vec<char> = src.chars().collect();
    let idx = (col as usize).wrapping_sub(1);
    if col == 0 || idx >= chars.len() {
        return format!("column {col} is past the end of line {line} ({} characters)", chars.len());
    }
    if is_ident(chars[idx]) {
        let mut start = idx;
        while start > 0 && is_ident(chars[start - 1]) {
            start -= 1;
        }
        let mut end = idx;
        while end + 1 < chars.len() && is_ident(chars[end + 1]) {
            end += 1;
        }
        let word: String = chars[start..=end].iter().collect();
        return format!("{line}:{col} is inside `{word}` (starts at column {}); the server has no answer for it", start + 1);
    }
    let what = if chars[idx].is_whitespace() { "whitespace".to_owned() } else { format!("`{}`", chars[idx]) };
    // Nearest identifier on the same line, by distance from the cursor.
    let best = identifiers(src)
        .into_iter()
        .map(|(start, word)| {
            let end = start + word.chars().count() - 1;
            let dist = if idx < start { start - idx } else { idx - end };
            (dist, start, word)
        })
        .min_by_key(|(dist, _, _)| *dist);
    match best {
        Some((_, start, word)) => {
            format!("no symbol at {line}:{col} ({what}); nearest identifier: `{word}` at {line}:{}", start + 1)
        }
        None => format!("no symbol at {line}:{col} ({what}); there is no identifier on that line"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn main() {\n    let quality = 1;\n}\n";

    #[test]
    fn inside_a_name() {
        let d = describe(SRC, 2, 11);
        assert!(d.contains("inside `quality`") && d.contains("column 9"), "{d}");
    }

    #[test]
    fn whitespace_names_the_nearest_identifier() {
        let d = describe(SRC, 2, 2);
        assert!(d.contains("whitespace") && d.contains("`let` at 2:5"), "{d}");
    }

    #[test]
    fn punctuation_is_named() {
        let d = describe(SRC, 1, 8);
        assert!(d.contains("`(`") && d.contains("`main`"), "{d}");
    }

    #[test]
    fn out_of_range() {
        assert!(describe(SRC, 9, 1).contains("past the end of the file (3 lines)"));
        assert!(describe(SRC, 1, 99).contains("past the end of line 1"));
        assert!(describe(SRC, 1, 0).contains("past the end of line 1"));
    }
}
