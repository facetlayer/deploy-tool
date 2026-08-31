//! Reads settings out of a `.qc` / `.deploy` config file.
//!
//! The old tool split this: the server parsed the activation/create/database
//! directives in `rust/src/config.rs`, and the CLI parsed the client-side
//! directives in TypeScript (`src/client/fileList.ts`). Both halves live here
//! now, so `deploy` and `deploy-server` cannot disagree about what a config
//! means.

use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::qc::{self, Query};

// ---------------------------------------------------------------------------
// Server side: activation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum AfterDeployAction {
    Shell { command: String },
    CandleRestart { service_name: String },
}

#[derive(Clone, Debug, Default)]
pub struct ActivationConfig {
    pub project_name: String,
    pub candle_config_path: Option<String>,
    pub after_deploy_actions: Vec<AfterDeployAction>,
}

/// Port of src/server/parseActivationConfig.ts
pub fn parse_activation_config(config_text: &str) -> ActivationConfig {
    let queries = qc::parse_file(config_text);
    let mut out = ActivationConfig::default();

    for query in &queries {
        match query.command.as_str() {
            "deploy-settings" => {
                if let Some(value) = query.get_string_value("project-name") {
                    out.project_name = value;
                }
                if query.has_attr("candle-config") {
                    out.candle_config_path = query.get_string_value("candle-config");
                }
            }
            "after-deploy" => {
                for tag in &query.tags {
                    if tag.attr == "shell" {
                        let shell = tag.to_original_string();
                        if !shell.is_empty() {
                            out.after_deploy_actions
                                .push(AfterDeployAction::Shell { command: shell });
                        }
                    } else if tag.attr == "candle-restart" {
                        let service_name = tag.to_original_string();
                        if !service_name.is_empty() {
                            out.after_deploy_actions
                                .push(AfterDeployAction::CandleRestart { service_name });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Server side: deployment creation
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DynamicRoute {
    pub pattern: String,
    pub file: String,
    pub metadata_source: Option<String>,
    pub metadata_cache_ttl: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct CreateSettings {
    pub web_static_dir: Option<String>,
    pub is_update_in_place: bool,
    pub dynamic_routes: Vec<DynamicRoute>,
}

/// Port of the config reading inside src/server/createDeployment.ts
pub fn parse_create_settings(config_text: &str) -> CreateSettings {
    let queries = qc::parse_file(config_text);
    let mut out = CreateSettings::default();

    for rule in &queries {
        if rule.command == "deploy-settings" {
            if rule.has_attr("update-in-place") {
                out.is_update_in_place = true;
            }
            if rule.has_attr("web-static-dir") {
                out.web_static_dir = rule.get_string_value("web-static-dir");
            }
        }

        if rule.command == "dynamic-route" {
            let from = rule.get_string_value("from");
            let to = rule.get_string_value("to");
            if let (Some(from), Some(to)) = (from, to) {
                if from.is_empty() || to.is_empty() {
                    continue;
                }
                let mut route = DynamicRoute {
                    pattern: from,
                    file: to,
                    metadata_source: None,
                    metadata_cache_ttl: None,
                };
                if rule.has_attr("metadata-source") {
                    if let Some(source) = rule.get_string_value("metadata-source") {
                        if !source.is_empty() {
                            route.metadata_source = Some(source);
                        }
                    }
                }
                if rule.has_attr("metadata-cache-ttl") {
                    if let Some(ttl) = rule.get_string_value("metadata-cache-ttl") {
                        if let Ok(parsed) = ttl.parse::<i64>() {
                            if parsed > 0 {
                                route.metadata_cache_ttl = Some(parsed);
                            }
                        }
                    }
                }
                out.dynamic_routes.push(route);
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Databases
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DeployDatabaseConfig {
    pub path: String,
    pub agent_sql_access_blocked: bool,
}

const AGENT_SQL_ACCESS_BLOCKED: &str = "agent-sql-access-blocked";

/// Port of src/shared/parseDeployDatabases.ts
pub fn parse_deploy_database_configs(config_text: &str) -> Vec<DeployDatabaseConfig> {
    let queries = qc::parse_file(config_text);
    let mut databases = Vec::new();

    for query in &queries {
        if query.command == "database" {
            if let Some(path_tag) = query.tags.first() {
                if !path_tag.attr.is_empty() {
                    databases.push(DeployDatabaseConfig {
                        path: path_tag.attr.clone(),
                        agent_sql_access_blocked: query.has_attr(AGENT_SQL_ACCESS_BLOCKED),
                    });
                }
            }
        }
    }

    databases
}

pub fn parse_deploy_databases(config_text: &str) -> Vec<String> {
    parse_deploy_database_configs(config_text)
        .into_iter()
        .map(|db| db.path)
        .collect()
}

// ---------------------------------------------------------------------------
// Preserved files
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct PreserveConfig {
    pub patterns: Vec<String>,
    pub max_age_ms: Option<i64>,
}

/// Parses a duration like "7d", "12h", "30m" into milliseconds.
fn parse_duration(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    let unit_char = trimmed.chars().last()?;
    let digits = &trimmed[..trimmed.len() - unit_char.len_utf8()];
    let unit = unit_char.to_string();
    let unit = unit.as_str();
    let digits = digits.trim_end();
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let multiplier: i64 = match unit {
        "s" => 1000,
        "m" => 60 * 1000,
        "h" => 60 * 60 * 1000,
        "d" => 24 * 60 * 60 * 1000,
        "w" => 7 * 24 * 60 * 60 * 1000,
        _ => return None,
    };
    digits.parse::<i64>().ok().map(|n| n * multiplier)
}

/// Port of `parsePreserveConfig` in src/server/preserveExistingFiles.ts
pub fn parse_preserve_config(config_text: &str) -> Result<PreserveConfig> {
    let queries = qc::parse_file(config_text);
    let mut out = PreserveConfig::default();

    for query in &queries {
        let value = query.joined_tag_attrs();

        if query.command == "preserve-existing-files" {
            if value.is_empty() {
                return Err(anyhow!(
                    "Missing pattern for preserve-existing-files directive"
                ));
            }
            out.patterns.push(value);
        } else if query.command == "preserve-existing-files-max-age" {
            if value.is_empty() {
                return Err(anyhow!(
                    "Missing duration for preserve-existing-files-max-age directive"
                ));
            }
            match parse_duration(&value) {
                Some(parsed) => out.max_age_ms = Some(parsed),
                None => {
                    return Err(anyhow!(
                        "Invalid duration for preserve-existing-files-max-age: \"{}\" (expected e.g. 7d, 12h, 30m)",
                        value
                    ))
                }
            }
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// The `deploy-settings` directives the CLI reads, plus the `before-deploy`
/// block it runs locally. Port of src/client/fileList.ts, extended with the
/// settings the rest of the TypeScript CLI read out of the same block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientSettings {
    pub project_name: Option<String>,
    pub dest_url: Option<String>,
    /// Raw `local-root` value, still relative to the config file's directory.
    /// Use [`resolve_local_root`] rather than this field directly — it is the
    /// only place that bounds the value against the working directory.
    pub local_root: Option<String>,
    /// A `local-dir=` value, the setting's old name. Kept only so
    /// [`resolve_local_root`] can reject it by name; see the note there.
    pub legacy_local_dir: Option<String>,
    pub secrets_file: Option<String>,
    pub update_in_place: bool,
    pub web_static_dir: Option<String>,
    pub track_git_commit: bool,
    pub allow_dirty_git_tree: bool,
    pub candle_config_path: Option<String>,
    /// Paths allowlisted out of the security scan, from repeated
    /// `ignore-security-scan(<path>)` tags.
    pub ignore_security_scan: Vec<String>,
    /// Shell commands from the top-level `before-deploy` block. These run on
    /// the client before uploading, mirroring how `after-deploy` runs on the
    /// server.
    pub before_deploy_commands: Vec<String>,
}

pub fn parse_client_settings(config_text: &str) -> ClientSettings {
    let queries = qc::parse_file(config_text);
    let mut out = ClientSettings::default();

    for query in &queries {
        match query.command.as_str() {
            "deploy-settings" => read_deploy_settings(query, &mut out),
            "before-deploy" => {
                for tag in &query.tags {
                    if tag.attr == "shell" {
                        let command = tag.to_original_string();
                        if !command.is_empty() {
                            out.before_deploy_commands.push(command);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    out
}

fn read_deploy_settings(query: &Query, out: &mut ClientSettings) {
    if query.has_attr("project-name") {
        out.project_name = query.get_string_value("project-name");
    }
    if query.has_attr("dest-url") {
        out.dest_url = query.get_string_value("dest-url");
    }
    if query.has_attr("local-root") {
        out.local_root = query.get_string_value("local-root");
    }
    if query.has_attr("local-dir") {
        out.legacy_local_dir = query.get_string_value("local-dir");
    }
    if query.has_attr("secrets-file") {
        out.secrets_file = query.get_string_value("secrets-file");
    }
    if query.has_attr("web-static-dir") {
        out.web_static_dir = query.get_string_value("web-static-dir");
    }
    if query.has_attr("candle-config") {
        out.candle_config_path = query.get_string_value("candle-config");
    }
    if query.has_attr("update-in-place") {
        out.update_in_place = true;
    }
    if query.has_attr("track-git-commit") {
        out.track_git_commit = true;
    }
    if query.has_attr("allow-dirty-git-tree") {
        out.allow_dirty_git_tree = true;
    }

    for tag in &query.tags {
        if tag.attr == "ignore-security-scan" {
            // Written as `ignore-security-scan(<path>)`, so the value is a tag
            // list whose original text is the path.
            let path = tag.to_original_string();
            if !path.is_empty() {
                out.ignore_security_scan.push(path);
            }
        }
    }
}

/// Resolves the local root that every `include` / `exclude` / `ignore` rule,
/// every `before-deploy` / `after-deploy` `shell(...)` hook and the
/// `track-git-commit` git checks are evaluated against.
///
/// Default root is the directory holding the config file. `local-root` moves
/// it, and is itself relative to that directory — so a config living in
/// `deploy/` can say `local-root=..` and keep writing project-root-relative
/// paths. hotlaps' api-staging.qc does exactly that; without it the deploy CLI
/// resolves every `include` against `deploy/` and ships an empty bundle.
///
/// The resolved root must be the working directory or a directory below it.
/// That is a sanity bound, not a sandbox: you deploy a project by cd-ing into
/// it, so a root that climbs above the directory you are standing in means the
/// config is being read from somewhere unexpected. `local-root=..` run from the
/// repo root is fine; `local-root=../..` is not, and neither is pointing at a
/// sibling checkout. `cd` to the project and the same config works.
///
/// `override_dir` is the programmatic override used by callers that already
/// know the root. It is supplied by the caller rather than by the config, so it
/// is not bounded.
pub fn resolve_local_root(
    config_path: &Path,
    settings: &ClientSettings,
    override_dir: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(dir) = override_dir {
        return absolutize(dir);
    }

    // An error rather than a silent alias, and rather than the unknown-setting
    // shrug every other directive gets: ignoring it would resolve the root to
    // the config's own directory, and `deploy/` holds no deployable files, so
    // the deploy would upload nothing — which under `update-in-place` deletes
    // the entire destination.
    if let Some(legacy) = settings.legacy_local_dir.as_deref() {
        return Err(anyhow!(
            "{} sets `local-dir={}`, which is the old name for this setting. \
             Rename it to `local-root={}`.",
            config_path.display(),
            legacy,
            legacy
        ));
    }

    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let resolved = match settings.local_root.as_deref() {
        Some(local_root) if !local_root.is_empty() => absolutize(&config_dir.join(local_root))?,
        _ => absolutize(config_dir)?,
    };

    let cwd = absolutize(Path::new("."))?;
    if !resolved.starts_with(&cwd) {
        let setting = match settings.local_root.as_deref() {
            Some(value) if !value.is_empty() => format!("`local-root={value}` in "),
            _ => String::new(),
        };
        return Err(anyhow!(
            "{}{} resolves the local root to {}, which is outside the current directory {}. \
             Run the deploy from the project directory instead.",
            setting,
            config_path.display(),
            resolved.display(),
            cwd.display()
        ));
    }

    Ok(resolved)
}

/// Node's `Path.resolve` semantics: make absolute against the process cwd, then
/// fold away `.` and `..` lexically. Deliberately lexical — the config may name
/// a directory a `before-deploy` step is about to create, so this must not
/// require the path to exist.
fn absolutize(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let cwd = std::env::current_dir()?;
        if path.as_os_str().is_empty() {
            cwd
        } else {
            cwd.join(path)
        }
    };

    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ~/tools/deploy/sample.deploy
    const SAMPLE: &str = r#"deploy-settings
  project-name=sample
  dest-url=http://localhost:4800
  update-in-place
  web-static-dir=web/out
  track-git-commit

before-deploy
  shell(pnpm build)

after-deploy
  candle-restart(sample-api)

include web
include package.json

exclude web/node_modules
exclude web/.next
"#;

    /// Trimmed from ~/biz/hotlaps/deploy/api-staging.qc, which is the config
    /// that motivates local-root.
    const HOTLAPS_STAGING: &str = r#"# STAGING — deploys to do2.
deploy-settings
  project-name=hotlaps-api
  dest-url=https://apf1.dev
  local-root=..
  update-in-place
  track-git-commit

database backend/data/app.db agent-sql-access-blocked

before-deploy
  shell(backend/build-release.sh)
  shell(node tools/release-bundle-check.ts --bundle)

after-deploy
  shell(bash release-api/post-deploy.sh)

include release-api

ignore backend/data

preserve-existing-files release-api/static/**
preserve-existing-files-max-age 7d
"#;

    #[test]
    fn sample_activation_config() {
        let config = parse_activation_config(SAMPLE);
        assert_eq!(config.project_name, "sample");
        assert_eq!(config.candle_config_path, None);
        assert_eq!(
            config.after_deploy_actions,
            vec![AfterDeployAction::CandleRestart {
                service_name: "sample-api".to_string()
            }]
        );
    }

    #[test]
    fn sample_create_settings() {
        let settings = parse_create_settings(SAMPLE);
        assert!(settings.is_update_in_place);
        assert_eq!(settings.web_static_dir.as_deref(), Some("web/out"));
        assert!(settings.dynamic_routes.is_empty());
    }

    #[test]
    fn sample_client_settings() {
        let settings = parse_client_settings(SAMPLE);
        assert_eq!(settings.project_name.as_deref(), Some("sample"));
        assert_eq!(settings.dest_url.as_deref(), Some("http://localhost:4800"));
        assert!(settings.update_in_place);
        assert!(settings.track_git_commit);
        assert!(!settings.allow_dirty_git_tree);
        assert_eq!(settings.web_static_dir.as_deref(), Some("web/out"));
        assert_eq!(settings.local_root, None);
        assert_eq!(settings.before_deploy_commands, vec!["pnpm build"]);
        // `candle-restart` in after-deploy is a server action, never a local one.
        assert!(settings.ignore_security_scan.is_empty());
    }

    #[test]
    fn hotlaps_client_settings() {
        let settings = parse_client_settings(HOTLAPS_STAGING);
        assert_eq!(settings.project_name.as_deref(), Some("hotlaps-api"));
        assert_eq!(settings.dest_url.as_deref(), Some("https://apf1.dev"));
        assert_eq!(settings.local_root.as_deref(), Some(".."));
        assert!(settings.update_in_place);
        assert_eq!(
            settings.before_deploy_commands,
            vec![
                "backend/build-release.sh",
                "node tools/release-bundle-check.ts --bundle",
            ]
        );
    }

    #[test]
    fn hotlaps_databases_and_preserve() {
        let dbs = parse_deploy_database_configs(HOTLAPS_STAGING);
        assert_eq!(dbs.len(), 1);
        assert_eq!(dbs[0].path, "backend/data/app.db");
        assert!(dbs[0].agent_sql_access_blocked);

        let preserve = parse_preserve_config(HOTLAPS_STAGING).unwrap();
        assert_eq!(preserve.patterns, vec!["release-api/static/**"]);
        assert_eq!(preserve.max_age_ms, Some(7 * 24 * 60 * 60 * 1000));
    }

    #[test]
    fn database_without_agent_block() {
        let dbs = parse_deploy_database_configs("database backend/data/app.db\n");
        assert_eq!(dbs.len(), 1);
        assert!(!dbs[0].agent_sql_access_blocked);
    }

    #[test]
    fn ignore_security_scan_tags_accumulate() {
        let text = r#"deploy-settings
  project-name=x
  ignore-security-scan(config/keys.example.json)
  ignore-security-scan(fixtures/secrets)
"#;
        let settings = parse_client_settings(text);
        assert_eq!(
            settings.ignore_security_scan,
            vec!["config/keys.example.json", "fixtures/secrets"]
        );
    }

    /// Resolution is lexical, so these build paths under the real working
    /// directory rather than a temp tree — the bound is measured against the
    /// process cwd, and tests share one cwd across threads, so none of them may
    /// change it.
    fn cwd() -> PathBuf {
        std::env::current_dir().unwrap()
    }

    fn settings_with_local_root(value: &str) -> ClientSettings {
        let mut settings = parse_client_settings(SAMPLE);
        settings.local_root = Some(value.to_string());
        settings
    }

    #[test]
    fn local_root_moves_the_root_up_out_of_the_config_directory() {
        let settings = parse_client_settings(HOTLAPS_STAGING);
        assert_eq!(settings.local_root.as_deref(), Some(".."));
        let resolved =
            resolve_local_root(&cwd().join("deploy/api-staging.qc"), &settings, None).unwrap();
        assert_eq!(resolved, cwd());
    }

    #[test]
    fn local_root_defaults_to_the_config_directory() {
        let settings = parse_client_settings(SAMPLE);
        let resolved = resolve_local_root(&cwd().join("sample.deploy"), &settings, None).unwrap();
        assert_eq!(resolved, cwd());
    }

    #[test]
    fn local_root_may_point_at_a_subdirectory() {
        let settings = settings_with_local_root("frontend");
        let resolved = resolve_local_root(&cwd().join("api.qc"), &settings, None).unwrap();
        assert_eq!(resolved, cwd().join("frontend"));
    }

    /// The bound: one `..` from `deploy/` lands on the working directory and is
    /// fine, two climbs above it and is not.
    #[test]
    fn a_local_root_above_the_working_directory_is_rejected() {
        let settings = settings_with_local_root("../..");
        let err = resolve_local_root(&cwd().join("deploy/api.qc"), &settings, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside the current directory"), "{err}");
        assert!(err.contains("local-root=../.."), "{err}");
    }

    /// Not an ancestor of the working directory, but still outside it.
    #[test]
    fn a_sibling_local_root_is_rejected() {
        let settings = settings_with_local_root("../../other-project");
        let err = resolve_local_root(&cwd().join("deploy/api.qc"), &settings, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside the current directory"), "{err}");
    }

    /// A path that only leaves the working directory after the lexical fold, so
    /// the check has to run on the resolved path rather than the raw value.
    #[test]
    fn a_local_root_that_climbs_back_out_is_rejected() {
        let settings = settings_with_local_root("frontend/../../../elsewhere");
        let err = resolve_local_root(&cwd().join("deploy/api.qc"), &settings, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside the current directory"), "{err}");
    }

    /// The default root is bounded too: a config read from a sibling checkout
    /// has no `local-root` to blame, so the message names only the config.
    #[test]
    fn a_config_outside_the_working_directory_is_rejected() {
        let settings = parse_client_settings(SAMPLE);
        let elsewhere = cwd().join("../other-project/api.qc");
        let err = resolve_local_root(&elsewhere, &settings, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside the current directory"), "{err}");
        assert!(!err.contains("local-root="), "{err}");
    }

    /// Renaming the setting has to be loud: silently ignoring `local-dir` roots
    /// a `deploy/`-hosted config at `deploy/`, and an empty file list plus
    /// `update-in-place` deletes the whole destination.
    #[test]
    fn the_old_local_dir_name_is_rejected() {
        let settings = parse_client_settings(&HOTLAPS_STAGING.replace("local-root", "local-dir"));
        assert_eq!(settings.local_root, None);
        assert_eq!(settings.legacy_local_dir.as_deref(), Some(".."));

        let err = resolve_local_root(&cwd().join("deploy/api.qc"), &settings, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("old name for this setting"), "{err}");
        assert!(err.contains("`local-root=..`"), "{err}");
    }

    #[test]
    fn an_explicit_override_is_not_bounded_by_the_working_directory() {
        let settings = parse_client_settings(HOTLAPS_STAGING);
        let resolved = resolve_local_root(
            &cwd().join("deploy/api-staging.qc"),
            &settings,
            Some(Path::new("/tmp/checkout")),
        )
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/checkout"));
    }

    #[test]
    fn dynamic_routes_are_parsed() {
        let text = r#"deploy-settings
  project-name=web

dynamic-route from=/post/detail to=post.html metadata-source=meta.json metadata-cache-ttl=60
dynamic-route from=/bad
"#;
        let settings = parse_create_settings(text);
        assert_eq!(settings.dynamic_routes.len(), 1);
        let route = &settings.dynamic_routes[0];
        assert_eq!(route.pattern, "/post/detail");
        assert_eq!(route.file, "post.html");
        assert_eq!(route.metadata_source.as_deref(), Some("meta.json"));
        assert_eq!(route.metadata_cache_ttl, Some(60));
    }

    #[test]
    fn preserve_durations() {
        assert_eq!(parse_duration("30s"), Some(30_000));
        assert_eq!(parse_duration("30m"), Some(30 * 60 * 1000));
        assert_eq!(parse_duration("12h"), Some(12 * 60 * 60 * 1000));
        assert_eq!(parse_duration("2w"), Some(2 * 7 * 24 * 60 * 60 * 1000));
        assert_eq!(parse_duration("7"), None);
        assert_eq!(parse_duration("d"), None);
    }

    #[test]
    fn bad_preserve_duration_is_an_error() {
        let err = parse_preserve_config("preserve-existing-files-max-age 7y\n").unwrap_err();
        assert!(err.to_string().contains("Invalid duration"));

        // A directive with no tags at all is dropped by the qc parser, so it
        // reaches this function as no query rather than as an empty one.
        assert!(parse_preserve_config("preserve-existing-files\n")
            .unwrap()
            .patterns
            .is_empty());
    }
}
