// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Lens resolution: which pipeline runs for this command, and why.
//!
//! Five layers, later wins. Provenance is recorded per field because that
//! column is the whole point of `lens plot` — without it users guess which
//! file turned a stage off and blame the filter.
//!
//! The runner executes the [`ResolvedPipeline`] this module returns; `plot`
//! prints it. One function, two callers, so the picture cannot drift from
//! the run.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pipeline;

/// Where a resolved value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A built-in lens shipped with the binary.
    Builtin,
    /// A TOML file, and the table inside it.
    File {
        /// Path as the user would recognize it.
        path: String,
        /// Lens name inside that file.
        lens: String,
    },
    /// An environment variable.
    Env(&'static str),
    /// A command-line flag.
    Flag(&'static str),
}

impl Source {
    /// A short label for the provenance column.
    pub fn label(&self) -> String {
        match self {
            Source::Builtin => "builtin".into(),
            Source::File { path, lens } => format!("{path} [lens.{lens}]"),
            Source::Env(name) => (*name).into(),
            Source::Flag(name) => (*name).into(),
        }
    }
}

/// A value and the layer that supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field<T> {
    /// The resolved value.
    pub value: T,
    /// Which layer won.
    pub source: Source,
}

/// The pipeline the runner will execute and plot will print.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedPipeline {
    /// Selected lens name.
    pub lens: Field<String>,
    /// Stage names, in order, including `budget` if the lens asked for it.
    pub stages: Field<Vec<String>>,
    /// Token budget, if the lens or the caller set one.
    pub budget: Field<Option<usize>>,
    /// Context blocks around a failure.
    pub context_blocks: Field<usize>,
    /// Adapter name. `"generic"` until a structured one exists for this command.
    pub adapter: Field<String>,
    /// The command this was resolved for.
    pub argv: Vec<String>,
}

impl ResolvedPipeline {
    /// Stages the pipeline runner can actually apply, in order.
    ///
    /// `budget` is excluded: it is applied across both streams after these.
    pub fn runnable_stages(&self) -> Vec<&'static dyn pipeline::Stage> {
        let named = pipeline::stages_named(&self.stages.value);
        if named.is_empty() { pipeline::default_stages() } else { named }
    }

    /// Does this lens include the budget stage?
    pub fn wants_budget(&self) -> bool {
        self.stages.value.iter().any(|name| name == "budget")
    }
}

/// Inputs the resolver needs from the process.
pub struct ResolveInput<'a> {
    /// Child argv, starting with the command name.
    pub argv: &'a [String],
    /// Working directory, for the `.lens.toml` walk.
    pub cwd: &'a Path,
    /// User config directory resolution.
    pub dirs: &'a crate::platform::Dirs,
    /// `LENS_CONFIG`, when set.
    pub config_override: Option<&'a std::ffi::OsString>,
    /// `--budget`, when given.
    pub cli_budget: Option<usize>,
    /// `--use`, when given.
    pub cli_use: Option<&'a str>,
    /// `LENS_BUDGET`, when set.
    pub env_budget: Option<usize>,
}

/// Why resolution could not pick a lens the caller named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `--use` named a lens that is not in the catalog.
    UnknownLens(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::UnknownLens(name) => {
                write!(f, "no lens named `{name}` — try: lens lenses")
            }
        }
    }
}

