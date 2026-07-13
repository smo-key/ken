//! Ingest recipes: `.ken/ingests/<slug>.md` — YAML frontmatter + a
//! plain-language instruction body. Shared text, so parsing is defensive
//! (one bad recipe never hides the others) and rewriting preserves fields
//! this version doesn't know about.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::project::Project;
use crate::{Error, Result};

pub const INGESTS_DIR: &str = ".ken/ingests";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Single,
    Collection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Refresh {
    OnChange,
    Manual,
}

/// Partial rules — recipe-level overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RulesOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_threshold_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_days: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedRules {
    pub review_threshold_pct: u8,
    pub stale_days: u32,
}

pub const DEFAULT_RULES: ResolvedRules = ResolvedRules {
    review_threshold_pct: 20,
    stale_days: 30,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Frontmatter {
    name: String,
    #[serde(default)]
    description: String,
    /// Source folders (project-relative). Empty = all included folders.
    #[serde(default)]
    sources: Vec<String>,
    output: String,
    mode: Mode,
    #[serde(default = "default_refresh")]
    refresh: Refresh,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rules: Option<RulesOverride>,
    #[serde(flatten)]
    extra: serde_yaml::Mapping,
}

fn default_refresh() -> Refresh {
    Refresh::OnChange
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub sources: Vec<String>,
    pub output: String,
    pub mode: Mode,
    pub refresh: Refresh,
    pub rules: Option<RulesOverride>,
    pub instruction: String,
    #[serde(skip)]
    pub(crate) extra: serde_yaml::Mapping,
}

impl Recipe {
    /// Build a brand-new recipe (no preserved extra fields).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        slug: String,
        name: String,
        description: String,
        sources: Vec<String>,
        output: String,
        mode: Mode,
        refresh: Refresh,
        rules: Option<RulesOverride>,
        instruction: String,
    ) -> Recipe {
        Recipe {
            slug,
            name,
            description,
            sources,
            output,
            mode,
            refresh,
            rules,
            instruction,
            extra: serde_yaml::Mapping::new(),
        }
    }

    /// Overwrite the form-editable fields, keeping slug and any unknown
    /// frontmatter fields intact.
    #[allow(clippy::too_many_arguments)]
    pub fn update_from_form(
        &mut self,
        name: String,
        description: String,
        sources: Vec<String>,
        output: String,
        mode: Mode,
        refresh: Refresh,
        rules: Option<RulesOverride>,
        instruction: String,
    ) {
        self.name = name;
        self.description = description;
        self.sources = sources;
        self.output = output;
        self.mode = mode;
        self.refresh = refresh;
        self.rules = rules;
        self.instruction = instruction;
    }
}

/// A recipe file that failed to parse — surfaced, never fatal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeError {
    pub slug: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RecipeEntry {
    Ok { recipe: Recipe },
    Broken { error: RecipeError },
}

pub fn ingests_dir(root: &Path) -> PathBuf {
    root.join(INGESTS_DIR)
}

pub fn recipe_path(root: &Path, slug: &str) -> PathBuf {
    ingests_dir(root).join(format!("{slug}.md"))
}

