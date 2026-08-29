//! JSON dump of parsed configs, used to diff this parser against the
//! TypeScript @facetlayer/qc implementation. See `deploy-server
//! debug-parse-config` and scripts/dumpParsedConfig.ts.

use serde_json::{json, Value as Json};

use super::query::{Query, Tag, Value};

fn tag_to_json(tag: &Tag) -> Json {
    json!({
        "attr": tag.attr,
        "value": value_to_json(&tag.value),
        "paramName": tag.param_name,
        "isAttrOptional": tag.is_attr_optional,
        "isValueOptional": tag.is_value_optional,
        "queryString": tag.to_query_string(),
        "originalString": tag.to_original_string(),
    })
}

fn value_to_json(value: &Value) -> Json {
    match value {
        Value::None => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Str(s) => json!(s),
        Value::Int(n) => json!(n),
        Value::Star => json!({ "t": "star" }),
        Value::TagList(list) => json!({
            "t": "taglist",
            "tags": list.tags.iter().map(tag_to_json).collect::<Vec<_>>(),
        }),
    }
}

fn query_to_json(query: &Query) -> Json {
    json!({
        "command": query.command,
        "tags": query.tags.iter().map(tag_to_json).collect::<Vec<_>>(),
    })
}

pub fn debug_dump(config_text: &str) -> Json {
    Json::Array(
        super::parse_file(config_text)
            .iter()
            .map(query_to_json)
            .collect(),
    )
}
