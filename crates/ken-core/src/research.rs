//! Deep research: one question in, one cited markdown report out. Composes
//! a research harness prompt and drives it through the hidden-TUI runner —
//! always interactive, because research may need to ask the user something
//! mid-run (the PTY registry + chat drawer make that answerable). The
//! report is written straight into the project as a NEW document; novelty
//! is enforced at plan time, so there is nothing to stage or overwrite.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::hooks::{install_hooks, HookListener};
use crate::project::Project;
use crate::runner::{self, CancelToken, RunOutcome, RunnerConfig, RunnerMode};
use crate::{Error, Result};

/// Research runs long — reading real sources takes real time.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

const SLUG_MAX: usize = 50;

/// Kebab-case filename stem for a research question, capped at roughly
/// [`SLUG_MAX`] characters (cut at a word boundary when there is one).
pub fn slugify(question: &str) -> String {
    let mut out = String::new();
    for c in question.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut slug = out.trim_matches('-').to_string();
    if slug.len() > SLUG_MAX {
        let mut end = SLUG_MAX;
        while !slug.is_char_boundary(end) {
            end -= 1;
        }
        let cut = slug[..end].rfind('-').unwrap_or(end);
        slug.truncate(cut);
    }
    if slug.is_empty() {
        "research".into()
    } else {
        slug
    }
}

/// The chosen output folder must live inside the project and must not be
/// Ken's own `.ken` state dir (or anything under it). It need not exist
/// yet. Returns the folder's absolute path.
pub fn validate_output_dir(project: &Project, rel_dir: &str) -> Result<PathBuf> {
    let rel = rel_dir.trim().trim_end_matches('/').replace('\\', "/");
    let abs = project.resolve(&rel)?;
    if rel == crate::project::CONFIG_DIR || rel.starts_with(".ken/") {
        return Err(Error::Other(
            "reports can't be written into Ken's own .ken folder".into(),
        ));
    }
    Ok(abs)
}

/// First free `<slug>.md`, `<slug>-2.md`, … in the folder — research
/// always creates a new document, never overwrites one.
pub fn unique_report_name(dir_abs: &Path, slug: &str) -> String {
    let mut name = format!("{slug}.md");
    let mut n = 2;
    while dir_abs.join(&name).exists() {
        name = format!("{slug}-{n}.md");
        n += 1;
    }
    name
}

/// Validate the folder and pick a fresh project-relative report path.
pub fn plan_report(project: &Project, rel_dir: &str, question: &str) -> Result<String> {
    let dir_abs = validate_output_dir(project, rel_dir)?;
    let name = unique_report_name(&dir_abs, &slugify(question));
    let rel = rel_dir.trim().trim_end_matches('/');
    Ok(if rel.is_empty() {
        name
    } else {
        format!("{rel}/{name}")
    })
}

/// The research harness prompt. The `OUTPUT_FILE=` line is the machine-
/// checkable contract (mirroring the ingest prompt's `STAGING_DIR=`).
pub fn compose_research_prompt(
    question: &str,
    output_rel_path: &str,
    output_abs: &Path,
) -> String {
    format!(
        r#"You are Ken's research assistant. Research the question below on the web and deliver a cited report.

## Question

{question}

## Method

- Break the question into the distinct angles it contains.
- Run multiple web searches per angle; don't settle for the first results you see.
- Read the strongest sources you find, not just their summaries.
- Cross-check every load-bearing claim across at least two independent sources.
- Where good sources disagree, note the disagreement honestly — don't paper over it.

## Output

Write ONE markdown report to `{output_rel_path}` in this project — the exact absolute path is:

OUTPUT_FILE={output_abs}

It must be a complete document with:

- A title and today's date.
- An executive summary of 3–5 sentences.
- Findings organized by theme.
- A "What remains uncertain" section.
- A "Sources" section listing every URL you used, each with a one-line note on what it supported.
- Inline citation markers like [1] tied to the Sources list.

## Rules

- Prefer primary sources over commentary about them.
- If the web tools are unavailable or the question can't be researched, still write the report file stating exactly that.
- The report file is the deliverable — writing it is mandatory.
- You may ask the user a clarifying question if truly necessary, but prefer stating your assumptions in the report instead.
"#,
        output_abs = output_abs.display(),
    )
}

