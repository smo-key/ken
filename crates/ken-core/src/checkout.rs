//! What a repo's checkout says now, for the wiki draft: what the code wires
//! together at start-up, the versions its build files pin, the values of the
//! data files its briefs name, and every name its code defines. A brief or a
//! README says what was true when it was written; all ten stale claims of
//! the second Shattered Realms draft (2026-10-06) copied such a line after
//! the code had moved on: 21 installers where `setup()` called 36, an
//! engine pin of `0.7.0-pre.3` where `gradle.properties` said `pre.5`, two
//! tests that no longer existed, rarity defaults, a mob count.
//!
//! Nothing here knows a product or a stack: entry points are what the build
//! files declare or what a file is called, wiring is a function's name, and
//! values are read the same way from any JSON, YAML or properties file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::codemap::{map_file, Lang};

/// Characters of [`registrations`] for one repo, and of one function in it.
const WIRING_MAX: usize = 12_000;
const ONE_FN_MAX: usize = 9_000;
/// Characters of one data file's values. A file is given whole or not at
/// all: an 800-character cut on 2026-10-07 dropped Sword and Staff from
/// `MobWeaponDamage.json`'s windups and three of six weapons from
/// `WeaponChargeScaling.json`, and the page gave the cut set as the whole.
pub const ONE_FILE_MAX: usize = 64_000;
/// Characters of a data file read before any larger one.
const SMALL_FILE: usize = 4_000;
/// A file larger than this is not read for names or values.
const MAX_BYTES: u64 = 1_000_000;

/// The functions that wire a program together, compared without case or
/// `_`: an entry point's `main` and `run`, and the start-up and registration
/// hooks of the common frameworks and plugin hosts. Only the last group
/// counts outside an entry-point file, where `run` and `start` mean anything.
const ENTRY_WIRING: &[&str] = &["main", "run", "start", "init", "initialize", "register"];
const WIRING: &[&str] =
    &["setup", "onenable", "onload", "oninitialize", "registerall", "install", "configure", "configureservices", "bootstrap", "createapp"];

fn wiring_key(name: &str) -> String {
    name.chars().filter(|c| *c != '_').collect::<String>().to_lowercase()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn read(root: &Path, rel: &str) -> Option<String> {
    let path = root.join(rel);
    if fs::metadata(&path).ok()?.len() > MAX_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).replace("\r\n", "\n"))
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} […]", &s[..end])
}

// ------------------------------------------------------------ entry points

/// Build files that may declare where a program starts.
const DECLARES_ENTRY: &[&str] = &[
    "gradle.properties",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "manifest.json",
    "plugin.yml",
    "plugin.yaml",
    "package.json",
    "pyproject.toml",
    "Cargo.toml",
];

/// The tracked file a declared entry names: a path (`./src/cli.ts`), a
/// class (`dev.app.Main`), or a Python `pkg.module:function`.
fn declared_file(value: &str, tracked: &[String]) -> Option<String> {
    let v = value.trim().trim_matches(['"', '\'', ',']).trim_start_matches("./");
    if v.is_empty() {
        return None;
    }
    if tracked.iter().any(|f| f == v) {
        return Some(v.to_string());
    }
    let module = v.split_once(':').map_or(v, |(m, _)| m);
    let is_dotted = module.contains('.') && module.split('.').all(|s| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_'));
    if !is_dotted {
        return None;
    }
    let base = module.replace('.', "/");
    ["java", "kt", "scala", "groovy", "py", "cs"].iter().find_map(|ext| {
        let suffix = format!("{base}.{ext}");
        tracked.iter().find(|f| *f == &suffix || f.ends_with(&format!("/{suffix}"))).cloned()
    })
}

/// The files a repo says it starts from: what its build files and plugin
/// manifests declare (a main class, `main` and `bin`, a script), and any code
/// file called `main`, `__main__` or `Program`, with a Rust crate's `lib.rs`
/// beside its `main.rs`. At most ten, declared ones first.
pub fn entry_files(root: &Path, tracked: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |f: String, out: &mut Vec<String>| {
        if !out.contains(&f) {
            out.push(f);
        }
    };
    let declaring = tracked.iter().filter(|f| DECLARES_ENTRY.contains(&basename(f)) && f.matches('/').count() <= 3);
    for file in declaring {
        let Some(text) = read(root, file) else { continue };
        let mut in_scripts = false;
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_scripts = t.contains("scripts") || t.contains("bin");
            }
            let lower = t.to_lowercase();
            if !(lower.contains("main") || lower.contains("\"bin\"") || in_scripts) || t.starts_with(['#', '/']) {
                continue;
            }
            // The value: after `=` or `:`, else the quoted strings of the line.
            let rhs = t.split_once(['=', ':']).map_or(t, |(_, r)| r);
            let quoted: Vec<&str> = rhs.split('"').skip(1).step_by(2).collect();
            let values: Vec<&str> = if quoted.is_empty() { vec![rhs] } else { quoted };
            for v in values {
                if let Some(f) = declared_file(v, tracked).filter(|f| Lang::of(f).is_some()) {
                    add(f, &mut out);
                }
            }
        }
    }
    for f in tracked.iter().filter(|f| Lang::of(f).is_some()) {
        let name = basename(f);
        let s = stem(name);
        if ["main", "Main", "__main__", "Program"].contains(&s) {
            add(f.clone(), &mut out);
            if name == "main.rs" {
                let lib = format!("{}lib.rs", &f[..f.len() - name.len()]);
                if tracked.contains(&lib) {
                    add(lib, &mut out);
                }
            }
        }
    }
    out.truncate(10);
    out
}

