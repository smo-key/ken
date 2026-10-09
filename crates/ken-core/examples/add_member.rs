//! Add one folder to an existing evaluation workspace as a member, with the
//! kinds and team given, without proposing or confirming the rest again:
//! `setup::confirm` would set every member's kind and group from a fresh
//! proposal, and the dry run's kinds were assigned by hand. Nothing else in
//! the workspace or the registry changes.
//!
//! ```text
//! set KEN_DATA_DIR=<scratch>\data      (required; never the app's own data)
//! cargo run --release -p ken-core --example add_member -- <parent> <member> <kind[,kind]> <team>
//! cargo run --release -p ken-core --example add_member -- <parent> Team-Docs team,wiki Team
//! ```
//!
//! The member keeps the `.ken/project.json` it carries; one without gets its
//! config in the data folder, so the repo gains no file. Run it again and it
//! changes nothing more.

use std::path::{Path, PathBuf};

use ken_core::project::Project;
use ken_core::registry::{Registry, RepoKind};
use ken_core::workspace::{self, Workspace};
use ken_core::{Error, Result};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("usage: add_member <parent> <member> <kind[,kind]> <team>");
        std::process::exit(2);
    }
    let Some(base) = std::env::var_os("KEN_DATA_DIR").map(PathBuf::from) else {
        eprintln!("set KEN_DATA_DIR to a scratch folder first");
        std::process::exit(2);
    };
    if let Err(e) = add(&base, Path::new(&args[0]), &args[1], &args[2], &args[3]) {
        eprintln!("FAILED: {e}");
        std::process::exit(1);
    }
}

fn kinds(list: &str) -> Result<Vec<RepoKind>> {
    list.split(',')
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| serde_json::from_value(serde_json::Value::String(k.to_lowercase())).map_err(|_| Error::Other(format!("unknown kind {k:?}"))))
        .collect()
}

fn add(base: &Path, parent: &Path, member: &str, kind: &str, team: &str) -> Result<()> {
    let kind = kinds(kind)?;
    let root = parent.join(member);
    if !root.is_dir() {
        return Err(Error::ProjectMissing(root));
    }
    let project = match Project::open(&root) {
        Ok(p) => p,
        Err(_) => Project::create_outside(&root, workspace::member_leaf(member))?,
    };

    let mut ws = Workspace::open(parent)?;
    if !ws.config.members.iter().any(|m| m == member) {
        ws.config.members.push(member.to_string());
    }
    // Into the team's group beside the others, not in place of them.
    let mut group = ws.config.group(team).map(|g| g.members.clone()).unwrap_or_default();
    if !group.iter().any(|m| m == member) {
        group.push(member.to_string());
    }
    ws.config.set_group(team, &group)?;
    ws.save()?;

    let mut reg = Registry::load(base)?;
    reg.add(&project);
    reg.set_kind(project.config.id, kind, Some(team.to_string()));
    let description = ken_core::setup::readme_summary(&root);
    if !description.is_empty() {
        reg.set_description(project.config.id, &description);
    }
    reg.save(base)?;

    let reg = Registry::load(base)?;
    let ws = Workspace::open(parent)?;
    println!("# `{member}` added to `{}`\n", ws.config.name);
    for m in &ws.members {
        let e = reg.entry_at(&parent.join(&m.name));
        println!(
            "- **{}** id={} kind={:?} index={:?} team={:?}",
            m.name,
            e.map(|e| e.id.to_string()).unwrap_or_else(|| "(not registered)".into()),
            e.map(|e| e.kind.clone()).unwrap_or_default(),
            e.and_then(|e| e.index),
            e.and_then(|e| e.team.clone()),
        );
    }
    Ok(())
}