/// List every recipe in the project, broken ones included.
pub fn list(project: &Project) -> Result<Vec<RecipeEntry>> {
    let dir = ingests_dir(&project.root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .map(|e| e.path())
        .collect();
    names.sort();
    for path in names {
        let slug = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        match load(&path, &slug) {
            Ok(recipe) => entries.push(RecipeEntry::Ok { recipe }),
            Err(e) => entries.push(RecipeEntry::Broken {
                error: RecipeError {
                    slug,
                    reason: e.to_string(),
                },
            }),
        }
    }
    Ok(entries)
}

pub fn load_slug(project: &Project, slug: &str) -> Result<Recipe> {
    load(&recipe_path(&project.root, slug), slug)
}

fn load(path: &Path, slug: &str) -> Result<Recipe> {
    let raw = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    let (fm_str, body) = split_frontmatter(&raw).ok_or_else(|| {
        Error::Other("missing frontmatter — the file must start with a --- block".into())
    })?;
    let fm: Frontmatter = serde_yaml::from_str(fm_str)
        .map_err(|e| Error::Other(format!("frontmatter problem: {e}")))?;
    let recipe = Recipe {
        slug: slug.to_string(),
        name: fm.name,
        description: fm.description,
        sources: fm.sources,
        output: fm.output,
        mode: fm.mode,
        refresh: fm.refresh,
        rules: fm.rules,
        instruction: body.trim().to_string(),
        extra: fm.extra,
    };
    validate(&recipe)?;
    Ok(recipe)
}

pub fn save(project: &Project, recipe: &Recipe) -> Result<()> {
    validate(recipe)?;
    let fm = Frontmatter {
        name: recipe.name.clone(),
        description: recipe.description.clone(),
        sources: recipe.sources.clone(),
        output: recipe.output.clone(),
        mode: recipe.mode,
        refresh: recipe.refresh,
        rules: recipe.rules.clone(),
        extra: recipe.extra.clone(),
    };
    let yaml =
        serde_yaml::to_string(&fm).map_err(|e| Error::Other(e.to_string()))?;
    let dir = ingests_dir(&project.root);
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let path = recipe_path(&project.root, &recipe.slug);
    let content = format!("---\n{yaml}---\n\n{}\n", recipe.instruction.trim());
    fs::write(&path, content).map_err(|e| Error::io(&path, e))
}

pub fn delete(project: &Project, slug: &str) -> Result<()> {
    let path = recipe_path(&project.root, slug);
    fs::remove_file(&path).map_err(|e| Error::io(&path, e))
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    let fail = |reason: &str| {
        Err(Error::Other(format!("recipe '{}': {reason}", recipe.slug)))
    };
    if recipe.slug.trim().is_empty()
        || recipe.slug.contains('/')
        || recipe.slug.starts_with('.')
    {
        return fail("the file name must be a simple slug");
    }
    if recipe.name.trim().is_empty() {
        return fail("name can't be empty");
    }
    if recipe.instruction.trim().is_empty() {
        return fail("the instruction can't be empty — say what Ken should extract");
    }
    let out = recipe.output.trim();
    if out.is_empty() {
        return fail("output location can't be empty");
    }
    if Path::new(out).is_absolute() || out.split('/').any(|c| c == "..") {
        return fail("output must be a folder or file inside the project");
    }
    if out.starts_with(".ken") {
        return fail("output can't live inside .ken");
    }
    if recipe.mode == Mode::Single && out.ends_with('/') {
        return fail("a single-document ingest needs a file path, not a folder");
    }
    Ok(())
}

/// Resolve effective rules: recipe override > project defaults > built-ins.
pub fn resolve_rules(recipe: &Recipe, project: &Project) -> ResolvedRules {
    let project_rules: RulesOverride = project
        .config
        .extra
        .get("rules")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let pick = |r: Option<u8>, p: Option<u8>, d: u8| r.or(p).unwrap_or(d);
    let pick32 = |r: Option<u32>, p: Option<u32>, d: u32| r.or(p).unwrap_or(d);
    let ro = recipe.rules.clone().unwrap_or_default();
    ResolvedRules {
        review_threshold_pct: pick(
            ro.review_threshold_pct,
            project_rules.review_threshold_pct,
            DEFAULT_RULES.review_threshold_pct,
        ),
        stale_days: pick32(
            ro.stale_days,
            project_rules.stale_days,
            DEFAULT_RULES.stale_days,
        ),
    }
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let fm = &rest[..end + 1];
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Some((fm, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn project() -> (tempfile::TempDir, Project) {
        let dir = tempdir().unwrap();
        let p = Project::create(dir.path(), "T").unwrap();
        (dir, p)
    }

    fn sample() -> Recipe {
        Recipe {
            slug: "people".into(),
            name: "People".into(),
            description: "Directory of everyone".into(),
            sources: vec!["notes".into(), "specs".into()],
            output: "knowledge/People.md".into(),
            mode: Mode::Single,
            refresh: Refresh::OnChange,
            rules: None,
            instruction: "Extract every person mentioned across the sources.".into(),
            extra: serde_yaml::Mapping::new(),
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        let loaded = load_slug(&p, "people").unwrap();
        assert_eq!(loaded, sample());
    }

    #[test]
    fn list_surfaces_broken_recipes() {
        let (_d, p) = project();
        save(&p, &sample()).unwrap();
        fs::write(recipe_path(&p.root, "broken"), "no frontmatter here").unwrap();
        let entries = list(&p).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| matches!(e, RecipeEntry::Broken { error } if error.slug == "broken")));
        assert!(entries.iter().any(|e| matches!(e, RecipeEntry::Ok { recipe } if recipe.slug == "people")));
    }

    #[test]
    fn unknown_frontmatter_fields_survive() {
        let (_d, p) = project();
        let path = recipe_path(&p.root, "custom");
        fs::create_dir_all(ingests_dir(&p.root)).unwrap();
        fs::write(&path, "---\nname: Custom\noutput: out.md\nmode: single\nfutureField: keep-me\n---\n\nDo the thing.\n").unwrap();

        let mut r = load_slug(&p, "custom").unwrap();
        r.description = "edited".into();
        save(&p, &r).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("futureField: keep-me"), "{raw}");
        assert!(raw.contains("edited"));
    }

    #[test]
    fn validation_rejects_bad_recipes() {
        let (_d, _p) = project();
        let mut r = sample();
        r.output = "/etc/passwd".into();
        assert!(validate(&r).is_err());
        r.output = "../outside.md".into();
        assert!(validate(&r).is_err());
        r.output = ".ken/hidden.md".into();
        assert!(validate(&r).is_err());
        r = sample();
        r.instruction = "  ".into();
        assert!(validate(&r).is_err());
        r = sample();
        r.mode = Mode::Single;
        r.output = "people/".into();
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rules_resolution_precedence() {
        let (_d, mut p) = project();
        let mut r = sample();
        // built-ins
        assert_eq!(resolve_rules(&r, &p), DEFAULT_RULES);
        // project defaults
        p.config.extra.insert(
            "rules".into(),
            serde_json::json!({"reviewThresholdPct": 35, "staleDays": 10}),
        );
        assert_eq!(resolve_rules(&r, &p).review_threshold_pct, 35);
        assert_eq!(resolve_rules(&r, &p).stale_days, 10);
        // recipe override wins
        r.rules = Some(RulesOverride {
            review_threshold_pct: Some(50),
            stale_days: None,
        });
        let resolved = resolve_rules(&r, &p);
        assert_eq!(resolved.review_threshold_pct, 50);
        assert_eq!(resolved.stale_days, 10);
    }

    #[test]
    fn crlf_and_missing_frontmatter() {
        assert!(split_frontmatter("---\r\nname: x\n---\nbody").is_some());
        assert!(split_frontmatter("just text").is_none());
    }
}
