//! Deployment tag lookup. Port of `src/server/deploymentTags.ts`.

use anyhow::{anyhow, Result};
use rusqlite::OptionalExtension;
use serde_json::Value as Json;

use deploy_core::rpc::{DeploymentTags, GetDeploymentTagsParams, GetDeploymentTagsResult};

use crate::handlers::parse_params;
use crate::state::AppState;

/// Reads the `tags_json` column.
///
/// A corrupt or non-object blob becomes an empty map rather than failing the
/// request: tags are decoration on a deployment record, and losing them must
/// not make the deployment unreadable. Non-string values are stringified,
/// because the wire type is string-to-string.
pub fn parse_tags_json(tags_json: Option<&str>) -> DeploymentTags {
    let text = match tags_json {
        Some(text) if !text.is_empty() => text,
        _ => return DeploymentTags::new(),
    };

    let parsed: Json = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(_) => return DeploymentTags::new(),
    };

    let object = match parsed {
        Json::Object(map) => map,
        _ => return DeploymentTags::new(),
    };

    object
        .into_iter()
        .filter_map(|(key, value)| match value {
            Json::String(text) => Some((key, text)),
            Json::Null => None,
            Json::Array(_) | Json::Object(_) => None,
            other => Some((key, other.to_string())),
        })
        .collect()
}

pub fn get_deployment_tags(state: &AppState, params: &Json) -> Result<Json> {
    let params: GetDeploymentTagsParams = parse_params(params)?;
    let project_name = params.project_name;

    let conn = state.db();

    let active_deploy_name: Option<String> = conn
        .query_row(
            "select deploy_name from active_deployment where project_name = ?",
            [&project_name],
            |row| row.get(0),
        )
        .optional()?;

    let requested_name = params
        .deploy_name
        .or_else(|| active_deploy_name.clone())
        .ok_or_else(|| anyhow!("Project '{}' has no active deployment", project_name))?;

    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "select deploy_name, created_at, tags_json from deployment
             where deploy_name = ? and project_name = ?",
            rusqlite::params![requested_name, project_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let (deploy_name, created_at, tags_json) = row.ok_or_else(|| {
        anyhow!(
            "Deployment '{}' not found for project '{}'",
            requested_name,
            project_name
        )
    })?;

    let is_active = Some(&deploy_name) == active_deploy_name.as_ref();

    Ok(serde_json::to_value(GetDeploymentTagsResult {
        deploy_name,
        created_at,
        is_active,
        tags: parse_tags_json(tags_json.as_deref()),
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_corrupt_tag_blobs_read_as_empty() {
        assert!(parse_tags_json(None).is_empty());
        assert!(parse_tags_json(Some("")).is_empty());
        assert!(parse_tags_json(Some("not json")).is_empty());
        assert!(parse_tags_json(Some("[1,2,3]")).is_empty());
    }

    #[test]
    fn reads_string_tags() {
        let tags = parse_tags_json(Some(r#"{"git-commit":"abc123","git-branch":"main"}"#));
        assert_eq!(tags.get("git-commit").unwrap(), "abc123");
        assert_eq!(tags.get("git-branch").unwrap(), "main");
    }
}
