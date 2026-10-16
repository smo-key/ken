//! One git helper for everything that reads a repo's history (knowledge-core
//! decision 27): the live `history` tool, drift, a new knowledge base's repo
//! facts and the person's git identity. Every run is quiet (no console window
//! on Windows, [`crate::proc::quiet`]) and never prompts: a remote that wants
//! a password nobody stored fails instead of hanging.
//!
//! [`log`] is the log reader the history tool uses. [`commits_citing`] lists
//! the ticket and ruling ids a range of commits names, for slice 9's project
//! graph (decision 21) to turn into edges. [`commit_paths`] is the one way
//! Ken commits: named paths only, in the knowledge base only (decision 36).

use std::path::Path;
use std::process::Command;

/// A `git` command in `dir`, quiet and never prompting. The caller adds the
/// arguments and runs it.
pub fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    crate::proc::quiet(&mut cmd)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
    cmd
}

/// Run git in `dir` and return what it printed, untouched. None when git is
/// missing or the command fails.
pub fn output_raw(dir: &Path, args: &[&str]) -> Option<String> {
    let out = command(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run git in `dir` and return what it printed, trimmed. None when git is
/// missing or the command fails; an empty answer is `Some("")`.
pub fn output(dir: &Path, args: &[&str]) -> Option<String> {
    output_raw(dir, args).map(|s| s.trim().to_string())
}

/// Run git in `dir` for its effect. The error is git's own last word, or why
/// it could not run.
pub fn run(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = command(dir).args(args).output().map_err(|e| format!("git could not run: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// A value of the person's global git config (`user.name`), None when git is
/// missing or the key is unset or empty.
pub fn global_config(key: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    let out = crate::proc::quiet(&mut cmd)
        .args(["config", "--global", "--get", key])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !value.is_empty()).then_some(value)
}

/// Every path in the repo at `dir` with work not yet committed: changed,
/// staged, removed or untracked (each file, not its folder). Empty when `dir`
/// is not a git repo. Ken reads it before it writes, so it never commits a
/// person's own work with its own (decision 36).
pub fn uncommitted(dir: &Path) -> Vec<String> {
    let Some(out) = output_raw(dir, &["status", "--porcelain", "-z", "--untracked-files=all"]) else { return Vec::new() };
    let mut paths = Vec::new();
    let mut entries = out.split('\0');
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        // A rename or copy is followed by the path it came from.
        if matches!(entry.as_bytes()[0], b'R' | b'C') {
            if let Some(from) = entries.next() {
                paths.push(from.to_string());
            }
        }
        paths.push(entry[3..].to_string());
    }
    paths
}

/// Commit exactly `paths` (relative to `dir`) with `message`, and nothing
/// else (knowledge-core decision 36): each path that is there is added by
/// name, never `git add -A`; a path that is gone is committed as removed when
/// git knew it; a path the repo ignores is left out. What a person had staged
/// stays staged and out of the commit (`git commit -- paths` commits only
/// those paths). Ken never pushes.
///
/// Returns the new commit's short sha, or None when `dir` is not a git repo
/// or none of the paths changed.
pub fn commit_paths(dir: &Path, paths: &[String], message: &str) -> Result<Option<String>, String> {
    if output(dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Ok(None);
    }
    let mut unique: Vec<&str> = Vec::new();
    for p in paths.iter().map(|p| p.trim_matches('/')).filter(|p| !p.is_empty()) {
        if !unique.contains(&p) {
            unique.push(p);
        }
    }
    if unique.is_empty() {
        return Ok(None);
    }
    // `check-ignore` names the ignored paths and exits 1 when there are none.
    let ignored: Vec<String> = command(dir)
        .arg("check-ignore")
        .arg("--")
        .args(&unique)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().replace('\\', "/")).collect())
        .unwrap_or_default();
    let mut present: Vec<&str> = Vec::new();
    let mut gone: Vec<&str> = Vec::new();
    for p in unique.into_iter().filter(|p| !ignored.iter().any(|i| i == p)) {
        if dir.join(p).exists() {
            present.push(p);
        } else if output(dir, &["ls-files", "--", p]).is_some_and(|s| !s.is_empty()) {
            gone.push(p);
        }
    }
    if !present.is_empty() {
        let mut args = vec!["add", "--"];
        args.extend(&present);
        run(dir, &args)?;
    }
    let all: Vec<&str> = present.into_iter().chain(gone).collect();
    if all.is_empty() {
        return Ok(None);
    }
    let mut status = vec!["status", "--porcelain", "--"];
    status.extend(&all);
    if output(dir, &status).is_none_or(|s| s.is_empty()) {
        return Ok(None);
    }
    let mut args = vec!["commit", "--quiet", "-m", message, "--"];
    args.extend(&all);
    run(dir, &args)?;
    Ok(output(dir, &["rev-parse", "--short", "HEAD"]))
}

/// One commit as [`log`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// The short sha.
    pub sha: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub author: String,
    pub subject: String,
    /// The files it touched, as of then, when the log was asked for them
    /// (`--name-only`); empty otherwise.
    pub files: Vec<String>,
}

