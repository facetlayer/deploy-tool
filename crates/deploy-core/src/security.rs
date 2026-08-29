//! Security scan of a resolved file list. Port of src/client/securityScan.ts.
//!
//! This runs on the client, between resolving the file list and uploading it.
//! It is a blunt instrument on purpose: it blocks obviously-sensitive files by
//! name so a stray `.env` or private key never leaves the machine. The
//! allowlist comes from `ignore-security-scan` directives in the config.

use anyhow::{bail, Result};
use std::path::Path;

/// Blocked when they match the basename or the whole relative path exactly.
pub const DISALLOWED_FILES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.development",
    ".env.production",
    ".env.test",
    ".git",
    ".gitignore",
    ".DS_Store",
    "Thumbs.db",
    ".ssh",
    "id_rsa",
    "id_ed25519",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    "private.key",
    "server.key",
    "ssl.key",
    "certificate.key",
    ".aws",
    ".gcp",
    ".azure",
    "secrets.json",
    "credentials.json",
    "database.env",
    ".npmrc",
    ".yarnrc",
];

/// The JS list is regexes; each is either a suffix, a prefix or a substring
/// test, so they are represented directly rather than pulling in a regex crate.
enum PathPattern {
    Contains(&'static str),
    EndsWith(&'static str),
    StartsWith(&'static str),
}

/// Matched against the full relative path *and* the basename, per the JS.
const DISALLOWED_PATH_PATTERNS: &[PathPattern] = &[
    PathPattern::Contains(".env."),     // .env.anything
    PathPattern::EndsWith(".pem"),      // any .pem file
    PathPattern::EndsWith(".key"),      // any .key file
    PathPattern::EndsWith(".p12"),      // any .p12 file
    PathPattern::EndsWith(".pfx"),      // any .pfx file
    PathPattern::EndsWith("_rsa"),      // SSH keys
    PathPattern::EndsWith("_ed25519"),  // SSH keys
    PathPattern::StartsWith(".git/"),   // .git directory contents
    PathPattern::StartsWith(".ssh/"),   // .ssh directory contents
    PathPattern::StartsWith(".aws/"),   // AWS config directory
    PathPattern::StartsWith(".gcp/"),   // Google Cloud config
    PathPattern::StartsWith(".azure/"), // Azure config
];

/// Keywords matched (case-insensitively) against the basename only. Matching
/// the full path would flag ordinary web routes like `forgot-password/page.js`.
pub const DISALLOWED_BASENAME_KEYWORDS: &[&str] = &["secret", "credential", "password"];

impl PathPattern {
    fn is_match(&self, text: &str) -> bool {
        match self {
            PathPattern::Contains(needle) => text.contains(needle),
            PathPattern::EndsWith(suffix) => text.ends_with(suffix),
            PathPattern::StartsWith(prefix) => text.starts_with(prefix),
        }
    }
}

fn basename(file: &str) -> &str {
    Path::new(file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(file)
}

fn is_ignored(file: &str, ignore_paths: &[String]) -> bool {
    ignore_paths
        .iter()
        .any(|ignore| file == ignore || file.starts_with(&format!("{}/", ignore)))
}

/// Whether one relative path is blocked, ignoring the allowlist.
pub fn is_dangerous_path(file: &str) -> bool {
    let basename = basename(file);

    if DISALLOWED_FILES.contains(&basename) || DISALLOWED_FILES.contains(&file) {
        return true;
    }

    for pattern in DISALLOWED_PATH_PATTERNS {
        if pattern.is_match(file) || pattern.is_match(basename) {
            return true;
        }
    }

    let lowered = basename.to_lowercase();
    DISALLOWED_BASENAME_KEYWORDS
        .iter()
        .any(|keyword| lowered.contains(keyword))
}

/// Every file in the list that is blocked and not allowlisted, in list order.
pub fn scan_file_list(files: &[String], ignore_paths: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|file| !is_ignored(file, ignore_paths) && is_dangerous_path(file))
        .cloned()
        .collect()
}

/// Fails if the file list contains anything that should not be deployed.
/// `ignore_paths` comes from the config's `ignore-security-scan` directives; a
/// path is allowlisted if it matches exactly or is under an allowlisted
/// directory.
pub fn validate_file_list(files: &[String], ignore_paths: &[String]) -> Result<()> {
    let dangerous = scan_file_list(files, ignore_paths);

    if !dangerous.is_empty() {
        let listed = dangerous
            .iter()
            .map(|file| format!("  - {}", file))
            .collect::<Vec<_>>()
            .join("\n");

        bail!(
            "Security Error: The following files should not be deployed as they may contain \
             sensitive information:\n{}\n\nPlease add these files to your exclude rules in the \
             config file, or use ignore-security-scan in deploy-settings to allowlist specific \
             paths.",
            listed
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(files: &[&str]) -> Result<()> {
        let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        validate_file_list(&files, &[])
    }

    fn check_with_ignores(files: &[&str], ignores: &[&str]) -> Result<()> {
        let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        let ignores: Vec<String> = ignores.iter().map(|f| f.to_string()).collect();
        validate_file_list(&files, &ignores)
    }

    fn assert_blocked(files: &[&str]) {
        let err = check(files).unwrap_err().to_string();
        assert!(
            err.contains("Security Error"),
            "expected a block: {:?}",
            files
        );
    }

    // --- Exact file matches ---

    #[test]
    fn blocks_env_files() {
        assert_blocked(&[".env"]);
        assert_blocked(&[".env.production"]);
        assert_blocked(&[".env.local"]);
    }

    #[test]
    fn blocks_ssh_key_files() {
        assert_blocked(&["id_rsa"]);
        assert_blocked(&["id_ed25519"]);
    }

    #[test]
    fn blocks_credentials_and_secrets_json() {
        assert_blocked(&["credentials.json"]);
        assert_blocked(&["secrets.json"]);
    }

    #[test]
    fn blocks_key_and_pem_files() {
        assert_blocked(&["private.key"]);
        assert_blocked(&["server.pem"]);
        assert_blocked(&["cert.pfx"]);
    }

    #[test]
    fn blocks_git_directory_contents() {
        assert_blocked(&[".git/config"]);
        assert_blocked(&[".git/HEAD"]);
    }

    #[test]
    fn blocks_cloud_config_directories() {
        assert_blocked(&[".aws/credentials"]);
        assert_blocked(&[".gcp/service-account.json"]);
        assert_blocked(&[".azure/config"]);
    }

    // --- config.json is deliberately not blocked ---

    #[test]
    fn allows_config_json() {
        check(&["config.json"]).unwrap();
        check(&["src/config.json"]).unwrap();
    }

    // --- Keyword patterns: basename-only matching ---

    #[test]
    fn blocks_password_in_the_basename() {
        assert_blocked(&["passwords.txt"]);
        assert_blocked(&["my-password-file.json"]);
    }

    #[test]
    fn allows_password_named_route_directories() {
        // Common web app routes, not sensitive files.
        check(&["forgot-password/page.js"]).unwrap();
        check(&["reset-password/page.tsx"]).unwrap();
        check(&["change-password/index.html"]).unwrap();
        check(&[".next/server/app/forgot-password/page.js"]).unwrap();
    }

    #[test]
    fn blocks_secret_in_the_basename() {
        assert_blocked(&["my-secret.json"]);
        assert_blocked(&["SECRET_KEY.txt"]);
    }

    #[test]
    fn allows_secret_named_directories_with_a_clean_basename() {
        check(&["secret-page/index.js"]).unwrap();
    }

    #[test]
    fn blocks_credential_in_the_basename() {
        assert_blocked(&["credential-store.yaml"]);
    }

    #[test]
    fn allows_credential_named_directories_with_a_clean_basename() {
        check(&["credential-setup/page.js"]).unwrap();
    }

    // --- Normal files ---

    #[test]
    fn allows_normal_application_files() {
        check(&[
            "index.js",
            "package.json",
            "src/app.ts",
            "public/styles.css",
            "dist/bundle.js",
        ])
        .unwrap();
    }

    // --- ignore-security-scan option ---

    #[test]
    fn ignored_paths_bypass_the_scan() {
        check_with_ignores(&[".env"], &[".env"]).unwrap();
    }

    #[test]
    fn ignored_directory_prefixes_bypass_the_scan() {
        check_with_ignores(
            &["secrets/api-key.json", "secrets/db-password.txt"],
            &["secrets"],
        )
        .unwrap();
    }

    #[test]
    fn still_blocks_non_ignored_files() {
        let err = check_with_ignores(&[".env", "credentials.json"], &[".env"]).unwrap_err();
        assert!(err.to_string().contains("Security Error"));
    }

    #[test]
    fn supports_multiple_ignore_paths() {
        check_with_ignores(
            &[".env", "credentials.json", "data/secret-config.yaml"],
            &[".env", "credentials.json", "data"],
        )
        .unwrap();
    }

    // --- Error message ---

    #[test]
    fn error_message_lists_every_dangerous_file() {
        let err = check(&[".env", "id_rsa", "index.js"])
            .unwrap_err()
            .to_string();
        assert!(err.contains(".env"));
        assert!(err.contains("id_rsa"));
        assert!(!err.contains("index.js"));
    }
}
