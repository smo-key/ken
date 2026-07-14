//! File import: copy an external file into a private staging area inside the
//! project so it can be previewed before it's placed, ask the assistant where
//! it should live, then move it to the chosen folder and index it. The original
//! is only ever read — the copy semantics (external → staging → final move)
//! leave the user's file untouched.
//!
//! Everything here is pure/deterministic policy the commands in `src-tauri`
//! drive: staging-path construction, the classify prompt/response contract, and
//! the collision-safe final name. The Claude CLI call itself stays behind the
//! `assistant::oneshot` seam in the command layer, so this module's parsing is
//! tested without shelling out.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Where staged imports live, relative to the project root. Under `.ken` so the
/// scanner (which skips hidden dirs) never indexes a half-imported file, and so
/// a stray staging dir travels with the project rather than polluting it.
pub const IMPORTS_DIR: &str = ".ken/imports";

/// A fresh, collision-free import identifier (the staging subfolder name).
pub fn new_import_id() -> String {
    Uuid::new_v4().to_string()
}

/// Absolute staging directory for one import: `<root>/.ken/imports/<id>`.
pub fn staging_dir(root: &Path, import_id: &str) -> PathBuf {
    root.join(IMPORTS_DIR).join(import_id)
}

/// Project-relative path of the staged file, the identifier the frontend hands
/// to the preview commands. Always forward-slashed (it's a URL-ish key, and
/// `Project::resolve` accepts it on every platform).
pub fn staging_rel(import_id: &str, file_name: &str) -> String {
    format!("{IMPORTS_DIR}/{import_id}/{file_name}")
}

/// The AI's (or default) placement decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    /// Project-relative destination folder; empty string means the project root.
    pub folder: String,
    /// True when `folder` doesn't exist yet (a proposed new folder).
    pub is_new: bool,
    /// One-line justification, when the model gave one.
    pub rationale: Option<String>,
}

impl Placement {
    /// The benign fallback used whenever classification can't run or its output
    /// is unusable: the project root, never new, no rationale. The import flow
    /// must never hard-fail just because the AI was unavailable.
    pub fn root() -> Placement {
        Placement { folder: String::new(), is_new: false, rationale: None }
    }
}

/// The prompt asking the assistant to choose a destination folder. The response
/// contract is a tiny fixed shape (`FOLDER:` / `NEW:` / `WHY:`) so parsing is
/// robust and testable; the folder list grounds the choice in real folders.
pub fn compose_classify_prompt(
    file_name: &str,
    kind: &str,
    excerpt: &str,
    folders: &[String],
) -> String {
    let mut p = String::new();
    p.push_str(
        "You are filing one imported file into a knowledge project. Decide the \
single best destination FOLDER for it — either one of the existing folders \
below, or a NEW project-relative folder path you propose when none fit.\n\n",
    );
    p.push_str(&format!("File name: {file_name}\nFile kind: {kind}\n\n"));
    p.push_str("Existing folders (project-relative; empty means the project root):\n");
    if folders.is_empty() {
        p.push_str("  (none yet — the project root is the only folder)\n");
    } else {
        for f in folders {
            p.push_str(&format!("  - {f}\n"));
        }
    }
    if !excerpt.trim().is_empty() {
        p.push_str("\nA snippet of the file's content:\n");
        p.push_str(excerpt.trim());
        p.push('\n');
    }
    p.push_str(
        "\nReply with EXACTLY these lines and nothing else:\n\
FOLDER: <project-relative folder path, or empty for the project root>\n\
NEW: <yes if the folder does not exist yet, otherwise no>\n\
WHY: <one short sentence>\n\
Do not modify, create, or read any files — only answer.\n",
    );
    p
}

