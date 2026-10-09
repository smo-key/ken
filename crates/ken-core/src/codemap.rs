//! The code map: where each symbol is defined, where it is used, what each
//! file imports, and so who calls what and which files depend on which. The
//! structure a person gets from "find usages" and "go to definition", for
//! Claude and for search.
//!
//! Parsed locally with tree-sitter and each grammar's own tags query, the
//! approach GitHub's code navigation takes: definitions and references by
//! name, no type resolution, no model. Imports come from a small pattern per
//! language and are resolved to files when asked. Filled as files are
//! indexed (`scan`), kept in each project's index.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use serde::Serialize;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::db::Db;
use crate::Result;

/// One definition or reference. Lines are 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Symbol {
    pub path: String,
    pub name: String,
    /// The grammar's syntax type: `function`, `method`, `class`,
    /// `interface`, `module`, `call`, `type`, `implementation`, …
    pub kind: String,
    pub line: i64,
    pub end_line: i64,
    pub is_def: bool,
    pub docs: Option<String>,
}

/// What one file holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMap {
    pub symbols: Vec<Symbol>,
    /// Import targets as written (`crate::db`, `./auth`, `app.core.auth`).
    pub imports: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    CSharp,
}

impl Lang {
    pub fn of(path: &str) -> Option<Lang> {
        let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
        Some(match ext.as_str() {
            "rs" => Lang::Rust,
            "py" | "pyi" => Lang::Python,
            "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "ts" | "mts" | "cts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "go" => Lang::Go,
            "java" => Lang::Java,
            "cs" => Lang::CSharp,
            _ => return None,
        })
    }

    fn config(self) -> Option<TagsConfiguration> {
        let js = tree_sitter_javascript::TAGS_QUERY;
        let ts = tree_sitter_typescript::TAGS_QUERY;
        let built = match self {
            Lang::Rust => TagsConfiguration::new(tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::TAGS_QUERY, ""),
            Lang::Python => TagsConfiguration::new(tree_sitter_python::LANGUAGE.into(), tree_sitter_python::TAGS_QUERY, ""),
            Lang::JavaScript => TagsConfiguration::new(tree_sitter_javascript::LANGUAGE.into(), js, ""),
            // TypeScript's tags query covers only what TypeScript adds
            // (interfaces, modules); functions and classes are JavaScript's.
            Lang::TypeScript => TagsConfiguration::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &format!("{js}\n{ts}\n{TS_MORE}"), ""),
            Lang::Tsx => TagsConfiguration::new(tree_sitter_typescript::LANGUAGE_TSX.into(), &format!("{js}\n{ts}\n{TS_MORE}"), ""),
            Lang::Go => TagsConfiguration::new(tree_sitter_go::LANGUAGE.into(), tree_sitter_go::TAGS_QUERY, ""),
            Lang::Java => TagsConfiguration::new(tree_sitter_java::LANGUAGE.into(), &format!("{}\n{JAVA_MORE}", tree_sitter_java::TAGS_QUERY), ""),
            // Its query repeats the namespace pattern with a bare `@module`
            // capture, which tree-sitter-tags refuses: without that line.
            Lang::CSharp => TagsConfiguration::new(
                tree_sitter_c_sharp::LANGUAGE.into(),
                &tree_sitter_c_sharp::TAGS_QUERY.lines().filter(|l| !l.trim_end().ends_with(") @module")).collect::<Vec<_>>().join("
"),
                "",
            ),
        };
        built.ok()
    }
}

