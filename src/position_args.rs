//! Friendlier ways to give a position to a query, turned into the plain
//! `<file> <line> <col>` arguments before clap parses the command line:
//!
//! - `file:line:col` (how compilers and grep print locations), and `file:line`;
//! - `--symbol <name>` in place of the column: the first occurrence of `name`
//!   on that line.

use crate::position::identifiers;
use std::collections::HashSet;

/// Query subcommands that take `<file> <line> <col>`.
const POSITION_COMMANDS: &[&str] = &[
    "hover", "definition", "implementation", "type-definition", "incoming-calls",
    "outgoing-calls", "references", "completion", "signature-help", "code-action",
    "rename", "context",
];

/// Arguments rewritten for clap, and a note to print (which occurrence of a
/// `--symbol` was used, when the line has more than one).
#[derive(Debug, PartialEq)]
pub struct Expanded {
    pub args: Vec<String>,
    pub note: Option<String>,
}

/// Split `path:line:col` or `path:line`, unless a file by that very name exists.
fn split_location(token: &str, exists: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    if exists(token) {
        return None;
    }
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = token.rsplitn(3, ':');
    let last = parts.next()?;
    let mid = parts.next()?;
    match parts.next() {
        Some(path) if !path.is_empty() && numeric(mid) && numeric(last) => {
            Some(vec![path.to_owned(), mid.to_owned(), last.to_owned()])
        }
        _ => {
            // `path:line`; `mid` is then the rest of the path, with its colons.
            let path = token.rsplit_once(':').map(|(p, _)| p)?;
            (!path.is_empty() && numeric(last)).then(|| vec![path.to_owned(), last.to_owned()])
        }
    }
}

/// 1-based column of the first `name` on 1-based `line` of `text`, as a whole
/// identifier, and how many times it is there.
pub fn find_symbol(text: &str, line: u32, name: &str) -> Result<(u32, usize), String> {
    let src = text
        .lines()
        .nth((line as usize).wrapping_sub(1))
        .ok_or_else(|| format!("line {line} is past the end of the file"))?;
    let hits: Vec<usize> = identifiers(src).into_iter().filter(|(_, w)| w == name).map(|(c, _)| c).collect();
    match hits.first() {
        Some(&start) => Ok((start as u32 + 1, hits.len())),
        None => Err(format!("`{name}` is not on line {line}: {}", src.trim())),
    }
}

