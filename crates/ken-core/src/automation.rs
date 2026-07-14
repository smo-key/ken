/// Match a project-relative path against a glob. `**` crosses `/`; `*` and `?`
/// do not. Case-sensitive. Pure so it's unit-tested without a filesystem.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    seg_match(&p, &s)
}

fn seg_match(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => {
            // `**` consumes zero or more path segments.
            if seg_match(&p[1..], s) {
                return true;
            }
            !s.is_empty() && seg_match(p, &s[1..])
        }
        Some(seg) => {
            if s.is_empty() {
                return false;
            }
            wildcard_match(seg, s[0]) && seg_match(&p[1..], &s[1..])
        }
    }
}

/// `*` (any run, no `/`) and `?` (one char) within a single path segment.
fn wildcard_match(pat: &str, text: &str) -> bool {
    let pat: Vec<char> = pat.chars().collect();
    let text: Vec<char> = text.chars().collect();
    fn go(pat: &[char], text: &[char]) -> bool {
        match pat.first() {
            None => text.is_empty(),
            Some('*') => go(&pat[1..], text) || (!text.is_empty() && go(pat, &text[1..])),
            Some('?') => !text.is_empty() && go(&pat[1..], &text[1..]),
            Some(&c) => !text.is_empty() && text[0] == c && go(&pat[1..], &text[1..]),
        }
    }
    go(&pat, &text)
}

#[cfg(test)]
mod glob_tests {
    use super::glob_match;
    #[test]
    fn star_does_not_cross_slash() {
        assert!(glob_match("Recordings/*.md", "Recordings/a.md"));
        assert!(!glob_match("Recordings/*.md", "Recordings/sub/a.md"));
    }
    #[test]
    fn doublestar_crosses_slash() {
        assert!(glob_match("Recordings/**/*.md", "Recordings/sub/a.md"));
        assert!(glob_match("**/*.md", "a/b/c.md"));
        assert!(!glob_match("**/*.md", "a/b/c.txt"));
    }
    #[test]
    fn question_matches_single_non_slash() {
        assert!(glob_match("a?.md", "ab.md"));
        assert!(!glob_match("a?.md", "a/b.md"));
    }
    #[test]
    fn literal_and_case_sensitive() {
        assert!(glob_match("Notes/People.md", "Notes/People.md"));
        assert!(!glob_match("Notes/People.md", "notes/people.md"));
    }
}