/// Parse the assistant's reply into a `Placement`, grounded against the real
/// folder list. Anything unusable (no `FOLDER:` line, an escaping/absolute
/// path) degrades to the project root rather than failing. `is_new` is decided
/// authoritatively by folder-list membership — the model's `NEW:` hint can't
/// contradict what actually exists on disk.
pub fn parse_placement(text: &str, folders: &[String]) -> Placement {
    let mut folder: Option<String> = None;
    let mut rationale: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = strip_ci(trimmed, "FOLDER:") {
            // Keep slashes here so the safety check below still sees a leading
            // "/" as the absolute-path escape it is; trailing slashes are
            // trimmed only after the value is proven safe.
            folder = Some(rest.trim().to_string());
        } else if let Some(rest) = strip_ci(trimmed, "WHY:") {
            let why = rest.trim();
            if !why.is_empty() {
                rationale = Some(why.to_string());
            }
        }
    }

    let folder = match folder {
        Some(f) if is_safe_relative_folder(&f) => f.trim_matches('/').to_string(),
        // No parseable/safe folder → root, and a rationale would be misleading.
        _ => return Placement::root(),
    };

    let is_new = !folder.is_empty() && !folders.iter().any(|f| f.trim_matches('/') == folder);
    Placement { folder, is_new, rationale }
}

/// A parsed folder is usable only if it stays inside the project: no absolute
/// paths, no `..` escapes, no Windows drive/UNC. The empty string (root) is
/// fine. Mirrors `Project::resolve`'s guarantee so the command layer's later
/// `resolve` can't reject what we accepted.
fn is_safe_relative_folder(folder: &str) -> bool {
    if folder.is_empty() {
        return true;
    }
    let path = Path::new(folder);
    if path.is_absolute() {
        return false;
    }
    !path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::Prefix(_)))
}

/// Case-insensitive prefix strip, returning the remainder when `s` starts with
/// `prefix`. Keeps the `FOLDER:`/`NEW:`/`WHY:` parse tolerant of casing.
fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

/// Join a (project-relative) destination folder and a file name into the final
/// project-relative path. The root folder yields a bare name.
pub fn final_rel(folder: &str, file_name: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        file_name.to_string()
    } else {
        format!("{folder}/{file_name}")
    }
}

