//! The links between pages: the library's third index, beside search and
//! the graph (docs-system, "Links"). A name link (`[[Note Name]]`) resolves
//! by a page's file name or one of its aliases; a path link (`[text](a/b.md)`)
//! resolves by path. The two are counted apart, code is stripped first, and
//! the report is a writing queue: broken path links by section, missing
//! names by how many pages ask for them, and every name two pages claim.
//!
//! Links are extracted per page at index time and stored raw; resolution
//! runs when asked, against the pages and aliases the index holds then, so
//! a rename or a new alias needs no re-extraction.

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;

use crate::db::Db;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    /// `[[Note Name]]`, resolved by file name or alias.
    Name,
    /// `[text](relative/path.md)`, resolved by path from the page's folder.
    Path,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Name => "name",
            LinkKind::Path => "path",
        }
    }
    pub fn parse(s: &str) -> Option<LinkKind> {
        match s {
            "name" => Some(LinkKind::Name),
            "path" => Some(LinkKind::Path),
            _ => None,
        }
    }
}

/// One link as written on a page. For a path link, `target` is already
/// resolved against the page's folder (`a/b/../c.md` → `a/c.md`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct Link {
    pub kind: LinkKind,
    pub target: String,
}

/// Fenced blocks and inline code removed, so an example like `[[Name]]` in
/// a code sample is not counted as a link.
pub fn strip_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<&str> = None;
    for line in text.lines() {
        let t = line.trim_start();
        let marker = if t.starts_with("```") { Some("```") } else if t.starts_with("~~~") { Some("~~~") } else { None };
        match (fence, marker) {
            (None, Some(m)) => {
                fence = Some(m);
                out.push('\n');
                continue;
            }
            (Some(f), Some(m)) if f == m => {
                fence = None;
                out.push('\n');
                continue;
            }
            (Some(_), _) => {
                out.push('\n');
                continue;
            }
            _ => {}
        }
        // Inline code: drop everything between backtick pairs.
        let mut in_tick = false;
        for c in line.chars() {
            if c == '`' {
                in_tick = !in_tick;
                continue;
            }
            if !in_tick {
                out.push(c);
            }
        }
        out.push('\n');
    }
    out
}

/// The links on the page at `from` (its repo-relative path), code stripped.
pub fn extract(from: &str, text: &str) -> Vec<Link> {
    let body = strip_code(text);
    let mut out: Vec<Link> = Vec::new();
    let mut push = |l: Link| {
        if !out.contains(&l) {
            out.push(l);
        }
    };
    // [[Target]], [[Target|label]], [[Target#heading]], ![[embed]]
    let mut rest = body.as_str();
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]") else { break };
        let inner = &after[..end];
        let name = inner.split(['|', '#']).next().unwrap_or("").trim();
        // A template placeholder (`[[{{ingested note}}]]`) is not a link.
        if !name.is_empty() && !name.contains('\n') && !name.contains("{{") {
            push(Link { kind: LinkKind::Name, target: name.to_string() });
        }
        rest = &after[end + 2..];
    }
    // [text](target) with a relative, non-URL target (an image embed counts
    // too: a missing image is a broken link like any other).
    let mut i = 0;
    while let Some(off) = body[i..].find("](") {
        let open = i + off + 2;
        let Some(close_off) = body[open..].find(')') else { break };
        let raw = body[open..open + close_off].trim();
        let raw = raw.split_whitespace().next().unwrap_or(""); // drop a "title"
        let raw = raw.trim_start_matches('<').trim_end_matches('>');
        let target = raw.split('#').next().unwrap_or("");
        let is_url = target.contains("://") || target.starts_with("mailto:") || target.starts_with('/');
        if !target.is_empty() && !is_url && !target.contains("{{") {
            let decoded = target.replace("%20", " ");
            push(Link { kind: LinkKind::Path, target: join(from, &decoded) });
        }
        i = open + close_off + 1;
    }
    out
}

/// `target` relative to the folder of `from`, normalized.
fn join(from: &str, target: &str) -> String {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop(); // the page's own file name
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md").or_else(|| name.strip_suffix(".markdown")).unwrap_or(name).to_lowercase()
}

/// Every page the index knows, with the names that reach it.
pub struct Resolver {
    /// Page file names and aliases. Two owners make a duplicate.
    by_name: HashMap<String, Vec<String>>,
    /// Other files by full file name (`world.png`), for `![[world.png]]`
    /// embeds; never reported as duplicates (every folder has a README).
    by_file: HashMap<String, Vec<String>>,
    paths: std::collections::HashSet<String>,
}