/// Java definitions the grammar's own tags query leaves out: enums and their
/// constants, records, annotation types, and fields, a `static final` field
/// as a constant. On 2026-10-06 the Shattered Realms map held no enum and no
/// field, so `StashTab` and `MAX_ROWS` had no definition. When two patterns
/// tag one name the first wins, so the constant pattern comes before the
/// field one.
const JAVA_MORE: &str = r#"
(enum_declaration name: (identifier) @name) @definition.enum
(enum_constant name: (identifier) @name) @definition.constant
(record_declaration name: (identifier) @name) @definition.record
(annotation_type_declaration name: (identifier) @name) @definition.interface
(field_declaration (modifiers "static" "final") declarator: (variable_declarator name: (identifier) @name)) @definition.constant
(field_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.field
(constant_declaration declarator: (variable_declarator name: (identifier) @name)) @definition.constant
"#;

/// TypeScript definitions its tags query leaves out: enums, their members,
/// and type aliases.
const TS_MORE: &str = r#"
(enum_declaration name: (identifier) @name) @definition.enum
(enum_body name: (property_identifier) @name @definition.constant)
(enum_assignment name: (property_identifier) @name) @definition.constant
(type_alias_declaration name: (type_identifier) @name) @definition.type
"#;

/// Files larger than this are not parsed (generated code, vendored bundles).
const MAX_BYTES: usize = 1_000_000;

thread_local! {
    // Building a tags configuration compiles its query: once per language per
    // thread, not per file.
    static CONFIGS: RefCell<HashMap<Lang, Option<TagsConfiguration>>> = RefCell::new(HashMap::new());
    static CONTEXT: RefCell<TagsContext> = RefCell::new(TagsContext::new());
}

/// The code map of one file, or None when its language is not mapped.
pub fn map_file(path: &str, text: &str) -> Option<FileMap> {
    let lang = Lang::of(path)?;
    if text.len() > MAX_BYTES {
        return Some(FileMap::default());
    }
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let line_of = |byte: usize| line_starts.partition_point(|&s| s <= byte) as i64;
    let mut symbols = Vec::new();
    CONFIGS.with(|configs| {
        let mut configs = configs.borrow_mut();
        let Some(config) = configs.entry(lang).or_insert_with(|| lang.config()).as_ref() else {
            return;
        };
        CONTEXT.with(|ctx| {
            let mut ctx = ctx.borrow_mut();
            let Ok((tags, _)) = ctx.generate_tags(config, text.as_bytes(), None) else {
                return;
            };
            for tag in tags.flatten() {
                let Some(name) = text.get(tag.name_range.clone()) else { continue };
                if name.is_empty() || name.len() > 200 {
                    continue;
                }
                symbols.push(Symbol {
                    path: path.to_string(),
                    name: name.to_string(),
                    kind: config.syntax_type_name(tag.syntax_type_id).to_string(),
                    line: line_of(tag.range.start),
                    end_line: line_of(tag.range.end.saturating_sub(1).max(tag.range.start)),
                    is_def: tag.is_definition,
                    docs: tag
                        .docs
                        .or_else(|| tag.is_definition.then(|| leading_comment(text, &line_starts, line_of(tag.range.start))).flatten())
                        .map(|d| d.chars().take(500).collect()),
                });
            }
        });
    });
    Some(FileMap { symbols, imports: imports_of(lang, text) })
}

/// The comment lines right above `line` (1-based): a definition's docs
/// when the grammar's query does not collect them.
fn leading_comment(text: &str, line_starts: &[usize], line: i64) -> Option<String> {
    let line_text = |n: i64| -> Option<&str> {
        let i = usize::try_from(n - 1).ok()?;
        let start = *line_starts.get(i)?;
        let end = line_starts.get(i + 1).map(|e| e - 1).unwrap_or(text.len());
        text.get(start..end).map(|l| l.trim())
    };
    let mut lines = Vec::new();
    let mut n = line - 1;
    while n >= 1 {
        let Some(l) = line_text(n) else { break };
        let body = ["///", "//!", "//", "#", "*", "/**", "--"].iter().find_map(|p| l.strip_prefix(p));
        match body {
            Some(b) if !l.starts_with("#[") && !l.starts_with("#!") => lines.push(b.trim_start_matches('/').trim().to_string()),
            _ if l.starts_with("#[") || l.starts_with('@') => {} // an attribute or decorator between
            _ => break,
        }
        n -= 1;
    }
    lines.reverse();
    let doc = lines.join(" ").trim().to_string();
    (!doc.is_empty()).then_some(doc)
}

/// What a file imports, as written, by a small pattern per language.
pub fn imports_of(lang: Lang, text: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let quoted = |s: &str| -> Option<String> {
        let start = s.find(['"', '\''])?;
        let q = s[start..].chars().next()?;
        let rest = &s[start + 1..];
        rest.find(q).map(|end| rest[..end].to_string())
    };
    let mut in_go_block = false;
    for raw in text.lines() {
        let line = raw.trim();
        match lang {
            Lang::Rust => {
                let l = line.strip_prefix("pub ").unwrap_or(line);
                let l = l.strip_prefix("pub(crate) ").unwrap_or(l);
                if let Some(rest) = l.strip_prefix("use ") {
                    let target = rest.trim_end_matches(';').split(['{', ' ']).next().unwrap_or("").trim_end_matches("::");
                    if !target.is_empty() {
                        out.insert(target.to_string());
                    }
                } else if let Some(rest) = l.strip_prefix("mod ") {
                    if let Some(name) = rest.strip_suffix(';') {
                        out.insert(format!("self::{}", name.trim()));
                    }
                }
            }
            Lang::Python => {
                if let Some(rest) = line.strip_prefix("from ") {
                    if let Some(module) = rest.split_whitespace().next() {
                        out.insert(module.to_string());
                    }
                } else if let Some(rest) = line.strip_prefix("import ") {
                    for m in rest.split(',') {
                        if let Some(module) = m.split_whitespace().next() {
                            out.insert(module.to_string());
                        }
                    }
                }
            }
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
                let is_import = line.starts_with("import ") || line.starts_with("export ") && line.contains(" from ");
                if is_import || line.contains("require(") || line.contains("import(") {
                    let from = line.rsplit_once(" from ").map(|(_, f)| f).unwrap_or(line);
                    let from = from.split_once("require(").map(|(_, r)| r).unwrap_or(from);
                    let from = from.split_once("import(").map(|(_, r)| r).unwrap_or(from);
                    if let Some(target) = quoted(from) {
                        out.insert(target);
                    }
                }
            }
            Lang::Go => {
                if line.starts_with("import (") {
                    in_go_block = true;
                } else if in_go_block {
                    if line.starts_with(')') {
                        in_go_block = false;
                    } else if let Some(target) = quoted(line) {
                        out.insert(target);
                    }
                } else if let Some(rest) = line.strip_prefix("import ") {
                    if let Some(target) = quoted(rest) {
                        out.insert(target);
                    }
                }
            }
            Lang::Java => {
                if let Some(rest) = line.strip_prefix("import ") {
                    let rest = rest.strip_prefix("static ").unwrap_or(rest);
                    out.insert(rest.trim_end_matches(';').trim().to_string());
                }
            }
            Lang::CSharp => {
                if let Some(rest) = line.strip_prefix("using ") {
                    if !rest.contains('(') && !rest.contains('=') {
                        out.insert(rest.trim_end_matches(';').trim().to_string());
                    }
                }
            }
        }
    }
    out.into_iter().filter(|t| !t.is_empty()).collect()
}