// ------------------------------------------------------------------ wiring

/// A body line as a statement worth listing: not blank, a comment, a lone
/// brace, or a value read back into a field (`x = y.config();`).
fn statement(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || ["//", "#", "*", "/*", "--", "@"].iter().any(|p| t.starts_with(p)) {
        return None;
    }
    if t.chars().all(|c| "{}()[];,".contains(c)) {
        return None;
    }
    let read_back = t.contains(" = ") && t.ends_with("();") && t.matches('(').count() == 1;
    (!read_back).then_some(t)
}

/// One wiring function as listed: `path:line name(): …`, the line its name
/// is on (a definition starts at its annotations), and its statements, each
/// with its line: the body starts after the line that opens it (`{`, or a
/// Python `:`).
fn wiring_block(path: &str, name: &str, line: i64, end: i64, text: &str) -> (usize, String) {
    let lines: Vec<&str> = text.lines().collect();
    let last = usize::try_from(end).unwrap_or(0).min(lines.len());
    let first = usize::try_from(line).unwrap_or(1).max(1);
    let named = (first..=last).find(|n| lines[n - 1].contains(&format!("{name}("))).unwrap_or(first);
    let opens = (named..=last).find(|n| {
        let t = lines[n - 1].trim_end();
        t.contains('{') || t.ends_with(':')
    });
    let Some(opens) = opens else { return (0, String::new()) };
    let body: Vec<String> = (opens + 1..last)
        .filter_map(|n| statement(lines[n - 1]).map(|s| format!("{n}: {}", clip(s, 160))))
        .collect();
    let head = format!("{path}:{named} {name}(): {} statements, in order (comments and values read back left out)\n", body.len());
    (body.len(), clip(&format!("{head}{}\n", body.join("\n")), ONE_FN_MAX))
}

/// What the code wires together at start-up, as the checkout has it: the
/// start-up and registration functions of the entry-point files
/// ([`entry_files`]), each statement with its line, then one line for each
/// start-up or registration function elsewhere (`setup`, `install`,
/// `configure`, `onEnable`, …), the largest first, while [`WIRING_MAX`]
/// lasts. Tests are left out. A brief's list of what `setup()` installs goes
/// stale with the next installer; this does not. None when the repo has no
/// such function.
pub fn registrations(root: &Path, tracked: &[String]) -> Option<String> {
    let entries = entry_files(root, tracked);
    let mut first: Vec<(usize, String)> = Vec::new();
    let mut rest: Vec<(String, String, usize)> = Vec::new();
    let needles: Vec<String> = WIRING.iter().map(|w| format!("{w}(")).collect();
    for f in tracked.iter().filter(|f| Lang::of(f).is_some() && !is_test_path(f)) {
        let is_entry = entries.contains(f);
        let Some(text) = read(root, f) else { continue };
        if !is_entry {
            let lower = text.to_lowercase().replace('_', "");
            if !needles.iter().any(|n| lower.contains(n.as_str())) {
                continue;
            }
        }
        let Some(map) = map_file(f, &text) else { continue };
        for s in map.symbols.iter().filter(|s| s.is_def && matches!(s.kind.as_str(), "function" | "method")) {
            let key = wiring_key(&s.name);
            let wanted = WIRING.contains(&key.as_str()) || (is_entry && ENTRY_WIRING.contains(&key.as_str()));
            if !wanted || s.end_line <= s.line {
                continue;
            }
            let (n, block) = wiring_block(f, &s.name, s.line, s.end_line, &text);
            if n == 0 {
                continue;
            }
            if is_entry {
                first.push((entries.iter().position(|e| e == f).unwrap_or(usize::MAX), block));
            } else {
                let head = block.lines().next().unwrap_or_default().split(", in order").next().unwrap_or_default().to_string();
                rest.push((f.clone(), head, n));
            }
        }
    }
    first.sort_by_key(|b| b.0);
    let mut out = String::new();
    for (_, block) in first {
        if out.len() + block.len() > WIRING_MAX {
            if out.is_empty() {
                out.push_str(&clip(&block, WIRING_MAX));
            }
            break;
        }
        out.push_str(&block);
        out.push('\n');
    }
    // Then where everything else registers, one line a function, the
    // largest first, in path order.
    rest.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    let mut index: Vec<(String, String)> = Vec::new();
    let mut used = out.len();
    for (path, head, _) in rest {
        if used + head.len() + 1 > WIRING_MAX {
            break;
        }
        used += head.len() + 1;
        index.push((path, head));
    }
    index.sort();
    if !index.is_empty() {
        out.push_str("other start-up and registration functions, by file:\n");
        out.push_str(&index.into_iter().map(|(_, h)| h).collect::<Vec<_>>().join("\n"));
        out.push('\n');
    }
    (!out.is_empty()).then_some(out)
}

