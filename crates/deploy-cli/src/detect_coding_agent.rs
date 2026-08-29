//! Port of src/shared/detectCodingAgent.ts.
//!
//! Detects whether this process is running inside an AI coding agent (Claude
//! Code, OpenAI Codex) by inspecting environment variables. The result rides
//! along on `executeSql` as `callerIsAgent`, and the server uses it to enforce
//! a database's `agent-sql-access-blocked` flag.
//!
//! This is a safety net against accidental access, not a security boundary — a
//! determined caller could unset these variables.

/// Environment markers set by coding agents. Each is matched on the presence
/// of a non-empty value.
const AGENT_ENV_MARKERS: &[(&str, &str)] = &[
    ("CLAUDECODE", "Claude Code"),
    ("CLAUDE_CODE_ENTRYPOINT", "Claude Code"),
    ("CODEX_SANDBOX", "Codex"),
    ("CODEX_SANDBOX_NETWORK_DISABLED", "Codex"),
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodingAgentDetection {
    pub is_agent: bool,
    /// Name of the detected agent, when known.
    pub agent_name: Option<&'static str>,
}

pub fn detect_coding_agent() -> CodingAgentDetection {
    detect_coding_agent_with(&|name| std::env::var(name).ok())
}

/// The body of [`detect_coding_agent`], with the environment injected so tests
/// do not have to mutate process-wide state.
pub fn detect_coding_agent_with(env: &dyn Fn(&str) -> Option<String>) -> CodingAgentDetection {
    for (variable, name) in AGENT_ENV_MARKERS {
        match env(variable) {
            Some(value) if !value.is_empty() => {
                return CodingAgentDetection {
                    is_agent: true,
                    agent_name: Some(name),
                }
            }
            _ => {}
        }
    }

    CodingAgentDetection {
        is_agent: false,
        agent_name: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn from_map(pairs: &[(&str, &str)]) -> CodingAgentDetection {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        detect_coding_agent_with(&|name| map.get(name).cloned())
    }

    #[test]
    fn reports_no_agent_for_a_plain_environment() {
        assert_eq!(
            from_map(&[("PATH", "/usr/bin")]),
            CodingAgentDetection {
                is_agent: false,
                agent_name: None
            }
        );
    }

    #[test]
    fn detects_claude_code_from_claudecode() {
        assert_eq!(
            from_map(&[("CLAUDECODE", "1")]),
            CodingAgentDetection {
                is_agent: true,
                agent_name: Some("Claude Code")
            }
        );
    }

    #[test]
    fn detects_claude_code_from_the_entrypoint_variable() {
        assert_eq!(
            from_map(&[("CLAUDE_CODE_ENTRYPOINT", "cli")]),
            CodingAgentDetection {
                is_agent: true,
                agent_name: Some("Claude Code")
            }
        );
    }

    #[test]
    fn detects_codex_from_codex_sandbox() {
        assert_eq!(
            from_map(&[("CODEX_SANDBOX", "seatbelt")]),
            CodingAgentDetection {
                is_agent: true,
                agent_name: Some("Codex")
            }
        );
    }

    #[test]
    fn detects_codex_from_the_network_disabled_variable() {
        assert_eq!(
            from_map(&[("CODEX_SANDBOX_NETWORK_DISABLED", "1")]),
            CodingAgentDetection {
                is_agent: true,
                agent_name: Some("Codex")
            }
        );
    }

    #[test]
    fn ignores_empty_marker_values() {
        assert_eq!(
            from_map(&[("CLAUDECODE", "")]),
            CodingAgentDetection {
                is_agent: false,
                agent_name: None
            }
        );
    }
}
