//! Your day (`docs/design/home-whitebox.html`, frames Y1 to Y1c): my tasks
//! and the team's ticket files, read and written the same way by the app
//! and `ken-mcp`.
//!
//! A **task** is one markdown file in a task home: the workspace home
//! (`.ken-workspace/tasks/`, where new tasks go) or my board in a family
//! (where an accepted inbox task lands). Tasks live in your own folder,
//! never inside a team repo, so a repo's `.ken/tasks/` is not read. Writes go
//! through `tasks.rs`'s byte-faithful patch core: only the keys a change
//! names are rewritten, and unknown keys, comments and line endings survive.
//!
//! Frontmatter: `id`, `title`, `status` (`open` | `done`; any other value
//! reads as open), `target` (`YYYY-MM-DD`, falling back to `due`), `repeat`
//! (`daily` | `weekdays` | `weekly:mon`..`weekly:sun` | `monthly:<1-31>`),
//! `links`, `from`, `done_on`, `created`, `updated`, `updated_by`. The body
//! is the description.
//!
//! A recurring task shows on its days only. It is done when `done_on` is
//! today, and marking it done sets `done_on` (its `status` stays open). Its
//! target is the next occurrence, computed, never stored. A recurring task
//! whose `status` is `done` has ended: it reads as done and leaves the list
//! after the day it was ended, like a done one-off.
//!
//! A **ticket** is `tickets/<ID>.md` (or one folder deeper) at the root of a
//! team repo. Ken never writes one. A task links to it by `<repo>/<ID>`
//! (that repo's ticket only) or by a bare `<ID>` (any repo's ticket with
//! that id; older links).
//!
//! Dates are caller-supplied `YYYY-MM-DD` strings (`today`) and timestamps
//! (`now`), the same convention `memory.rs` and `tasks.rs` use: no clock is
//! read here.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

use crate::tasks::{self, HomeKind, TaskHome};
use crate::{Error, Result};

/// `updated_by` for a change made in the app.
pub const BY_YOU: &str = "you";
/// `updated_by` for a change made by Ken's chat.
pub const BY_CHAT: &str = "chat";
/// `updated_by` for a change made through `ken-mcp`.
pub const BY_MCP: &str = "mcp";
/// `updated_by` for a next step an ingest put on my day.
pub const BY_INGEST: &str = "ingest";

/// Where ticket files live in a repo.
pub const TICKETS_DIR: &str = "tickets";

// ---------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------

/// A calendar date. Ordered like the date it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ymd {
    pub y: i64,
    pub m: u32,
    pub d: u32,
}

impl Ymd {
    /// A strict `YYYY-MM-DD` (a longer stamp such as `2026-10-01T09:15` is
    /// read by its first ten characters). `None` for anything else,
    /// including a day the month does not have.
    pub fn parse(s: &str) -> Option<Ymd> {
        let s = s.trim();
        let s = s.get(..10)?;
        let b = s.as_bytes();
        if b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let y: i64 = s[..4].parse().ok()?;
        let m: u32 = s[5..7].parse().ok()?;
        let d: u32 = s[8..10].parse().ok()?;
        if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
            return None;
        }
        Some(Ymd { y, m, d })
    }

    fn days(self) -> i64 {
        days_from_civil(self.y, self.m as i64, self.d as i64)
    }

    fn from_days(z: i64) -> Ymd {
        let (y, m, d) = civil_from_days(z);
        Ymd { y, m: m as u32, d: d as u32 }
    }

    pub fn add_days(self, n: i64) -> Ymd {
        Ymd::from_days(self.days() + n)
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> u32 {
        // 1970-01-01 was a Thursday.
        (self.days() + 3).rem_euclid(7) as u32
    }
}

impl std::fmt::Display for Ymd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

/// True for a strict `YYYY-MM-DD`.
pub fn is_date(s: &str) -> bool {
    s.trim().len() == 10 && Ymd::parse(s).is_some()
}

fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(y) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Howard Hinnant's `days_from_civil` (public domain): days since
/// 1970-01-01 in the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------
// Recurrence
// ---------------------------------------------------------------------

const WEEKDAY_NAMES: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// A task's `repeat`. Anything that does not parse is a one-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Daily,
    /// Monday to Friday.
    Weekdays,
    /// On one weekday, 0 = Monday.
    Weekly(u32),
    /// On one day of the month; a day the month lacks means its last day
    /// (`monthly:31` falls on the 30th in September).
    Monthly(u32),
}

impl Repeat {
    pub fn parse(s: &str) -> Option<Repeat> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "daily" => Some(Repeat::Daily),
            "weekdays" => Some(Repeat::Weekdays),
            _ => {
                if let Some(day) = s.strip_prefix("weekly:") {
                    let day = day.trim();
                    WEEKDAY_NAMES
                        .iter()
                        .position(|w| day.starts_with(w))
                        .map(|i| Repeat::Weekly(i as u32))
                } else if let Some(n) = s.strip_prefix("monthly:") {
                    n.trim().parse::<u32>().ok().filter(|n| (1..=31).contains(n)).map(Repeat::Monthly)
                } else {
                    None
                }
            }
        }
    }

    /// The canonical spelling written to a file.
    pub fn as_string(&self) -> String {
        match self {
            Repeat::Daily => "daily".into(),
            Repeat::Weekdays => "weekdays".into(),
            Repeat::Weekly(w) => format!("weekly:{}", WEEKDAY_NAMES[(*w as usize) % 7]),
            Repeat::Monthly(n) => format!("monthly:{n}"),
        }
    }

    pub fn occurs_on(&self, date: Ymd) -> bool {
        match self {
            Repeat::Daily => true,
            Repeat::Weekdays => date.weekday() < 5,
            Repeat::Weekly(w) => date.weekday() == *w,
            Repeat::Monthly(n) => date.d == (*n).min(days_in_month(date.y, date.m)),
        }
    }

    /// The first day on or after `from` it occurs on.
    pub fn next_on_or_after(&self, from: Ymd) -> Ymd {
        let mut day = from;
        // Every rule recurs within 31 days; 62 is a safe bound.
        for _ in 0..62 {
            if self.occurs_on(day) {
                return day;
            }
            day = day.add_days(1);
        }
        from
    }
}

// ---------------------------------------------------------------------
// Tasks
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DayTaskState {
    Open,
    Done,
}

impl DayTaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            DayTaskState::Open => "open",
            DayTaskState::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(DayTaskState::Open),
            "done" => Some(DayTaskState::Done),
            _ => None,
        }
    }
}

/// A task as Your day reads it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayTask {
    pub id: String,
    pub title: String,
    /// For a recurring task, whether it is done today.
    pub state: DayTaskState,
    /// `YYYY-MM-DD`; the next occurrence (on or after today) for a
    /// recurring task.
    pub target: Option<String>,
    pub repeat: Option<String>,
    pub links: Vec<String>,
    pub from: Option<String>,
    pub description: String,
    pub updated_by: Option<String>,
    pub updated: Option<String>,
    pub created: Option<String>,
    pub done_on: Option<String>,
    /// `status` exactly as the file states it.
    pub status_raw: String,
    /// The file still carries a `due` value (an older task): clearing the
    /// target must clear it too, or the fallback brings it back.
    #[serde(skip)]
    pub has_due: bool,
    pub path: PathBuf,
    pub home: HomeKind,
    /// The `tasks/` directory the file lives in (where it archives to).
    pub home_dir: PathBuf,
}

impl DayTask {
    pub fn recurrence(&self) -> Option<Repeat> {
        self.repeat.as_deref().and_then(Repeat::parse)
    }

    /// The file says `status: done`: for a recurring task, it has ended.
    pub fn status_done(&self) -> bool {
        self.status_raw.eq_ignore_ascii_case("done")
    }