fn is_page(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".markdown")
}

impl Resolver {
    pub fn from_db(db: &Db) -> Result<Resolver> {
        let pages = db.page_paths()?;
        let aliases = db.page_aliases_by_path()?;
        Ok(Resolver::new(&pages, &aliases))
    }

    pub fn new(pages: &[String], aliases: &[(String, Vec<String>)]) -> Resolver {
        let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<String>> = HashMap::new();
        for p in pages {
            if is_page(p) {
                by_name.entry(stem(p)).or_default().push(p.clone());
            } else {
                let name = p.rsplit('/').next().unwrap_or(p).to_lowercase();
                by_file.entry(name).or_default().push(p.clone());
            }
        }
        for (p, names) in aliases {
            for n in names {
                let key = n.trim().to_lowercase();
                let owners = by_name.entry(key).or_default();
                if !owners.contains(p) {
                    owners.push(p.clone());
                }
            }
        }
        Resolver { by_name, by_file, paths: pages.iter().cloned().collect() }
    }

    /// The pages a link reaches: zero (missing or broken), one, or more
    /// than one (a name two pages claim).
    pub fn resolve(&self, link: &Link) -> Vec<String> {
        match link.kind {
            LinkKind::Name => {
                let key = link.target.trim().to_lowercase();
                let key = key.strip_suffix(".md").unwrap_or(&key);
                self.by_name
                    .get(key)
                    .or_else(|| self.by_file.get(key))
                    .cloned()
                    .unwrap_or_default()
            }
            LinkKind::Path => {
                let t = link.target.clone();
                let with_md = format!("{t}.md");
                [t, with_md].into_iter().find(|c| self.paths.contains(c)).into_iter().collect()
            }
        }
    }

    /// Names claimed by more than one page (file name or alias).
    pub fn duplicate_names(&self) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = self
            .by_name
            .iter()
            .filter(|(_, owners)| owners.len() > 1)
            .map(|(n, owners)| {
                let mut o = owners.clone();
                o.sort();
                (n.clone(), o)
            })
            .collect();
        out.sort();
        out
    }
}

/// The link report, a writing queue.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkReport {
    pub name_links: usize,
    pub path_links: usize,
    /// Broken path links grouped by the linking page's section.
    pub broken_paths: BTreeMap<String, Vec<(String, String)>>,
    /// Missing names, most asked-for first: (name, pages asking).
    pub missing_names: Vec<(String, Vec<String>)>,
    pub duplicate_names: Vec<(String, Vec<String>)>,
}

impl LinkReport {
    pub fn is_clean(&self) -> bool {
        self.broken_paths.is_empty() && self.missing_names.is_empty() && self.duplicate_names.is_empty()
    }

    /// The report as text: short, grouped, most useful first.
    pub fn to_markdown(&self) -> String {
        let mut s = format!("{} name links and {} path links checked.\n", self.name_links, self.path_links);
        if !self.missing_names.is_empty() {
            s.push_str("\n**Missing pages**, most asked-for first: the top one is the next page to write.\n");
            for (name, askers) in self.missing_names.iter().take(20) {
                s.push_str(&format!("- [[{name}]] — asked for by {}: {}\n", askers.len(), askers.join(", ")));
            }
        }
        if !self.broken_paths.is_empty() {
            s.push_str("\n**Broken path links**, by section:\n");
            for (section, links) in &self.broken_paths {
                s.push_str(&format!("- {section}: {}\n", links.len()));
                for (from, to) in links.iter().take(10) {
                    s.push_str(&format!("  - {from} → {to}\n"));
                }
            }
        }
        if !self.duplicate_names.is_empty() {
            s.push_str("\n**Names two pages claim** (a name link to them is ambiguous):\n");
            for (name, owners) in self.duplicate_names.iter().take(20) {
                s.push_str(&format!("- {name}: {}\n", owners.join(", ")));
            }
        }
        s
    }
}

