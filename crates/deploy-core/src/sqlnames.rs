//! Table-name extraction for `deploy sql`. Port of
//! src/shared/parseSqlTableNames.ts (and of the old daemon's src/sql.rs, which
//! already ported it).
//!
//! A project can declare several SQLite files; the server routes a query to
//! whichever of them actually holds the referenced tables. That only needs the
//! names a statement mentions, so this is a lexical scan for identifiers after
//! FROM / JOIN / INTO / UPDATE / TABLE — not a SQL parser, and it should stay
//! that way. When it can't tell, it returns nothing and the caller falls back
//! to its own default.

/// SQL keywords that appear immediately before a table name.
const TABLE_PRECEDING_KEYWORDS: &[&str] = &["from", "join", "into", "update", "table"];

/// SQL keywords that are never table names.
const SQL_KEYWORDS: &[&str] = &[
    "select", "from", "where", "join", "inner", "outer", "left", "right", "cross", "full", "on",
    "as", "and", "or", "not", "in", "is", "null", "like", "between", "case", "when", "then",
    "else", "end", "having", "group", "by", "order", "limit", "offset", "distinct", "all",
    "exists", "set", "values", "returning", "with", "recursive", "insert", "delete", "create",
    "drop", "alter", "if", "replace", "ignore", "rollback", "abort", "fail", "union", "intersect",
    "except",
];

#[derive(Debug, PartialEq)]
enum SqlToken {
    Ident(String),
    Other,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Tokenizes SQL far enough to find identifiers, skipping whitespace,
/// `--` / `#` / `//` line comments, `/* */` block comments and quoted strings.
fn tokenize(sql: &str) -> Vec<SqlToken> {
    let chars: Vec<char> = sql.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c.is_whitespace() {
            i += 1;
            continue;
        }

        if (c == '-' && chars.get(i + 1) == Some(&'-')) || c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            continue;
        }

        // A quoted string or quoted identifier is opaque: it becomes one
        // `Other` token, so `FROM "users"` yields no table name rather than a
        // wrong one.
        if c == '\'' || c == '"' || c == '`' {
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            tokens.push(SqlToken::Other);
            continue;
        }

        if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_continue(chars[i]) {
                i += 1;
            }
            tokens.push(SqlToken::Ident(chars[start..i].iter().collect()));
            continue;
        }

        tokens.push(SqlToken::Other);
        i += 1;
    }

    tokens
}

/// The unique lowercase table names a statement references, in the order they
/// appear. Empty when none can be determined.
///
/// Handles:
///   SELECT ... FROM tableName
///   SELECT ... FROM t1 JOIN t2 LEFT JOIN t3
///   INSERT INTO tableName / INSERT OR IGNORE INTO tableName
///   UPDATE tableName SET ...
///   DELETE FROM tableName
///   CREATE TABLE [IF NOT EXISTS] tableName
///   DROP TABLE [IF EXISTS] tableName
///   ALTER TABLE tableName
pub fn parse_sql_table_names(sql: &str) -> Vec<String> {
    let tokens = tokenize(sql);
    let mut tables: Vec<String> = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        let word = match &tokens[i] {
            SqlToken::Ident(word) => word.to_lowercase(),
            SqlToken::Other => {
                i += 1;
                continue;
            }
        };
        i += 1;

        if !TABLE_PRECEDING_KEYWORDS.contains(&word.as_str()) {
            continue;
        }

        // Skip an "IF NOT EXISTS" / "IF EXISTS" prefix (CREATE/DROP TABLE).
        if let Some(SqlToken::Ident(next)) = tokens.get(i) {
            if next.eq_ignore_ascii_case("if") {
                i += 1;
                if let Some(SqlToken::Ident(next)) = tokens.get(i) {
                    if next.eq_ignore_ascii_case("not") {
                        i += 1;
                    }
                }
                if let Some(SqlToken::Ident(next)) = tokens.get(i) {
                    if next.eq_ignore_ascii_case("exists") {
                        i += 1;
                    }
                }
            }
        }

        if let Some(SqlToken::Ident(name)) = tokens.get(i) {
            let lowered = name.to_lowercase();
            if !SQL_KEYWORDS.contains(&lowered.as_str()) && !tables.contains(&lowered) {
                tables.push(lowered);
            }
            i += 1;
        }
    }

    tables
}