/// Rewrite `args` (as from `std::env::args`) for clap. `value_flags` are the
/// options that take a separate value (`--root DIR`), so that value is not
/// taken for a positional argument; `read` reads a file, `exists` tells
/// whether a path exists.
pub fn expand(
    args: Vec<String>,
    value_flags: &HashSet<String>,
    read: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Expanded, String> {
    // Take `--symbol` out, and find where the positional arguments are.
    let mut out: Vec<String> = Vec::with_capacity(args.len() + 2);
    let mut positions: Vec<usize> = Vec::new(); // indices into `out`
    let mut symbol: Option<String> = None;
    let mut iter = args.into_iter();
    if let Some(program) = iter.next() {
        out.push(program);
    }
    let mut only_positionals = false;
    while let Some(arg) = iter.next() {
        if only_positionals {
            positions.push(out.len());
            out.push(arg);
        } else if arg == "--" {
            only_positionals = true;
            out.push(arg);
        } else if arg == "--symbol" {
            symbol = Some(iter.next().ok_or("--symbol needs a name")?);
        } else if let Some(name) = arg.strip_prefix("--symbol=") {
            symbol = Some(name.to_owned());
        } else if arg.starts_with('-') && arg.len() > 1 {
            let takes_value = !arg.contains('=') && value_flags.contains(&arg);
            out.push(arg);
            if takes_value && let Some(value) = iter.next() {
                out.push(value);
            }
        } else {
            positions.push(out.len());
            out.push(arg);
        }
    }

    let is_position_query = positions.len() >= 2
        && out[positions[0]] == "query"
        && POSITION_COMMANDS.contains(&out[positions[1]].as_str());
    if !is_position_query {
        if symbol.is_some() {
            return Err("--symbol only goes with a position query (hover, definition, references, …)".into());
        }
        return Ok(Expanded { args: out, note: None });
    }
    let Some(&file_at) = positions.get(2) else {
        return Ok(Expanded { args: out, note: None });
    };

    // `file:line:col` / `file:line`.
    let mut after_file = positions.len() - 3; // positionals after the file
    if let Some(parts) = split_location(&out[file_at], exists) {
        after_file += parts.len() - 1;
        out.splice(file_at..=file_at, parts);
    }

    let Some(name) = symbol else {
        return Ok(Expanded { args: out, note: None });
    };
    // With --symbol: the file and the line, then (for rename) the new name.
    let command = out[positions[1]].clone();
    let extra = if command == "rename" { 1 } else { 0 };
    if after_file == 0 {
        return Err("--symbol needs the line: <file> <line> --symbol <name>, or <file>:<line> --symbol <name>".into());
    }
    if after_file > 1 + extra {
        return Err("give either a column or --symbol, not both".into());
    }
    let file = out[file_at].clone();
    let line: u32 = out[file_at + 1].parse().map_err(|_| format!("not a line number: {}", out[file_at + 1]))?;
    let text = read(&file).ok_or_else(|| format!("cannot read {file}"))?;
    let (col, count) = find_symbol(&text, line, &name).map_err(|e| format!("{file}: {e}"))?;
    out.insert(file_at + 2, col.to_string());
    let note = (count > 1).then(|| {
        format!("Note: `{name}` is on line {line} {count} times; using the first, at {line}:{col}.")
    });
    Ok(Expanded { args: out, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn main() {\n    let total = total_of(total);\n}\n";

    fn run(args: &[&str]) -> Result<Expanded, String> {
        let flags: HashSet<String> = ["--root".to_owned()].into();
        expand(
            args.iter().map(|s| s.to_string()).collect(),
            &flags,
            &|_| Some(SRC.to_owned()),
            &|p| p == "weird:1",
        )
    }

    fn args(e: Result<Expanded, String>) -> Vec<String> {
        e.unwrap().args.into_iter().skip(1).collect()
    }

    #[test]
    fn plain_arguments_are_left_alone() {
        assert_eq!(args(run(&["x", "query", "hover", "a.rs", "2", "9"])), ["query", "hover", "a.rs", "2", "9"]);
        assert_eq!(args(run(&["x", "session", "status"])), ["session", "status"]);
    }

    #[test]
    fn file_line_col_is_split() {
        assert_eq!(args(run(&["x", "query", "hover", "src/a.rs:2:9"])), ["query", "hover", "src/a.rs", "2", "9"]);
        assert_eq!(
            args(run(&["x", "query", "rename", "a.rs:2:9", "sum"])),
            ["query", "rename", "a.rs", "2", "9", "sum"]
        );
    }

    #[test]
    fn a_flag_value_is_not_taken_for_the_file() {
        assert_eq!(
            args(run(&["x", "query", "hover", "--root", "/p", "a.rs:2:9", "--json"])),
            ["query", "hover", "--root", "/p", "a.rs", "2", "9", "--json"]
        );
    }

    #[test]
    fn an_existing_file_with_a_colon_is_not_split() {
        assert_eq!(args(run(&["x", "query", "hover", "weird:1", "2", "9"])), ["query", "hover", "weird:1", "2", "9"]);
    }

    #[test]
    fn symbol_picks_the_first_whole_identifier_and_says_so() {
        let e = run(&["x", "query", "hover", "a.rs:2", "--symbol", "total"]).unwrap();
        // `total` at column 9, not inside `total_of`.
        assert_eq!(e.args[1..], ["query", "hover", "a.rs", "2", "9"]);
        let note = e.note.unwrap();
        assert!(note.contains("2 times") && note.contains("2:9"), "{note}");

        let e = run(&["x", "query", "definition", "a.rs", "2", "--symbol=total_of"]).unwrap();
        assert_eq!(e.args[1..], ["query", "definition", "a.rs", "2", "17"]);
        assert!(e.note.is_none());
    }

    #[test]
    fn symbol_works_for_rename() {
        assert_eq!(
            args(run(&["x", "query", "rename", "a.rs:2", "sum", "--symbol", "total_of"])),
            ["query", "rename", "a.rs", "2", "17", "sum"]
        );
    }

    #[test]
    fn symbol_errors() {
        assert!(run(&["x", "query", "hover", "a.rs", "2", "--symbol", "nope"]).unwrap_err().contains("not on line 2"));
        assert!(run(&["x", "query", "hover", "a.rs", "2", "9", "--symbol", "total"]).unwrap_err().contains("not both"));
        assert!(run(&["x", "query", "hover", "a.rs", "--symbol", "total"]).unwrap_err().contains("needs the line"));
        assert!(run(&["x", "query", "symbols", "a.rs", "--symbol", "total"]).unwrap_err().contains("position query"));
        assert!(run(&["x", "query", "hover", "a.rs", "9", "--symbol", "total"]).unwrap_err().contains("past the end"));
    }
}