// ---------------------------------------------------------------- queries

/// Where `name` is defined.
pub fn definitions(db: &Db, name: &str) -> Result<Vec<Symbol>> {
    db.code_symbols(Some(name), None, Some(true), 200)
}

/// One use of a symbol, with the definition it sits in (its caller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub path: String,
    pub line: i64,
    pub kind: String,
    /// The function, method or class the use is inside, if any.
    pub within: Option<String>,
}

/// Where `name` is used, each with the definition that holds the use.
pub fn usages(db: &Db, name: &str, limit: usize) -> Result<Vec<Usage>> {
    let refs = db.code_symbols(Some(name), None, Some(false), limit)?;
    let mut defs_by_path: HashMap<String, Vec<Symbol>> = HashMap::new();
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        let defs = match defs_by_path.get(&r.path) {
            Some(d) => d,
            None => {
                let d = db.code_symbols(None, Some(&r.path), Some(true), 5000)?;
                defs_by_path.entry(r.path.clone()).or_insert(d)
            }
        };
        let within = defs
            .iter()
            .filter(|d| d.line <= r.line && r.line <= d.end_line && d.name != r.name)
            .min_by_key(|d| d.end_line - d.line)
            .map(|d| d.name.clone());
        out.push(Usage { path: r.path, line: r.line, kind: r.kind, within });
    }
    Ok(out)
}

