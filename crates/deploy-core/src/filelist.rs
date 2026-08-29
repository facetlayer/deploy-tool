//! Include/exclude rules and the directory walks that apply them.
//!
//! Port of @facetlayer/file-manifest: `FileMatchRule`, `parseRulesFile`,
//! `resolveFileList`, `findLeftoverFiles`, `setupEmptyDirectories`, `FileList`
//! and `FileEntry`. This decides what ships and, in `find_leftover_files`, what
//! gets deleted, so it is a deliberately literal port.

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::glob::Pattern;
use crate::qc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuleType {
    Include,
    Exclude,
    /// Applies only to the destination scan, never to the source file list.
    IgnoreDestination,
    /// Applies to both the source file list and the destination scan.
    Ignore,
}

pub struct FileMatchRule {
    pub rule_type: RuleType,
    pub pattern: String,
    compiled: Option<Pattern>,
    /// The pattern split on '/', each segment compiled on its own, for
    /// `could_contain_match`.
    segments: Vec<(String, Option<Pattern>)>,
}

impl FileMatchRule {
    pub fn new(rule_type: RuleType, pattern: &str) -> FileMatchRule {
        let segments = pattern
            .split('/')
            .map(|seg| (seg.to_string(), Pattern::new(seg)))
            .collect();

        FileMatchRule {
            rule_type,
            pattern: pattern.to_string(),
            compiled: Pattern::new(pattern),
            segments,
        }
    }

    pub fn is_match(&self, rel_path: &str) -> bool {
        match &self.compiled {
            Some(pattern) => pattern.is_match(rel_path),
            None => false,
        }
    }

    /// Whether a directory could contain files matching this rule's pattern,
    /// walking path segments and pattern segments together.
    fn could_contain_match(&self, dir_rel_path: &str) -> bool {
        let dir_parts: Vec<&str> = dir_rel_path.split('/').collect();

        let mut di = 0;
        let mut pi = 0;

        while di < dir_parts.len() && pi < self.segments.len() {
            let (text, compiled) = &self.segments[pi];

            // `**` matches any number of segments, so anything below here could match.
            if text == "**" {
                return true;
            }

            let matched = compiled
                .as_ref()
                .map(|p| p.is_match(dir_parts[di]))
                .unwrap_or(false);

            if !matched {
                return false;
            }

            di += 1;
            pi += 1;
        }

        // All directory segments consumed with pattern segments left over: this
        // directory is an ancestor of potential matches.
        di == dir_parts.len() && pi < self.segments.len()
    }
}

/// One file in a `FileList`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: u32,
    #[serde(rename = "relPath")]
    pub rel_path: String,
    #[serde(rename = "sourcePath")]
    pub source_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha: Option<String>,
}

/// Port of the `FileList` class: an insertion-ordered list of files with
/// lookup by relative path.
#[derive(Debug, Default)]
pub struct FileList {
    files: Vec<FileEntry>,
    by_rel_path: HashMap<String, usize>,
    next_id: u32,
}

impl FileList {
    pub fn new() -> FileList {
        FileList {
            files: Vec::new(),
            by_rel_path: HashMap::new(),
            next_id: 1,
        }
    }

    pub fn insert(&mut self, rel_path: &str, source_path: &Path) {
        self.insert_entry(rel_path, source_path, None);
    }

    pub fn insert_entry(&mut self, rel_path: &str, source_path: &Path, sha: Option<String>) {
        let id = self.next_id;
        self.next_id += 1;

        let index = self.files.len();
        self.files.push(FileEntry {
            id,
            rel_path: rel_path.to_string(),
            source_path: source_path.to_path_buf(),
            sha,
        });
        self.by_rel_path.insert(rel_path.to_string(), index);
    }

