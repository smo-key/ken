//! Refresh engine internals: plan (what changed, what prompt), and the
//! staging → rules → apply pipeline. Ken, not the agent, decides what
//! reaches real output files.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use similar::TextDiff;

use crate::db::Db;
use crate::project::Project;
use crate::recipe::{Mode, Recipe, ResolvedRules};
use crate::{Error, Result};

pub const STAGING_ROOT: &str = ".ken/.staging";
pub const REMOVED_MANIFEST: &str = "_removed.txt";

pub fn staging_dir(root: &Path, slug: &str) -> PathBuf {
    root.join(STAGING_ROOT).join(slug)
}

#[derive(Debug, Clone)]
pub struct RefreshPlan {
    pub slug: String,
    pub prompt: String,
    pub changed: Vec<String>,
    pub first_run: bool,
    pub staging: PathBuf,
    /// Output files and their mtimes at plan time — used to detect human
    /// edits that land while the agent is working.
    pub output_snapshot: HashMap<String, i64>,
}

/// Is this indexed file one of the recipe's sources?
fn in_sources(recipe: &Recipe, rel_path: &str) -> bool {
    if under_output(recipe, rel_path) {
        return false; // an ingest's own outputs never count as its inputs/triggers
    }
    if recipe.sources.is_empty() {
        return true;
    }
    recipe.sources.iter().any(|s| {
        let s = s.trim_matches('/');
        rel_path == s || rel_path.starts_with(&format!("{s}/"))
    })
}

pub fn under_output(recipe: &Recipe, rel_path: &str) -> bool {
    let out = recipe.output.trim_matches('/');
    rel_path == out || rel_path.starts_with(&format!("{out}/"))
}

/// Does a set of changed paths warrant queueing this recipe?
pub fn triggers(recipe: &Recipe, changed_or_scanned: &[String]) -> bool {
    changed_or_scanned.iter().any(|p| in_sources(recipe, p))
}

/// Build the plan for a refresh: which files changed since the last
/// successful run, and the full prompt. Returns None when nothing changed
/// and `force_full` is false.
pub fn plan(
    project: &Project,
    db: &Db,
    recipe: &Recipe,
    rules: &ResolvedRules,
    force_full: bool,
) -> Result<Option<RefreshPlan>> {
    let last_success = db.last_success_at(&recipe.slug)?;
    let files = db.list_files()?;

    let source_files: Vec<_> = files
        .iter()
        .filter(|f| in_sources(recipe, &f.rel_path))
        .collect();

    let first_run = last_success.is_none();
    let changed: Vec<String> = if first_run {
        source_files.iter().map(|f| f.rel_path.clone()).collect()
    } else {
        let since = last_success.unwrap();
        source_files
            .iter()
            .filter(|f| f.mtime > since)
            .map(|f| f.rel_path.clone())
            .collect()
    };

    if changed.is_empty() && !force_full {
        return Ok(None);
    }
    let effective_changed: Vec<String> = if changed.is_empty() {
        source_files.iter().map(|f| f.rel_path.clone()).collect()
    } else {
        changed
    };

    let output_files = current_output_files(project, recipe)?;
    let output_snapshot = output_files
        .iter()
        .map(|rel| {
            let mtime = file_mtime(&project.root.join(rel)).unwrap_or(0);
            (rel.clone(), mtime)
        })
        .collect();

    let staging = staging_dir(&project.root, &recipe.slug);
    // Fresh staging for every run.
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| Error::io(&staging, e))?;
    }
    fs::create_dir_all(&staging).map_err(|e| Error::io(&staging, e))?;

    let prompt = compose_prompt(
        recipe,
        rules,
        &effective_changed,
        first_run || changed_is_everything(&effective_changed, &source_files),
        &output_files,
        &staging,
    );

    Ok(Some(RefreshPlan {
        slug: recipe.slug.clone(),
        prompt,
        changed: effective_changed,
        first_run,
        staging,
        output_snapshot,
    }))
}

fn changed_is_everything(changed: &[String], sources: &[&crate::db::FileRow]) -> bool {
    changed.len() == sources.len()
}

