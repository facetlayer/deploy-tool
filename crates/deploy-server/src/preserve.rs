//! `preserve-existing-files` support.
//!
//! Port of the old daemon's `preserve.rs` (from
//! `src/server/preserveExistingFiles.ts`), with the rule parsing and the
//! destination scan now coming from `deploy_core`.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use deploy_core::config::{parse_preserve_config, PreserveConfig};
use deploy_core::filelist::{
    find_leftover_files, parse_rules_file, FileList, FileMatchRule, RuleType,
};

pub struct LeftoverScan {
    /// Destination files that should be deleted, with preserve rules already
    /// applied. DANGEROUS: the caller deletes these.
    pub leftovers: FileList,
    /// The plain file rules, without preserve patterns folded in. Needed to
    /// recompute what preservation actually saved.
    pub parsed_rules: Vec<FileMatchRule>,
    pub preserve: PreserveConfig,
}

/// Computes the destination files that should be deleted, honoring any
/// `preserve-existing-files` directives.
pub fn find_leftovers_respecting_preserve(
    deploy_dir: &Path,
    incoming_files: &FileList,
    source_config: &str,
) -> Result<LeftoverScan> {
    let parsed_rules = parse_rules_file(source_config)?;
    let preserve = parse_preserve_config(source_config)?;

    // Preserve patterns become ignore-destination rules, so the leftover scan
    // simply never reports them.
    let mut rules_with_preserve = parse_rules_file(source_config)?;
    for pattern in &preserve.patterns {
        rules_with_preserve.push(FileMatchRule::new(RuleType::IgnoreDestination, pattern));
    }

    let leftovers = find_leftover_files(deploy_dir, incoming_files, &rules_with_preserve);

    Ok(LeftoverScan {
        leftovers,
        parsed_rules,
        preserve,
    })
}

/// Garbage-collects preserved files older than `max_age_ms`. The preserved set
/// is the leftovers that *would* have been deleted without the preserve rules,
/// minus the ones actually kept.
pub fn prune_stale_preserved_files(
    deploy_dir: &Path,
    incoming_files: &FileList,
    parsed_rules: &[FileMatchRule],
    kept_leftovers: &FileList,
    max_age_ms: i64,
) {
    let all_leftovers = find_leftover_files(deploy_dir, incoming_files, parsed_rules);

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_millis(max_age_ms.max(0) as u64))
        .unwrap_or(UNIX_EPOCH);

    for file in all_leftovers.list_all() {
        // Already scheduled for deletion by the caller, not a preserved file.
        if kept_leftovers.has_rel_path(&file.rel_path) {
            continue;
        }

        match std::fs::metadata(&file.source_path).and_then(|meta| meta.modified()) {
            Ok(modified) => {
                if modified < cutoff {
                    println!("  Pruning stale preserved file: {}", file.rel_path);
                    if let Err(error) = std::fs::remove_file(&file.source_path) {
                        eprintln!("Failed to prune {}: {}", file.rel_path, error);
                    }
                }
            }
            Err(error) => {
                eprintln!("Failed to prune {}: {}", file.rel_path, error);
            }
        }
    }
}