/// True when a statement produces a result set, so the caller knows whether to
/// report rows or a rows-affected count.
pub fn is_query_statement(sql: &str) -> bool {
    match tokenize(sql).first() {
        Some(SqlToken::Ident(word)) => matches!(
            word.to_lowercase().as_str(),
            "select" | "with" | "explain" | "values"
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str) -> Vec<String> {
        parse_sql_table_names(sql)
    }

    #[test]
    fn simple_select() {
        assert_eq!(names("SELECT * FROM users"), vec!["users"]);
    }

    #[test]
    fn lowercases_names() {
        assert_eq!(names("select * from Users"), vec!["users"]);
    }

    #[test]
    fn joins() {
        assert_eq!(
            names("SELECT * FROM t1 JOIN t2 ON t1.id = t2.id LEFT JOIN t3 ON t3.id = t1.id"),
            vec!["t1", "t2", "t3"]
        );
    }

    #[test]
    fn insert_into() {
        assert_eq!(names("INSERT INTO users (name) VALUES ('a')"), vec!["users"]);
        assert_eq!(
            names("INSERT OR IGNORE INTO sessions (id) VALUES (1)"),
            vec!["sessions"]
        );
    }

    #[test]
    fn update_and_delete() {
        assert_eq!(names("UPDATE users SET name = 'a' WHERE id = 1"), vec!["users"]);
        assert_eq!(names("DELETE FROM sessions WHERE id = 1"), vec!["sessions"]);
    }

    #[test]
    fn create_and_drop_table() {
        assert_eq!(names("CREATE TABLE users (id INTEGER)"), vec!["users"]);
        assert_eq!(
            names("CREATE TABLE IF NOT EXISTS users (id INTEGER)"),
            vec!["users"]
        );
        assert_eq!(names("DROP TABLE users"), vec!["users"]);
        assert_eq!(names("DROP TABLE IF EXISTS users"), vec!["users"]);
    }

    #[test]
    fn alter_table() {
        assert_eq!(names("ALTER TABLE users ADD COLUMN age INTEGER"), vec!["users"]);
    }

    #[test]
    fn deduplicates_in_first_seen_order() {
        assert_eq!(
            names("SELECT * FROM users JOIN orders ON 1 JOIN users ON 1"),
            vec!["users", "orders"]
        );
    }

    #[test]
    fn skips_keywords_that_follow_a_preceding_keyword() {
        // "FROM (SELECT ...)": the next identifier is a keyword, not a table.
        assert_eq!(names("SELECT * FROM (SELECT 1)"), Vec::<String>::new());
    }

    #[test]
    fn ignores_comments() {
        assert_eq!(
            names("-- from commented_out\nSELECT * FROM users"),
            vec!["users"]
        );
        assert_eq!(
            names("/* from commented_out */ SELECT * FROM users"),
            vec!["users"]
        );
        assert_eq!(names("# from commented_out\nSELECT * FROM users"), vec!["users"]);
    }

    #[test]
    fn ignores_string_literals() {
        assert_eq!(
            names("SELECT * FROM users WHERE name = 'from other_table'"),
            vec!["users"]
        );
    }

    #[test]
    fn no_tables_when_none_can_be_determined() {
        assert_eq!(names("SELECT 1"), Vec::<String>::new());
        assert_eq!(names(""), Vec::<String>::new());
    }

    #[test]
    fn detects_result_set_statements() {
        assert!(is_query_statement("SELECT 1"));
        assert!(is_query_statement("  with x as (select 1) select * from x"));
        assert!(is_query_statement("EXPLAIN SELECT 1"));
        assert!(is_query_statement("VALUES (1)"));
        assert!(!is_query_statement("INSERT INTO users VALUES (1)"));
        assert!(!is_query_statement("UPDATE users SET a = 1"));
        assert!(!is_query_statement(""));
    }
}
