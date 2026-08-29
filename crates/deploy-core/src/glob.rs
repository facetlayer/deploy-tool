//! Glob matching compatible with picomatch's defaults, which is what
//! @facetlayer/file-manifest uses to apply include/exclude/ignore rules.

use globset::{Glob, GlobBuilder, GlobMatcher};

pub struct Pattern {
    matcher: GlobMatcher,
    /// Pattern split on '/', used for the dotfile guard below.
    segments: Vec<String>,
    has_globstar: bool,
}

impl Pattern {
    pub fn new(pattern: &str) -> Option<Pattern> {
        let glob: Glob = GlobBuilder::new(pattern)
            // picomatch's default: a single `*` never crosses a path separator.
            .literal_separator(true)
            .build()
            .ok()?;

        let segments: Vec<String> = pattern.split('/').map(|s| s.to_string()).collect();
        let has_globstar = segments.iter().any(|s| s.contains("**"));

        Some(Pattern {
            matcher: glob.compile_matcher(),
            segments,
            has_globstar,
        })
    }

    pub fn is_match(&self, rel_path: &str) -> bool {
        if !self.matcher.is_match(rel_path) {
            return false;
        }

        // picomatch defaults to `dot: false`: a wildcard does not match a
        // leading dot in a path segment. Enforce that here, since globset has
        // no equivalent option.
        if self.has_globstar {
            return true;
        }

        let path_segments: Vec<&str> = rel_path.split('/').collect();
        if path_segments.len() != self.segments.len() {
            return true;
        }

        for (path_seg, pattern_seg) in path_segments.iter().zip(self.segments.iter()) {
            if path_seg.starts_with('.') && !pattern_seg.starts_with('.') {
                return false;
            }
        }

        true
    }
}