    /// Recurring and not ended: shows on its days, done for a day at a time.
    pub fn recurring(&self) -> Option<Repeat> {
        self.recurrence().filter(|_| !self.status_done())
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Path relative to the member root that owns its home (the tail of
    /// its `ken://` address).
    pub fn address_rel_path(&self) -> String {
        tasks::home_rel_path(self.home, &self.home_dir, &self.file_name())
    }

    /// Does this task link to ticket `id` of repo `repo` (a member name)?
    /// A link `<repo>/<ID>` (or `<repo>/tickets/<ID>.md`) names that repo's
    /// ticket only; a bare `<ID>` (or `tickets/<ID>.md`) names a ticket
    /// with that id in any repo. `repo: None` asks about the id in any
    /// repo. All compared case-insensitively.
    pub fn links_ticket(&self, repo: Option<&str>, id: &str) -> bool {
        let id = id.trim();
        let repo = repo.map(str::trim).filter(|r| !r.is_empty());
        !id.is_empty()
            && self.links.iter().any(|l| match link_ticket(l) {
                Some((link_repo, link_id)) => {
                    link_id.eq_ignore_ascii_case(id)
                        && match (link_repo, repo) {
                            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                            _ => true,
                        }
                }
                None => false,
            })
    }
}

/// Split a ticket reference: `<repo>/<ID>` names one repo's ticket, a
/// bare `<ID>` has no repo.
pub fn split_ticket_ref(s: &str) -> (Option<&str>, &str) {
    let s = s.trim();
    match s.split_once('/') {
        Some((repo, id)) if !repo.trim().is_empty() && !id.contains('/') => (Some(repo.trim()), id.trim()),
        _ => (None, s),
    }
}

/// The ticket a link could name, as `(repo, id)`: `ATT-014`,
/// `att-opmodel/ATT-014`, `tickets/ATT-014.md`, `tickets/q4/ATT-014.md`,
/// `att-opmodel/tickets/ATT-014.md`. A longer path to a ticket file
/// (`ken://<id>/tickets/ATT-014.md`) names the id in any repo. `None` for
/// a link that is no ticket reference at all.
fn link_ticket(link: &str) -> Option<(Option<&str>, &str)> {
    let link = link.trim().trim_end_matches('/');
    if link.is_empty() {
        return None;
    }
    let parts: Vec<&str> = link.split(['/', '\\']).collect();
    let last = parts[parts.len() - 1];
    if let Some(stem) = last.strip_suffix(".md").or_else(|| last.strip_suffix(".MD")) {
        let at = parts.iter().position(|p| p.eq_ignore_ascii_case(TICKETS_DIR))?;
        let depth = parts.len() - at - 1;
        if !(depth == 1 || depth == 2) || stem.is_empty() {
            return None;
        }
        let repo = match at {
            1 if !parts[0].contains(':') && !parts[0].is_empty() => Some(parts[0]),
            _ => None,
        };
        return Some((repo, stem));
    }
    match parts.as_slice() {
        [id] => Some((None, id)),
        [repo, id] if !repo.is_empty() && !id.is_empty() => Some((Some(repo), id)),
        _ => None,
    }
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

fn frontmatter_map(raw: &str) -> (serde_yaml::Mapping, &str) {
    match tasks::split_frontmatter(raw) {
        Some((fm, body)) => (serde_yaml::from_str::<serde_yaml::Mapping>(fm).unwrap_or_default(), body),
        None => (serde_yaml::Mapping::new(), raw),
    }
}

/// Parse a task file. Never fails: a file with no frontmatter or broken
/// YAML still reads as an open task titled by its first line.
pub fn parse_task(path: &Path, home: HomeKind, raw: &str, today: &str) -> DayTask {
    let (map, body) = frontmatter_map(raw);
    let get = |k: &str| tasks::map_str(&map, k);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let description = body.trim().to_string();
    let id = opt(get("id")).unwrap_or(stem);
    let title = opt(get("title")).unwrap_or_else(|| first_line(&description));
    let status_raw = get("status").trim().to_string();
    let due = opt(get("due"));
    let target = opt(get("target")).or_else(|| due.clone());
    let repeat = opt(get("repeat"));
    let done_on = opt(get("done_on"));

    let recurrence = repeat.as_deref().and_then(Repeat::parse);
    let status_done = status_raw.eq_ignore_ascii_case("done");
    let (state, target) = match recurrence {
        // A recurring task with `status: done` has ended: done, and listed
        // like a done one-off.
        Some(_) if status_done => (DayTaskState::Done, target),
        Some(r) => {
            let done = done_on.as_deref().is_some_and(|d| d.get(..10) == today.get(..10));
            let next = Ymd::parse(today).map(|t| r.next_on_or_after(t).to_string());
            (if done { DayTaskState::Done } else { DayTaskState::Open }, next.or(target))
        }
        None => (if status_done { DayTaskState::Done } else { DayTaskState::Open }, target),
    };

    DayTask {
        id,
        title,
        state,
        target,
        repeat,
        links: tasks::map_list(&map, "links"),
        from: opt(get("from")),
        description,
        updated_by: opt(get("updated_by")),
        updated: opt(get("updated")),
        created: opt(get("created")),
        done_on,
        status_raw,
        has_due: due.is_some(),
        path: path.to_path_buf(),
        home,
        home_dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
    }
}

/// Every `.md` file directly in one home, sorted by name. A missing folder
/// is no tasks, and a file that cannot be read is skipped.
pub fn list_home(home: TaskHome, today: &str) -> Vec<DayTask> {
    let dir = home.tasks_dir();
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("md")))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let raw = fs::read_to_string(&p).ok()?;
            Some(parse_task(&p, home.kind(), &raw, today))
        })
        .collect()
}

/// Every task in every home, in the order given; a duplicate id (a file
/// copied between homes) keeps the first.
pub fn scan(homes: &[TaskHome], today: &str) -> Vec<DayTask> {
    let mut out: Vec<DayTask> = Vec::new();
    for home in homes {
        for task in list_home(*home, today) {
            if !out.iter().any(|t| t.id == task.id) {
                out.push(task);
            }
        }
    }
    out
}

pub fn find<'a>(tasks: &'a [DayTask], id: &str) -> Option<&'a DayTask> {
    tasks.iter().find(|t| t.id == id)
}

/// Is this task on today's list? A recurring task, only on its days. A
/// one-off, while it is open, and on the day it was done (until midnight):
/// a done task whose `updated` is before today stays on disk, off the list.
/// An ended recurring task (`status: done`) is listed like a done one-off.
pub fn listed_today(task: &DayTask, today: &str) -> bool {
    if let Some(r) = task.recurring() {
        return Ymd::parse(today).is_some_and(|t| r.occurs_on(t));
    }
    match task.state {
        DayTaskState::Open => true,
        DayTaskState::Done => task
            .updated
            .as_deref()
            .and_then(|u| u.get(..10))
            .zip(today.get(..10))
            .is_some_and(|(u, t)| u >= t),
    }
}

/// Today's order: open tasks by target date, then open ones without one,
/// then the done ones.
pub fn sort_for_day(tasks: &mut [DayTask]) {
    tasks.sort_by(|a, b| {
        let key = |t: &DayTask| {
            (
                t.state == DayTaskState::Done,
                t.target.is_none(),
                t.target.clone().unwrap_or_default(),
                t.created.clone().unwrap_or_default(),
                t.title.to_lowercase(),
                t.id.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
}

/// The list Your day shows: [`listed_today`], in [`sort_for_day`] order.
pub fn today_list(all: Vec<DayTask>, today: &str) -> Vec<DayTask> {
    let mut out: Vec<DayTask> = all.into_iter().filter(|t| listed_today(t, today)).collect();
    sort_for_day(&mut out);
    out
}

/// `task_list`'s filters. `state: None` is every task.
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    pub state: Option<DayTaskState>,
    /// Only tasks with a target strictly before this date.
    pub target_before: Option<String>,
    /// Only tasks linked to this ticket: `<repo>/<ID>` (that repo's
    /// ticket), or a bare `<ID>` (that id in any repo).
    pub linked: Option<String>,
}

pub fn query<'a>(all: &'a [DayTask], q: &TaskQuery) -> Vec<&'a DayTask> {
    let mut out: Vec<&DayTask> = all
        .iter()
        .filter(|t| q.state.is_none_or(|s| t.state == s))
        .filter(|t| match q.target_before.as_deref() {
            Some(before) => t.target.as_deref().is_some_and(|d| d < before),
            None => true,
        })
        .filter(|t| {
            q.linked.as_deref().is_none_or(|r| {
                let (repo, id) = split_ticket_ref(r);
                t.links_ticket(repo, id)
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let key = |t: &DayTask| (t.state == DayTaskState::Done, t.target.is_none(), t.target.clone(), t.title.to_lowercase());
        key(a).cmp(&key(b))
    });
    out
}

/// Who wrote a change, and when: `today` is `YYYY-MM-DD` (for `done_on`),
/// `now` the timestamp written to `created`/`updated` (`YYYY-MM-DDTHH:MM`
/// local, so "updated by chat, Tue 09:15" can be shown).
#[derive(Debug, Clone, Copy)]
pub struct Stamp<'a> {
    pub today: &'a str,
    pub now: &'a str,
    pub by: &'a str,
}

/// A new task (`day_task_create`, `task_create`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DayTaskInput {
    pub title: String,
    pub target: Option<String>,
    pub description: Option<String>,
    pub repeat: Option<String>,
    pub links: Option<Vec<String>>,
}

/// A change to a task. A field left out is left alone; `target` or
/// `repeat` given as `null` (or `""`) clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DayTaskPatch {
    pub title: Option<String>,
    #[serde(deserialize_with = "present")]
    pub target: Option<Option<String>>,
    pub description: Option<String>,
    #[serde(deserialize_with = "present")]
    pub repeat: Option<Option<String>>,
    pub links: Option<Vec<String>>,
    pub state: Option<DayTaskState>,
}