fn compose_prompt(
    recipe: &Recipe,
    rules: &ResolvedRules,
    changed: &[String],
    full_pass: bool,
    output_files: &[String],
    staging: &Path,
) -> String {
    let mode_text = match recipe.mode {
        Mode::Single => format!(
            "This ingest maintains ONE document: `{out}`.\nWrite the complete updated document to `{staging}/{out}`.",
            out = recipe.output,
            staging = staging.display()
        ),
        Mode::Collection => format!(
            "This ingest maintains a COLLECTION: one markdown file per entity in `{out}`.\nWrite each complete updated or new file to `{staging}/{out}<kebab-case-name>.md`.\nIf an existing entity should be removed, add its filename (one per line) to `{staging}/{out}{manifest}`.",
            out = with_trailing_slash(&recipe.output),
            staging = staging.display(),
            manifest = REMOVED_MANIFEST
        ),
    };
    let sources_text = if recipe.sources.is_empty() {
        "all project folders".to_string()
    } else {
        recipe.sources.join(", ")
    };
    let changed_text = if full_pass {
        format!(
            "Full pass — read all source material:\n{}",
            bullet_list(changed)
        )
    } else {
        format!(
            "Only these files changed since the last successful run — focus on them:\n{}",
            bullet_list(changed)
        )
    };
    let outputs_text = if output_files.is_empty() {
        "None yet — this is the first run; create the output from scratch.".to_string()
    } else {
        format!(
            "Read these current documents FIRST; they are canonical:\n{}",
            bullet_list(output_files)
        )
    };

    format!(
        r#"You are Ken's ingest runner, refreshing the structured document(s) for the ingest "{name}".

## Instruction

{instruction}

## Rules (non-negotiable)

- The current output documents are canonical. Update only what the changed source material implies; keep everything else exactly as it is.
- Human edits in the current outputs must be preserved, even where they disagree with your own phrasing.
- Never ask the user questions. Where the data is ambiguous, make the reasonable assumption and record it in an "Open questions" section of the output document.
- Write ONLY inside the staging directory named below. Do not modify any other file.
- Every file you write must be the complete document, not a fragment or diff.
- (For reference: changes above {threshold}% of a document are held for human review before being applied.)

## Source material (folders: {sources})

{changed}

## Current output

{outputs}

## Where to write

STAGING_DIR={staging}
{mode}
"#,
        name = recipe.name,
        instruction = recipe.instruction,
        threshold = rules.review_threshold_pct,
        sources = sources_text,
        changed = changed_text,
        outputs = outputs_text,
        staging = staging.display(),
        mode = mode_text,
    )
}

fn bullet_list(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("- {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn with_trailing_slash(s: &str) -> String {
    let t = s.trim_end_matches('/');
    format!("{t}/")
}

/// Project-relative paths of the recipe's current output files on disk.
pub fn current_output_files(project: &Project, recipe: &Recipe) -> Result<Vec<String>> {
    let mut out = Vec::new();
    match recipe.mode {
        Mode::Single => {
            let rel = recipe.output.trim_matches('/').to_string();
            if project.root.join(&rel).is_file() {
                out.push(rel);
            }
        }
        Mode::Collection => {
            let dir = project.root.join(recipe.output.trim_matches('/'));
            if dir.is_dir() {
                collect_files(&dir, &project.root, &mut out)?;
            }
        }
    }
    out.sort();
    Ok(out)
}

fn collect_files(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, root, out)?;
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOutcome {
    /// 0.0..=1.0 — fraction of output content changed.
    pub change_ratio: f64,
    pub applied: bool,
    pub files_written: usize,
    pub files_removed: usize,
    pub summary: String,
}

/// Everything the agent staged: (project-relative target, staged absolute
/// path), plus targets listed for removal.
struct Staged {
    writes: Vec<(String, PathBuf)>,
    removals: Vec<String>,
}

fn read_staging(_project: &Project, recipe: &Recipe, staging: &Path) -> Result<Staged> {
    let mut files = Vec::new();
    if staging.is_dir() {
        collect_files(staging, staging, &mut files)?;
    }
    let mut writes = Vec::new();
    let mut removals = Vec::new();
    for rel in files {
        let abs = staging.join(&rel);
        if rel.ends_with(REMOVED_MANIFEST) {
            let content = fs::read_to_string(&abs).unwrap_or_default();
            let base = with_trailing_slash(recipe.output.trim_matches('/'));
            for line in content.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let name = line.trim_start_matches(&base).trim_start_matches('/');
                removals.push(format!("{base}{name}"));
            }
            continue;
        }
        // Only accept files under the recipe's output path — anything else
        // the agent wrote is ignored, never applied.
        if under_output(recipe, &rel) {
            writes.push((rel, abs));
        }
    }
    writes.sort();
    removals.sort();
    Ok(Staged { writes, removals })
}