/// Whether a path is a test's: a test or fixture folder, or a file named as
/// a test (`x.test.ts`, `x_test.go`, `XTest.java`, `test_x.py`).
pub fn is_test_path(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').collect();
    let name = segs.last().copied().unwrap_or_default();
    let s = stem(name);
    segs[..segs.len() - 1].iter().any(|d| matches!(*d, "test" | "tests" | "__tests__" | "e2e" | "spec" | "fixtures" | "testdata"))
        || name.contains(".test.")
        || name.contains(".spec.")
        || s.ends_with("_test")
        || s.starts_with("test_")
        || (s.ends_with("Test") || s.ends_with("Tests")) && s.len() > 4
}

// ---------------------------------------------------------------- versions

/// One version a build file pins: the file, the key (`hytale_version`,
/// `dependencies.vitest`) and the version, its range marks (`^`, `~`, `>=`)
/// and any leading `v` dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub file: String,
    pub key: String,
    pub value: String,
}

/// `s` as a version, normalized: digits and dots with at least one dot, then
/// an optional `-pre.5` or `+build`. `^4.1.10` is `4.1.10`; `0.+`, `*` and
/// `workspace:*` are not versions.
pub fn as_version(s: &str) -> Option<String> {
    let t = s.trim().trim_matches(['"', '\'']).trim_start_matches(['^', '~', '>', '<', '=', ' ']);
    let t = t.strip_prefix(['v', 'V']).unwrap_or(t);
    let core_end = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let core = &t[..core_end];
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let tail = &t[core_end..];
    let tail_ok = tail.is_empty()
        || (tail.starts_with(['-', '+'])
            && tail.len() > 1
            && tail[1..].chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+'));
    tail_ok.then(|| t.to_string())
}

/// The numbers of a version before any `-` or `+`: `0.7.0` of `0.7.0-pre.5`.
pub fn version_core(v: &str) -> &str {
    v.split(['-', '+']).next().unwrap_or(v)
}

/// The build files whose versions are read, by file name.
const PIN_FILES: &[&str] = &["gradle.properties", "package.json", "Cargo.toml", "pyproject.toml", "tauri.conf.json", "go.mod"];

/// The `key = value` lines of a properties file, comments left out.
fn properties(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with(['#', '!']))
        .filter_map(|l| l.split_once(['=', ':']).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

/// `name = "1.2"` and `name = { version = "1.2", … }` lines of the tables of
/// a TOML file whose header passes `table`, as (table.name, value).
fn toml_versions(text: &str, table: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            current = line.trim_matches(['[', ']']).to_string();
            continue;
        }
        if !table(&current) || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = match v.strip_prefix('{') {
            Some(inline) => inline.split(',').find_map(|kv| kv.split_once('=').filter(|(k, _)| k.trim() == "version").map(|(_, v)| v.trim())),
            None => Some(v),
        };
        if let Some(v) = v {
            out.push((format!("{current}.{}", k.trim()), v.trim_matches(['"', '\'']).to_string()));
        }
    }
    out
}

/// Every version the build files of a repo pin ([`PIN_FILES`], at any depth
/// git tracks them): gradle.properties keys, each package.json's `version`,
/// `packageManager`, `engines` and dependencies, Cargo.toml's package and
/// dependency versions, pyproject's version, tauri.conf.json's version and
/// go.mod's `go` and requires.
pub fn pins(root: &Path, tracked: &[String]) -> Vec<Pin> {
    let mut out = Vec::new();
    for file in tracked.iter().filter(|f| PIN_FILES.contains(&basename(f))) {
        let Some(text) = read(root, file) else { continue };
        let mut kv: Vec<(String, String)> = Vec::new();
        match basename(file) {
            "gradle.properties" => kv = properties(&text),
            "package.json" | "tauri.conf.json" => {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
                if let Some(s) = v.get("version").and_then(|s| s.as_str()) {
                    kv.push(("version".into(), s.into()));
                }
                if let Some(s) = v.get("packageManager").and_then(|s| s.as_str()) {
                    kv.push(("packageManager".into(), s.rsplit('@').next().unwrap_or(s).into()));
                }
                for table in ["engines", "dependencies", "devDependencies", "peerDependencies"] {
                    for (k, x) in v.get(table).and_then(|t| t.as_object()).into_iter().flatten() {
                        if let Some(s) = x.as_str() {
                            kv.push((format!("{table}.{k}"), s.into()));
                        }
                    }
                }
            }
            "Cargo.toml" => {
                kv = toml_versions(&text, |t| {
                    matches!(t, "package" | "workspace.package") || t.ends_with("dependencies")
                });
                kv.retain(|(k, _)| !k.ends_with(".edition") && !k.ends_with(".name"));
            }
            "pyproject.toml" => kv = toml_versions(&text, |t| matches!(t, "project" | "tool.poetry")),
            "go.mod" => {
                for line in text.lines().map(str::trim) {
                    let line = line.strip_prefix("require ").unwrap_or(line);
                    let mut w = line.split_whitespace();
                    if let (Some(k), Some(v), None) = (w.next(), w.next(), w.next()) {
                        kv.push((k.to_string(), v.to_string()));
                    }
                }
            }
            _ => {}
        }
        for (key, value) in kv {
            if let Some(value) = as_version(&value) {
                out.push(Pin { file: file.clone(), key, value });
            }
        }
    }
    out
}