/// Resolve every stored link against the pages the index holds now.
pub fn report(db: &Db) -> Result<LinkReport> {
    let resolver = Resolver::from_db(db)?;
    let mut r = LinkReport::default();
    let mut missing: HashMap<String, Vec<String>> = HashMap::new();
    // The names links use: two pages sharing a file name (every section's
    // Index.md) only matter when a link names it.
    let mut named: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (from, link) in db.all_page_links()? {
        match link.kind {
            LinkKind::Name => {
                r.name_links += 1;
                let key = link.target.trim().to_lowercase();
                named.insert(key.strip_suffix(".md").unwrap_or(&key).to_string());
            }
            LinkKind::Path => r.path_links += 1,
        }
        if !resolver.resolve(&link).is_empty() {
            continue;
        }
        match link.kind {
            LinkKind::Name => missing.entry(link.target.clone()).or_default().push(from),
            LinkKind::Path => {
                let section = crate::pagemeta::section_of(&from).map_or("(no section)", |s| s.name());
                r.broken_paths.entry(section.to_string()).or_default().push((from, link.target));
            }
        }
    }
    let mut missing: Vec<(String, Vec<String>)> = missing.into_iter().collect();
    missing.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    r.missing_names = missing;
    r.duplicate_names = resolver.duplicate_names().into_iter().filter(|(n, _)| named.contains(n)).collect();
    Ok(r)
}

/// A page's links both ways: the pages it reaches and the pages that reach
/// it, for the Map's neighbours.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageLinks {
    pub outgoing: Vec<String>,
    pub incoming: Vec<String>,
}

pub fn page_links(db: &Db, path: &str) -> Result<PageLinks> {
    let resolver = Resolver::from_db(db)?;
    let mut out = PageLinks::default();
    for (from, link) in db.all_page_links()? {
        let to = resolver.resolve(&link);
        if from == path {
            for t in to.iter().filter(|t| t.as_str() != path) {
                if !out.outgoing.contains(t) {
                    out.outgoing.push(t.clone());
                }
            }
        } else if to.iter().any(|t| t == path) && !out.incoming.contains(&from) {
            out.incoming.push(from);
        }
    }
    out.outgoing.sort();
    out.incoming.sort();
    Ok(out)
}

/// One entry of the link report for the Team screen: a page to write, a
/// broken link, or a name two pages claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// `missing-page` · `broken-link` · `ambiguous-name`
    pub kind: String,
    pub title: String,
    pub detail: String,
    /// The page to open: the first one asking, the linking page, or the
    /// first claimant.
    pub path: String,
}