/// Weighted change ratio between current outputs and staged outputs.
fn change_ratio(project: &Project, staged: &Staged) -> f64 {
    let mut weighted_change = 0.0;
    let mut total_weight = 0.0;
    for (rel, staged_abs) in &staged.writes {
        let old = fs::read_to_string(project.root.join(rel)).unwrap_or_default();
        let new = fs::read_to_string(staged_abs).unwrap_or_default();
        let old_lines = old.lines().count();
        let new_lines = new.lines().count();
        let weight = old_lines.max(new_lines).max(1) as f64;
        let similarity = TextDiff::from_lines(&old, &new).ratio() as f64;
        weighted_change += weight * (1.0 - similarity);
        total_weight += weight;
    }
    for rel in &staged.removals {
        let old = fs::read_to_string(project.root.join(rel)).unwrap_or_default();
        let weight = old.lines().count().max(1) as f64;
        weighted_change += weight;
        total_weight += weight;
    }
    if total_weight == 0.0 {
        0.0
    } else {
        weighted_change / total_weight
    }
}

/// Have any output files changed on disk since the plan snapshot?
fn outputs_changed_since(project: &Project, snapshot: &HashMap<String, i64>) -> bool {
    snapshot.iter().any(|(rel, &mtime)| {
        file_mtime(&project.root.join(rel)).map(|m| m > mtime).unwrap_or(true)
    })
}

fn file_mtime(path: &Path) -> Option<i64> {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
}

/// Decide and (maybe) apply a completed run's staged output.
pub fn evaluate(
    project: &Project,
    recipe: &Recipe,
    rules: &ResolvedRules,
    plan: &RefreshPlan,
) -> Result<ApplyOutcome> {
    let staged = read_staging(project, recipe, &plan.staging)?;
    if staged.writes.is_empty() && staged.removals.is_empty() {
        return Ok(ApplyOutcome {
            change_ratio: 0.0,
            applied: true,
            files_written: 0,
            files_removed: 0,
            summary: "No changes were needed.".into(),
        });
    }

    let ratio = change_ratio(project, &staged);
    let threshold = rules.review_threshold_pct as f64 / 100.0;
    let human_edited_mid_run = outputs_changed_since(project, &plan.output_snapshot);

    let hold = !plan.first_run && (ratio > threshold || human_edited_mid_run);
    if hold {
        let reason = if human_edited_mid_run {
            "an output file was edited while the run was in flight".to_string()
        } else {
            format!(
                "this refresh would change {:.0}% of the output (threshold {}%)",
                ratio * 100.0,
                rules.review_threshold_pct
            )
        };
        return Ok(ApplyOutcome {
            change_ratio: ratio,
            applied: false,
            files_written: staged.writes.len(),
            files_removed: staged.removals.len(),
            summary: format!("Held for your review — {reason}."),
        });
    }

    apply_staged(project, recipe, &plan.staging)
}

/// Move staged files into place (used on auto-apply and on explicit
/// approval) and clean up staging.
pub fn apply_staged(project: &Project, recipe: &Recipe, staging: &Path) -> Result<ApplyOutcome> {
    let staged = read_staging(project, recipe, staging)?;
    let ratio = change_ratio(project, &staged);
    let mut written = 0;
    for (rel, staged_abs) in &staged.writes {
        let target = project.root.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::rename(staged_abs, &target).or_else(|_| {
            fs::copy(staged_abs, &target)
                .map(|_| ())
                .map_err(|e| Error::io(&target, e))
        })?;
        written += 1;
    }
    let mut removed = 0;
    for rel in &staged.removals {
        let target = project.root.join(rel);
        if target.is_file() {
            fs::remove_file(&target).map_err(|e| Error::io(&target, e))?;
            removed += 1;
        }
    }
    let _ = fs::remove_dir_all(staging);
    Ok(ApplyOutcome {
        change_ratio: ratio,
        applied: true,
        files_written: written,
        files_removed: removed,
        summary: summary_line(written, removed),
    })
}

pub fn discard_staged(project: &Project, slug: &str) -> Result<()> {
    let staging = staging_dir(&project.root, slug);
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| Error::io(&staging, e))?;
    }
    Ok(())
}