/// The versions a line of prose states: each word that [`as_version`]
/// reads, a version tag's leading `v` dropped.
pub fn versions_in(line: &str) -> Vec<String> {
    line.split(|c: char| c.is_whitespace() || "`|()[]{}*,;!?\"'<>→".contains(c))
        .map(|w| w.trim_end_matches(['.', ':']))
        .filter(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit() || c == 'v' || c == 'V'))
        .filter_map(as_version)
        .collect()
}

// ------------------------------------------------------------------ values

/// Build and data files are named in prose with these extensions.
const DATA_EXTS: &[&str] = &["json", "jsonc", "yaml", "yml", "toml", "properties", "ini", "cfg"];
/// Files whose values the pins already give, or that hold no values.
const NOT_DATA: &[&str] = &["package.json", "package-lock.json", "Cargo.toml", "pyproject.toml", "tauri.conf.json", "gates.json"];

/// One JSON value, flattened to `a.b = 1.3` lines: scalars and short lists
/// as they are; a long list or a big map by its count, so an index file says
/// how many entries it has.
fn flatten(v: &serde_json::Value, at: &str, depth: usize, out: &mut Vec<String>) {
    use serde_json::Value;
    let name = if at.is_empty() { "(top)" } else { at };
    let scalar = |x: &Value| !x.is_object() && !x.is_array();
    match v {
        Value::Object(m) if depth >= 3 || m.len() > 24 => {
            let keys: Vec<&str> = m.keys().take(5).map(String::as_str).collect();
            let more = if m.len() > 5 { format!("; the first {} of {}, not the whole set", keys.len(), m.len()) } else { String::new() };
            out.push(format!("{name}: {} keys ({}{more})", m.len(), keys.join(", ")));
        }
        Value::Object(m) => {
            for (k, x) in m {
                flatten(x, &if at.is_empty() { k.clone() } else { format!("{at}.{k}") }, depth + 1, out);
            }
        }
        Value::Array(a) if a.len() <= 6 && a.iter().all(scalar) => {
            out.push(format!("{name} = [{}]", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")))
        }
        Value::Array(a) => out.push(format!("{name}: a list of {}", a.len())),
        x => out.push(format!("{name} = {x}")),
    }
}

/// A data file's values: JSON and YAML flattened ([`flatten`]), anything
/// else its lines, comments left out; whole, or None when they pass
/// [`ONE_FILE_MAX`].
fn file_values(rel: &str, text: &str) -> Option<String> {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let value: Option<serde_json::Value> = match ext.as_str() {
        "json" | "jsonc" => serde_json::from_str(text).ok(),
        "yaml" | "yml" => serde_yaml::from_str::<serde_yaml::Value>(text).ok().and_then(|y| serde_json::to_value(y).ok()),
        _ => None,
    };
    let lines: Vec<String> = match value {
        Some(v) => {
            let mut out = Vec::new();
            flatten(&v, "", 0, &mut out);
            out
        }
        None => text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with(['#', ';', '/'])).map(String::from).collect(),
    };
    let all = lines.join("\n");
    (all.len() <= ONE_FILE_MAX).then_some(all)
}

/// The values in the checkout, as the build and data files hold them now:
/// every key of gradle.properties, every version the build files pin
/// ([`pins`]), then each data or config file the briefs (`brief_text`) name
/// by file name, most named first, with its values ([`file_values`]): the
/// counts, defaults and multipliers a brief quotes and the code has since
/// changed. Every file whole within `budget` characters; a file too big for
/// [`ONE_FILE_MAX`] or for what is left is named with its size, never cut.
/// None when there is nothing to say.
pub fn values(root: &Path, tracked: &[String], brief_text: &str, budget: usize) -> Option<String> {
    let mut out = String::new();
    let pinned = pins(root, tracked);
    for file in tracked.iter().filter(|f| basename(f) == "gradle.properties") {
        let Some(text) = read(root, file) else { continue };
        let kv: Vec<String> = properties(&text).into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k} = {}", clip(&v, 120))).collect();
        out.push_str(&format!("{file}:\n{}\n", kv.join("\n")));
    }
    let mut by_file: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    // A nested package's dependencies repeat the root's: its own version only.
    let shown = |p: &&Pin| basename(&p.file) != "gradle.properties" && !(p.file.contains('/') && p.key.contains("ependencies"));
    for p in pinned.iter().filter(shown) {
        by_file.entry(&p.file).or_default().push(format!("{} = {}", p.key, p.value));
    }
    for (file, kv) in by_file {
        out.push_str(&format!("{file}: {}\n", kv.join(", ")));
    }
    // Data files the briefs name, by how often, then where first named.
    let mut named: HashMap<String, (usize, usize)> = HashMap::new();
    let is_word = |c: char| c.is_alphanumeric() || "._-".contains(c);
    let mut start: Option<usize> = None;
    for (i, c) in brief_text.char_indices().chain(std::iter::once((brief_text.len(), ' '))) {
        match (start, is_word(c)) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                start = None;
                let w = brief_text[s..i].trim_matches(['.', '-']);
                let Some((name, ext)) = w.rsplit_once('.') else { continue };
                if name.is_empty() || !DATA_EXTS.contains(&ext.to_ascii_lowercase().as_str()) || NOT_DATA.contains(&w) || w.starts_with("tsconfig") {
                    continue;
                }
                named.entry(w.to_string()).or_insert((0, s)).0 += 1;
            }
            _ => {}
        }
    }
    let mut names: Vec<(String, (usize, usize))> = named.into_iter().collect();
    names.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.1 .1.cmp(&b.1 .1)));
    let mut blocks: Vec<(&String, String)> = Vec::new();
    let mut left_out: Vec<String> = Vec::new();
    for (name, _) in names {
        let files = tracked.iter().filter(|f| basename(f) == name && !f.starts_with("docs/")).take(3);
        for f in files {
            let Some(text) = read(root, f) else { continue };
            match file_values(f, &text) {
                Some(v) => blocks.push((f, format!("{f}:\n{v}\n"))),
                None => left_out.push(format!("{f} ({} characters of values: too big to give whole)", text.len())),
            }
        }
    }
    // The small files first, most named first, then the big ones while
    // room is left: on 2026-10-07 four token files crowded out the
    // 153 characters of `RarityScaling.json`.
    let mut data = String::new();
    for small in [true, false] {
        for (f, block) in blocks.iter().filter(|(_, b)| (b.len() <= SMALL_FILE) == small) {
            if out.len() + data.len() + block.len() > budget {
                left_out.push(format!("{f} ({} characters of values: no room left)", block.len()));
                continue;
            }
            data.push_str(block);
        }
    }
    if !data.is_empty() {
        out.push_str("\ndata and config files the briefs name, as they are now, each whole:\n");
        out.push_str(&data);
    }
    if !left_out.is_empty() {
        out.push_str("\nnamed by the briefs but not given here (never cut; read the file):\n");
        out.push_str(&left_out.join("\n"));
        out.push('\n');
    }
    (!out.trim().is_empty()).then_some(out)
}