/// The report as findings, the most asked-for missing page first, at most
/// `limit` of each kind.
pub fn findings(report: &LinkReport, limit: usize) -> Vec<Finding> {
    let mut out = Vec::new();
    for (name, askers) in report.missing_names.iter().take(limit) {
        out.push(Finding {
            kind: "missing-page".into(),
            title: format!("[[{name}]] has no page"),
            detail: format!(
                "Asked for by {} page{}: {}.",
                askers.len(),
                if askers.len() == 1 { "" } else { "s" },
                askers.join(", ")
            ),
            path: askers.first().cloned().unwrap_or_default(),
        });
    }
    for (section, links) in &report.broken_paths {
        for (from, to) in links.iter().take(limit) {
            out.push(Finding {
                kind: "broken-link".into(),
                title: format!("{from} links to {to}, which is not there"),
                detail: format!("In {section}."),
                path: from.clone(),
            });
        }
    }
    for (name, owners) in report.duplicate_names.iter().take(limit) {
        out.push(Finding {
            kind: "ambiguous-name".into(),
            title: format!("Two pages claim the name {name}"),
            detail: format!("A link to [[{name}]] is ambiguous: {}.", owners.join(", ")),
            path: owners.first().cloned().unwrap_or_default(),
        });
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_name_and_path_links_and_ignores_code_and_urls() {
        let text = "See [[Rules]] and [[Team|the team]] and [[Lifecycle#L3]].\n\
                    Also [the map](../Platform/World.md \"title\") and [site](https://x.io) and [top](#here).\n\
                    ```\n[[Not A Link]]\n```\n\
                    Inline `[[Also Not]]` code. A template's [[{{ingested note}}]] and [x]({{path}}).\n";
        let links = extract("Ways-of-Working/Index.md", text);
        assert_eq!(
            links,
            vec![
                Link { kind: LinkKind::Name, target: "Rules".into() },
                Link { kind: LinkKind::Name, target: "Team".into() },
                Link { kind: LinkKind::Name, target: "Lifecycle".into() },
                Link { kind: LinkKind::Path, target: "Platform/World.md".into() },
            ]
        );
    }

    #[test]
    fn names_resolve_by_stem_or_alias_and_paths_by_path() {
        let pages = vec!["Ways-of-Working/Rules.md".to_string(), "Current/Team.md".to_string(), "Old/Team.md".to_string()];
        let aliases = vec![("Ways-of-Working/Rules.md".to_string(), vec!["binding rules".to_string()])];
        let r = Resolver::new(&pages, &aliases);
        let name = |t: &str| Link { kind: LinkKind::Name, target: t.into() };
        assert_eq!(r.resolve(&name("rules")), vec!["Ways-of-Working/Rules.md"]);
        assert_eq!(r.resolve(&name("Binding Rules")), vec!["Ways-of-Working/Rules.md"]);
        assert_eq!(r.resolve(&name("Team")).len(), 2, "two pages claim Team");
        assert!(r.resolve(&name("Ghost")).is_empty());
        let path = |t: &str| Link { kind: LinkKind::Path, target: t.into() };
        assert_eq!(r.resolve(&path("Current/Team")), vec!["Current/Team.md"], ".md may be left off");
        assert!(r.resolve(&path("Current/Gone.md")).is_empty());
        assert_eq!(r.duplicate_names(), vec![("team".to_string(), vec!["Current/Team.md".to_string(), "Old/Team.md".to_string()])]);
    }

    #[test]
    fn an_embed_resolves_by_file_name_and_other_files_are_never_duplicates() {
        let pages = vec!["img/world.png".to_string(), "a/README.txt".to_string(), "b/README.txt".to_string()];
        let r = Resolver::new(&pages, &[]);
        assert_eq!(r.resolve(&Link { kind: LinkKind::Name, target: "World.png".into() }), vec!["img/world.png"]);
        assert!(r.duplicate_names().is_empty());
    }

    /// End to end over a scanned library: the report counts both kinds,
    /// ranks the missing name asked for most, groups the broken path by
    /// section, gives page neighbours both ways, and reads as findings.
    #[test]
    fn a_scanned_library_reports_its_links_as_findings() {
        use crate::project::Project;
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Ways-of-Working")).unwrap();
        fs::create_dir_all(root.join("Current")).unwrap();
        fs::write(
            root.join("Ways-of-Working/Rules.md"),
            "---\naliases: [\"binding rules\"]\n---\n# Rules\nSee [[Team]], [[Ghost]] and [the map](../Platform/Map.md).\n",
        )
        .unwrap();
        fs::write(root.join("Current/Team.md"), "# Team\nWe follow the [[binding rules]]. Also [[Ghost]] and [[Phantom]].\n").unwrap();
        let project = Project::create(root, "Lib").unwrap();
        let mut db = Db::open_in_memory().unwrap();
        crate::scan::scan(&project, &mut db).unwrap();

        let r = report(&db).unwrap();
        assert_eq!((r.name_links, r.path_links), (5, 1));
        assert_eq!(r.missing_names[0], ("Ghost".to_string(), vec!["Current/Team.md".to_string(), "Ways-of-Working/Rules.md".to_string()]));
        assert_eq!(r.missing_names[1].0, "Phantom");
        assert_eq!(
            r.broken_paths.get("Ways-of-Working"),
            Some(&vec![("Ways-of-Working/Rules.md".to_string(), "Platform/Map.md".to_string())])
        );
        assert!(r.duplicate_names.is_empty());

        let n = page_links(&db, "Current/Team.md").unwrap();
        assert_eq!(n.outgoing, vec!["Ways-of-Working/Rules.md"], "reached through an alias");
        assert_eq!(n.incoming, vec!["Ways-of-Working/Rules.md"]);

        let found = findings(&r, 20);
        assert_eq!(found[0].kind, "missing-page");
        assert!(found[0].title.contains("[[Ghost]]") && found[0].path == "Current/Team.md", "{found:?}");
        assert!(found.iter().any(|f| f.kind == "broken-link" && f.path == "Ways-of-Working/Rules.md"));
        assert!(findings(&LinkReport::default(), 20).is_empty(), "clean: nothing found");
    }

    #[test]
    fn a_path_link_is_resolved_from_the_pages_folder() {
        assert_eq!(join("a/b/page.md", "../c/d.md"), "a/c/d.md");
        assert_eq!(join("page.md", "./x.md"), "x.md");
        assert_eq!(join("a/page.md", "x%20y.md".replace("%20", " ").as_str()), "a/x y.md");
    }
}