fn summary_line(written: usize, removed: usize) -> String {
    match (written, removed) {
        (0, 0) => "No changes were needed.".into(),
        (w, 0) => format!("Updated {w} document{}.", plural(w)),
        (0, r) => format!("Removed {r} document{}.", plural(r)),
        (w, r) => format!("Updated {w} and removed {r} document{}.", plural(w + r)),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{Recipe, Refresh, DEFAULT_RULES};
    use crate::scan;

    fn setup(mode: Mode, output: &str) -> (tempfile::TempDir, Project, Db, Recipe) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/a.md"), "# A\nPriya owns billing.\n").unwrap();
        fs::write(dir.path().join("notes/b.md"), "# B\nMarcus is backup.\n").unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        scan::scan(&project, &mut db).unwrap();
        let recipe = Recipe {
            slug: "people".into(),
            name: "People".into(),
            description: String::new(),
            sources: vec!["notes".into()],
            output: output.into(),
            mode,
            refresh: Refresh::OnChange,
            rules: None,
            instruction: "Extract people.".into(),
            extra: Default::default(),
        };
        crate::recipe::save(&project, &recipe).unwrap();
        (dir, project, db, recipe)
    }

    fn stage(project: &Project, slug: &str, rel: &str, content: &str) {
        let p = staging_dir(&project.root, slug).join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    #[test]
    fn first_run_covers_corpus_and_prompt_mentions_staging() {
        let (_d, project, db, recipe) = setup(Mode::Single, "knowledge/People.md");
        let plan = plan(&project, &db, &recipe, &DEFAULT_RULES, false)
            .unwrap()
            .unwrap();
        assert!(plan.first_run);
        assert_eq!(plan.changed.len(), 2);
        assert!(plan.prompt.contains("STAGING_DIR="));
        assert!(plan.prompt.contains("notes/a.md"));
        assert!(plan.prompt.contains("first run"));
        assert!(plan.prompt.contains("Extract people."));
    }

    #[test]
    fn incremental_plan_lists_only_changed_files() {
        let (dir, project, mut db, recipe) = setup(Mode::Single, "knowledge/People.md");
        db.insert_run("people", None, file_mtime(&dir.path().join("notes/a.md")).unwrap() + 10)
            .unwrap();
        db.update_run(1, "fresh", None, None, None, None).unwrap();

        // Nothing changed since → no plan.
        assert!(plan(&project, &db, &recipe, &DEFAULT_RULES, false).unwrap().is_none());
        // force_full still yields a full pass.
        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, true).unwrap().unwrap();
        assert_eq!(p.changed.len(), 2);

        // Touch one source with a newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(dir.path().join("notes/b.md"), "# B\nnew fact\n").unwrap();
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(100);
        fs::File::options().write(true).open(dir.path().join("notes/b.md")).unwrap()
            .set_modified(future).unwrap();
        crate::scan::scan(&project, &mut db).unwrap();

        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, false).unwrap().unwrap();
        assert_eq!(p.changed, vec!["notes/b.md".to_string()]);
        assert!(p.prompt.contains("Only these files changed"));
    }

    #[test]
    fn own_outputs_do_not_trigger_or_feed() {
        let (_d, project, _db, recipe) = setup(Mode::Single, "knowledge/People.md");
        assert!(!triggers(&recipe, &["knowledge/People.md".into()]));
        assert!(triggers(&recipe, &["notes/a.md".into()]));
        assert!(!triggers(&recipe, &["elsewhere/c.md".into()]));
        drop(project);
    }

    #[test]
    fn first_run_applies_without_threshold() {
        let (_d, project, db, recipe) = setup(Mode::Single, "knowledge/People.md");
        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, false).unwrap().unwrap();
        stage(&project, "people", "knowledge/People.md", "# People\n- Priya\n- Marcus\n");
        let outcome = evaluate(&project, &recipe, &DEFAULT_RULES, &p).unwrap();
        assert!(outcome.applied);
        assert!(project.root.join("knowledge/People.md").is_file());
        assert!(!p.staging.exists(), "staging cleaned after apply");
    }

    #[test]
    fn small_change_applies_large_change_holds() {
        let (_d, project, mut db, recipe) = setup(Mode::Single, "knowledge/People.md");
        // Existing output with 20 lines.
        let existing: String = (0..20).map(|i| format!("line {i}\n")).collect();
        fs::create_dir_all(project.root.join("knowledge")).unwrap();
        fs::write(project.root.join("knowledge/People.md"), &existing).unwrap();
        db_seed_success(&mut db);

        // Small change: one line different.
        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, true).unwrap().unwrap();
        let mut small = existing.clone();
        small = small.replace("line 3", "line three");
        stage(&project, "people", "knowledge/People.md", &small);
        let outcome = evaluate(&project, &recipe, &DEFAULT_RULES, &p).unwrap();
        assert!(outcome.applied, "{outcome:?}");

        // Large change: everything different.
        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, true).unwrap().unwrap();
        stage(&project, "people", "knowledge/People.md", "totally new\ncontent\n");
        let outcome = evaluate(&project, &recipe, &DEFAULT_RULES, &p).unwrap();
        assert!(!outcome.applied);
        assert!(outcome.summary.contains("Held for your review"));
        // Output untouched, staging kept.
        assert!(fs::read_to_string(project.root.join("knowledge/People.md"))
            .unwrap()
            .contains("line three"));
        assert!(p.staging.exists());

        // Approval applies it.
        let approved = apply_staged(&project, &recipe, &p.staging).unwrap();
        assert!(approved.applied);
        assert!(fs::read_to_string(project.root.join("knowledge/People.md"))
            .unwrap()
            .contains("totally new"));
    }

    fn db_seed_success(db: &mut Db) {
        // A long-past success so later plans are incremental, not first-run.
        let id = db.insert_run("people", None, 1).unwrap();
        db.update_run(id, "fresh", None, None, None, None).unwrap();
    }

    #[test]
    fn mid_run_human_edit_demotes_to_hold() {
        let (_d, project, mut db, recipe) = setup(Mode::Single, "knowledge/People.md");
        fs::create_dir_all(project.root.join("knowledge")).unwrap();
        fs::write(project.root.join("knowledge/People.md"), "original\n").unwrap();
        db_seed_success(&mut db);

        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, true).unwrap().unwrap();
        // Human edits the output after the plan snapshot…
        let future = std::time::SystemTime::now() + std::time::Duration::from_secs(100);
        fs::write(project.root.join("knowledge/People.md"), "human edit\n").unwrap();
        fs::File::options().write(true).open(project.root.join("knowledge/People.md"))
            .unwrap().set_modified(future).unwrap();
        // …then the agent finishes with a tiny change.
        stage(&project, "people", "knowledge/People.md", "original plus\n");

        let outcome = evaluate(&project, &recipe, &DEFAULT_RULES, &p).unwrap();
        assert!(!outcome.applied);
        assert!(outcome.summary.contains("edited while the run was in flight"));
        assert_eq!(
            fs::read_to_string(project.root.join("knowledge/People.md")).unwrap(),
            "human edit\n"
        );
    }

    #[test]
    fn collection_mode_removals_and_stray_writes() {
        let (_d, project, db, recipe) = setup(Mode::Collection, "people/");
        fs::create_dir_all(project.root.join("people")).unwrap();
        fs::write(project.root.join("people/old-timer.md"), "# Old\n").unwrap();

        let p = plan(&project, &db, &recipe, &DEFAULT_RULES, false).unwrap().unwrap();
        stage(&project, "people", "people/priya.md", "# Priya\n");
        stage(&project, "people", &format!("people/{REMOVED_MANIFEST}"), "old-timer.md\n");
        // Stray write outside the output path must be ignored.
        stage(&project, "people", "etc/evil.md", "nope");

        let outcome = evaluate(&project, &recipe, &DEFAULT_RULES, &p).unwrap();
        assert!(outcome.applied); // first run
        assert!(project.root.join("people/priya.md").is_file());
        assert!(!project.root.join("people/old-timer.md").exists());
        assert!(!project.root.join("etc/evil.md").exists());
        assert_eq!(outcome.files_removed, 1);
    }

    #[test]
    fn discard_removes_staging() {
        let (_d, project, _db, _recipe) = setup(Mode::Single, "knowledge/People.md");
        stage(&project, "people", "knowledge/People.md", "x");
        assert!(staging_dir(&project.root, "people").exists());
        discard_staged(&project, "people").unwrap();
        assert!(!staging_dir(&project.root, "people").exists());
    }
}
