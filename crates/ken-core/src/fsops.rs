//! Small pure helpers for user-driven file operations in the Files tree
//! (create / rename / move, §12). Pure so the naming and safety policies are
//! unit-tested without a filesystem; the Tauri commands are thin shells.

/// Pick a document name that doesn't collide: `Untitled.md` → `Untitled 2.md`
/// → `Untitled 3.md` (space + counter, before the extension — the §12 style;
/// imports keep their own `report (2).pdf` style in `import.rs`). `exists` is
/// injected so the policy tests without disk.
pub fn numbered_name(desired: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(desired) {
        return desired.to_string();
    }
    let (stem, ext) = split_stem_ext(desired);
    let mut n = 2u32;
    loop {
        let candidate = format!("{stem} {n}{ext}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Split a file name into (stem, extension-including-dot). The extension is the
/// final `.suffix` only when a non-empty stem precedes it, so a dotfile like
/// `.env` stays whole and the counter appends after it.
fn split_stem_ext(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name, ""),
    }
}

/// Whether moving `from_rel` to `to_rel` would put a folder onto itself or
/// inside its own subtree. Rel paths use '/' separators (the project-relative
/// convention everywhere in Ken).
pub fn is_into_own_subtree(from_rel: &str, to_rel: &str) -> bool {
    to_rel == from_rel || to_rel.starts_with(&format!("{from_rel}/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn taken(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn numbered_name_returns_the_desired_name_when_free() {
        let t = taken(&[]);
        assert_eq!(numbered_name("Untitled.md", |c| t.contains(c)), "Untitled.md");
    }

    #[test]
    fn numbered_name_counts_up_past_collisions() {
        let t = taken(&["Untitled.md"]);
        assert_eq!(numbered_name("Untitled.md", |c| t.contains(c)), "Untitled 2.md");
        let t = taken(&["Untitled.md", "Untitled 2.md"]);
        assert_eq!(numbered_name("Untitled.md", |c| t.contains(c)), "Untitled 3.md");
    }

    #[test]
    fn numbered_name_keeps_dotfiles_and_multi_dot_names_sane() {
        let t = taken(&[".env"]);
        assert_eq!(numbered_name(".env", |c| t.contains(c)), ".env 2");
        let t = taken(&["a.tar.gz"]);
        assert_eq!(numbered_name("a.tar.gz", |c| t.contains(c)), "a.tar 2.gz");
    }

    #[test]
    fn subtree_guard_blocks_self_and_descendants_only() {
        assert!(is_into_own_subtree("A", "A"));
        assert!(is_into_own_subtree("A", "A/B"));
        assert!(is_into_own_subtree("Meetings/2026", "Meetings/2026/Q1"));
        assert!(!is_into_own_subtree("A", "AB")); // sibling sharing a name prefix
        assert!(!is_into_own_subtree("A/B", "A")); // moving OUT of a subtree is fine
        assert!(!is_into_own_subtree("A", "B/A"));
    }
}