/// Run one research session to completion. Synchronous — call from a
/// worker thread. Always hidden-TUI (never headless, regardless of the
/// project's `ingestRunner` setting): research must be able to ask the
/// user questions, and the runner's startup-gate check plus `on_blocked`
/// route those to the chat drawer. On Completed the report file must
/// exist; a session that "finished" without writing it is a failure.
#[allow(clippy::too_many_arguments)]
pub fn run_research(
    project: &Project,
    binary: &Path,
    session_id: &str,
    question: &str,
    report_rel_path: &str,
    hooks: &HookListener,
    timeout: Duration,
    cancel: &CancelToken,
    on_blocked: impl FnMut(),
) -> Result<RunOutcome> {
    let report_abs = project.resolve(report_rel_path)?;
    install_hooks(&project.root, &hooks.hook_url())?;
    let cfg = RunnerConfig {
        binary: binary.to_path_buf(),
        mode: RunnerMode::HiddenTui,
        timeout,
    };
    let prompt = compose_research_prompt(question, report_rel_path, &report_abs);
    let outcome = runner::run_session(
        &cfg,
        &project.root,
        session_id,
        &prompt,
        hooks,
        cancel,
        on_blocked,
    )?;
    Ok(match outcome {
        RunOutcome::Completed if !report_abs.is_file() => RunOutcome::Failed(
            "The research session finished but never wrote its report — nothing was saved."
                .into(),
        ),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::write_fake_claude;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn slugify_kebabs_and_caps() {
        assert_eq!(
            slugify("What's the state of EU AI regulation in 2026?"),
            "what-s-the-state-of-eu-ai-regulation-in-2026"
        );
        assert_eq!(slugify("  Hello,   World!  "), "hello-world");
        assert_eq!(slugify("???"), "research");
        assert_eq!(slugify(""), "research");
        let long = slugify(
            "an extremely long question that keeps going and going and going well past any cap",
        );
        assert!(long.len() <= SLUG_MAX, "{long}");
        assert!(!long.ends_with('-'));
        // Caps cut at a word boundary, not mid-word.
        assert!(long.split('-').all(|w| "an extremely long question that keeps going and going and going well past any cap".contains(w)), "{long}");
    }

    #[test]
    fn unique_report_name_suffixes_collisions() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_report_name(dir.path(), "topic"), "topic.md");
        std::fs::write(dir.path().join("topic.md"), "x").unwrap();
        assert_eq!(unique_report_name(dir.path(), "topic"), "topic-2.md");
        std::fs::write(dir.path().join("topic-2.md"), "x").unwrap();
        assert_eq!(unique_report_name(dir.path(), "topic"), "topic-3.md");
    }

    #[test]
    fn plan_report_returns_fresh_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        assert_eq!(
            plan_report(&project, "research", "Solar panels?").unwrap(),
            "research/solar-panels.md"
        );
        std::fs::create_dir_all(dir.path().join("research")).unwrap();
        std::fs::write(dir.path().join("research/solar-panels.md"), "x").unwrap();
        assert_eq!(
            plan_report(&project, "research/", "Solar panels?").unwrap(),
            "research/solar-panels-2.md"
        );
    }

    #[test]
    fn validate_output_dir_rejects_escapes_and_ken() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::create(dir.path(), "T").unwrap();
        assert!(validate_output_dir(&project, "../elsewhere").is_err());
        assert!(validate_output_dir(&project, "/tmp").is_err());
        assert!(validate_output_dir(&project, ".ken").is_err());
        assert!(validate_output_dir(&project, ".ken/reports").is_err());
        // Fine even before the folder exists.
        assert!(validate_output_dir(&project, "research").is_ok());
        assert!(validate_output_dir(&project, "notes/deep").is_ok());
    }

    #[test]
    fn prompt_carries_the_contract() {
        let p = compose_research_prompt(
            "How do heat pumps compare to gas boilers?",
            "research/heat-pumps.md",
            Path::new("/proj/research/heat-pumps.md"),
        );
        assert!(p.contains("research assistant"));
        assert!(p.contains("How do heat pumps compare to gas boilers?"));
        assert!(p.contains("`research/heat-pumps.md`"));
        assert!(p.contains("OUTPUT_FILE=/proj/research/heat-pumps.md"));
        assert!(p.contains("at least two independent sources"));
        assert!(p.contains("executive summary"));
        assert!(p.contains("What remains uncertain"));
        assert!(p.contains("Sources"));
        assert!(p.contains("still write the report file"));
        assert!(p.contains("writing it is mandatory"));
    }

    fn rig(behavior: &str) -> (tempfile::TempDir, tempfile::TempDir, Project, PathBuf, HookListener)
    {
        let project_dir = tempfile::tempdir().unwrap();
        let app_dir = tempfile::tempdir().unwrap();
        let project = Project::create(project_dir.path(), "T").unwrap();
        let bin = write_fake_claude(app_dir.path(), behavior);
        let hooks = HookListener::start().unwrap();
        (project_dir, app_dir, project, bin, hooks)
    }

    #[test]
    fn run_research_writes_report_and_completes() {
        let (_p, _a, project, bin, hooks) = rig("complete");
        let rel = plan_report(&project, "research", "Test question?").unwrap();
        let outcome = run_research(
            &project,
            &bin,
            "sess-research-1",
            "Test question?",
            &rel,
            &hooks,
            Duration::from_secs(30),
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        assert_eq!(outcome, RunOutcome::Completed);
        let report = project.root.join(&rel);
        assert!(report.is_file());
        assert!(std::fs::read_to_string(report).unwrap().contains("Sources"));
    }

    #[test]
    fn run_research_failure_leaves_no_report() {
        let (_p, _a, project, bin, hooks) = rig("fail");
        let rel = plan_report(&project, "research", "Test question?").unwrap();
        let outcome = run_research(
            &project,
            &bin,
            "sess-research-2",
            "Test question?",
            &rel,
            &hooks,
            Duration::from_secs(30),
            &CancelToken::new(),
            || {},
        )
        .unwrap();
        match outcome {
            RunOutcome::Failed(detail) => assert!(detail.contains("boom"), "{detail}"),
            other => panic!("expected failure, got {other:?}"),
        }
        assert!(!project.root.join(&rel).exists());
    }

    #[test]
    fn run_research_blocked_then_cancel() {
        let (_p, _a, project, bin, hooks) = rig("block");
        let rel = plan_report(&project, "research", "Test question?").unwrap();
        let cancel = CancelToken::new();
        let cancel2 = cancel.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let blocked2 = blocked.clone();
        let outcome = run_research(
            &project,
            &bin,
            "sess-research-3",
            "Test question?",
            &rel,
            &hooks,
            Duration::from_secs(30),
            &cancel,
            move || {
                blocked2.store(true, Ordering::SeqCst);
                cancel2.cancel();
            },
        )
        .unwrap();
        assert!(blocked.load(Ordering::SeqCst), "blocked callback should fire");
        assert_eq!(outcome, RunOutcome::Cancelled);
    }
}