    /// The incoming-manifest side of `find_leftover_files` only needs relative
    /// paths; the server has those without any local source file.
    pub fn from_rel_paths<I, S>(rel_paths: I) -> FileList
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut list = FileList::new();
        for rel_path in rel_paths {
            let rel_path = rel_path.as_ref();
            list.insert(rel_path, Path::new(rel_path));
        }
        list
    }

    pub fn list_all(&self) -> &[FileEntry] {
        &self.files
    }

    pub fn rel_paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.rel_path.clone()).collect()
    }

    pub fn get_by_rel_path(&self, rel_path: &str) -> Option<&FileEntry> {
        self.by_rel_path.get(rel_path).map(|i| &self.files[*i])
    }

    pub fn has_rel_path(&self, rel_path: &str) -> bool {
        self.by_rel_path.contains_key(rel_path)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Port of `parseRulesFile`: pulls the include / exclude / ignore /
/// ignore-destination rules out of a `.qc` config.
pub fn parse_rules_file(rule_config: &str) -> Result<Vec<FileMatchRule>> {
    let queries = qc::parse_file(rule_config);
    let mut rules = Vec::new();

    for query in &queries {
        // The pattern is every tag's attr text re-joined with no separator, not
        // a tag value. A glob like `web/**/*.ts` lexes into several tags, and
        // reading it as a string value would mangle it.
        let pattern = query.joined_tag_attrs();

        let rule_type = match query.command.as_str() {
            "include" => RuleType::Include,
            "exclude" => RuleType::Exclude,
            "ignore-destination" => RuleType::IgnoreDestination,
            "ignore" => RuleType::Ignore,
            // Unrecognized commands are skipped. Note the JS checks for a
            // missing pattern before it looks at the command, so a bare
            // `include` with no argument is an error for any command name.
            _ => {
                if pattern.is_empty() {
                    return Err(anyhow!("Missing pattern for {} rule", query.command));
                }
                continue;
            }
        };

        if pattern.is_empty() {
            return Err(anyhow!("Missing pattern for {} rule", query.command));
        }

        rules.push(FileMatchRule::new(rule_type, &pattern));
    }

    Ok(rules)
}

/// Relative paths are always '/'-separated, in the manifest, on the wire and in
/// the database.
fn path_to_rel_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolves `.` and `..` textually, without touching the filesystem.
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }

    out
}

/// Matches the JS use of `lstat`: a symlink to a directory counts as a file and
/// is not descended into.
fn is_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Directory entries in source order, sorted by name. Node's `readdir` order is
/// filesystem-dependent; sorting makes a deployment's file list reproducible.
fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
    paths.sort();
    Ok(paths)
}

struct Resolver<'a> {
    source_dir: &'a Path,
    rules: &'a [FileMatchRule],
    files: FileList,
}

impl<'a> Resolver<'a> {
    fn rel_path(&self, local_path: &Path) -> String {
        match local_path.strip_prefix(self.source_dir) {
            Ok(rel) => path_to_rel_string(rel),
            Err(_) => path_to_rel_string(local_path),
        }
    }

    /// Whether a path is included by the rules. Exclude and ignore rules take
    /// priority over include rules.
    fn should_include(&self, rel_path: &str, default_value: bool) -> bool {
        for rule in self.rules {
            if matches!(rule.rule_type, RuleType::Exclude | RuleType::Ignore)
                && rule.is_match(rel_path)
            {
                return false;
            }
        }

        for rule in self.rules {
            if rule.rule_type == RuleType::Include && rule.is_match(rel_path) {
                return true;
            }
        }

        default_value
    }

    /// Whether this directory is an ancestor of any include pattern, so we know
    /// to traverse into it even though it isn't itself included.
    fn is_ancestor_of_include(&self, rel_path: &str) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.rule_type == RuleType::Include && rule.could_contain_match(rel_path))
    }

    /// `assume_include_contents`:
    ///  - `false` for the top-level source directory. A top-level file is not
    ///    included unless it is explicitly included.
    ///  - `true` for a subdirectory that already matched an `include` rule, so
    ///    its full recursive contents are included unless stated otherwise.
    fn recursive_include_sub_directory(
        &mut self,
        local_dir: &Path,
        assume_include_contents: bool,
    ) -> Result<()> {
        for local_sub_file in read_dir_sorted(local_dir)? {
            let rel_path = self.rel_path(&local_sub_file);

            if is_directory(&local_sub_file) {
                if self.should_include(&rel_path, assume_include_contents) {
                    self.recursive_include_sub_directory(&local_sub_file, true)?;
                } else if self.is_ancestor_of_include(&rel_path) {
                    self.recursive_include_sub_directory(&local_sub_file, false)?;
                }
                continue;
            }

            if !self.should_include(&rel_path, assume_include_contents) {
                continue;
            }

            self.files.insert(&rel_path, &local_sub_file);
        }

        Ok(())
    }
}