/// Every other line in code that names `name` as a whole word: the uses a
/// grammar's tags query does not count as calls (passed as a value, as in
/// `Depends(get_current_user)`, re-exported, named in a type or a decorator).
/// Candidate files come from the keyword index; lines from their stored
/// text. `known` holds the (path, line) already reported, left out here.
pub fn mentions(db: &Db, name: &str, known: &[(String, i64)], limit: usize) -> Result<Vec<Usage>> {
    let mut paths: Vec<String> = Vec::new();
    for hit in db.search_chunks_fts(name, 400)? {
        if Lang::of(&hit.path).is_some() && !paths.contains(&hit.path) {
            paths.push(hit.path);
        }
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for path in paths {
        let Some(text) = db.get_text(&path)? else { continue };
        let defs = db.code_symbols(None, Some(&path), Some(true), 5000)?;
        for (i, line) in text.lines().enumerate() {
            let n = i as i64 + 1;
            let whole = line.match_indices(name).any(|(at, _)| {
                let before = line[..at].chars().next_back().is_none_or(|c| !is_word(c));
                let after = line[at + name.len()..].chars().next().is_none_or(|c| !is_word(c));
                before && after
            });
            if !whole || known.iter().any(|(p, l)| p == &path && *l == n) {
                continue;
            }
            if defs.iter().any(|d| d.name == name && d.line == n) {
                continue; // the definition itself
            }
            let within = defs
                .iter()
                .filter(|d| d.line <= n && n <= d.end_line && d.name != name)
                .min_by_key(|d| d.end_line - d.line)
                .map(|d| d.name.clone());
            out.push(Usage { path: path.clone(), line: n, kind: "mention".into(), within });
            if out.len() >= limit {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// A file's definitions, in order, each with how deep it is nested.
pub fn outline(db: &Db, path: &str) -> Result<Vec<(usize, Symbol)>> {
    let defs = db.code_symbols(None, Some(path), Some(true), 5000)?;
    let mut out = Vec::with_capacity(defs.len());
    let mut open: Vec<i64> = Vec::new(); // end lines of the enclosing defs
    for d in defs {
        while open.last().is_some_and(|&end| end < d.line) {
            open.pop();
        }
        out.push((open.len(), d.clone()));
        open.push(d.end_line);
    }
    Ok(out)
}

/// A file's neighbours in the code: what it imports and what imports it,
/// resolved to indexed files where Ken can tell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Related {
    pub imports: Vec<String>,
    /// Imports Ken could not tie to a file (a library, most likely).
    pub external: Vec<String>,
    pub imported_by: Vec<String>,
}

pub fn related(db: &Db, path: &str) -> Result<Related> {
    let files: Vec<String> = db.page_paths()?;
    let all = db.code_imports()?;
    let mut out = Related::default();
    for (from, target) in &all {
        if from == path {
            match resolve_import(path, target, &files) {
                Some(f) => {
                    if !out.imports.contains(&f) {
                        out.imports.push(f)
                    }
                }
                None => {
                    if !out.external.contains(target) {
                        out.external.push(target.clone())
                    }
                }
            }
        } else if resolve_import(from, target, &files).as_deref() == Some(path) && !out.imported_by.contains(from) {
            out.imported_by.push(from.clone());
        }
    }
    out.imports.sort();
    out.imported_by.sort();
    out.external.sort();
    Ok(out)
}

/// The indexed file an import names, when one clearly does: a relative path
/// (`./auth`, `../lib/db`), a module path (`app.core.auth`, `crate::db`,
/// `com.acme.Auth`) matched against the file's own folders, else None.
pub fn resolve_import(from: &str, target: &str, files: &[String]) -> Option<String> {
    let lang = Lang::of(from)?;
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let exts: &[&str] = match lang {
        Lang::Rust => &["rs"],
        Lang::Python => &["py"],
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => &["ts", "tsx", "js", "jsx", "mjs", "svelte"],
        Lang::Go => &["go"],
        Lang::Java => &["java"],
        Lang::CSharp => &["cs"],
    };
    let exists = |p: &str| files.iter().any(|f| f == p);
    let with_exts = |base: &str| -> Option<String> {
        if exists(base) {
            return Some(base.to_string());
        }
        for e in exts {
            for candidate in [format!("{base}.{e}"), format!("{base}/index.{e}"), format!("{base}/mod.{e}"), format!("{base}/__init__.{e}")] {
                if exists(&candidate) {
                    return Some(candidate);
                }
            }
        }
        None
    };
    // Relative: ./x, ../x
    if target.starts_with("./") || target.starts_with("../") {
        let mut parts: Vec<&str> = if dir.is_empty() { Vec::new() } else { dir.split('/').collect() };
        for seg in target.split('/') {
            match seg {
                "." | "" => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        return with_exts(&parts.join("/"));
    }
    // A module path: its segments as folders, matched as the tail of a file's
    // path (the same module may sit under any source root).
    let sep = if target.contains("::") { "::" } else { "." };
    let mut segs: Vec<&str> = target.split(sep).filter(|s| !s.is_empty()).collect();
    if lang == Lang::Rust {
        segs.retain(|s| !matches!(*s, "crate" | "self" | "super"));
    }
    if lang == Lang::Python && target.starts_with('.') {
        // `from .auth import x`: next to this file.
        let rel = target.trim_start_matches('.').replace('.', "/");
        return with_exts(&if dir.is_empty() { rel } else { format!("{dir}/{rel}") });
    }
    // Longest tail first: `crate::db::Db` is db.rs, not Db.rs.
    for take in (1..=segs.len()).rev() {
        let tail = segs[..take].join("/");
        for e in exts {
            for suffix in [format!("{tail}.{e}"), format!("{tail}/mod.{e}"), format!("{tail}/__init__.{e}"), format!("{tail}/index.{e}")] {
                let slashed = format!("/{suffix}");
                let hit: Vec<&String> = files.iter().filter(|f| *f == &suffix || f.ends_with(&slashed)).collect();
                if let [only] = hit.as_slice() {
                    return Some((*only).clone());
                }
                // Several files end so: the one nearest the importer.
                if hit.len() > 1 {
                    if let Some(near) = hit.iter().max_by_key(|f| common_prefix(f, from)) {
                        return Some((*near).clone());
                    }
                }
            }
        }
    }
    None
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rust_file_maps_its_definitions_calls_and_imports() {
        let src = "use crate::db::Db;\nmod auth;\n\n/// Saves the world.\npub fn save(db: &Db) {\n    write_all(db);\n}\n\nstruct Store;\n\nimpl Store {\n    fn open() -> Store {\n        Store\n    }\n}\n";
        let map = map_file("src/save.rs", src).unwrap();
        let defs: Vec<(&str, &str, i64)> = map.symbols.iter().filter(|s| s.is_def).map(|s| (s.name.as_str(), s.kind.as_str(), s.line)).collect();
        assert!(defs.contains(&("save", "function", 5)), "{defs:?}");
        assert!(defs.iter().any(|d| d.0 == "Store"), "{defs:?}");
        assert!(defs.iter().any(|d| d.0 == "open"), "{defs:?}");
        let save = map.symbols.iter().find(|s| s.name == "save" && s.is_def).unwrap();
        assert_eq!(save.end_line, 7);
        assert!(save.docs.as_deref().is_some_and(|d| d.contains("Saves the world")));
        assert!(map.symbols.iter().any(|s| s.name == "write_all" && !s.is_def && s.line == 6), "{:?}", map.symbols);
        assert_eq!(map.imports, vec!["crate::db::Db".to_string(), "self::auth".to_string()]);
    }

    #[test]
    fn python_typescript_go_java_and_csharp_all_map() {
        let py = map_file("app/core/auth.py", "from app.core import config\nimport jwt\n\ndef validate(token):\n    return jwt.decode(token)\n\nclass User:\n    pass\n").unwrap();
        assert!(py.symbols.iter().any(|s| s.name == "validate" && s.is_def && s.line == 4));
        assert!(py.symbols.iter().any(|s| s.name == "User" && s.is_def));
        assert_eq!(py.imports, vec!["app.core".to_string(), "jwt".to_string()]);

        let ts = map_file("src/lib/api.ts", "import { invoke } from \"@tauri-apps/api/core\";\nimport { parse } from './citation';\nexport interface Hit { path: string }\nexport function search(q: string) { return invoke(q); }\n").unwrap();
        assert!(ts.symbols.iter().any(|s| s.name == "search" && s.is_def), "{:?}", ts.symbols);
        assert!(ts.symbols.iter().any(|s| s.name == "Hit" && s.is_def), "interfaces come from TypeScript's own query: {:?}", ts.symbols);
        assert!(ts.symbols.iter().any(|s| s.name == "invoke" && !s.is_def));
        assert_eq!(ts.imports, vec!["./citation".to_string(), "@tauri-apps/api/core".to_string()]);

        let go = map_file("cmd/main.go", "package main\n\nimport (\n\t\"fmt\"\n\t\"acme/store\"\n)\n\nfunc main() { fmt.Println(store.Open()) }\n").unwrap();
        assert!(go.symbols.iter().any(|s| s.name == "main" && s.is_def));
        assert_eq!(go.imports, vec!["acme/store".to_string(), "fmt".to_string()]);

        let java = map_file("src/Auth.java", "import com.acme.Store;\npublic class Auth { void check() { Store.open(); } }\n").unwrap();
        assert!(java.symbols.iter().any(|s| s.name == "Auth" && s.is_def));
        assert_eq!(java.imports, vec!["com.acme.Store".to_string()]);

        let cs = map_file("Auth.cs", "using Acme.Store;\nclass Auth { void Check() { } }\n").unwrap();
        assert!(cs.symbols.iter().any(|s| s.name == "Auth" && s.is_def));
        assert_eq!(cs.imports, vec!["Acme.Store".to_string()]);

        assert!(map_file("README.md", "# no").is_none(), "not code");
    }

    #[test]
    fn java_enums_records_fields_and_constants_are_definitions() {
        let src = "package a;\npublic enum StashTab {\n    ITEMS,\n    GEAR;\n    public static final short MAX_ROWS = 7;\n    private int count;\n    public short defaultCapacity() { return 9; }\n}\nrecord Slot(int i) {}\n@interface Marker {}\ninterface Limits { int CAP = 3; }\n";
        let map = map_file("src/StashTab.java", src).unwrap();
        let defs: Vec<(&str, &str)> = map.symbols.iter().filter(|s| s.is_def).map(|s| (s.name.as_str(), s.kind.as_str())).collect();
        for want in [
            ("StashTab", "enum"),
            ("ITEMS", "constant"),
            ("GEAR", "constant"),
            ("MAX_ROWS", "constant"),
            ("count", "field"),
            ("defaultCapacity", "method"),
            ("Slot", "record"),
            ("Marker", "interface"),
            ("Limits", "interface"),
            ("CAP", "constant"),
        ] {
            assert!(defs.contains(&want), "{want:?} in {defs:?}");
        }
        let max = map.symbols.iter().find(|s| s.name == "MAX_ROWS").unwrap();
        assert_eq!(max.line, 5);
    }

    #[test]
    fn typescript_enums_members_and_type_aliases_are_definitions() {
        let src = "export enum Tier { Crude, Sturdy = 2 }\nexport type Path = string[];\nexport interface Hit { path: Path }\n";
        let map = map_file("src/tier.ts", src).unwrap();
        let defs: Vec<(&str, &str)> = map.symbols.iter().filter(|s| s.is_def).map(|s| (s.name.as_str(), s.kind.as_str())).collect();
        for want in [("Tier", "enum"), ("Crude", "constant"), ("Sturdy", "constant"), ("Path", "type"), ("Hit", "interface")] {
            assert!(defs.contains(&want), "{want:?} in {defs:?}");
        }
        let tsx = map_file("src/a.tsx", "enum Mode { On }\ntype P = { x: number };\n").unwrap();
        assert!(tsx.symbols.iter().any(|s| s.name == "Mode" && s.is_def), "{:?}", tsx.symbols);
        assert!(tsx.symbols.iter().any(|s| s.name == "P" && s.is_def), "{:?}", tsx.symbols);
    }

    #[test]
    fn imports_resolve_to_files() {
        let files: Vec<String> = [
            "src/lib/api.ts",
            "src/lib/citation.ts",
            "src/chat/index.ts",
            "crates/core/src/db.rs",
            "crates/core/src/auth/mod.rs",
            "app/core/auth.py",
            "app/core/__init__.py",
            "src/com/acme/Store.java",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let r = |from: &str, t: &str| resolve_import(from, t, &files);
        assert_eq!(r("src/lib/api.ts", "./citation").as_deref(), Some("src/lib/citation.ts"));
        assert_eq!(r("src/lib/api.ts", "../chat").as_deref(), Some("src/chat/index.ts"));
        assert_eq!(r("crates/core/src/lib.rs", "crate::db::Db").as_deref(), Some("crates/core/src/db.rs"));
        assert_eq!(r("crates/core/src/lib.rs", "self::auth").as_deref(), Some("crates/core/src/auth/mod.rs"));
        assert_eq!(r("app/api/routes.py", "app.core.auth").as_deref(), Some("app/core/auth.py"));
        assert_eq!(r("app/core/x.py", ".auth").as_deref(), Some("app/core/auth.py"));
        assert_eq!(r("src/com/acme/Auth.java", "com.acme.Store").as_deref(), Some("src/com/acme/Store.java"));
        assert_eq!(r("src/lib/api.ts", "@tauri-apps/api/core"), None, "a library");
    }
}

