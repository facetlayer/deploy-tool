//! The one permission bit a deployment carries: whether a file is executable.
//!
//! Only the execute bit travels, not the whole mode. Owner, group and the
//! read/write bits on the server are the server's business — a laptop's
//! `0600` must not make a file unreadable to the service that runs it — but
//! whether `bin/migrate` can be exec'd is part of what was deployed.
//! `deploy run-on-server` and `after-deploy` scripts depend on it.

/// Whether a local file counts as executable: any execute bit set, the way
/// git decides between `100755` and `100644`.
#[cfg(unix)]
pub fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// No execute bit to read, so the manifest says nothing and the server leaves
/// the file as it is.
#[cfg(not(unix))]
pub fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// `mode` with its execute bits set or cleared. Setting grants execute
/// exactly where read is granted (`0644` → `0755`, `0640` → `0750`), so it
/// never widens who can see the file.
pub fn with_executable(mode: u32, executable: bool) -> u32 {
    if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_execute_follows_the_read_bits() {
        assert_eq!(with_executable(0o644, true), 0o755);
        assert_eq!(with_executable(0o640, true), 0o750);
        assert_eq!(with_executable(0o600, true), 0o700);
        assert_eq!(with_executable(0o755, true), 0o755);
    }

    #[test]
    fn clearing_execute_touches_nothing_else() {
        assert_eq!(with_executable(0o755, false), 0o644);
        assert_eq!(with_executable(0o4755, false), 0o4644);
        assert_eq!(with_executable(0o644, false), 0o644);
    }
}