/// Choose a file name that doesn't collide in the destination, disambiguating
/// `report.pdf` → `report (2).pdf` → `report (3).pdf`. `exists` is injected so
/// the policy is testable without a filesystem. The original is only ever read,
/// so a collision must never overwrite a teammate's file of the same name.
pub fn disambiguate_name(file_name: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(file_name) {
        return file_name.to_string();
    }
    let (stem, ext) = split_stem_ext(file_name);
    let mut n = 2;
    loop {
        let candidate = format!("{stem} ({n}){ext}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Split a file name into (stem, extension-including-dot). The extension is the
/// final `.suffix` only when there's a non-empty stem before it, so a dotfile
/// like `.gitignore` stays whole and a counter appends after it.
fn split_stem_ext(file_name: &str) -> (&str, &str) {
    match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_rel_is_under_ken_imports() {
        let rel = staging_rel("abc-123", "Report.pdf");
        assert_eq!(rel, ".ken/imports/abc-123/Report.pdf");
        // Hidden (dot) prefix guarantees the scanner skips it until committed.
        assert!(rel.starts_with(".ken/"));
    }

    #[test]
    fn staging_dir_joins_under_root() {
        let dir = staging_dir(Path::new("/proj"), "id7");
        assert!(dir.ends_with("id7"));
        assert!(dir.starts_with("/proj/.ken/imports"));
    }

    #[test]
    fn new_import_ids_are_unique() {
        assert_ne!(new_import_id(), new_import_id());
    }

    #[test]
    fn parse_placement_reads_existing_folder_not_new() {
        let folders = vec!["notes".to_string(), "reports/2024".to_string()];
        let p = parse_placement("FOLDER: reports/2024\nNEW: no\nWHY: quarterly report\n", &folders);
        assert_eq!(p.folder, "reports/2024");
        assert!(!p.is_new, "an existing folder is never new");
        assert_eq!(p.rationale.as_deref(), Some("quarterly report"));
    }

    #[test]
    fn parse_placement_marks_unknown_folder_new() {
        let folders = vec!["notes".to_string()];
        // Even though the model said NEW: no, the folder isn't on disk → new.
        let p = parse_placement("FOLDER: invoices\nNEW: no\n", &folders);
        assert_eq!(p.folder, "invoices");
        assert!(p.is_new, "a folder absent from the list is proposed-new");
    }

    #[test]
    fn parse_placement_root_is_never_new() {
        let p = parse_placement("FOLDER:\nNEW: yes\n", &[]);
        assert_eq!(p.folder, "");
        assert!(!p.is_new);
    }

    #[test]
    fn parse_placement_defaults_to_root_when_unparseable() {
        let p = parse_placement("I think it belongs in reports.", &["reports".into()]);
        assert_eq!(p, Placement::root());
    }

    #[test]
    fn parse_placement_rejects_escaping_folder() {
        // A path that would escape the project falls back to the root.
        assert_eq!(parse_placement("FOLDER: ../secrets\n", &[]), Placement::root());
        assert_eq!(parse_placement("FOLDER: /etc\n", &[]), Placement::root());
    }

    #[test]
    fn parse_placement_is_case_insensitive_and_trims_slashes() {
        let folders = vec!["notes".to_string()];
        let p = parse_placement("folder:  notes/ \nwhy: fits\n", &folders);
        assert_eq!(p.folder, "notes");
        assert!(!p.is_new);
        assert_eq!(p.rationale.as_deref(), Some("fits"));
    }

    #[test]
    fn final_rel_joins_folder_and_name() {
        assert_eq!(final_rel("reports/2024", "q3.pdf"), "reports/2024/q3.pdf");
    }

    #[test]
    fn final_rel_root_folder_is_bare_name() {
        assert_eq!(final_rel("", "q3.pdf"), "q3.pdf");
        assert_eq!(final_rel("/", "q3.pdf"), "q3.pdf");
    }

    #[test]
    fn disambiguate_name_returns_original_when_free() {
        let taken: std::collections::HashSet<&str> = ["other.pdf"].into_iter().collect();
        assert_eq!(disambiguate_name("report.pdf", |c| taken.contains(c)), "report.pdf");
    }

    #[test]
    fn disambiguate_name_appends_counter_preserving_extension() {
        let taken: std::collections::HashSet<&str> =
            ["report.pdf", "report (2).pdf"].into_iter().collect();
        assert_eq!(disambiguate_name("report.pdf", |c| taken.contains(c)), "report (3).pdf");
    }

    #[test]
    fn disambiguate_name_handles_no_extension_and_dotfiles() {
        let taken1: std::collections::HashSet<&str> = ["notes"].into_iter().collect();
        assert_eq!(disambiguate_name("notes", |c| taken1.contains(c)), "notes (2)");
        // A leading-dot name has no stem before the dot, so it stays whole.
        let taken2: std::collections::HashSet<&str> = [".env"].into_iter().collect();
        assert_eq!(disambiguate_name(".env", |c| taken2.contains(c)), ".env (2)");
        // Compound extension: only the final segment is treated as the ext.
        let taken3: std::collections::HashSet<&str> = ["a.tar.gz"].into_iter().collect();
        assert_eq!(disambiguate_name("a.tar.gz", |c| taken3.contains(c)), "a.tar (2).gz");
    }

    #[test]
    fn compose_classify_prompt_lists_folders_and_file() {
        let prompt =
            compose_classify_prompt("budget.xlsx", "xlsx", "Q3 numbers", &["finance".into()]);
        assert!(prompt.contains("budget.xlsx"));
        assert!(prompt.contains("finance"));
        assert!(prompt.contains("FOLDER:"));
        assert!(prompt.contains("Q3 numbers"));
    }
}