/// The fields of one commit in [`log`]'s output: a record separator before
/// each, the fields split by unit separators.
const LOG_FORMAT: &str = "--format=%x1e%h%x1f%ad%x1f%an%x1f%s";

/// The newest `limit` commits in `dir`, within the last `days` when given,
/// narrowed by `extra` (`--follow -- path`, `--grep=words`, `-Swords`, and
/// `--name-only` for each commit's files). None when git is missing, `dir` is
/// not a repo, or git refuses the arguments.
pub fn log(dir: &Path, limit: usize, days: Option<u64>, extra: &[&str]) -> Option<Vec<Commit>> {
    let n = format!("-n{limit}");
    let since = days.map(|d| format!("--since={d}.days.ago"));
    let mut args: Vec<&str> = vec!["log", &n, "--date=short", LOG_FORMAT];
    if let Some(since) = &since {
        args.push(since);
    }
    args.extend_from_slice(extra);
    output_raw(dir, &args).map(|out| parse_log(&out))
}

/// [`log`]'s output as commits; a record that is not whole is skipped.
pub fn parse_log(out: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for record in out.split('\u{1e}').filter(|r| !r.trim().is_empty()) {
        let mut lines = record.lines();
        let head: Vec<&str> = lines.next().unwrap_or("").split('\u{1f}').collect();
        let [sha, date, author, subject] = head[..] else { continue };
        commits.push(Commit {
            sha: sha.to_string(),
            date: date.to_string(),
            author: author.to_string(),
            subject: subject.to_string(),
            files: lines.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect(),
        });
    }
    commits
}

/// The ids a commit message names: ticket ids and ruling ids, both written
/// `PREFIX-number` (`SR-101`, `D-410`), in order, each once. These are
/// candidates: the graph keeps the ones the knowledge base has as a ticket
/// or a ruling (`graph::read_commits`), so a word of the same shape
/// (`UTF-8`) is dropped there.
pub fn cited_ids(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = message.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let starts_word = i == 0 || !chars[i - 1].is_alphanumeric();
        if !(starts_word && chars[i].is_ascii_uppercase()) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_uppercase() || chars[j].is_ascii_digit()) {
            j += 1;
        }
        if j < chars.len() && chars[j] == '-' {
            let mut k = j + 1;
            while k < chars.len() && chars[k].is_ascii_digit() {
                k += 1;
            }
            let ends_word = k == chars.len() || !chars[k].is_alphanumeric();
            if k > j + 1 && ends_word {
                let id: String = chars[i..k].iter().collect();
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            i = k.max(i + 1);
        } else {
            i = j.max(i + 1);
        }
    }
    out
}

/// A commit whose message names ids ([`commits_citing`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citing {
    /// The short sha.
    pub sha: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    /// The message's first line.
    pub subject: String,
    /// The ids it names, in order ([`cited_ids`]).
    pub ids: Vec<String>,
}