// ------------------------------------------------------------------- names

/// The names a repo's checkout has, to check the names a page writes as
/// code against: `defined` is every definition the code map finds and every
/// file's and folder's name; `used` every identifier in its code, data and
/// build files (not its docs), for a name that is a key or an id rather than
/// a definition.
#[derive(Debug, Default)]
pub struct Names {
    pub defined: HashSet<String>,
    pub used: HashSet<String>,
}

/// The file kinds whose words count as used: code, data, build and markup.
fn is_text_for_names(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    Lang::of(rel).is_some()
        || [
            "json", "jsonc", "yaml", "yml", "toml", "properties", "gradle", "kts", "xml", "svelte", "vue", "ui", "lang", "kt", "scala",
            "groovy", "c", "h", "cpp", "hpp", "sh", "ps1", "cmd", "bat", "sql", "html", "css", "scss",
        ]
        .contains(&ext.as_str())
}

impl Names {
    /// Whether the checkout has `name` whole or as a run of a longer
    /// identifier's parts ([`inside_identifier`]): on 2026-10-07 the check
    /// read `kill_stats` as missing while `reset_kill_stats` registered it,
    /// and a correction call wrote that it was gone.
    pub fn has(&self, name: &str) -> bool {
        if self.defined.contains(name) || self.used.contains(name) {
            return true;
        }
        let want = ident_parts(name).concat();
        if want.len() < 4 {
            return false;
        }
        self.used.iter().chain(&self.defined).any(|w| {
            w.len() > name.len() && w.to_lowercase().replace(['_', '-'], "").contains(&want) && inside_identifier(w, name)
        })
    }
}

/// An identifier's parts, lowercased: split at `_` and `-` and at its camel
/// humps, an acronym kept whole (`SRResetKillStats` is sr, reset, kill,
/// stats).
pub fn ident_parts(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let hump = c.is_uppercase()
            && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next.is_some_and(char::is_lowercase)));
        if hump && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether `name` is `word` or a run of `word`'s parts ([`ident_parts`]):
/// `kill_stats` is inside `reset_kill_stats` and `SRResetKillStatsCommand`,
/// `kill` is not inside `skill`.
pub fn inside_identifier(word: &str, name: &str) -> bool {
    let want = ident_parts(name).concat();
    if want.is_empty() {
        return false;
    }
    let parts = ident_parts(word);
    for i in 0..parts.len() {
        let mut run = String::new();
        for p in &parts[i..] {
            run.push_str(p);
            if run == want {
                return true;
            }
            if run.len() >= want.len() {
                break;
            }
        }
    }
    false
}

