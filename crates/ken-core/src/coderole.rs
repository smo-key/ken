//! A code file's role (knowledge-core decision 17): what part it plays in
//! its repo, such as a service, a controller or a repository. A code file's
//! kind is its role: its hit label says `code · service · 2026-10-01`, and
//! the role is in its chunks' names field and embedded header, so "the
//! payment service" finds `PaymentService.java` by meaning as well as words.
//!
//! Worked out without a model, at index time:
//!
//! 1. The repo's own rules, a gitignore-style pattern to a role, in the
//!    knowledge base's settings under the repo's `repos` entry
//!    (`knowledge_settings`). The repo analysis proposes them in the Inbox
//!    when the repo is added (`repo_analysis`); once a person accepts they
//!    are written there, and a person edits them there. Where
//!    several patterns match, the longest wins.
//! 2. Else the built-in core ([`core_role`]), read from the file's name and
//!    then the folders it sits in: `UserService.java`, `services/user.py`.
//! 3. Else none: the label then says the file's tier.

use std::path::Path;

/// The built-in core: each role, the folder names that give it, and the
/// last word of a file name that gives it (`PaymentService`, `user_controller`).
const CORE: &[(&str, &[&str], &[&str])] = &[
    ("controller", &["controllers", "controller"], &["controller"]),
    ("service", &["services", "service"], &["service"]),
    ("repository", &["repositories", "repository", "dao", "daos"], &["repository", "repo", "dao"]),
    ("handler", &["handlers", "handler"], &["handler"]),
    ("model", &["models", "model", "entities", "entity"], &["model", "entity"]),
    ("component", &["components", "component", "widgets"], &["component", "widget"]),
    ("view", &["views", "view", "pages", "screens"], &["view", "screen"]),
    ("route", &["routes", "routers", "router", "routing"], &["route", "routes", "router"]),
    ("middleware", &["middleware", "middlewares"], &["middleware"]),
    ("config", &["config", "configs", "configuration", "settings"], &["config", "settings", "configuration"]),
    ("migration", &["migrations"], &["migration"]),
    ("util", &["utils", "util", "helpers", "helper"], &["util", "utils", "helper", "helpers"]),
    ("script", &["scripts"], &[]),
    ("store", &["stores", "store"], &["store"]),
    ("client", &["clients"], &["client"]),
];

/// The roles the built-in core knows.
pub fn core_roles() -> impl Iterator<Item = &'static str> {
    CORE.iter().map(|(role, _, _)| *role)
}

/// The built-in role of the code file at `rel_path`, from its name, else
/// the nearest folder that names one. None for a file that is not code.
pub fn core_role(rel_path: &str) -> Option<&'static str> {
    if crate::contenttype::of(rel_path) != crate::contenttype::ContentType::Code {
        return None;
    }
    let path = rel_path.replace('\\', "/");
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let name = segments.pop().unwrap_or("");
    let stem = name.split('.').next().unwrap_or(name);
    if let Some(last) = words(stem).last() {
        if let Some((role, _, _)) = CORE.iter().find(|(_, _, names)| names.contains(&last.as_str())) {
            return Some(role);
        }
    }
    for dir in segments.iter().rev() {
        let dir = dir.to_ascii_lowercase();
        if let Some((role, _, _)) = CORE.iter().find(|(_, dirs, _)| dirs.contains(&dir.as_str())) {
            return Some(role);
        }
    }
    None
}

/// The words of a name, lowercased: `PaymentService` is `payment service`,
/// `user_controller` and `user-controller` are `user controller`.
fn words(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A repo's role rules and the built-in core behind them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleRules {
    /// Pattern and role, the longest pattern first.
    rules: Vec<(String, String)>,
}

impl RoleRules {
    pub fn new(mut rules: Vec<(String, String)>) -> RoleRules {
        rules.retain(|(p, r)| !p.trim().is_empty() && !r.trim().is_empty());
        for (_, r) in rules.iter_mut() {
            *r = normalize_role(r);
        }
        rules.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(&b.0)));
        RoleRules { rules }
    }

    /// The rules the repo at `root` reads from its knowledge base, none
    /// when it has none (the built-in core still applies).
    pub fn for_repo(root: &Path) -> RoleRules {
        RoleRules::new(crate::knowledge_settings::for_repo(root).map(|s| s.role_rules()).unwrap_or_default())
    }

    /// The role of the code file at `rel_path`: the longest matching rule,
    /// else the built-in core. None for a file that is not code.
    pub fn role_of(&self, rel_path: &str) -> Option<String> {
        if crate::contenttype::of(rel_path) != crate::contenttype::ContentType::Code {
            return None;
        }
        let path = rel_path.replace('\\', "/");
        self.rules
            .iter()
            .find(|(pattern, _)| crate::authority::glob_matches(pattern, &path))
            .map(|(_, role)| role.clone())
            .or_else(|| core_role(&path).map(str::to_string))
    }
}