/// Each commit in `range` (`old..new`, or one ref for its whole history),
/// at most `limit` of them, whose message names an id, newest first. A
/// commit citing a ticket or a ruling becomes a graph edge (decision 21;
/// `graph::read_commits` keeps the ids the knowledge base has). None when
/// git cannot read the range.
pub fn commits_citing(dir: &Path, range: &str, limit: usize) -> Option<Vec<Citing>> {
    let n = format!("-n{limit}");
    let out = output_raw(dir, &["log", &n, "--date=short", "--format=%x1e%h%x1f%ad%x1f%B", range])?;
    Some(
        out.split('\u{1e}')
            .filter_map(|record| {
                let mut fields = record.splitn(3, '\u{1f}');
                let (sha, date, message) = (fields.next()?, fields.next()?, fields.next()?);
                let ids = cited_ids(message);
                (!ids.is_empty()).then(|| Citing {
                    sha: sha.trim().to_string(),
                    date: date.trim().to_string(),
                    subject: message.trim().lines().next().unwrap_or("").trim().to_string(),
                    ids,
                })
            })
            .collect(),
    )
}

/// A tag, as [`tags`] reads it: a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// `YYYY-MM-DD`: the tag's own date when it is annotated, else its
    /// commit's.
    pub date: String,
    /// The tag's message, else its commit's subject.
    pub subject: String,
}