/// A field that is present deserializes to `Some`, even when it is `null`
/// (missing fields fall to the struct's `default`, i.e. `None`).
fn present<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

fn clean_target(t: Option<&str>) -> Result<Option<String>> {
    match t.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) if is_date(t) => Ok(Some(t.to_string())),
        Some(t) => Err(Error::Other(format!("target '{t}' is not a date (YYYY-MM-DD)"))),
    }
}

fn clean_repeat(r: Option<&str>) -> Result<Option<String>> {
    match r.map(str::trim).filter(|r| !r.is_empty() && !r.eq_ignore_ascii_case("never")) {
        None => Ok(None),
        Some(r) => Repeat::parse(r).map(|p| Some(p.as_string())).ok_or_else(|| {
            Error::Other(format!(
                "repeat '{r}' is not one of daily, weekdays, weekly:<mon..sun>, monthly:<1-31>"
            ))
        }),
    }
}

fn clean_links(links: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in links {
        let l = l.trim();
        if !l.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(l)) {
            out.push(l.to_string());
        }
    }
    out
}

/// Write a new task file into the workspace home. `id` is for tests; a
/// fresh ULID otherwise.
pub fn create_task(workspace_root: &Path, input: &DayTaskInput, stamp: &Stamp, id: Option<&str>) -> Result<DayTask> {
    let title = input.title.trim();
    if title.is_empty() {
        return Err(Error::Other("a task needs a title".into()));
    }
    let target = clean_target(input.target.as_deref())?;
    let repeat = clean_repeat(input.repeat.as_deref())?;
    let links = clean_links(input.links.as_deref().unwrap_or(&[]));
    let id = id.map(str::to_string).unwrap_or_else(tasks::new_ulid);

    let dir = tasks::workspace_tasks_dir(workspace_root);
    let path = dir.join(tasks::task_file_name(&id, title));
    if path.exists() {
        return Err(Error::Other(format!("a task file already exists at {}", path.display())));
    }

    let mut edits: Vec<(&str, Vec<String>)> = vec![
        ("id", tasks::scalar_lines("id", &id)),
        ("title", tasks::scalar_lines("title", title)),
        ("status", tasks::scalar_lines("status", DayTaskState::Open.as_str())),
    ];
    if let Some(t) = &target {
        edits.push(("target", tasks::scalar_lines("target", t)));
    }
    if let Some(r) = &repeat {
        edits.push(("repeat", tasks::scalar_lines("repeat", r)));
    }
    if !links.is_empty() {
        edits.push(("links", tasks::seq_lines("links", &links)));
    }
    edits.push(("created", tasks::scalar_lines("created", stamp.now)));
    edits.push(("updated", tasks::scalar_lines("updated", stamp.now)));
    edits.push(("updated_by", tasks::scalar_lines("updated_by", stamp.by)));
    let body = input.description.as_deref().unwrap_or("").trim();
    let text = tasks::patch_text("", &edits, (!body.is_empty()).then_some(body));

    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    fs::write(&path, &text).map_err(|e| Error::io(&path, e))?;
    Ok(parse_task(&path, HomeKind::Workspace, &text, stamp.today))
}

/// Apply a change to a task file: only the keys it names, plus `updated`
/// and `updated_by`, are rewritten; a description replaces the body.
pub fn update_task(task: &DayTask, patch: &DayTaskPatch, stamp: &Stamp) -> Result<()> {
    let mut edits: Vec<(&str, Vec<String>)> = Vec::new();
    if let Some(t) = &patch.title {
        let t = t.trim();
        if t.is_empty() {
            return Err(Error::Other("a task needs a title".into()));
        }
        edits.push(("title", tasks::scalar_lines("title", t)));
    }
    if let Some(t) = &patch.target {
        let t = clean_target(t.as_deref())?;
        edits.push(("target", tasks::scalar_lines("target", t.as_deref().unwrap_or(""))));
        if task.has_due {
            edits.push(("due", tasks::scalar_lines("due", "")));
        }
    }
    // Recurring and not ended. Setting a repeat (re)starts the rule, so
    // the status goes back to open.
    let mut recurring = task.recurring().is_some();
    if let Some(r) = &patch.repeat {
        let r = clean_repeat(r.as_deref())?;
        edits.push(("repeat", tasks::scalar_lines("repeat", r.as_deref().unwrap_or(""))));
        recurring = r.is_some();
        if recurring {
            edits.push(("status", tasks::scalar_lines("status", DayTaskState::Open.as_str())));
        }
    }
    if let Some(l) = &patch.links {
        edits.push(("links", tasks::seq_lines("links", &clean_links(l))));
    }
    if let Some(s) = patch.state {
        if recurring {
            // Done for today only; `status` stays open.
            let on = if s == DayTaskState::Done { stamp.today } else { "" };
            edits.push(("done_on", tasks::scalar_lines("done_on", on)));
        } else {
            edits.push(("status", tasks::scalar_lines("status", s.as_str())));
            if s == DayTaskState::Open && task.recurrence().is_some() && task.done_on.is_some() {
                // Reopening an ended recurring task: not done today either.
                edits.push(("done_on", tasks::scalar_lines("done_on", "")));
            }
        }
    }
    edits.push(("updated", tasks::scalar_lines("updated", stamp.now)));
    edits.push(("updated_by", tasks::scalar_lines("updated_by", stamp.by)));
    let description = patch.description.clone();
    tasks::apply_text_change(&task.path, &|raw: &str| {
        let next = tasks::patch_text(raw, &edits, None);
        match &description {
            Some(d) => tasks::replace_body(&next, d),
            None => next,
        }
    })
}

/// Delete a task: the file moves into its home's `archive/YYYY-MM/`.
pub fn archive_task(task: &DayTask, today: &str) -> Result<PathBuf> {
    tasks::archive_file(&task.path, &task.home_dir, today)
}

/// The one-line journal entry for a task marked done (memory is built in):
/// the title and the task's `ken://` address.
pub fn journal_line(task: &DayTask, host: &str) -> String {
    format!("Completed task \"{}\" (ken://{host}/{})", task.title, task.address_rel_path())
}

// ---------------------------------------------------------------------
// Tickets
// ---------------------------------------------------------------------

/// A ticket file, read-only.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub id: String,
    pub title: String,
    /// `status` as the file states it (free text; may be empty).
    pub state: String,
    /// Open unless the status is `done` or `cancelled`.
    pub open: bool,
    pub assignees: Vec<String>,
    pub target: Option<String>,
    /// Inside its repo, with forward slashes.
    pub rel_path: String,
}

/// Parse a ticket file. `id` falls back to the file stem, `title` to the
/// first `# ` heading and then the id, `target` to `due`.
pub fn parse_ticket(rel_path: &str, raw: &str) -> Ticket {
    let (map, body) = frontmatter_map(raw);
    let get = |k: &str| tasks::map_str(&map, k);
    let rel = rel_path.replace('\\', "/");
    let stem = rel
        .rsplit('/')
        .next()
        .map(|n| n.strip_suffix(".md").or_else(|| n.strip_suffix(".MD")).unwrap_or(n))
        .unwrap_or("")
        .to_string();
    let id = opt(get("id")).unwrap_or(stem);
    let title = opt(get("title"))
        .or_else(|| body.lines().find_map(|l| l.trim().strip_prefix("# ").map(|t| t.trim().to_string())))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.clone());
    let state = get("status").trim().to_string();
    let closed = ["done", "cancelled", "canceled"].iter().any(|s| state.eq_ignore_ascii_case(s));
    Ticket {
        id,
        title,
        open: !closed,
        state,
        // Only a YAML sequence is several assignees: `Staud, Chris` is one.
        assignees: tasks::map_values(&map, "assignee"),
        target: opt(get("target")).or_else(|| opt(get("due"))),
        rel_path: rel,
    }
}

/// Files in `tickets/` that are about the folder, not a ticket.
fn is_ticket_file_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(stem) = lower.strip_suffix(".md") else { return false };
    !matches!(stem, "readme" | "index" | "template" | "_template" | "rule" | "rules")
}

/// Is `rel_path` (inside a repo) a ticket file: `tickets/<ID>.md` or
/// `tickets/<folder>/<ID>.md`?
pub fn is_ticket_path(rel_path: &str) -> bool {
    let rel = rel_path.replace('\\', "/");
    let parts: Vec<&str> = rel.split('/').collect();
    (parts.len() == 2 || parts.len() == 3) && parts[0] == TICKETS_DIR && is_ticket_file_name(parts[parts.len() - 1])
}

