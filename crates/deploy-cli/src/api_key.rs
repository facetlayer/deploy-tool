//! Deploy API key resolution. Port of src/client/apiKey.ts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parses a `.env`-style secrets file into key/value pairs.
///
/// This is deliberately lenient rather than a real dotenv implementation: the
/// files it reads are hand-written, so `export ` prefixes, surrounding quotes
/// and `#` comment lines all have to be tolerated.
pub fn parse_env_file(contents: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();

    for raw_line in contents.split('\n') {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let stripped = match line.strip_prefix("export ") {
            Some(rest) => rest.trim(),
            None => line,
        };

        let Some(eq) = stripped.find('=') else {
            continue;
        };

        let key = stripped[..eq].trim().to_string();
        let mut value = stripped[eq + 1..].trim();

        let quoted = value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')));
        if quoted {
            value = &value[1..value.len() - 1];
        }

        result.insert(key, value.to_string());
    }

    result
}

/// Expands a leading `~` so a config can say `~/secrets/deploy-prod.env`
/// without hardcoding an absolute path.
pub fn expand_home(file_path: &str) -> PathBuf {
    let Some(home) = home_dir() else {
        return PathBuf::from(file_path);
    };

    if file_path == "~" {
        return home;
    }
    if let Some(rest) = file_path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(file_path)
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The shared fallback secrets file, `~/secrets/deploy.env`.
pub fn default_secrets_file() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("secrets")
        .join("deploy.env")
}

fn read_key_from_secrets_file(secrets_file: &Path) -> Option<String> {
    // An unreadable or missing file is not an error: it just means this source
    // has no key and the next one in the precedence order gets a turn.
    let contents = std::fs::read_to_string(secrets_file).ok()?;
    let parsed = parse_env_file(&contents);
    parsed
        .get("DEPLOY_API_KEY")
        .or_else(|| parsed.get("GOOBERNETES_API_KEY"))
        .filter(|value| !value.is_empty())
        .cloned()
}

/// Resolve the deploy API key. Precedence:
///
///   1. A config-specified `secrets-file` (per-server key), when the file
///      exists and contains a key. This lets a single client deploy to servers
///      that require different keys (e.g. a hardened prod that rejects the
///      shared key).
///   2. The `DEPLOY_API_KEY` / `GOOBERNETES_API_KEY` environment variables.
///      Kept above the global default so CI (which sets the env var and has no
///      secrets file on disk) keeps working.
///   3. The shared `~/secrets/deploy.env` fallback.
pub fn find_api_key(secrets_file: Option<&str>) -> Option<String> {
    let env = |name: &str| std::env::var(name).ok();
    find_api_key_with(secrets_file, &env, &default_secrets_file())
}

/// The body of [`find_api_key`], with the environment and the fallback file
/// injected so tests do not have to mutate process-wide state.
pub fn find_api_key_with(
    secrets_file: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
    fallback_secrets_file: &Path,
) -> Option<String> {
    if let Some(secrets_file) = secrets_file {
        if let Some(key) = read_key_from_secrets_file(&expand_home(secrets_file)) {
            return Some(key);
        }
    }

    env("DEPLOY_API_KEY")
        .filter(|value| !value.is_empty())
        .or_else(|| env("GOOBERNETES_API_KEY").filter(|value| !value.is_empty()))
        .or_else(|| read_key_from_secrets_file(fallback_secrets_file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env(_name: &str) -> Option<String> {
        None
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let path = std::env::temp_dir().join(format!(
                "deploy-cli-apikey-{}-{}",
                name,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let file = self.path.join(name);
            std::fs::write(&file, contents).unwrap();
            file
        }

        fn join(&self, name: &str) -> PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn find(secrets_file: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
        let missing_fallback = std::env::temp_dir().join("deploy-cli-no-such-fallback.env");
        find_api_key_with(
            secrets_file.map(|p| p.to_str().unwrap()),
            env,
            &missing_fallback,
        )
    }

    #[test]
    fn reads_the_key_from_a_config_specified_secrets_file() {
        let dir = TempDir::new("from-config");
        let file = dir.write("prod.env", "DEPLOY_API_KEY=prod-key\n");
        assert_eq!(find(Some(&file), &empty_env).as_deref(), Some("prod-key"));
    }

    #[test]
    fn secrets_file_overrides_the_env_var() {
        let dir = TempDir::new("override-env");
        let file = dir.write("prod.env", "DEPLOY_API_KEY=prod-key\n");
        let env = |name: &str| (name == "DEPLOY_API_KEY").then(|| "env-key".to_string());
        assert_eq!(find(Some(&file), &env).as_deref(), Some("prod-key"));
    }

    #[test]
    fn falls_back_to_the_env_var_when_the_secrets_file_is_missing() {
        let dir = TempDir::new("missing-file");
        let env = |name: &str| (name == "DEPLOY_API_KEY").then(|| "env-key".to_string());
        assert_eq!(
            find(Some(&dir.join("does-not-exist.env")), &env).as_deref(),
            Some("env-key")
        );
    }

    #[test]
    fn falls_back_to_the_env_var_when_the_secrets_file_has_no_key() {
        let dir = TempDir::new("no-key");
        let file = dir.write("empty.env", "# no key here\n");
        let env = |name: &str| (name == "DEPLOY_API_KEY").then(|| "env-key".to_string());
        assert_eq!(find(Some(&file), &env).as_deref(), Some("env-key"));
    }

    #[test]
    fn tolerates_export_prefixes_and_quotes() {
        let dir = TempDir::new("quoted");
        let file = dir.write("quoted.env", "export DEPLOY_API_KEY=\"quoted-key\"\n");
        assert_eq!(find(Some(&file), &empty_env).as_deref(), Some("quoted-key"));
    }

    #[test]
    fn uses_the_env_var_when_no_secrets_file_is_given() {
        let env = |name: &str| (name == "DEPLOY_API_KEY").then(|| "env-key".to_string());
        assert_eq!(find(None, &env).as_deref(), Some("env-key"));
    }

    #[test]
    fn goobernetes_env_var_is_the_second_choice() {
        let env =
            |name: &str| (name == "GOOBERNETES_API_KEY").then(|| "goobernetes-key".to_string());
        assert_eq!(find(None, &env).as_deref(), Some("goobernetes-key"));
    }

    #[test]
    fn falls_back_to_the_shared_secrets_file() {
        let dir = TempDir::new("shared");
        let fallback = dir.write("deploy.env", "DEPLOY_API_KEY=shared-key\n");
        assert_eq!(
            find_api_key_with(None, &empty_env, &fallback).as_deref(),
            Some("shared-key")
        );
    }

    #[test]
    fn env_file_parser_handles_comments_quotes_and_blank_lines() {
        let parsed = parse_env_file(
            "# a comment\n\
             \n\
             export DEPLOY_API_KEY='single'\n\
             OTHER=\"double\"\n\
             BARE=plain\n\
             not-an-assignment\n",
        );
        assert_eq!(parsed.get("DEPLOY_API_KEY").unwrap(), "single");
        assert_eq!(parsed.get("OTHER").unwrap(), "double");
        assert_eq!(parsed.get("BARE").unwrap(), "plain");
        assert!(parsed.get("not-an-assignment").is_none());
    }

    #[test]
    fn env_file_parser_keeps_equals_signs_inside_a_value() {
        let parsed = parse_env_file("DEPLOY_API_KEY=abc=def==\n");
        assert_eq!(parsed.get("DEPLOY_API_KEY").unwrap(), "abc=def==");
    }
}
