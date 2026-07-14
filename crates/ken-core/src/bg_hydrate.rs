//! Background hydration policy: which cloud-only files the low-priority
//! worker downloads-and-indexes on its own, and whether the feature is on.
//!
//! The user wants cloud-offline *documents* to be searchable without having
//! to open each one. This module holds the pure decision — "is this row worth
//! pulling down in the background?" — so the selection rule is unit-tested
//! without a real `SF_DATALESS` file (which can't be fabricated in a test).
//! The worker that acts on it (throttling, backoff, off-lock I/O) lives in the
//! app layer, alongside the transcript and knowledge workers it must not fight.

use std::path::Path;

use crate::db::FileRow;
use crate::extract::{FileKind, MAX_EXTRACT_BYTES};
use crate::project::Project;
use crate::scan;

/// Cap on a cloud-only file's size before the background worker will download
/// it. Placeholders still report their true size in metadata, so this reads
/// the index's stored `size`. Kept at the extractor's own limit: a file we
/// couldn't extract even once local is not worth spending bandwidth to pull
/// down, and large media/archives stay the on-open path's job.
pub const MAX_BACKGROUND_BYTES: u64 = MAX_EXTRACT_BYTES;

/// Is background indexing of cloud-offline files enabled for this project?
///
/// Persisted like the sync toggle — `project.json` extra `"backgroundIndex"` —
/// and ON by default: the whole point is that files are searchable without the
/// user knowing they were offline, so it has to work out of the box.
pub fn background_index_enabled(project: &Project) -> bool {
    project
        .config
        .extra
        .get("backgroundIndex")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// Should the background worker download-and-index this indexed row?
///
/// Deliberately narrow. Only `cloud_only` rows have bytes still in the cloud;
/// only text-bearing, non-video kinds under the size cap are worth the
/// bandwidth (videos and huge media are left to the on-open path); and an
/// excluded path is never touched even if a stale row lingers in the index.
pub fn wants_background_index(row: &FileRow, excluded: bool) -> bool {
    if row.status != scan::STATUS_CLOUD_ONLY {
        return false;
    }
    if excluded {
        return false;
    }
    let kind = FileKind::from_path(Path::new(&row.rel_path));
    if kind == FileKind::Video || !kind.has_content() {
        return false;
    }
    // A negative or oversized placeholder is skipped: nothing sane to download.
    row.size >= 0 && (row.size as u64) <= MAX_BACKGROUND_BYTES
}

/// The project's cloud-only documents worth pulling down now, as rel paths.
/// Filters on `project.is_excluded` here (not only in the scanner) because the
/// worker reads the index directly, where a just-excluded folder's rows can
/// still be present until the next rescan removes them.
pub fn pending_documents(files: &[FileRow], project: &Project) -> Vec<String> {
    files
        .iter()
        .filter(|f| wants_background_index(f, project.is_excluded(&f.rel_path)))
        .map(|f| f.rel_path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rel: &str, status: &str, size: i64) -> FileRow {
        FileRow {
            rel_path: rel.into(),
            kind: FileKind::from_path(Path::new(rel)).as_str().into(),
            size,
            mtime: 0,
            status: status.into(),
            error: None,
        }
    }

    /// The feature ships on: a fresh project with no stored preference must
    /// index cloud files in the background without the user opting in.
    #[test]
    fn background_index_defaults_on() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "Fixture").unwrap();
        assert!(background_index_enabled(&project));
    }

    #[test]
    fn stored_preference_overrides_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project::create(dir.path(), "Fixture").unwrap();
        project
            .config
            .extra
            .insert("backgroundIndex".into(), serde_json::Value::Bool(false));
        assert!(!background_index_enabled(&project));
    }

    /// A cloud-only document under the cap is exactly what the worker exists
    /// to pull down.
    #[test]
    fn selects_a_cloud_only_document() {
        assert!(wants_background_index(
            &row("notes/meeting.docx", scan::STATUS_CLOUD_ONLY, 4096),
            false,
        ));
        assert!(wants_background_index(
            &row("plan.md", scan::STATUS_CLOUD_ONLY, 120),
            false,
        ));
    }

    /// Already-local rows have nothing to download; only `cloud_only` qualifies.
    #[test]
    fn ignores_rows_that_are_not_cloud_only() {
        for status in [
            scan::STATUS_INDEXED,
            scan::STATUS_METADATA_ONLY,
            scan::STATUS_FAILED,
        ] {
            assert!(
                !wants_background_index(&row("notes/meeting.docx", status, 4096), false),
                "{status} should not be a background-hydrate candidate"
            );
        }
    }

    /// Video and other large media are deliberately left to the on-open path —
    /// downloading a movie in the background is exactly what the user objected
    /// to. A video is skipped regardless of its (placeholder) size.
    #[test]
    fn skips_video_even_when_it_is_cloud_only() {
        assert!(!wants_background_index(
            &row("clips/standup.mp4", scan::STATUS_CLOUD_ONLY, 1024),
            false,
        ));
    }

    /// Images and opaque binaries carry no extractable text, so pulling their
    /// bytes down would cost bandwidth for nothing.
    #[test]
    fn skips_kinds_without_text_content() {
        assert!(!wants_background_index(
            &row("photos/team.jpg", scan::STATUS_CLOUD_ONLY, 4096),
            false,
        ));
        assert!(!wants_background_index(
            &row("archive.zip", scan::STATUS_CLOUD_ONLY, 4096),
            false,
        ));
    }

    /// The size cap keeps huge documents (a 200 MB PDF) on the on-open path.
    #[test]
    fn skips_documents_over_the_size_cap() {
        let big = MAX_BACKGROUND_BYTES as i64 + 1;
        assert!(!wants_background_index(
            &row("big.pdf", scan::STATUS_CLOUD_ONLY, big),
            false,
        ));
        // Exactly at the cap is still fine.
        assert!(wants_background_index(
            &row("edge.pdf", scan::STATUS_CLOUD_ONLY, MAX_BACKGROUND_BYTES as i64),
            false,
        ));
    }

    /// An excluded path is never downloaded, even if a stale cloud-only row for
    /// it is still in the index.
    #[test]
    fn skips_excluded_paths() {
        assert!(!wants_background_index(
            &row("archive/old.docx", scan::STATUS_CLOUD_ONLY, 4096),
            true,
        ));
    }

    /// The list selector applies exclusions from the live project config, not
    /// just whatever the scanner last stored.
    #[test]
    fn pending_documents_respects_live_exclusions() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project::create(dir.path(), "Fixture").unwrap();
        project.set_excluded(vec!["archive".into()]).unwrap();

        let files = vec![
            row("notes/meeting.docx", scan::STATUS_CLOUD_ONLY, 4096),
            row("archive/old.docx", scan::STATUS_CLOUD_ONLY, 4096),
            row("clips/standup.mp4", scan::STATUS_CLOUD_ONLY, 4096),
            row("plan.md", scan::STATUS_INDEXED, 200),
        ];
        let pending = pending_documents(&files, &project);
        assert_eq!(pending, vec!["notes/meeting.docx".to_string()]);
    }
}