/// Every ticket file in a repo: `tickets/*.md` and one folder deeper,
/// sorted by id.
pub fn scan_tickets(repo_root: &Path) -> Vec<Ticket> {
    let mut out = Vec::new();
    let top = repo_root.join(TICKETS_DIR);
    let Ok(entries) = fs::read_dir(&top) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            let Ok(inner) = fs::read_dir(&path) else { continue };
            for e in inner.flatten() {
                let p = e.path();
                let n = e.file_name().to_string_lossy().into_owned();
                if p.is_file() && is_ticket_file_name(&n) {
                    if let Ok(raw) = fs::read_to_string(&p) {
                        out.push(parse_ticket(&format!("{TICKETS_DIR}/{name}/{n}"), &raw));
                    }
                }
            }
        } else if path.is_file() && is_ticket_file_name(&name) {
            if let Ok(raw) = fs::read_to_string(&path) {
                out.push(parse_ticket(&format!("{TICKETS_DIR}/{name}"), &raw));
            }
        }
    }
    out.sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()).then(a.rel_path.cmp(&b.rel_path)));
    out
}

/// Tickets in today's order: by target date, then those without one, then
/// by id.
pub fn ticket_order(a: &Ticket, b: &Ticket) -> std::cmp::Ordering {
    (a.target.is_none(), &a.target, a.id.to_lowercase()).cmp(&(b.target.is_none(), &b.target, b.id.to_lowercase()))
}

/// How many tasks link to ticket `id` of repo `repo` (see
/// [`DayTask::links_ticket`]), and how many of those are done.
pub fn linked_counts(tasks: &[DayTask], repo: Option<&str>, id: &str) -> (usize, usize) {
    let linked: Vec<&DayTask> = tasks.iter().filter(|t| t.links_ticket(repo, id)).collect();
    let done = linked.iter().filter(|t| t.state == DayTaskState::Done).count();
    (linked.len(), done)
}

// ---------------------------------------------------------------------
// Escalations
// ---------------------------------------------------------------------

/// A team repo's folder of questions only a named person can answer.
pub const ESCALATIONS_DIR: &str = "escalations";

/// An escalation file (`escalations/E-nnn.md`), read-only.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Escalation {
    pub id: String,
    /// The question: `title`, else the first `# ` heading, else the id.
    pub title: String,
    pub raised_by: Option<String>,
    /// Who it is for (`to:`; a sequence is several people).
    pub to: Vec<String>,
    pub status: String,
    /// Open unless the status says it was answered or closed.
    pub open: bool,
    pub ticket: Option<String>,
    pub raised: Option<String>,
    pub blocks: Option<String>,
    /// Inside its repo, with forward slashes.
    pub rel_path: String,
}

/// Parse an escalation file. `id` falls back to the file stem.
pub fn parse_escalation(rel_path: &str, raw: &str) -> Escalation {
    let (map, body) = frontmatter_map(raw);
    let get = |k: &str| tasks::map_str(&map, k);
    let rel = rel_path.replace('\\', "/");
    let stem = rel.rsplit('/').next().map(|n| n.strip_suffix(".md").unwrap_or(n)).unwrap_or("").to_string();
    let id = opt(get("id")).unwrap_or(stem);
    let title = opt(get("title"))
        .or_else(|| body.lines().find_map(|l| l.trim().strip_prefix("# ").map(|t| t.trim().to_string())))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.clone());
    let status = get("status").trim().to_string();
    let closed = ["answered", "closed", "resolved", "done", "cancelled", "canceled", "withdrawn"]
        .iter()
        .any(|s| status.eq_ignore_ascii_case(s));
    Escalation {
        id,
        title,
        raised_by: opt(get("raised_by")),
        to: tasks::map_values(&map, "to"),
        open: !closed,
        status,
        ticket: opt(get("ticket")),
        raised: opt(get("raised")),
        blocks: opt(get("blocks")).filter(|b| !b.eq_ignore_ascii_case("nothing")),
        rel_path: rel,
    }
}

/// Every escalation file in a repo's `escalations/`, sorted by id. The
/// folder's own template and index are left out.
/// The statuses a ticket moves through (the method's ticket template).
pub const TICKET_STATUSES: [&str; 7] = ["todo", "in-progress", "blocked", "in-review", "testing", "done", "cancelled"];

/// `text` with frontmatter `key` set to `value`: the line replaced where it
/// is, or added at the end of the frontmatter. A file with no frontmatter
/// gets one. Line endings are kept.
pub fn set_frontmatter(text: &str, key: &str, value: &str) -> String {
    let crlf = text.contains("\r\n");
    let t = text.replace("\r\n", "\n");
    let mut lines: Vec<String> = t.lines().map(str::to_string).collect();
    let line = format!("{key}: {value}");
    let head_end = if lines.first().is_some_and(|l| l.trim() == "---") {
        lines.iter().skip(1).position(|l| l.trim() == "---").map(|i| i + 1)
    } else {
        None
    };
    match head_end {
        Some(end) => {
            let prefix = format!("{key}:");
            match lines[1..end].iter().position(|l| l.trim_start().starts_with(&prefix)) {
                Some(i) => {
                    // Keep a trailing `# comment` the template carries.
                    let old = &lines[i + 1];
                    let comment = old
                        .find(" #")
                        .map(|h| old[old[..h + 1].trim_end().len()..].to_string())
                        .unwrap_or_default();
                    lines[i + 1] = format!("{line}{comment}");
                }
                None => lines.insert(end, line),
            }
        }
        None => {
            lines.insert(0, "---".into());
            lines.insert(1, line);
            lines.insert(2, "---".into());
        }
    }
    let out = lines.join("\n") + "\n";
    if crlf {
        out.replace('\n', "\r\n")
    } else {
        out
    }
}

/// An escalation's text with a reply added at the end of its `## Thread`
/// (the section is made when it has none), in the method's form:
/// `- **Name, date:** words`. `resolve` also sets `status: resolved` and a
/// `resolved:` date in the frontmatter, and marks the entry as the answer.
pub fn escalation_reply(text: &str, who: &str, today: &str, reply: &str, resolve: bool) -> String {
    let crlf = text.contains("\r\n");
    let text = text.replace("\r\n", "\n");
    let words = reply.split_whitespace().collect::<Vec<_>>().join(" ");
    let entry = if resolve {
        format!("- **{who}, {today}, resolved:** {words}")
    } else {
        format!("- **{who}, {today}:** {words}")
    };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();

    if resolve {
        // The frontmatter: between the first two `---` lines.
        if lines.first().is_some_and(|l| l.trim() == "---") {
            if let Some(end) = lines.iter().skip(1).position(|l| l.trim() == "---").map(|i| i + 1) {
                match lines[1..end].iter().position(|l| l.trim_start().starts_with("status:")) {
                    Some(i) => lines[i + 1] = "status: resolved".into(),
                    None => lines.insert(end, "status: resolved".into()),
                }
                let end = lines.iter().skip(1).position(|l| l.trim() == "---").map(|i| i + 1).unwrap_or(end);
                if !lines[1..end].iter().any(|l| l.trim_start().starts_with("resolved:")) {
                    lines.insert(end, format!("resolved: {today}"));
                }
            }
        }
    }

    let thread = lines.iter().position(|l| l.trim().eq_ignore_ascii_case("## thread"));
    match thread {
        Some(at) => {
            // The end of the section: the next heading of its level or above.
            let mut end = lines[at + 1..]
                .iter()
                .position(|l| l.starts_with("# ") || l.starts_with("## "))
                .map(|i| at + 1 + i)
                .unwrap_or(lines.len());
            while end > at + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            lines.insert(end, entry);
        }
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(String::new());
            lines.push("## Thread".into());
            lines.push(String::new());
            lines.push(entry);
        }
    }
    let out = lines.join("\n") + "\n";
    if crlf {
        out.replace('\n', "\r\n")
    } else {
        out
    }
}

pub fn scan_escalations(repo_root: &Path) -> Vec<Escalation> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(repo_root.join(ESCALATIONS_DIR)) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let Some(stem) = lower.strip_suffix(".md") else { continue };
        if !path.is_file() || matches!(stem, "readme" | "index" | "escalation" | "template" | "_template") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            out.push(parse_escalation(&format!("{ESCALATIONS_DIR}/{name}"), &raw));
        }
    }
    out.sort_by(|a, b| a.id.to_lowercase().cmp(&b.id.to_lowercase()).then(a.rel_path.cmp(&b.rel_path)));
    out
}

/// Who "me" is on this machine: git's global `user.name` and `user.email`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Me {
    pub name: Option<String>,
    pub email: Option<String>,
}

impl Me {
    pub fn is_known(&self) -> bool {
        self.name.is_some() || self.email.is_some()
    }