/// [`Names`] of a repo. `deep` reads the files (the code map and every
/// identifier); without it, for a large reference repo the team only reads,
/// only file and folder names count.
pub fn names(root: &Path, tracked: &[String], deep: bool) -> Names {
    let mut n = Names::default();
    for f in tracked {
        let segs: Vec<&str> = f.split('/').collect();
        for (i, s) in segs.iter().enumerate() {
            n.defined.insert(if i + 1 == segs.len() { stem(s).to_string() } else { s.to_string() });
        }
    }
    if !deep {
        return n;
    }
    for f in tracked.iter().filter(|f| is_text_for_names(f)) {
        let Some(text) = read(root, f) else { continue };
        if Lang::of(f).is_some() {
            if let Some(map) = map_file(f, &text) {
                n.defined.extend(map.symbols.into_iter().filter(|s| s.is_def).map(|s| s.name));
            }
        }
        for w in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if w.len() >= 4 && !n.used.contains(w) {
                n.used.insert(w.to_string());
            }
        }
    }
    n
}

/// A backticked token that names code, as (the name, whether it is a
/// class): `QuestInstaller`, `OneNameLineComposerTest`, the class of
/// `ChargeBreakpoints.step()` or `dev.app.Main`, and, not a class,
/// `injectLoadsAfter` or `hytale_version`. A class is CamelCase with two
/// humps or more; any other name has a lowercase-to-uppercase step or an
/// underscore. A plain word (`Stamina`), an acronym (`JSON`), a path, a
/// command or a citation is not a name.
pub fn as_symbol(token: &str) -> Option<(String, bool)> {
    let t = token.trim();
    let t = t.split_once('(').map_or(t, |(head, _)| head);
    if t.is_empty() || t.contains(|c: char| c.is_whitespace() || "/\\-@=<>,'\"$%{}[]|".contains(c)) {
        return None;
    }
    if t.replace("::", "").contains(':') {
        return None;
    }
    let segs: Vec<&str> = t.split(['.', '#']).flat_map(|s| s.split("::")).collect();
    if segs.iter().any(|s| s.is_empty() || !s.chars().all(|c| c.is_alphanumeric() || c == '_')) {
        return None;
    }
    let is_class = |s: &str| {
        let chars: Vec<char> = s.chars().collect();
        chars[0].is_uppercase()
            && !s.contains('_')
            && chars.windows(2).any(|w| w[0].is_lowercase() && w[1].is_uppercase())
            && chars.iter().any(|c| c.is_lowercase())
    };
    let is_symbol = |s: &str| {
        let chars: Vec<char> = s.chars().collect();
        let camel = chars[0].is_lowercase() && chars.windows(2).any(|w| w[0].is_lowercase() && w[1].is_uppercase());
        let snake = !s.starts_with('_') && s.trim_end_matches('_').contains('_') && chars.iter().any(|c| c.is_alphabetic());
        camel || snake
    };
    let pick = segs.iter().find(|s| s.len() >= 4 && is_class(s)).map(|s| (s.to_string(), true));
    pick.or_else(|| segs.first().filter(|s| s.len() >= 4 && is_symbol(s)).map(|s| (s.to_string(), false)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<String>) {
        let d = tempfile::tempdir().unwrap();
        for (p, t) in files {
            let path = d.path().join(p);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, t).unwrap();
        }
        let ok = Command::new("git").args(["init", "-q"]).current_dir(d.path()).status().unwrap().success();
        assert!(ok);
        let mut tracked: Vec<String> = files.iter().map(|(p, _)| p.to_string()).collect();
        tracked.sort();
        (d, tracked)
    }

    /// The start-up function the build declares is listed whole, statement
    /// by statement with its lines, comments and values read back left out;
    /// then the largest registration functions elsewhere.
    #[test]
    fn registrations_list_what_the_entry_point_wires_in_order() {
        let main = "package dev.app;\n\npublic class GamePlugin {\n    @Override\n    protected void setup() {\n        // codecs first\n        CodecInstaller.install(registries);\n        configs = ConfigInstaller.install(registries, log);\n        this.mobs = configs.mobs();\n        QuestInstaller.install(registries, mobs);\n        RiftInstaller.install(registries);\n    }\n\n    private void helper() {\n        other();\n    }\n}\n";
        let quest = "package dev.app.plugin;\n\nclass QuestInstaller {\n    static void install(Registries r, Mobs m) {\n        r.command(new QuestCommand());\n        r.system(new QuestTick(m));\n    }\n}\n";
        let (d, tracked) = repo(&[
            ("gradle.properties", "# the entry\nmain_class = dev.app.GamePlugin\nhytale_version = 0.7.0-pre.5\n"),
            ("src/main/java/dev/app/GamePlugin.java", main),
            ("src/main/java/dev/app/plugin/QuestInstaller.java", quest),
        ]);
        assert_eq!(entry_files(d.path(), &tracked), vec!["src/main/java/dev/app/GamePlugin.java"]);
        let r = registrations(d.path(), &tracked).unwrap();
        let setup = r.find("src/main/java/dev/app/GamePlugin.java:5 setup(): 4 statements").unwrap_or_else(|| panic!("{r}"));
        let quests = r.find("src/main/java/dev/app/plugin/QuestInstaller.java:4 install(): 2 statements").unwrap_or_else(|| panic!("{r}"));
        assert!(setup < quests, "the entry point first: {r}");
        for line in ["7: CodecInstaller.install(registries);", "10: QuestInstaller.install(registries, mobs);", "11: RiftInstaller.install(registries);"] {
            assert!(r.contains(line), "{line}: {r}");
        }
        assert!(!r.contains("codecs first") && !r.contains("configs.mobs()") && !r.contains("helper"), "{r}");
    }

    #[test]
    fn versions_are_read_from_the_build_files_and_from_prose() {
        let (d, tracked) = repo(&[
            ("gradle.properties", "# was 0.7.0-pre.3\nhytale_version = 0.7.0-pre.5\njava_version = 25\nversion = 0.0.15\n"),
            ("package.json", r#"{"name": "tools", "version": "0.0.0", "packageManager": "pnpm@10.4.1", "devDependencies": {"vitest": "^4.1.10", "@x/y": "workspace:*"}}"#),
            ("apps/shell/src-tauri/tauri.conf.json", r#"{"version": "0.5.35"}"#),
            ("Cargo.toml", "[workspace.package]\nversion = \"0.3.1\"\nedition = \"2021\"\n\n[workspace.dependencies]\nserde = { version = \"1.0.200\", features = [\"derive\"] }\ntauri = \"2.1\"\n"),
        ]);
        let p: Vec<(String, String, String)> = pins(d.path(), &tracked).into_iter().map(|p| (p.file, p.key, p.value)).collect();
        let has = |f: &str, k: &str, v: &str| p.contains(&(f.to_string(), k.to_string(), v.to_string()));
        assert!(has("gradle.properties", "hytale_version", "0.7.0-pre.5") && has("gradle.properties", "version", "0.0.15"), "{p:?}");
        assert!(!p.iter().any(|x| x.1 == "java_version"), "25 is not a version: {p:?}");
        assert!(has("package.json", "packageManager", "10.4.1") && has("package.json", "devDependencies.vitest", "4.1.10"), "{p:?}");
        assert!(!p.iter().any(|x| x.1.contains("@x/y")));
        assert!(has("apps/shell/src-tauri/tauri.conf.json", "version", "0.5.35"));
        assert!(has("Cargo.toml", "workspace.package.version", "0.3.1") && has("Cargo.toml", "workspace.dependencies.serde", "1.0.200"), "{p:?}");
        assert!(has("Cargo.toml", "workspace.dependencies.tauri", "2.1"));

        assert_eq!(versions_in("pins the engine at 0.7.0-pre.3, see `v0.0.15` (2026-09-25) and 4.1."), vec!["0.7.0-pre.3", "0.0.15", "4.1"]);
        assert_eq!(as_version("0.+"), None);
        assert_eq!(as_version("2026-09-25"), None);
        assert_eq!(version_core("0.7.0-pre.5"), "0.7.0");
    }

    /// A data file a brief names is read as it is now: its defaults, and an
    /// index's count.
    #[test]
    fn values_give_the_data_files_a_brief_names_as_they_are() {
        let (d, tracked) = repo(&[
            ("gradle.properties", "hytale_version = 0.7.0-pre.5\n"),
            ("src/main/resources/data/equipment/RarityScaling.json", r#"{"Multipliers": {"Common": 1.0, "Uncommon": 1.1, "Mythic": 2.562}}"#),
            ("src/main/resources/data/mobs/mob_index.json", &format!("{{\"Mobs\": [{}]}}", (0..76).map(|i| format!("\"m{i}\"")).collect::<Vec<_>>().join(","))),
            ("src/main/resources/data/unnamed.json", r#"{"x": 1}"#),
        ]);
        let brief = "Rarity multiplies base stats (`RarityScaling.json`): Uncommon 1.2x.\nAdd the path to `data/mobs/mob_index.json`; see package.json.\n";
        let v = values(d.path(), &tracked, brief, 16_000).unwrap();
        assert!(v.contains("hytale_version = 0.7.0-pre.5"), "{v}");
        assert!(v.contains("src/main/resources/data/equipment/RarityScaling.json:\nMultipliers."), "{v}");
        for line in ["\nMultipliers.Common = 1.0\n", "\nMultipliers.Uncommon = 1.1\n", "\nMultipliers.Mythic = 2.562\n"] {
            assert!(v.contains(line), "{line}: {v}");
        }
        assert!(v.contains("src/main/resources/data/mobs/mob_index.json:\nMobs: a list of 76"), "{v}");
        assert!(!v.contains("unnamed.json"), "only what a brief names: {v}");
    }

    #[test]
    fn a_name_written_as_code_is_a_class_or_a_symbol() {
        assert_eq!(as_symbol("OneNameLineComposerTest"), Some(("OneNameLineComposerTest".into(), true)));
        assert_eq!(as_symbol("ChargeBreakpoints.step()"), Some(("ChargeBreakpoints".into(), true)));
        assert_eq!(as_symbol("dev.hytalemodding.ShatteredRealmsRPG"), Some(("ShatteredRealmsRPG".into(), true)));
        assert_eq!(as_symbol("injectLoadsAfter(Beam.class)"), Some(("injectLoadsAfter".into(), false)));
        assert_eq!(as_symbol("hytale_version"), Some(("hytale_version".into(), false)));
        assert_eq!(as_symbol("INVENTORY_CAPACITY"), Some(("INVENTORY_CAPACITY".into(), false)));
        for not in ["Stamina", "JSON", "README", "data/mobs", "Shattered-Realms:CLAUDE.md:12", "pnpm run build", "--offline", "v0.0.15", "Pack", "_x"] {
            assert_eq!(as_symbol(not), None, "{not}");
        }
    }

    #[test]
    fn names_are_the_definitions_files_and_words_of_the_checkout() {
        let (d, tracked) = repo(&[
            ("src/Game.java", "class Game {\n  static final int MAX_ROWS = 3;\n  void startRound() {}\n}\n"),
            ("src/test/ItemNameLineTest.java", "// was OneNameLineComposerTest\nclass ItemNameLineTest {}\n"),
            ("data/causes/SR_Light.json", "{\"WeaponModMultiplier\": 1.17}\n"),
            ("CLAUDE.md", "`GhostInstaller` registers ghosts.\n"),
        ]);
        let n = names(d.path(), &tracked, true);
        for name in ["Game", "MAX_ROWS", "startRound", "ItemNameLineTest", "SR_Light", "causes"] {
            assert!(n.defined.contains(name), "{name}");
        }
        assert!(!n.defined.contains("OneNameLineComposerTest"), "a comment is not a definition");
        assert!(n.used.contains("WeaponModMultiplier") && n.used.contains("OneNameLineComposerTest"));
        assert!(!n.used.contains("GhostInstaller"), "a doc's words are not the code's");
        let shallow = names(d.path(), &tracked, false);
        assert!(shallow.defined.contains("Game") && !shallow.defined.contains("startRound") && shallow.used.is_empty());
    }

    /// A name inside a longer identifier is the checkout's: `kill_stats` in
    /// `reset_kill_stats`, never `kill` in `skill`.
    #[test]
    fn a_name_inside_a_longer_identifier_is_found() {
        assert_eq!(ident_parts("SRResetKillStatsCommand"), vec!["sr", "reset", "kill", "stats", "command"]);
        assert_eq!(ident_parts("reset_kill_stats"), vec!["reset", "kill", "stats"]);
        assert!(inside_identifier("reset_kill_stats", "kill_stats"));
        assert!(inside_identifier("SRResetKillStatsCommand", "kill_stats"));
        assert!(inside_identifier("kill_stats", "kill_stats"));
        assert!(!inside_identifier("skill_stats", "kill_stats"), "a part is whole");
        assert!(!inside_identifier("reset_kill_statsx", "kill_stats"));
        let (d, tracked) = repo(&[("src/Reset.java", "class Reset {\n  String NAME = \"reset_kill_stats\";\n}\n")]);
        let n = names(d.path(), &tracked, true);
        assert!(!n.used.contains("kill_stats"), "not a word of its own");
        assert!(n.has("kill_stats") && n.has("Reset") && !n.has("kill_points"));
    }

    /// A data file is given whole, however long; one too big for the
    /// budget is named with its size, never cut.
    #[test]
    fn values_give_each_data_file_whole_or_name_it() {
        let weapons = ["BattleAxe", "Bow", "Crossbow", "Dagger", "Staff", "Sword"];
        let charge: String = weapons
            .iter()
            .map(|w| format!("\"{w}\": {{\"AttackCycleTime\": 1, \"AttackReach\": 3.4, \"BarMaxHoldTime\": 1.35, \"KnockbackScale\": 1, \"Breakpoints\": [1, 2]}}"))
            .collect::<Vec<_>>()
            .join(", ");
        let big = format!("{{\"Rows\": {{{}}}}}", (0..20).map(|i| format!("\"k{i}\": \"{}\"", "x".repeat(400))).collect::<Vec<_>>().join(", "));
        let (d, tracked) = repo(&[
            ("data/WeaponChargeScaling.json", &format!("{{\"ChargeScaling\": {{{charge}}}}}")),
            ("data/Big.json", &big),
        ]);
        let brief = "Charge is in `WeaponChargeScaling.json`; see also `Big.json`.\n";
        let v = values(d.path(), &tracked, brief, 4_000).unwrap();
        assert!(v.len() > 800, "no 800-character cut: {v}");
        for w in weapons {
            assert!(v.contains(&format!("ChargeScaling.{w}.AttackCycleTime = 1")), "{w}: {v}");
        }
        assert!(!v.contains("[…]") && !v.contains("Rows.k0"), "never cut mid-way: {v}");
        assert!(v.contains("data/Big.json (") && v.contains("no room left"), "a file left out is named: {v}");
    }
}