/// A role as it is stored and shown: lowercase, words joined by spaces.
pub fn normalize_role(role: &str) -> String {
    role.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Work out and store the role of every code file in `db`, with the rules
/// the repo at `root` reads. A file whose role changed has its chunks'
/// names written again and their vectors dropped, so the next semantic
/// rebuild embeds them under the new role. Returns the paths that changed.
pub fn refresh(db: &crate::db::Db, root: &Path) -> crate::Result<Vec<String>> {
    refresh_with(db, &RoleRules::for_repo(root))
}

/// [`refresh`] with the rules already read.
pub fn refresh_with(db: &crate::db::Db, rules: &RoleRules) -> crate::Result<Vec<String>> {
    let rows: Vec<(String, Option<String>)> = db
        .indexed_paths()?
        .into_iter()
        .map(|p| {
            let role = rules.role_of(&p);
            (p, role)
        })
        .collect();
    db.set_file_roles(&rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_core_reads_the_file_name_then_its_folders() {
        let cases = [
            ("src/main/java/com/x/PaymentService.java", Some("service")),
            ("app/controllers/users.py", Some("controller")),
            ("app/user_controller.py", Some("controller")),
            ("internal/store/order_repository.go", Some("repository")),
            ("src/components/Button.svelte", Some("component")),
            ("src/routes/api/+server.ts", Some("route")),
            ("src/lib/utils/date.ts", Some("util")),
            ("src/services/payments/RetryHandler.java", Some("handler")),
            ("src/main.rs", None),
            ("docs/services/overview.md", None),
            ("src/Contest.java", None),
        ];
        for (path, want) in cases {
            assert_eq!(core_role(path), want, "{path}");
        }
    }

    #[test]
    fn a_repo_rule_wins_and_the_longest_pattern_first() {
        let rules = RoleRules::new(vec![
            ("src/net/**".into(), "Protocol".into()),
            ("src/net/legacy/**".into(), "legacy_protocol".into()),
            ("**/*System.java".into(), "system".into()),
            ("".into(), "nothing".into()),
        ]);
        assert_eq!(rules.role_of("src/net/Packet.java").as_deref(), Some("protocol"));
        assert_eq!(rules.role_of("src/net/legacy/OldPacket.java").as_deref(), Some("legacy protocol"));
        assert_eq!(rules.role_of("src/world/WeatherSystem.java").as_deref(), Some("system"));
        assert_eq!(rules.role_of("src/world/SaveService.java").as_deref(), Some("service"), "the core behind the rules");
        assert_eq!(rules.role_of("src/net/README.md"), None, "only code has a role");
    }

    #[test]
    fn refresh_stores_roles_and_reports_changes() {
        let mut db = crate::db::Db::open_in_memory().unwrap();
        for (p, kind) in [("src/PayService.java", "code"), ("README.md", "md")] {
            db.upsert_file(p, kind, 1, 1, "indexed", None, "x").unwrap();
            let chunks = crate::chunker::chunk_file(p, "x", &crate::chunker::IndexProfile::default_for(p));
            db.upsert_chunks(p, &chunks).unwrap();
        }
        assert_eq!(refresh_with(&db, &RoleRules::default()).unwrap(), vec!["src/PayService.java".to_string()]);
        assert_eq!(db.file_role("src/PayService.java").unwrap().as_deref(), Some("service"));
        assert!(refresh_with(&db, &RoleRules::default()).unwrap().is_empty(), "unchanged");
        let rules = RoleRules::new(vec![("src/Pay*".into(), "billing".into())]);
        assert_eq!(refresh_with(&db, &rules).unwrap().len(), 1);
        assert_eq!(db.file_role("src/PayService.java").unwrap().as_deref(), Some("billing"));
        assert!(db.chunk_names("src/PayService.java").unwrap().iter().any(|n| n.contains("billing")), "the role is in the names field");
    }
}