    /// Does one assignee value name me? Case-insensitive against my name,
    /// my email, or my email's local part; a leading `@` is ignored. A
    /// `Name <email>` value matches on either half.
    pub fn matches(&self, assignee: &str) -> bool {
        let a = assignee.trim();
        if let Some((name, rest)) = a.split_once('<') {
            if let Some(email) = rest.trim().strip_suffix('>') {
                let name = name.trim().trim_matches('"').trim();
                return (!name.is_empty() && self.matches_one(name)) || self.matches_one(email);
            }
        }
        // `Staud, Chris` is one person, written last name first.
        if let Some((last, first)) = a.split_once(',') {
            if !first.contains(',') && self.matches_one(&format!("{} {}", first.trim(), last.trim())) {
                return true;
            }
        }
        self.matches_one(a)
    }

    fn matches_one(&self, assignee: &str) -> bool {
        let a = assignee.trim().trim_start_matches('@').trim().to_lowercase();
        if a.is_empty() {
            return false;
        }
        if self.name.as_deref().is_some_and(|n| n.trim().to_lowercase() == a) {
            return true;
        }
        if let Some(email) = self.email.as_deref() {
            let email = email.trim().to_lowercase();
            if email == a {
                return true;
            }
            if let Some((local, _)) = email.split_once('@') {
                if !local.is_empty() && local == a {
                    return true;
                }
            }
        }
        false
    }

    pub fn assigned(&self, ticket: &Ticket) -> bool {
        ticket.assignees.iter().any(|a| self.matches(a))
    }

    /// Whether an escalation is for me: a `to:` value names me, or my first
    /// name alone, as an ingest writes who a question is for.
    pub fn addressed(&self, e: &Escalation) -> bool {
        let first = self.name.as_deref().and_then(|n| n.split_whitespace().next());
        e.to.iter().any(|t| {
            let t = t.trim().trim_start_matches('@').trim();
            self.matches(t) || (!t.contains(' ') && first.is_some_and(|f| f.eq_ignore_ascii_case(t)))
        })
    }
}