/// One lens as written in TOML.
#[derive(Debug, Clone, Default, Deserialize)]
struct LensDef {
    #[serde(rename = "match")]
    match_on: Option<String>,
    extends: Option<String>,
    budget: Option<usize>,
    stages: Option<Vec<String>>,
    #[serde(default)]
    context_lines: Option<usize>,
    adapter: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FileDef {
    #[serde(default)]
    lens: BTreeMap<String, LensDef>,
}

#[derive(Debug, Clone)]
struct CatalogEntry {
    def: LensDef,
    source: Source,
    fields: FieldSources,
}

/// Provenance for each field that a layer actually set.
///
/// A later file that only overrides `budget` must not retag `stages` as
/// coming from that file — plot's column would then lie.
#[derive(Debug, Clone, Default)]
struct FieldSources {
    budget: Option<Source>,
    stages: Option<Source>,
    context_lines: Option<Source>,
    adapter: Option<Source>,
}

fn sources_for(def: &LensDef, src: &Source) -> FieldSources {
    FieldSources {
        budget: def.budget.is_some().then(|| src.clone()),
        stages: def.stages.is_some().then(|| src.clone()),
        context_lines: def.context_lines.is_some().then(|| src.clone()),
        adapter: def.adapter.is_some().then(|| src.clone()),
    }
}

fn merge_entry(base: &mut CatalogEntry, child: &CatalogEntry) {
    overlay(&mut base.def, &child.def);
    if child.def.budget.is_some() {
        base.fields.budget = child.fields.budget.clone();
    }
    if child.def.stages.is_some() {
        base.fields.stages = child.fields.stages.clone();
    }
    if child.def.context_lines.is_some() {
        base.fields.context_lines = child.fields.context_lines.clone();
    }
    if child.def.adapter.is_some() {
        base.fields.adapter = child.fields.adapter.clone();
    }
    base.source = child.source.clone();
}

/// Resolve the pipeline for `input`.
///
/// # Errors
///
/// [`ResolveError::UnknownLens`] when `--use` names something that does not
/// exist. Every other failure — missing files, unparseable TOML — skips that
/// layer rather than refusing to run a command.
pub fn resolve(input: &ResolveInput<'_>) -> Result<ResolvedPipeline, ResolveError> {
    let catalog = load_catalog(input);
    let name = select_lens(&catalog, input.argv, input.cli_use)?;
    let flat = flatten(&catalog, &name);

    let stages = Field {
        value: flat.def.stages.unwrap_or_else(|| {
            pipeline::default_stage_names().iter().map(|s| (*s).to_string()).collect()
        }),
        source: flat.fields.stages.unwrap_or(Source::Builtin),
    };

    let mut budget =
        Field { value: flat.def.budget, source: flat.fields.budget.unwrap_or(Source::Builtin) };

    let context_blocks = Field {
        value: flat.def.context_lines.unwrap_or(3),
        source: flat.fields.context_lines.unwrap_or(Source::Builtin),
    };

    let adapter = Field {
        value: flat.def.adapter.unwrap_or_else(|| "generic".into()),
        source: flat.fields.adapter.unwrap_or(Source::Builtin),
    };

    if let Some(n) = input.env_budget {
        budget = Field { value: Some(n), source: Source::Env("LENS_BUDGET") };
    }
    if let Some(n) = input.cli_budget {
        budget = Field { value: Some(n), source: Source::Flag("--budget") };
    }

    Ok(ResolvedPipeline {
        lens: Field { value: name, source: flat.source },
        stages,
        budget,
        context_blocks,
        adapter,
        argv: input.argv.to_vec(),
    })
}

/// Every lens the catalog knows, in name order, for `lens lenses`.
pub fn list(input: &ResolveInput<'_>) -> Vec<(String, Source, Option<String>)> {
    load_catalog(input)
        .into_iter()
        .map(|(name, entry)| (name, entry.source, entry.def.match_on))
        .collect()
}

fn load_catalog(input: &ResolveInput<'_>) -> BTreeMap<String, CatalogEntry> {
    let mut catalog = BTreeMap::new();

    for (name, def) in builtins() {
        catalog.insert(
            name.to_string(),
            CatalogEntry {
                fields: sources_for(&def, &Source::Builtin),
                source: Source::Builtin,
                def,
            },
        );
    }

    let config_file = input.dirs.config_file(input.config_override);
    merge_file(&mut catalog, &config_file);

    let lenses_dir = input.dirs.lenses_dir(input.config_override);
    if let Ok(entries) = fs::read_dir(&lenses_dir) {
        let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        for path in files {
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                merge_file(&mut catalog, &path);
            }
        }
    }

    for path in project_lenses(input.cwd) {
        merge_file(&mut catalog, &path);
    }

    catalog
}

fn merge_file(catalog: &mut BTreeMap<String, CatalogEntry>, path: &Path) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<FileDef>(&text) else {
        return;
    };
    let shown = path.display().to_string();
    for (name, def) in parsed.lens {
        let source = Source::File { path: shown.clone(), lens: name.clone() };
        let incoming = CatalogEntry { fields: sources_for(&def, &source), source, def };
        if let Some(existing) = catalog.get_mut(&name) {
            merge_entry(existing, &incoming);
        } else {
            catalog.insert(name, incoming);
        }
    }
}