/// Port of `resolveFileList`: the recursive walk of a source directory that
/// produces the file list a deployment ships.
pub fn resolve_file_list(source_dir: &Path, rules: &[FileMatchRule]) -> Result<FileList> {
    if !is_directory(source_dir) {
        return Err(anyhow!(
            "Usage error: sourceDir must be a directory: {}",
            source_dir.display()
        ));
    }

    let mut resolver = Resolver {
        source_dir,
        rules,
        files: FileList::new(),
    };

    resolver.recursive_include_sub_directory(source_dir, false)?;

    Ok(resolver.files)
}

/// `resolve_file_list` starting from unparsed config text.
pub fn resolve_file_list_from_config(source_dir: &Path, rule_config: &str) -> Result<FileList> {
    let rules = parse_rules_file(rule_config)?;
    resolve_file_list(source_dir, &rules)
}

/// Port of `findLeftoverFiles`: every file under the destination directory that
/// is not in the incoming manifest and is not ignored.
///
/// DANGEROUS: callers delete what this returns. An `ignore` /
/// `ignore-destination` rule is the only thing standing between a server-side
/// file and deletion — on 2026-05-23 a missing `ignore backend/data` in
/// hotlaps' api-staging config let an update-in-place deploy treat a live
/// production SQLite database as orphaned and delete it. Any change here needs
/// to keep failing safe: when in doubt, do not report a file as leftover.
pub fn find_leftover_files(
    target_dir: &Path,
    incoming_files: &FileList,
    rules: &[FileMatchRule],
) -> FileList {
    let mut leftovers = FileList::new();
    scan_directory(
        target_dir,
        target_dir,
        incoming_files,
        rules,
        &mut leftovers,
    );
    leftovers
}