/// Read git's global identity. Missing git or unset keys read as `None`.
pub fn git_me() -> Me {
    let read = |key: &str| -> Option<String> {
        let mut cmd = std::process::Command::new("git");
        let out = crate::proc::quiet(&mut cmd)
            .args(["config", "--global", "--get", key])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        opt(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    Me { name: read("user.name"), email: read("user.email") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const E001: &str = "---\nid: E-001\nraised_by: Dana Reyes\nto: Chris Staud\nstatus: open\n---\n\n# Path or source?\n\n## Thread\n\n- **Dana Reyes, 2026-10-01:** Raising this.\n\n## Notes\n\nx\n";

    #[test]
    fn a_ticket_status_is_set_in_its_frontmatter_and_keeps_its_comment() {
        let t = "---\nid: AUR-101\nstatus: todo          # todo | in-progress\nassignee: Chris\n---\n# T\n";
        let got = set_frontmatter(t, "status", "in-progress");
        assert_eq!(got, "---\nid: AUR-101\nstatus: in-progress          # todo | in-progress\nassignee: Chris\n---\n# T\n");
        assert!(set_frontmatter("---\nid: X\n---\n", "status", "done").contains("id: X\nstatus: done\n---"));
        assert_eq!(set_frontmatter("# T\r\n", "status", "done"), "---\r\nstatus: done\r\n---\r\n# T\r\n");
    }

    #[test]
    fn a_reply_goes_at_the_end_of_the_thread() {
        let t = escalation_reply(E001, "Chris Staud", "2026-10-02", "Looking at it  today.", false);
        assert!(t.contains("- **Dana Reyes, 2026-10-01:** Raising this.\n- **Chris Staud, 2026-10-02:** Looking at it today.\n\n## Notes"));
        assert!(t.contains("status: open"));
        let e = parse_escalation("escalations/E-001.md", &t);
        assert!(e.open);
    }

    #[test]
    fn resolving_closes_it_and_marks_the_answer() {
        let t = escalation_reply(E001, "Chris Staud", "2026-10-02", "Path only.", true);
        assert!(t.contains("status: resolved\nresolved: 2026-10-02\n---"));
        assert!(t.contains("- **Chris Staud, 2026-10-02, resolved:** Path only."));
        assert!(!parse_escalation("escalations/E-001.md", &t).open);
    }

    #[test]
    fn a_file_with_no_thread_gets_one_and_keeps_crlf() {
        let t = escalation_reply("---\r\nstatus: open\r\n---\r\n# Q\r\n", "Chris", "2026-10-02", "Yes.", false);
        assert!(t.ends_with("# Q\r\n\r\n## Thread\r\n\r\n- **Chris, 2026-10-02:** Yes.\r\n"), "{t:?}");
    }

    const TODAY: &str = "2026-10-01"; // a Thursday

    fn d(s: &str) -> Ymd {
        Ymd::parse(s).unwrap()
    }

    fn task(raw: &str) -> DayTask {
        parse_task(Path::new("/w/.ken-workspace/tasks/t1-x.md"), HomeKind::Workspace, raw, TODAY)
    }

    fn stamp(by: &str) -> Stamp<'_> {
        Stamp { today: TODAY, now: "2026-10-01T09:15", by }
    }

    #[test]
    fn dates_parse_and_count() {
        assert_eq!(d("2026-10-01").weekday(), 3, "a Thursday");
        assert_eq!(d("1970-01-01").weekday(), 3);
        assert_eq!(d("2026-02-28").add_days(1).to_string(), "2026-03-01");
        assert_eq!(d("2024-02-28").add_days(1).to_string(), "2024-02-29");
        assert_eq!(d("2026-12-31").add_days(1).to_string(), "2027-01-01");
        assert!(Ymd::parse("2026-02-30").is_none());
        assert!(Ymd::parse("2026-13-01").is_none());
        assert!(Ymd::parse("soon").is_none());
        assert!(is_date("2026-10-01"));
        assert!(!is_date("2026-10-01T09:00"));
        assert_eq!(d("2026-10-01T09:15").to_string(), "2026-10-01", "a stamp reads by its date");
    }

    #[test]
    fn status_maps_to_open_or_done() {
        for (status, want) in [
            ("open", DayTaskState::Open),
            ("done", DayTaskState::Done),
            ("Done", DayTaskState::Done),
            ("backlog", DayTaskState::Open),
            ("todo", DayTaskState::Open),
            ("doing", DayTaskState::Open),
            ("review", DayTaskState::Open),
            ("", DayTaskState::Open),
        ] {
            let t = task(&format!("---\nid: t1\ntitle: T\nstatus: {status}\n---\n\nbody\n"));
            assert_eq!(t.state, want, "status {status:?}");
            assert_eq!(t.status_raw, status);
        }
    }

    #[test]
    fn target_falls_back_to_due() {
        let t = task("---\nid: t1\ntitle: T\ndue: '2026-10-03'\n---\n");
        assert_eq!(t.target.as_deref(), Some("2026-10-03"));
        assert!(t.has_due);
        let t = task("---\nid: t1\ntitle: T\ntarget: 2026-10-05\ndue: '2026-10-03'\n---\n");
        assert_eq!(t.target.as_deref(), Some("2026-10-05"), "target wins");
        let t = task("---\nid: t1\ntitle: T\n---\n");
        assert_eq!(t.target, None);
    }

    #[test]
    fn hand_written_file_without_frontmatter() {
        let t = parse_task(Path::new("/w/tasks/note.md"), HomeKind::Workspace, "# Ask sam\n\nwhich sites\n", TODAY);
        assert_eq!(t.id, "note");
        assert_eq!(t.title, "Ask sam");
        assert_eq!(t.state, DayTaskState::Open);
        assert!(t.description.contains("which sites"));
    }

    #[test]
    fn repeat_parses_and_normalizes() {
        assert_eq!(Repeat::parse("daily"), Some(Repeat::Daily));
        assert_eq!(Repeat::parse("Weekdays"), Some(Repeat::Weekdays));
        assert_eq!(Repeat::parse("weekly:tue"), Some(Repeat::Weekly(1)));
        assert_eq!(Repeat::parse("weekly:Sunday"), Some(Repeat::Weekly(6)));
        assert_eq!(Repeat::parse("monthly:31"), Some(Repeat::Monthly(31)));
        assert_eq!(Repeat::parse("monthly:0"), None);
        assert_eq!(Repeat::parse("monthly:32"), None);
        assert_eq!(Repeat::parse("weekly:someday"), None);
        assert_eq!(Repeat::parse("yearly"), None);
        assert_eq!(Repeat::Weekly(4).as_string(), "weekly:fri");
    }

    #[test]
    fn recurrence_days() {
        // 2026-10-03 is a Saturday.
        assert!(Repeat::Daily.occurs_on(d("2026-10-03")));
        assert!(Repeat::Weekdays.occurs_on(d("2026-10-02")));
        assert!(!Repeat::Weekdays.occurs_on(d("2026-10-03")));
        assert!(!Repeat::Weekdays.occurs_on(d("2026-10-04")));
        assert_eq!(Repeat::Weekdays.next_on_or_after(d("2026-10-03")).to_string(), "2026-10-05");
        assert!(Repeat::Weekly(3).occurs_on(d(TODAY)));
        assert_eq!(Repeat::Weekly(1).next_on_or_after(d(TODAY)).to_string(), "2026-10-06");
        assert_eq!(Repeat::Monthly(15).next_on_or_after(d(TODAY)).to_string(), "2026-10-15");
        assert_eq!(Repeat::Monthly(1).next_on_or_after(d("2026-10-02")).to_string(), "2026-11-01");
    }

    #[test]
    fn monthly_31st_falls_on_the_last_day_of_a_short_month() {
        let r = Repeat::Monthly(31);
        assert!(r.occurs_on(d("2026-09-30")), "September has 30 days");
        assert!(!r.occurs_on(d("2026-09-29")));
        assert!(r.occurs_on(d("2026-10-31")));
        assert!(!r.occurs_on(d("2026-10-30")), "October has its 31st");
        assert!(r.occurs_on(d("2026-02-28")));
        assert!(Repeat::Monthly(30).occurs_on(d("2024-02-29")), "leap February");
        assert_eq!(r.next_on_or_after(d("2026-09-01")).to_string(), "2026-09-30");
    }

    #[test]
    fn recurring_state_comes_from_done_on_and_target_is_next_occurrence() {
        let raw = |done_on: &str| {
            format!("---\nid: r1\ntitle: Wiki pass\nstatus: open\nrepeat: weekly:tue\ntarget: '2020-01-01'\ndone_on: '{done_on}'\n---\n")
        };
        let t = parse_task(Path::new("/w/tasks/r1.md"), HomeKind::Workspace, &raw("2026-09-29"), "2026-09-29");
        assert_eq!(t.state, DayTaskState::Done, "done on its day");
        assert_eq!(t.target.as_deref(), Some("2026-09-29"));
        let next_week = parse_task(Path::new("/w/tasks/r1.md"), HomeKind::Workspace, &raw("2026-09-29"), "2026-10-06");
        assert_eq!(next_week.state, DayTaskState::Open, "open again the next week");
        assert_eq!(next_week.target.as_deref(), Some("2026-10-06"));
        // Not due today (a Thursday): computed target is next Tuesday, and it is not listed.
        let t = task(&raw(""));
        assert_eq!(t.target.as_deref(), Some("2026-10-06"), "stored target is ignored");
        assert!(!listed_today(&t, TODAY));
        let daily = task("---\nid: r2\ntitle: Inbox zero\nrepeat: daily\n---\n");
        assert!(listed_today(&daily, TODAY));
        assert_eq!(daily.target.as_deref(), Some(TODAY));
    }

    #[test]
    fn done_yesterday_is_hidden_done_today_is_shown() {
        let done = |updated: &str| task(&format!("---\nid: t1\ntitle: T\nstatus: done\nupdated: '{updated}'\n---\n"));
        assert!(listed_today(&done("2026-10-01T08:00"), TODAY));
        assert!(listed_today(&done("2026-10-01"), TODAY));
        assert!(!listed_today(&done("2026-09-30T23:59"), TODAY));
        assert!(!listed_today(&task("---\nid: t1\ntitle: T\nstatus: done\n---\n"), TODAY), "no date: not today");
        assert!(listed_today(&task("---\nid: t1\ntitle: T\nstatus: open\nupdated: '2020-01-01'\n---\n"), TODAY));
    }

    #[test]
    fn day_order_is_target_then_none_then_done() {
        let mk = |id: &str, extra: &str| {
            parse_task(Path::new(&format!("/w/tasks/{id}.md")), HomeKind::Workspace, &format!("---\nid: {id}\ntitle: {id}\n{extra}---\n"), TODAY)
        };
        let all = vec![
            mk("none", ""),
            mk("done", "status: done\nupdated: '2026-10-01T09:40'\n"),
            mk("late", "target: '2026-10-14'\n"),
            mk("soon", "target: '2026-10-02'\n"),
            mk("old", "status: done\nupdated: '2026-09-01'\n"),
        ];
        let ids: Vec<String> = today_list(all, TODAY).into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["soon", "late", "none", "done"]);
    }

    #[test]
    fn create_writes_the_your_day_keys() {
        let dir = tempdir().unwrap();
        let t = create_task(
            dir.path(),
            &DayTaskInput {
                title: "Update the retry test".into(),
                target: Some("2026-10-01".into()),
                description: Some("Assert the delays.".into()),
                repeat: None,
                links: Some(vec!["ATT-014".into(), " ".into(), "att-014".into(), "att-opmodel/src/retry.test.ts".into()]),
            },
            &stamp(BY_YOU),
            Some("01J0DAY"),
        )
        .unwrap();
        assert!(t.path.ends_with(".ken-workspace/tasks/01J0DAY-update-the-retry-test.md"));
        let raw = fs::read_to_string(&t.path).unwrap();
        assert!(raw.starts_with("---\nid: '01J0DAY'\ntitle: Update the retry test\nstatus: open\ntarget: '2026-10-01'\n"), "{raw}");
        assert!(raw.contains("links:\n  - ATT-014\n  - att-opmodel/src/retry.test.ts\n"), "{raw}");
        assert!(raw.contains("updated_by: you\n"));
        assert!(raw.ends_with("---\n\nAssert the delays.\n"), "{raw:?}");
        assert_eq!(t.links, vec!["ATT-014", "att-opmodel/src/retry.test.ts"]);
        assert_eq!(t.updated_by.as_deref(), Some("you"));
        assert_eq!(t.created.as_deref(), Some("2026-10-01T09:15"));
        assert_eq!(t.state, DayTaskState::Open);

        let bad = DayTaskInput { title: "x".into(), target: Some("Friday".into()), ..Default::default() };
        assert!(create_task(dir.path(), &bad, &stamp(BY_YOU), Some("01J0BAD")).is_err());
        let bad = DayTaskInput { title: "x".into(), repeat: Some("yearly".into()), ..Default::default() };
        assert!(create_task(dir.path(), &bad, &stamp(BY_YOU), Some("01J0BAD")).is_err());
        assert!(create_task(dir.path(), &DayTaskInput::default(), &stamp(BY_YOU), None).is_err(), "no title");
    }

    fn reread(t: &DayTask) -> DayTask {
        parse_task(&t.path, t.home, &fs::read_to_string(&t.path).unwrap(), TODAY)
    }

    #[test]
    fn unknown_keys_and_comments_survive_an_update() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t1.md");
        let raw = "---\r\n# kept\r\nid: t1\r\ntitle: Old\r\nstatus: doing\r\nkind: ai\r\nseverity: high\r\ndue: '2026-10-09'\r\n---\r\n\r\nOld body.\r\n";
        fs::write(&path, raw).unwrap();
        let t = reread(&parse_task(&path, HomeKind::Workspace, raw, TODAY));
        update_task(
            &t,
            &DayTaskPatch {
                title: Some("New".into()),
                target: Some(None),
                state: Some(DayTaskState::Done),
                description: Some("New body.\nTwo lines.".into()),
                ..Default::default()
            },
            &stamp(BY_MCP),
        )
        .unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("# kept\r\n"));
        assert!(after.contains("kind: ai\r\n"));
        assert!(after.contains("severity: high\r\n"));
        assert!(after.contains("title: New\r\n"));
        assert!(after.contains("status: done\r\n"));
        assert!(after.contains("due: ''\r\n"), "clearing the target clears due too: {after}");
        assert!(after.ends_with("---\r\n\r\nNew body.\r\nTwo lines.\r\n"), "{after:?}");
        assert_eq!(after.matches('\n').count(), after.matches("\r\n").count(), "CRLF kept");
        let t = reread(&t);
        assert_eq!(t.target, None);
        assert_eq!(t.state, DayTaskState::Done);
        assert_eq!(t.updated_by.as_deref(), Some("mcp"));
        assert_eq!(t.description, "New body.\r\nTwo lines.");

        // Leaving a field out leaves it alone.
        update_task(&t, &DayTaskPatch { links: Some(vec!["ATT-1".into()]), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        let t2 = reread(&t);
        assert_eq!(t2.title, "New");
        assert_eq!(t2.links, vec!["ATT-1"]);
        assert!(fs::read_to_string(&path).unwrap().contains("New body."));
    }

    #[test]
    fn marking_a_recurring_task_done_sets_done_on_only() {
        let dir = tempdir().unwrap();
        let t = create_task(
            dir.path(),
            &DayTaskInput { title: "Inbox zero".into(), repeat: Some("daily".into()), ..Default::default() },
            &stamp(BY_YOU),
            Some("01J0REP"),
        )
        .unwrap();
        update_task(&t, &DayTaskPatch { state: Some(DayTaskState::Done), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        let raw = fs::read_to_string(&t.path).unwrap();
        assert!(raw.contains("status: open\n"), "status stays open: {raw}");
        assert!(raw.contains("done_on: '2026-10-01'\n"));
        assert_eq!(reread(&t).state, DayTaskState::Done);
        let tomorrow = parse_task(&t.path, t.home, &raw, "2026-10-02");
        assert_eq!(tomorrow.state, DayTaskState::Open);
        update_task(&t, &DayTaskPatch { state: Some(DayTaskState::Open), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        assert_eq!(reread(&t).state, DayTaskState::Open);
    }

    #[test]
    fn a_recurring_task_with_status_done_has_ended() {
        let raw = |updated: &str| {
            format!("---\nid: r1\ntitle: Standup notes\nstatus: done\nrepeat: daily\nupdated: '{updated}'\n---\n")
        };
        let t = task(&raw("2026-10-01T08:00"));
        assert_eq!(t.state, DayTaskState::Done, "ended, not open again today");
        assert!(t.recurring().is_none());
        assert!(listed_today(&t, TODAY), "shown, greyed, on the day it ended");
        assert!(!listed_today(&task(&raw("2026-09-30T17:00")), TODAY), "hidden after its day");

        // Setting a repeat on it starts it again: status back to open.
        let dir = tempdir().unwrap();
        let path = dir.path().join("r1.md");
        fs::write(&path, raw("2026-09-30T17:00")).unwrap();
        let t = reread(&parse_task(&path, HomeKind::Workspace, &raw("2026-09-30T17:00"), TODAY));
        update_task(&t, &DayTaskPatch { repeat: Some(Some("weekdays".into())), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: open\n"), "{after}");
        assert!(after.contains("repeat: weekdays\n"));
        let t = reread(&t);
        assert_eq!(t.state, DayTaskState::Open);
        assert!(listed_today(&t, TODAY), "a Thursday is a weekday");

        // Marking it done again is done for today only.
        update_task(&t, &DayTaskPatch { state: Some(DayTaskState::Done), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.contains("status: open\n"), "{after}");
        assert!(after.contains("done_on: '2026-10-01'\n"));
        assert_eq!(reread(&t).state, DayTaskState::Done);
    }

    #[test]
    fn reopening_an_ended_recurring_task_reopens_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("r2.md");
        let raw = "---\nid: r2\ntitle: Wiki pass\nstatus: done\nrepeat: daily\ndone_on: '2026-10-01'\n---\n";
        fs::write(&path, raw).unwrap();
        let t = parse_task(&path, HomeKind::Workspace, raw, TODAY);
        update_task(&t, &DayTaskPatch { state: Some(DayTaskState::Open), ..Default::default() }, &stamp(BY_YOU)).unwrap();
        let t = reread(&t);
        assert_eq!(t.status_raw, "open");
        assert_eq!(t.done_on, None);
        assert_eq!(t.state, DayTaskState::Open);
        assert!(t.recurring().is_some());
    }

    #[test]
    fn patch_json_distinguishes_absent_from_null() {
        let p: DayTaskPatch = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
        assert_eq!(p.target, None);
        let p: DayTaskPatch = serde_json::from_str(r#"{"target":null,"repeat":"weekly:mon","state":"done"}"#).unwrap();
        assert_eq!(p.target, Some(None));
        assert_eq!(p.repeat, Some(Some("weekly:mon".into())));
        assert_eq!(p.state, Some(DayTaskState::Done));
    }

    #[test]
    fn delete_archives_the_file() {
        let dir = tempdir().unwrap();
        let t = create_task(dir.path(), &DayTaskInput { title: "Gone".into(), ..Default::default() }, &stamp(BY_YOU), Some("01J0DEL")).unwrap();
        let moved = archive_task(&t, TODAY).unwrap();
        assert!(!t.path.exists());
        assert!(moved.ends_with("tasks/archive/2026-10/01J0DEL-gone.md"));
        assert!(scan(&[TaskHome::Workspace { workspace_root: dir.path() }], TODAY).is_empty());
    }

    #[test]
    fn scan_reads_the_workspace_home_and_my_board_and_dedupes() {
        let dir = tempdir().unwrap();
        let ws = dir.path().join("ws");
        let board = dir.path().join("families/FAM1/members/mem-1/board");
        fs::create_dir_all(&board).unwrap();
        fs::write(board.join("a.md"), "---\nid: a\ntitle: Accepted\nstatus: backlog\nfrom: dee\n---\n").unwrap();
        fs::write(board.join("b.md"), "---\nid: dup\ntitle: Board copy\n---\n").unwrap();
        fs::create_dir_all(board.join("archive/2026-09")).unwrap();
        fs::write(board.join("archive/2026-09/c.md"), "---\nid: c\ntitle: Archived\n---\n").unwrap();
        create_task(&ws, &DayTaskInput { title: "New".into(), ..Default::default() }, &stamp(BY_YOU), Some("dup")).unwrap();
        let homes = [TaskHome::Workspace { workspace_root: &ws }, TaskHome::Family { board_dir: &board }];
        let all = scan(&homes, TODAY);
        assert_eq!(all.len(), 2, "archive is a subfolder, not read");
        assert_eq!(find(&all, "dup").unwrap().title, "New", "first home wins");
        let a = find(&all, "a").unwrap();
        assert_eq!(a.state, DayTaskState::Open);
        assert_eq!(a.from.as_deref(), Some("dee"));
        assert_eq!(a.address_rel_path(), "members/mem-1/board/a.md");
        let moved = archive_task(a, TODAY).unwrap();
        assert!(moved.ends_with("members/mem-1/board/archive/2026-10/a.md"), "{}", moved.display());
    }

    #[test]
    fn a_repos_own_task_folder_is_not_a_home() {
        // Ways of Working: tasks live in your own folder, never in a
        // team repo. A `.ken/tasks/` file inside a repo is not read.
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".ken/tasks")).unwrap();
        fs::write(repo.join(".ken/tasks/a.md"), "---\nid: a\ntitle: Old board task\n---\n").unwrap();
        assert!(scan(&[TaskHome::Workspace { workspace_root: dir.path() }], TODAY).is_empty());
    }

    #[test]
    fn query_filters() {
        let mk = |id: &str, extra: &str| {
            parse_task(Path::new(&format!("/w/tasks/{id}.md")), HomeKind::Workspace, &format!("---\nid: {id}\ntitle: {id}\n{extra}---\n"), TODAY)
        };
        let all = vec![
            mk("a", "target: '2026-10-02'\nlinks: [ATT-014]\n"),
            mk("b", "target: '2026-10-09'\n"),
            mk("c", "status: done\nlinks:\n  - tickets/att-014.md\n"),
            mk("d", ""),
        ];
        let ids = |q: TaskQuery| query(&all, &q).iter().map(|t| t.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(TaskQuery { state: Some(DayTaskState::Open), ..Default::default() }), vec!["a", "b", "d"]);
        assert_eq!(ids(TaskQuery { state: Some(DayTaskState::Done), ..Default::default() }), vec!["c"]);
        assert_eq!(ids(TaskQuery { target_before: Some("2026-10-09".into()), ..Default::default() }), vec!["a"]);
        assert_eq!(ids(TaskQuery { linked: Some("att-014".into()), ..Default::default() }), vec!["a", "c"]);
    }

    #[test]
    fn ticket_fallbacks() {
        let t = parse_ticket("tickets/ATT-014.md", "---\nstatus: In Progress\nassignee: chris\ntarget: 2026-10-01\n---\n\n# Retry rule for the scheduler\n");
        assert_eq!(t.id, "ATT-014", "id from the stem");
        assert_eq!(t.title, "Retry rule for the scheduler", "title from the first heading");
        assert_eq!(t.state, "In Progress");
        assert!(t.open);
        assert_eq!(t.target.as_deref(), Some("2026-10-01"));
        let t = parse_ticket("tickets/pilot/ATT-011.md", "---\nid: ATT-011\ntitle: Pilot date\nstatus: Cancelled\ndue: '2026-10-03'\n---\n");
        assert_eq!(t.title, "Pilot date");
        assert!(!t.open);
        assert_eq!(t.target.as_deref(), Some("2026-10-03"), "target falls back to due");
        let t = parse_ticket("tickets/ATT-009.md", "no frontmatter, no heading\n");
        assert_eq!(t.title, "ATT-009", "title falls back to the id");
        assert_eq!(t.state, "");
        assert!(t.open);
        assert!(!parse_ticket("tickets/X.md", "---\nstatus: DONE\n---\n").open);
    }

    #[test]
    fn ticket_paths_and_scan() {
        assert!(is_ticket_path("tickets/ATT-014.md"));
        assert!(is_ticket_path("tickets/q4/ATT-014.md"));
        assert!(!is_ticket_path("tickets/a/b/ATT-014.md"));
        assert!(!is_ticket_path("docs/tickets/ATT-014.md"));
        assert!(!is_ticket_path("tickets/README.md"));
        let dir = tempdir().unwrap();
        let t = dir.path().join("tickets");
        fs::create_dir_all(t.join("q4/deeper")).unwrap();
        fs::write(t.join("ATT-2.md"), "---\nid: ATT-2\n---\n").unwrap();
        fs::write(t.join("q4/ATT-1.md"), "---\nid: ATT-1\n---\n").unwrap();
        fs::write(t.join("q4/deeper/ATT-3.md"), "---\nid: ATT-3\n---\n").unwrap();
        fs::write(t.join("README.md"), "# Tickets\n").unwrap();
        fs::write(t.join("notes.txt"), "x").unwrap();
        let found: Vec<(String, String)> = scan_tickets(dir.path()).into_iter().map(|t| (t.id, t.rel_path)).collect();
        assert_eq!(
            found,
            vec![("ATT-1".to_string(), "tickets/q4/ATT-1.md".to_string()), ("ATT-2".to_string(), "tickets/ATT-2.md".to_string())]
        );
        assert!(scan_tickets(&dir.path().join("nowhere")).is_empty());
    }

    #[test]
    fn tickets_order_by_target_then_none_then_id() {
        let mut t = vec![
            parse_ticket("tickets/B.md", "---\n---\n"),
            parse_ticket("tickets/C.md", "---\ntarget: '2026-10-09'\n---\n"),
            parse_ticket("tickets/A.md", "---\n---\n"),
            parse_ticket("tickets/D.md", "---\ntarget: '2026-10-02'\n---\n"),
        ];
        t.sort_by(ticket_order);
        let ids: Vec<&str> = t.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["D", "C", "A", "B"]);
    }

    #[test]
    fn assignee_matching() {
        let me = Me { name: Some("Chris Staud".into()), email: Some("Chris.Staud@example.com".into()) };
        assert!(me.matches("chris staud"));
        assert!(me.matches("chris.staud@example.com"));
        assert!(me.matches("chris.staud"), "the email's local part");
        assert!(me.matches("@Chris.Staud"));
        assert!(!me.matches("chris"));
        assert!(!me.matches(""));
        let t = parse_ticket("tickets/A.md", "---\nassignee: [dee, chris.staud]\n---\n");
        assert!(me.assigned(&t));
        let t = parse_ticket("tickets/A.md", "---\nassignee: dee, sam\n---\n");
        assert!(!me.assigned(&t));
        assert_eq!(t.assignees, vec!["dee, sam"], "a scalar is one value, not split on commas");

        // `Name <email>`, either half.
        assert!(me.matches("Chris Staud <someone@else.example>"));
        assert!(me.matches("C. S. <chris.staud@example.com>"));
        assert!(me.matches("\"Chris Staud\" <x@y.z>"));
        assert!(!me.matches("Dee Ray <dee@example.com>"));
        // Last name first is one person.
        let t = parse_ticket("tickets/A.md", "---\nassignee: Staud, Chris\n---\n");
        assert_eq!(t.assignees, vec!["Staud, Chris"]);
        assert!(me.assigned(&t));
        let t = parse_ticket("tickets/A.md", "---\nassignee:\n  - Dee Ray <dee@example.com>\n  - Chris Staud <chris.staud@example.com>\n---\n");
        assert!(me.assigned(&t));
        let nobody = Me::default();
        assert!(!nobody.is_known());
        assert!(!nobody.matches("chris"));
    }

    #[test]
    fn linked_counts_count_links_and_done() {
        let mk = |id: &str, extra: &str| {
            parse_task(Path::new(&format!("/w/tasks/{id}.md")), HomeKind::Workspace, &format!("---\nid: {id}\ntitle: {id}\n{extra}---\n"), TODAY)
        };
        let all = vec![
            mk("a", "links: [ATT-014, retry.ts]\n"),
            mk("b", "status: done\nlinks: [att-014]\n"),
            mk("c", "links: [ATT-0140]\n"),
            mk("d", ""),
        ];
        assert_eq!(linked_counts(&all, None, "ATT-014"), (2, 1));
        assert_eq!(linked_counts(&all, Some("att-opmodel"), "ATT-014"), (2, 1), "bare links match any repo");
        assert_eq!(linked_counts(&all, None, "ATT-999"), (0, 0));
        assert_eq!(linked_counts(&all, None, ""), (0, 0));
    }

    #[test]
    fn a_repo_qualified_link_names_only_that_repos_ticket() {
        let mk = |id: &str, extra: &str| {
            parse_task(Path::new(&format!("/w/tasks/{id}.md")), HomeKind::Workspace, &format!("---\nid: {id}\ntitle: {id}\n{extra}---\n"), TODAY)
        };
        let all = vec![
            mk("ops", "links: [att-opmodel/ATT-014]\n"),
            mk("web", "status: done\nlinks: [ATT-Web/att-014]\n"),
            mk("bare", "links: [ATT-014]\n"),
            mk("path", "links: [att-opmodel/tickets/ATT-014.md]\n"),
            mk("deep", "links: ['tickets/q4/ATT-014.md']\n"),
            mk("addr", "links: ['ken://0b1c/tickets/ATT-014.md']\n"),
            mk("file", "links: [att-opmodel/src/retry.ts]\n"),
        ];
        let linked = |repo: Option<&str>, id: &str| {
            all.iter().filter(|t| t.links_ticket(repo, id)).map(|t| t.id.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(linked(Some("att-opmodel"), "ATT-014"), vec!["ops", "bare", "path", "deep", "addr"]);
        assert_eq!(linked(Some("att-web"), "ATT-014"), vec!["web", "bare", "deep", "addr"]);
        assert_eq!(linked(Some("other"), "ATT-014"), vec!["bare", "deep", "addr"]);
        assert_eq!(linked(None, "att-014").len(), 6, "no repo: the id in any repo");
        assert_eq!(linked_counts(&all, Some("att-web"), "ATT-014"), (4, 1));

        // ticket_tasks / task_list's `linked` takes the same forms.
        let ids = |r: &str| {
            query(&all, &TaskQuery { linked: Some(r.into()), ..Default::default() }).iter().map(|t| t.id.clone()).collect::<Vec<_>>()
        };
        assert!(!ids("att-opmodel/ATT-014").contains(&"web".to_string()));
        assert!(ids("att-opmodel/ATT-014").contains(&"bare".to_string()));
        assert_eq!(ids("ATT-014").len(), 6);
        assert_eq!(split_ticket_ref("att-web/ATT-1"), (Some("att-web"), "ATT-1"));
        assert_eq!(split_ticket_ref(" ATT-1 "), (None, "ATT-1"));
    }

    #[test]
    fn journal_line_cites_the_address() {
        let t = task("---\nid: t1\ntitle: Ship it\n---\n");
        assert_eq!(journal_line(&t, "workspace"), "Completed task \"Ship it\" (ken://workspace/tasks/t1-x.md)");
    }

    #[test]
    fn escalations_for_me_are_read_from_the_team_repo() {
        let dir = tempdir().unwrap();
        let esc = dir.path().join(ESCALATIONS_DIR);
        fs::create_dir_all(&esc).unwrap();
        let ingest = crate::ingest::escalation_text("E-002", "Which Jira types map to tune?", Some("chris"), "2026-09-24", "2026-09-24-standup");
        fs::write(esc.join("E-002.md"), ingest).unwrap();
        fs::write(esc.join("E-001.md"), "---\nid: E-001\nto: Kate\nstatus: open\n---\n\n# Who owns saves?\n").unwrap();
        fs::write(esc.join("E-003.md"), "---\nid: E-003\nto: [Chris Staud, Kate]\nstatus: answered\nticket: SR-012\nblocks: SR-012\n---\n\n# Old one\n").unwrap();
        fs::write(esc.join("ESCALATION.md"), "---\nid: E-nnn\n---\n").unwrap();
        let all = scan_escalations(dir.path());
        assert_eq!(all.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["E-001", "E-002", "E-003"]);
        let e2 = &all[1];
        assert_eq!(e2.title, "Which Jira types map to tune?");
        assert_eq!(e2.raised_by.as_deref(), Some("ingest of 2026-09-24-standup"));
        assert_eq!((e2.raised.as_deref(), e2.blocks.as_deref(), e2.ticket.as_deref()), (Some("2026-09-24"), None, None));
        assert!(e2.open && !all[2].open);
        assert_eq!(all[2].blocks.as_deref(), Some("SR-012"));
        let me = Me { name: Some("Chris Staud".into()), email: Some("chris.staud@example.com".into()) };
        let mine: Vec<&str> = all.iter().filter(|e| e.open && me.addressed(e)).map(|e| e.id.as_str()).collect();
        assert_eq!(mine, vec!["E-002"], "first name alone is me; Kate's is not; an answered one is not open");
        assert!(scan_escalations(&dir.path().join("nowhere")).is_empty());
    }
}
