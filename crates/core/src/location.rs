//! `path:line[:col]` references in plain output (compiler errors, test
//! failures, stack traces), for the Run and Terminal panels to make clickable.

use std::ops::Range;

/// Every `path:line[:col]` in `line`: byte range in the line, path, 1-based
/// line and column. Windows paths with a drive (`D:\src\main.rs:3`) count.
pub fn find_locations(line: &str) -> Vec<(Range<usize>, String, u32, u32)> {
    let is_sep = |c: char| c.is_whitespace() || "()[]{}<>'\",;`".contains(c);
    let mut found = Vec::new();
    let mut start = 0;
    for token in line.split(is_sep) {
        let begin = start;
        start += token.len() + token_sep_len(line, start + token.len());
        let t = token.trim_end_matches(['.', ':']);
        let (drive, rest) = split_drive(t);
        let mut parts = rest.splitn(3, ':');
        let (Some(path), Some(l)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(l) = l.parse::<u32>() else { continue };
        let col = parts
            .next()
            .and_then(|c| c.parse::<u32>().ok())
            .unwrap_or(1);
        // Looks like a file: has an extension, not a URL or a time.
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if path.contains("://") || !name.contains('.') {
            continue;
        }
        if name
            .rsplit('.')
            .next()
            .is_none_or(|ext| ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            continue;
        }
        found.push((
            begin..begin + t.len(),
            format!("{drive}{path}"),
            l.max(1),
            col.max(1),
        ));
    }
    found
}

/// The first of [`find_locations`].
pub fn find_location(line: &str) -> Option<(Range<usize>, String, u32, u32)> {
    find_locations(line).into_iter().next()
}

/// Byte length of the separator character at `at` (0 at the end of the line).
fn token_sep_len(line: &str, at: usize) -> usize {
    line[at..].chars().next().map_or(0, char::len_utf8)
}

/// `D:\x` → (`D:`, `\x`); anything else → (``, it).
fn split_drive(t: &str) -> (&str, &str) {
    let b = t.as_bytes();
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
        t.split_at(2)
    } else {
        ("", t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_file_locations() {
        let l = "  --> src/main.rs:10:5";
        let (r, p, line, col) = find_location(l).unwrap();
        assert_eq!(
            (&l[r], p.as_str(), line, col),
            ("src/main.rs:10:5", "src/main.rs", 10, 5)
        );
        assert_eq!(
            find_location("error at (app.py:3)").map(|x| (x.1, x.2)),
            Some(("app.py".into(), 3))
        );
        assert_eq!(
            find_location("thread 'main' panicked at src/lib.rs:7:9:").map(|x| x.2),
            Some(7)
        );
        assert!(find_location("see https://x.dev:443/a").is_none());
        assert!(find_location("at 12:30:01 done").is_none());
        assert!(find_location("plain text").is_none());
    }

    #[test]
    fn windows_paths_and_several_per_line() {
        let l = r"D:\proj\src\main.rs:12:3: error";
        let (r, p, line, col) = find_location(l).unwrap();
        assert_eq!(
            (&l[r], p.as_str(), line, col),
            (r"D:\proj\src\main.rs:12:3", r"D:\proj\src\main.rs", 12, 3)
        );
        let l = "a.rs:1 → b.rs:2";
        let all = find_locations(l);
        assert_eq!(all.len(), 2);
        assert_eq!(&l[all[1].0.clone()], "b.rs:2");
    }
}