fn scan_directory(
    target_dir: &Path,
    current_dir: &Path,
    incoming_files: &FileList,
    rules: &[FileMatchRule],
    leftovers: &mut FileList,
) {
    // An unreadable directory yields nothing, rather than aborting the scan.
    let entries = match read_dir_sorted(current_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for full_path in entries {
        let rel_path = match full_path.strip_prefix(target_dir) {
            Ok(rel) => path_to_rel_string(rel),
            Err(_) => continue,
        };

        let should_ignore = rules.iter().any(|rule| {
            matches!(
                rule.rule_type,
                RuleType::IgnoreDestination | RuleType::Ignore
            ) && rule.is_match(&rel_path)
        });

        if should_ignore {
            continue;
        }

        if is_directory(&full_path) {
            scan_directory(target_dir, &full_path, incoming_files, rules, leftovers);
        } else if !incoming_files.has_rel_path(&rel_path) {
            leftovers.insert(&rel_path, &full_path);
        }
    }
}

/// Port of `setupEmptyDirectories`: create every parent directory a manifest
/// implies, so file writes don't each have to create their own.
pub fn setup_empty_directories<I, S>(target_dir: &Path, rel_paths: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut needed: BTreeSet<PathBuf> = BTreeSet::new();
    let target_dir = lexically_normalize(target_dir);

    for rel_path in rel_paths {
        // Normalized before the containment check below: `Path::starts_with`
        // compares components, so an un-normalized `<target>/../escaped` would
        // otherwise pass as being inside `<target>`. Node's `path.join`
        // normalizes on its own, which is what made the JS check sound.
        let local_path = lexically_normalize(&target_dir.join(rel_path.as_ref()));
        let mut next = local_path.parent().map(|p| p.to_path_buf());

        while let Some(dir) = next {
            // Path traversal guard: a manifest entry with `..` in it can name a
            // directory outside the deployment, and this is the one place that
            // would otherwise create it.
            if dir == target_dir || !dir.starts_with(&target_dir) {
                break;
            }

            // Already known, so its parents are known too.
            if !needed.insert(dir.clone()) {
                break;
            }

            next = dir.parent().map(|p| p.to_path_buf());
        }
    }

    // Shortest paths first, so a parent is created before its children.
    let mut ordered: Vec<&PathBuf> = needed.iter().collect();
    ordered.sort_by_key(|p| p.as_os_str().len());

    for dir in ordered {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("deploy-core-filelist-{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_files(root: &Path, files: &[&str]) {
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "contents").unwrap();
        }
    }

    /// The `samplefiles` fixture from the file-manifest test suite.
    fn sample_dir(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        write_files(
            &dir,
            &[
                "file-1",
                "file-2",
                "dir-1/file-3",
                "dir-2/file-4",
                "dir-2/file-5",
                "dir-2/subdir-1/file-6",
            ],
        );
        dir
    }

    /// The `samplefiles-glob` fixture from the file-manifest test suite.
    fn glob_sample_dir(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        write_files(
            &dir,
            &[
                "package.json",
                "src/index.ts",
                "src/index.test.ts",
                "src/utils.ts",
                "src/lib/helper.ts",
                "src/lib/helper.test.ts",
            ],
        );
        dir
    }

    fn resolved(dir: &Path, config: &str) -> Vec<String> {
        resolve_file_list_from_config(dir, config)
            .unwrap()
            .rel_paths()
    }

    // --- parse_rules_file ---

    #[test]
    fn parses_each_rule_type() {
        let rules = parse_rules_file("include src/").unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[0].pattern, "src/");

        let rules = parse_rules_file("exclude node_modules").unwrap();
        assert_eq!(rules[0].rule_type, RuleType::Exclude);
        assert_eq!(rules[0].pattern, "node_modules");

        let rules = parse_rules_file("ignore-destination .cache").unwrap();
        assert_eq!(rules[0].rule_type, RuleType::IgnoreDestination);
        assert_eq!(rules[0].pattern, ".cache");

        let rules = parse_rules_file("ignore .DS_Store").unwrap();
        assert_eq!(rules[0].rule_type, RuleType::Ignore);
        assert_eq!(rules[0].pattern, ".DS_Store");
    }

    #[test]
    fn parses_multiple_rules() {
        let rules = parse_rules_file(
            "
        include src/
        include assets/
        exclude node_modules
        ignore .DS_Store
    ",
        )
        .unwrap();

        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].rule_type, RuleType::Include);
        assert_eq!(rules[0].pattern, "src/");
        assert_eq!(rules[1].pattern, "assets/");
        assert_eq!(rules[2].rule_type, RuleType::Exclude);
        assert_eq!(rules[2].pattern, "node_modules");
        assert_eq!(rules[3].rule_type, RuleType::Ignore);
        assert_eq!(rules[3].pattern, ".DS_Store");
    }

    #[test]
    fn ignores_unrecognized_commands() {
        let rules = parse_rules_file(
            "
        include src/
        unknown-command foo
        exclude bar
    ",
        )
        .unwrap();

        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "src/");
        assert_eq!(rules[1].pattern, "bar");
    }

    #[test]
    fn empty_input_yields_no_rules() {
        assert_eq!(parse_rules_file("").unwrap().len(), 0);
        assert_eq!(parse_rules_file("\n\n    ").unwrap().len(), 0);
    }

    #[test]
    fn glob_patterns_survive_the_tokenizer() {
        // The reason patterns are read as joined tag attrs and not as a value.
        let rules = parse_rules_file("include web/**/*.ts").unwrap();
        assert_eq!(rules[0].pattern, "web/**/*.ts");
    }

    // --- resolve_file_list ---

    #[test]
    fn include_dir_and_file() {
        let dir = sample_dir("include-dir-and-file");
        assert_eq!(
            resolved(&dir, "include dir-1\ninclude file-1\n"),
            vec!["dir-1/file-3", "file-1"]
        );
    }

    #[test]
    fn include_dir_recursively() {
        let dir = sample_dir("include-dir-2");
        assert_eq!(
            resolved(&dir, "include dir-2\n"),
            vec!["dir-2/file-4", "dir-2/file-5", "dir-2/subdir-1/file-6"]
        );
    }

    #[test]
    fn exclude_a_nested_file() {
        let dir = sample_dir("exclude-nested-file");
        assert_eq!(
            resolved(&dir, "include dir-2\nexclude dir-2/file-5\n"),
            vec!["dir-2/file-4", "dir-2/subdir-1/file-6"]
        );
    }

    #[test]
    fn exclude_a_nested_directory() {
        let dir = sample_dir("exclude-nested-dir");
        assert_eq!(
            resolved(&dir, "include dir-2\nexclude dir-2/subdir-1\n"),
            vec!["dir-2/file-4", "dir-2/file-5"]
        );
    }

    #[test]
    fn include_a_subdirectory() {
        let dir = sample_dir("include-subdir");
        assert_eq!(
            resolved(&dir, "include dir-2/subdir-1\n"),
            vec!["dir-2/subdir-1/file-6"]
        );
    }

    #[test]
    fn include_a_file_inside_a_subdirectory() {
        let dir = sample_dir("include-nested-file");
        assert_eq!(
            resolved(&dir, "include dir-2/file-4\n"),
            vec!["dir-2/file-4"]
        );
    }

    #[test]
    fn top_level_files_need_an_explicit_include() {
        // The root default is "not included": nothing ships without a rule.
        let dir = sample_dir("no-rules");
        assert!(resolved(&dir, "").is_empty());
    }

    #[test]
    fn glob_wildcard_matching_directories() {
        let dir = sample_dir("glob-dirs");
        assert_eq!(
            resolved(&dir, "include dir-*\n"),
            vec![
                "dir-1/file-3",
                "dir-2/file-4",
                "dir-2/file-5",
                "dir-2/subdir-1/file-6"
            ]
        );
    }

    #[test]
    fn glob_wildcard_matching_files() {
        let dir = sample_dir("glob-files");
        assert_eq!(resolved(&dir, "include file-*\n"), vec!["file-1", "file-2"]);
    }

    #[test]
    fn glob_globstar_matches_deep_paths() {
        let dir = glob_sample_dir("glob-deep");
        assert_eq!(
            resolved(&dir, "include src/**/*.ts\n"),
            vec![
                "src/index.test.ts",
                "src/index.ts",
                "src/lib/helper.test.ts",
                "src/lib/helper.ts",
                "src/utils.ts",
            ]
        );
    }

    #[test]
    fn glob_exclude_with_globstar() {
        let dir = glob_sample_dir("glob-exclude");
        assert_eq!(
            resolved(&dir, "include src/**/*.ts\nexclude **/*.test.ts\n"),
            vec!["src/index.ts", "src/lib/helper.ts", "src/utils.ts"]
        );
    }

    #[test]
    fn glob_extension_wildcard_at_top_level() {
        let dir = glob_sample_dir("glob-toplevel");
        assert_eq!(resolved(&dir, "include *.json\n"), vec!["package.json"]);
    }

    #[test]
    fn glob_mixed_with_exact_patterns() {
        let dir = glob_sample_dir("glob-mixed");
        assert_eq!(
            resolved(&dir, "include src\ninclude *.json\nexclude src/lib\n"),
            vec![
                "package.json",
                "src/index.test.ts",
                "src/index.ts",
                "src/utils.ts"
            ]
        );
    }

    #[test]
    fn ignore_rules_apply_to_the_source_list_too() {
        let dir = sample_dir("source-ignore");
        assert_eq!(
            resolved(&dir, "include dir-2\nignore dir-2/file-5\n"),
            vec!["dir-2/file-4", "dir-2/subdir-1/file-6"]
        );
    }

    #[test]
    fn source_dir_must_be_a_directory() {
        let dir = sample_dir("not-a-dir");
        assert!(resolve_file_list_from_config(&dir.join("file-1"), "include file-1").is_err());
    }

    // --- find_leftover_files ---

    #[test]
    fn finds_files_not_in_the_incoming_manifest() {
        let dir = sample_dir("leftovers");
        let incoming = FileList::from_rel_paths(["file-1"]);

        let leftovers = find_leftover_files(&dir, &incoming, &[]);
        assert_eq!(
            leftovers.rel_paths(),
            vec![
                "dir-1/file-3",
                "dir-2/file-4",
                "dir-2/file-5",
                "dir-2/subdir-1/file-6",
                "file-2",
            ]
        );
    }

    #[test]
    fn finds_nothing_when_the_manifest_covers_everything() {
        let dir = sample_dir("leftovers-none");
        let incoming = FileList::from_rel_paths([
            "file-1",
            "file-2",
            "dir-1/file-3",
            "dir-2/file-4",
            "dir-2/file-5",
            "dir-2/subdir-1/file-6",
        ]);

        assert!(find_leftover_files(&dir, &incoming, &[]).is_empty());
    }

    #[test]
    fn respects_ignore_destination_rules() {
        let dir = sample_dir("leftovers-ignore-dest");
        let incoming = FileList::from_rel_paths(["file-1"]);
        let rules = parse_rules_file("ignore-destination file-2").unwrap();

        assert_eq!(
            find_leftover_files(&dir, &incoming, &rules).rel_paths(),
            vec![
                "dir-1/file-3",
                "dir-2/file-4",
                "dir-2/file-5",
                "dir-2/subdir-1/file-6",
            ]
        );
    }

    #[test]
    fn respects_ignore_rules() {
        let dir = sample_dir("leftovers-ignore");
        let incoming = FileList::from_rel_paths(["file-1"]);
        let rules = parse_rules_file("ignore file-2").unwrap();

        assert_eq!(
            find_leftover_files(&dir, &incoming, &rules).rel_paths(),
            vec![
                "dir-1/file-3",
                "dir-2/file-4",
                "dir-2/file-5",
                "dir-2/subdir-1/file-6",
            ]
        );
    }

    #[test]
    fn an_ignored_directory_is_never_descended_into() {
        // The 2026-05-23 incident: `ignore backend/data` keeps a live server-side
        // SQLite database out of the delete list.
        let dir = temp_dir("leftovers-ignored-dir");
        write_files(&dir, &["backend/data/app.db", "backend/index.js"]);
        let incoming = FileList::from_rel_paths(["backend/index.js"]);
        let rules = parse_rules_file("ignore backend/data").unwrap();

        assert!(find_leftover_files(&dir, &incoming, &rules).is_empty());

        // Without the rule, that database is reported for deletion.
        let leftovers = find_leftover_files(&dir, &incoming, &[]);
        assert_eq!(leftovers.rel_paths(), vec!["backend/data/app.db"]);
    }

    #[test]
    fn missing_destination_directory_yields_no_leftovers() {
        let dir = temp_dir("leftovers-missing");
        let incoming = FileList::new();
        assert!(find_leftover_files(&dir.join("nope"), &incoming, &[]).is_empty());
    }

    // --- setup_empty_directories ---

    #[test]
    fn creates_the_directory_skeleton() {
        let dir = temp_dir("skeleton");
        setup_empty_directories(&dir, ["a/b/c/file.txt", "a/d/other.txt", "top.txt"]).unwrap();

        assert!(dir.join("a/b/c").is_dir());
        assert!(dir.join("a/d").is_dir());
        assert!(!dir.join("top.txt").exists());
    }

    #[test]
    fn does_not_create_directories_outside_the_target() {
        let dir = temp_dir("skeleton-traversal");
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();

        setup_empty_directories(&target, ["../escaped/file.txt"]).unwrap();

        assert!(!dir.join("escaped").exists());
    }
}
