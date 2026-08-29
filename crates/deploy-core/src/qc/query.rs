//! Port of the Query / Tag / TagList model from @facetlayer/qc.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The JS `undefined`/`null` case: `hasValue()` is false.
    None,
    Bool(bool),
    Str(String),
    Int(i64),
    TagList(TagList),
    Star,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Tag {
    pub attr: String,
    pub value: Value,
    pub param_name: Option<String>,
    /// Set for `--flag` syntax, which renders back with its leading dashes.
    pub is_flag: bool,
    pub is_attr_optional: bool,
    pub is_value_optional: bool,
}

impl Default for Value {
    fn default() -> Self {
        Value::None
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TagList {
    pub tags: Vec<Tag>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Query {
    pub command: String,
    pub tags: Vec<Tag>,
}

/// Mirrors JS `needsQuoting`: /[\s"']/
fn needs_quoting(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'')
}

impl Tag {
    pub fn new(attr: &str) -> Tag {
        Tag {
            attr: attr.to_string(),
            ..Default::default()
        }
    }

    pub fn has_value(&self) -> bool {
        !matches!(self.value, Value::None)
    }

    pub fn is_parameter(&self) -> bool {
        self.param_name.is_some()
    }

    /// Equivalent of `getStringValue()`, returning None where JS would throw.
    pub fn get_string_value(&self) -> Option<String> {
        match &self.value {
            Value::Str(s) => Some(s.clone()),
            Value::Int(n) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn to_query_string(&self) -> String {
        if self.attr == "*" {
            return "*".to_string();
        }

        let mut out = String::new();
        let param_matches_attr = self.param_name.as_deref() == Some(self.attr.as_str());

        if self.is_parameter() && param_matches_attr {
            out.push('$');
        }
        out.push_str(&self.attr);
        if self.is_attr_optional {
            out.push('?');
        }

        if self.is_parameter() && !param_matches_attr {
            out.push_str("=$");
            out.push_str(self.param_name.as_deref().unwrap_or(""));
        } else if self.is_flag {
            out = format!("--{}", out);
        } else if self.has_value() {
            match &self.value {
                Value::TagList(list) => {
                    out.push('(');
                    out.push_str(&list.to_query_string());
                    out.push(')');
                }
                Value::Star => out.push_str("=*"),
                Value::Str(s) => {
                    out.push('=');
                    if needs_quoting(s) {
                        out.push('"');
                        out.push_str(s);
                        out.push('"');
                    } else {
                        out.push_str(s);
                    }
                }
                Value::Int(n) => {
                    out.push('=');
                    out.push_str(&n.to_string());
                }
                Value::Bool(b) => {
                    out.push('=');
                    out.push_str(if *b { "true" } else { "false" });
                }
                Value::None => {}
            }
        }

        out
    }

    /// A tag written as `name(...)` renders back as just its inner contents.
    /// This is how `after-deploy shell(pnpm build)` recovers "pnpm build".
    pub fn to_original_string(&self) -> String {
        match &self.value {
            Value::TagList(list) => list.to_query_string(),
            _ => self.to_query_string(),
        }
    }
}

impl TagList {
    pub fn to_query_string(&self) -> String {
        self.tags
            .iter()
            .map(|t| t.to_query_string())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl Query {
    pub fn get_attr(&self, attr: &str) -> Option<&Tag> {
        // JS builds a Map keyed by attr, so a later duplicate wins.
        self.tags.iter().rev().find(|t| t.attr == attr)
    }

    pub fn has_attr(&self, attr: &str) -> bool {
        self.get_attr(attr).is_some()
    }

    /// Equivalent of `getStringValue()` but returns None instead of throwing.
    pub fn get_string_value(&self, attr: &str) -> Option<String> {
        self.get_attr(attr).and_then(|t| t.get_string_value())
    }

    /// The joined attr text of every tag, which is how file-manifest reads
    /// `include`/`exclude` patterns.
    pub fn joined_tag_attrs(&self) -> String {
        self.tags
            .iter()
            .map(|t| t.attr.as_str())
            .collect::<Vec<_>>()
            .join("")
    }
}