/// `.lens.toml` files from the filesystem root down to `cwd`, so the closer
/// file wins when both define the same lens.
fn project_lenses(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut dir = cwd.to_path_buf();
    loop {
        dirs.push(dir.clone());
        let Some(parent) = dir.parent() else { break };
        if parent == dir {
            break;
        }
        dir = parent.to_path_buf();
    }
    dirs.reverse();
    dirs.into_iter().map(|d| d.join(".lens.toml")).filter(|p| p.is_file()).collect()
}

fn builtins() -> Vec<(&'static str, LensDef)> {
    vec![
        (
            "default",
            LensDef {
                match_on: None,
                stages: Some(
                    pipeline::default_stage_names().iter().map(|s| (*s).to_string()).collect(),
                ),
                context_lines: Some(3),
                adapter: Some("generic".into()),
                ..LensDef::default()
            },
        ),
        (
            "git",
            LensDef {
                match_on: Some("git".into()),
                budget: Some(4000),
                adapter: Some("generic".into()),
                ..LensDef::default()
            },
        ),
        (
            "git-diff",
            LensDef {
                match_on: Some("git diff".into()),
                budget: Some(6000),
                stages: Some(
                    ["ansi", "classify", "context", "rank", "budget"]
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                ),
                adapter: Some("generic".into()),
                ..LensDef::default()
            },
        ),
    ]
}

fn select_lens(
    catalog: &BTreeMap<String, CatalogEntry>,
    argv: &[String],
    forced: Option<&str>,
) -> Result<String, ResolveError> {
    if let Some(name) = forced {
        if catalog.contains_key(name) {
            return Ok(name.to_string());
        }
        return Err(ResolveError::UnknownLens(name.to_string()));
    }

    let cmd = basename(argv.first().map(String::as_str).unwrap_or(""));
    let sub = first_subcommand(argv);
    let pair = sub.as_ref().map(|s| format!("{cmd} {s}"));

    let mut best: Option<(usize, String)> = None;
    for (name, entry) in catalog {
        let Some(pat) = &entry.def.match_on else { continue };
        let spec = pat.split_whitespace().count();
        let hits = match spec {
            1 => pat == &cmd,
            _ => pair.as_deref() == Some(pat.as_str()),
        };
        if hits && best.as_ref().is_none_or(|(s, _)| spec > *s) {
            best = Some((spec, name.clone()));
        }
    }
    Ok(best.map(|(_, name)| name).unwrap_or_else(|| "default".into()))
}

fn flatten(catalog: &BTreeMap<String, CatalogEntry>, name: &str) -> CatalogEntry {
    let empty = || CatalogEntry {
        def: LensDef::default(),
        source: Source::Builtin,
        fields: FieldSources::default(),
    };
    let Some(entry) = catalog.get(name) else {
        return empty();
    };
    let parent = entry.def.extends.clone();
    let mut acc = match parent.as_deref() {
        Some(parent) => flatten(catalog, parent),
        None => empty(),
    };
    merge_entry(&mut acc, entry);
    acc
}

fn overlay(base: &mut LensDef, child: &LensDef) {
    if child.match_on.is_some() {
        base.match_on = child.match_on.clone();
    }
    if child.budget.is_some() {
        base.budget = child.budget;
    }
    if child.stages.is_some() {
        base.stages = child.stages.clone();
    }
    if child.context_lines.is_some() {
        base.context_lines = child.context_lines;
    }
    if child.adapter.is_some() {
        base.adapter = child.adapter.clone();
    }
    base.extends = child.extends.clone();
}

fn basename(cmd: &str) -> String {
    cmd.rsplit('/').next().unwrap_or(cmd).to_string()
}

