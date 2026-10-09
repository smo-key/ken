//! The team's roster: `people/*.md` in the team repo, one file a person.
//! Ken counts a person once by what the roster says and guesses nothing: a
//! commit identity or a name is a roster person only when their file lists
//! it, as their name, an alias or an address.

use std::fs;
use std::path::{Path, PathBuf};

/// One person on the roster: their file's `id` and `name`, every address
/// they commit from (`emails`) and every other name they commit or are known
/// under (`aliases`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub emails: Vec<String>,
    pub aliases: Vec<String>,
}

impl Person {
    /// Whether a commit by `name <email>` is this person: the address is one
    /// of theirs (a GitHub noreply address by its user name), or the name is
    /// theirs or an alias, in any case.
    pub fn commits_as(&self, name: &str, email: &str) -> bool {
        let key = person_key(email);
        let name = name.trim().to_lowercase();
        (!key.is_empty() && self.emails.iter().any(|e| person_key(e) == key))
            || (!name.is_empty() && self.names().any(|n| n.trim().to_lowercase() == name))
    }

    /// Whether an entity called `name` is this person: their name, an alias
    /// or an address, compared after [`crate::federation::normalize_name`].
    pub fn is_named(&self, name: &str) -> bool {
        let norm = crate::federation::normalize_name(name);
        !norm.is_empty()
            && self.names().chain(self.emails.iter()).any(|n| crate::federation::normalize_name(n) == norm)
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        std::iter::once(&self.name).chain(self.aliases.iter())
    }
}

/// The person behind an email: a GitHub noreply address
/// (`123+name@users.noreply.github.com`) counts by its user name, any other
/// by the address itself, in lower case.
pub fn person_key(email: &str) -> String {
    let e = email.trim().to_lowercase();
    match e.strip_suffix("@users.noreply.github.com") {
        Some(local) => format!("github:{}", local.split_once('+').map_or(local, |(_, user)| user)),
        None => e,
    }
}

/// A repo's `people/` files, in file-name order, its README and the
/// PERSON.md template aside.
pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(root.join("people"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter(|p| p.file_name().is_some_and(|n| !n.eq_ignore_ascii_case("README.md") && !n.eq_ignore_ascii_case("PERSON.md")))
        .collect();
    files.sort();
    files
}

/// The people of a repo's `people/` [`files`]. A file whose frontmatter does
/// not parse, or that names nobody (no `name`, no `id`), is left out; a
/// person with no `name` goes by their `id`. A value still holding a `{{…}}`
/// placeholder is not a value.
pub fn roster(root: &Path) -> Vec<Person> {
    files(root)
        .into_iter()
        .filter_map(|p| {
            let text = fs::read_to_string(&p).ok()?;
            let (fm, _) = crate::tasks::split_frontmatter(&text)?;
            let map: serde_yaml::Mapping = serde_yaml::from_str(fm).ok()?;
            let id = strings(map.get("id")).into_iter().next().unwrap_or_default();
            let name = strings(map.get("name")).into_iter().next().unwrap_or_else(|| id.clone());
            if name.is_empty() {
                return None;
            }
            let id = if id.is_empty() { p.file_stem()?.to_string_lossy().to_string() } else { id };
            Some(Person { id, name, emails: strings(map.get("emails")), aliases: strings(map.get("aliases")) })
        })
        .collect()
}

/// Every roster of `roots`, in order, a person whose `id` is already listed
/// left out: the team repo and a wiki that holds the team's folders are read
/// alike.
pub fn roster_of<'a>(roots: impl IntoIterator<Item = &'a Path>) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    for root in roots {
        for p in roster(root) {
            if !out.iter().any(|o| o.id == p.id) {
                out.push(p);
            }
        }
    }
    out
}

/// The roster person a commit by `name <email>` belongs to.
pub fn by_commit<'a>(roster: &'a [Person], name: &str, email: &str) -> Option<&'a Person> {
    roster.iter().find(|p| p.commits_as(name, email))
}

/// The roster person an entity called `name` is.
pub fn by_name<'a>(roster: &'a [Person], name: &str) -> Option<&'a Person> {
    roster.iter().find(|p| p.is_named(name))
}

/// A frontmatter value as strings: one string, or a list of them, trimmed,
/// with empty ones and placeholders left out.
fn strings(v: Option<&serde_yaml::Value>) -> Vec<String> {
    let one = |v: &serde_yaml::Value| match v {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let all: Vec<String> = match v {
        Some(serde_yaml::Value::Sequence(items)) => items.iter().filter_map(one).collect(),
        Some(v) => one(v).into_iter().collect(),
        None => Vec::new(),
    };
    all.into_iter().filter(|s| !s.is_empty() && !s.contains("{{")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, file: &str, text: &str) {
        fs::create_dir_all(root.join("people")).unwrap();
        fs::write(root.join("people").join(file), text).unwrap();
    }

    #[test]
    fn roster_reads_people_files_with_emails_and_aliases() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(
            root,
            "chris.md",
            "---\nid: chris\nname: Chris\nemails: [14892384+Stingbro@users.noreply.github.com, noname@example.com, alpha@example.com]\n\
             aliases:\n  - Stingbro\n  - AlpahSignalAI\npersona: Build   # for this team\n---\n\n# Chris\n",
        );
        write(root, "ana.md", "---\nid: ana\nemails: ana@example.com\n---\n");
        write(root, "nobody.md", "---\nid: {{handle}}\nname: {{Full Name}}\n---\n");
        write(root, "PERSON.md", "---\nid: x\nname: Template\n---\n");
        write(root, "README.md", "---\nid: y\nname: Readme\n---\n");
        let people = roster(root);
        assert_eq!(people.len(), 2, "{people:?}");
        assert_eq!(people[0], Person { id: "ana".into(), name: "ana".into(), emails: vec!["ana@example.com".into()], aliases: vec![] });
        let chris = &people[1];
        assert_eq!((chris.id.as_str(), chris.name.as_str()), ("chris", "Chris"));
        assert_eq!(chris.emails.len(), 3);
        assert_eq!(chris.aliases, vec!["Stingbro".to_string(), "AlpahSignalAI".to_string()]);

        // A noreply address matches by its user name; a name or alias in any case.
        assert!(chris.commits_as("Someone", "Stingbro@users.noreply.github.com"));
        assert!(chris.commits_as("alpahsignalai", "other@example.com"));
        assert!(!chris.commits_as("Chrissy", "chrissy@example.com"));
        assert!(chris.is_named("alpahsignalai") && chris.is_named("Alpha@Example.COM") && chris.is_named("stingbro"));
        assert!(!chris.is_named("Signal") && !chris.is_named(""));
        assert!(roster(&root.join("missing")).is_empty());
    }
}