/// The newest `limit` tags in `dir`, newest first. None when git is missing
/// or `dir` is not a repo.
pub fn tags(dir: &Path, limit: usize) -> Option<Vec<Tag>> {
    let count = format!("--count={limit}");
    let out = output_raw(
        dir,
        &["for-each-ref", "--sort=-creatordate", &count, "--format=%(creatordate:short)%1f%(refname:short)%1f%(subject)", "refs/tags"],
    )?;
    Some(
        out.lines()
            .filter_map(|line| {
                let mut f = line.splitn(3, '\u{1f}');
                let (date, name, subject) = (f.next()?, f.next()?, f.next().unwrap_or(""));
                (!name.trim().is_empty()).then(|| Tag { name: name.trim().into(), date: date.trim().into(), subject: subject.trim().into() })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=Tess", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn a_log_reads_each_commit_with_its_files() {
        let dir = tempfile::tempdir().unwrap();
        git_ok(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("auth.py"), "def check():\n    pass\n").unwrap();
        git_ok(dir.path(), &["add", "auth.py"]);
        git_ok(dir.path(), &["commit", "-q", "-m", "add the token check (SR-101)"]);
        std::fs::write(dir.path().join("auth.py"), "def check():\n    verify_signature()\n").unwrap();
        git_ok(dir.path(), &["commit", "-q", "-am", "harden it per D-410 and SR-101"]);

        let commits = log(dir.path(), 10, None, &["--name-only"]).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "harden it per D-410 and SR-101");
        assert_eq!(commits[0].author, "Tess");
        assert_eq!(commits[0].files, vec!["auth.py".to_string()]);
        assert_eq!(commits[0].date.len(), 10, "YYYY-MM-DD");
        let by_change = log(dir.path(), 10, None, &["-Sverify_signature"]).unwrap();
        assert_eq!(by_change.len(), 1);
        assert!(by_change[0].files.is_empty(), "no --name-only, no files");

        let first = output(dir.path(), &["rev-list", "--max-parents=0", "HEAD"]).unwrap();
        let cited = commits_citing(dir.path(), &format!("{first}..HEAD"), 50).unwrap();
        assert_eq!(cited.len(), 1, "the range leaves the first commit out");
        assert_eq!(cited[0].ids, vec!["D-410".to_string(), "SR-101".to_string()]);
        assert_eq!(cited[0].subject, "harden it per D-410 and SR-101");
        assert_eq!(cited[0].date.len(), 10, "YYYY-MM-DD");

        git_ok(dir.path(), &["tag", "-a", "v1.0", "-m", "First release"]);
        let tagged = tags(dir.path(), 10).unwrap();
        assert_eq!(tagged.len(), 1);
        assert_eq!((tagged[0].name.as_str(), tagged[0].subject.as_str()), ("v1.0", "First release"));
        assert_eq!(tagged[0].date.len(), 10, "YYYY-MM-DD");

        assert_eq!(output(dir.path(), &["no-such-command"]), None);
        assert!(run(dir.path(), &["no-such-command"]).is_err());
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(log(plain.path(), 5, None, &[]), None, "not a repo");
        assert_eq!(tags(plain.path(), 5), None, "not a repo");
    }

    #[test]
    fn a_message_names_ticket_and_ruling_ids() {
        assert_eq!(cited_ids("Fix SR-101: per D-410, see SR-101 again"), vec!["SR-101".to_string(), "D-410".to_string()]);
        assert!(cited_ids("bump x-1 and ABC- and -12").is_empty(), "lowercase, no number, no prefix");
        assert!(cited_ids("XSR-12a").is_empty(), "the number must end the word");
    }

    #[test]
    fn a_commit_holds_the_named_paths_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_ok(root, &["init", "-q"]);
        git_ok(root, &["config", "user.name", "Tess"]);
        git_ok(root, &["config", "user.email", "t@t"]);
        std::fs::create_dir_all(root.join("Raw")).unwrap();
        std::fs::write(root.join("Raw/source.txt"), "said\n").unwrap();
        std::fs::write(root.join("Page.md"), "old\n").unwrap();
        std::fs::write(root.join("Other.md"), "a person's\n").unwrap();
        std::fs::write(root.join(".gitignore"), "cache/\n").unwrap();
        git_ok(root, &["add", "-A"]);
        git_ok(root, &["commit", "-q", "-m", "start"]);

        // An ingest: the source moved, a page changed, a note made. A person's
        // own edit and a staged file of theirs stay out.
        std::fs::create_dir_all(root.join("Ingested")).unwrap();
        std::fs::rename(root.join("Raw/source.txt"), root.join("Ingested/source.txt")).unwrap();
        std::fs::write(root.join("Ingested/note.md"), "# Note\n").unwrap();
        std::fs::write(root.join("Page.md"), "new\n").unwrap();
        std::fs::write(root.join("Other.md"), "edited by a person\n").unwrap();
        std::fs::write(root.join("Staged.md"), "theirs\n").unwrap();
        git_ok(root, &["add", "Staged.md"]);
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/x"), "x").unwrap();
        let paths: Vec<String> =
            ["Ingested/note.md", "Ingested/source.txt", "Raw/source.txt", "Page.md", "cache/x", "Page.md"].iter().map(|s| s.to_string()).collect();
        let sha = commit_paths(root, &paths, "ingest: note\n\nPages changed:\n- Page.md").unwrap().unwrap();
        assert!(!sha.is_empty());

        // Without `--no-renames`, git shows the source's move as its new path alone.
        let files = output(root, &["show", "--name-only", "--no-renames", "--format=", "HEAD"]).unwrap();
        let mut files: Vec<&str> = files.lines().collect();
        files.sort();
        assert_eq!(files, vec!["Ingested/note.md", "Ingested/source.txt", "Page.md", "Raw/source.txt"]);
        assert_eq!(output(root, &["log", "-1", "--format=%s"]).unwrap(), "ingest: note");
        let status = output_raw(root, &["status", "--porcelain"]).unwrap();
        assert!(status.contains(" M Other.md") && status.contains("A  Staged.md"), "a person's work is left as it was: {status}");
        let mut open = uncommitted(root);
        open.sort();
        assert_eq!(open, vec!["Other.md", "Staged.md"], "ignored files are not work");
        std::fs::create_dir_all(root.join("New")).unwrap();
        std::fs::write(root.join("New/page one.md"), "x").unwrap();
        assert!(uncommitted(root).contains(&"New/page one.md".to_string()), "an untracked file, not its folder");

        // Nothing changed: no commit. Not a repo: no commit.
        assert_eq!(commit_paths(root, &paths, "again").unwrap(), None);
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(plain.path().join("a.md"), "x").unwrap();
        assert_eq!(commit_paths(plain.path(), &["a.md".to_string()], "x").unwrap(), None);
    }
}