fn first_subcommand(argv: &[String]) -> Option<String> {
    argv.iter().skip(1).find(|a| !a.starts_with('-')).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn dirs() -> crate::platform::Dirs {
        crate::platform::Dirs {
            cache: PathBuf::from("/tmp/lens-config-test/cache"),
            config: PathBuf::from("/tmp/lens-config-test/config"),
            state: PathBuf::from("/tmp/lens-config-test/state"),
        }
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_unknown_command_gets_the_default_lens() {
        let args = argv(&["cargo", "test"]);
        let cwd = Path::new("/");
        let dirs = dirs();
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd,
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.lens.value, "default");
        assert_eq!(resolved.adapter.value, "generic");
        assert!(resolved.stages.value.contains(&"dedupe".into()));
        assert!(resolved.wants_budget());
    }

    #[test]
    fn git_diff_is_more_specific_than_git() {
        let args = argv(&["git", "diff"]);
        let dirs = dirs();
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.lens.value, "git-diff");
        assert_eq!(resolved.budget.value, Some(6000));
        assert!(!resolved.stages.value.contains(&"progress".into()));
        assert_eq!(resolved.adapter.value, "generic");
        assert_eq!(resolved.lens.source, Source::Builtin);
    }

    #[test]
    fn git_status_selects_the_git_lens() {
        let args = argv(&["git", "status"]);
        let dirs = dirs();
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.lens.value, "git");
        assert_eq!(resolved.budget.value, Some(4000));
    }

    #[test]
    fn use_forces_a_lens_that_would_not_match() {
        let args = argv(&["cargo", "test"]);
        let dirs = dirs();
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: Some("git-diff"),
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.lens.value, "git-diff");
    }

    #[test]
    fn use_of_an_unknown_lens_is_an_error() {
        let args = argv(&["true"]);
        let dirs = dirs();
        let err = resolve(&ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: Some("no-such-lens"),
            env_budget: None,
        })
        .unwrap_err();
        assert_eq!(err, ResolveError::UnknownLens("no-such-lens".into()));
    }

    #[test]
    fn later_layers_win_and_keep_their_provenance() {
        let root = std::env::temp_dir().join(format!("lens-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("lenses")).unwrap();
        fs::write(
            root.join("config.toml"),
            r#"
[lens.git]
budget = 1111
"#,
        )
        .unwrap();
        fs::write(
            root.join(".lens.toml"),
            r#"
[lens.git]
budget = 2222
"#,
        )
        .unwrap();

        let dirs = crate::platform::Dirs {
            cache: root.clone(),
            config: root.clone(),
            state: root.clone(),
        };
        // config_file uses config/lens/config.toml unless overridden.
        let config_file = root.join("config.toml");
        let override_os = OsString::from(config_file.as_os_str());
        let args = argv(&["git", "status"]);
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: &root,
            dirs: &dirs,
            config_override: Some(&override_os),
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.budget.value, Some(2222), "project file is later than user config");
        assert_eq!(resolved.lens.value, "git");
        assert_eq!(
            resolved.stages.source,
            Source::Builtin,
            "a budget-only overlay must not retag stages"
        );
        match &resolved.budget.source {
            Source::File { path, lens } => {
                assert!(path.ends_with(".lens.toml"), "{path}");
                assert_eq!(lens, "git");
            }
            other => panic!("{other:?}"),
        }

        let with_cli = resolve(&ResolveInput {
            argv: &args,
            cwd: &root,
            dirs: &dirs,
            config_override: Some(&override_os),
            cli_budget: Some(9),
            cli_use: None,
            env_budget: Some(8),
        })
        .unwrap();
        assert_eq!(with_cli.budget.value, Some(9));
        assert_eq!(with_cli.budget.source, Source::Flag("--budget"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn extends_inherits_then_overrides() {
        let root = std::env::temp_dir().join(format!("lens-extends-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            r#"
[lens.base]
match = "tool"
budget = 100
stages = ["ansi", "rank", "budget"]

[lens.child]
extends = "base"
budget = 200
"#,
        )
        .unwrap();
        let dirs = crate::platform::Dirs {
            cache: root.clone(),
            config: root.clone(),
            state: root.clone(),
        };
        let override_os = OsString::from(root.join("config.toml").as_os_str());
        let args = argv(&["tool"]);
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: &root,
            dirs: &dirs,
            config_override: Some(&override_os),
            cli_budget: None,
            cli_use: Some("child"),
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.budget.value, Some(200));
        assert_eq!(resolved.stages.value, vec!["ansi", "rank", "budget"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_flag_between_git_and_diff_does_not_hide_the_subcommand() {
        let args = argv(&["git", "--no-pager", "diff"]);
        let dirs = dirs();
        let resolved = resolve(&ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: None,
            cli_use: None,
            env_budget: None,
        })
        .unwrap();
        assert_eq!(resolved.lens.value, "git-diff");
    }

    #[test]
    fn plot_and_the_runner_see_the_same_pipeline() {
        // The property that makes plot honest: one function, two callers.
        let args = argv(&["git", "diff"]);
        let dirs = dirs();
        let input = ResolveInput {
            argv: &args,
            cwd: Path::new("/"),
            dirs: &dirs,
            config_override: None,
            cli_budget: Some(50),
            cli_use: None,
            env_budget: None,
        };
        let a = resolve(&input).unwrap();
        let b = resolve(&input).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.budget.value, Some(50));
    }
}
