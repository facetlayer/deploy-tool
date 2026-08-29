//! Port of the parser from @facetlayer/qc (parseFile / parseQuery / parseQueryTag).

use super::lexer::{LexedText, LexerSettings, Tok, Token};
use super::query::{Query, Tag, TagList, Value};

struct TokenIterator<'a> {
    lexed: &'a LexedText,
    position: usize,
}

/// Stand-in for the synthetic past-the-end token the JS iterator returns.
struct Peeked {
    line_start: usize,
    leading_indent: usize,
}

impl<'a> TokenIterator<'a> {
    fn new(lexed: &'a LexedText) -> Self {
        TokenIterator { lexed, position: 0 }
    }

    fn token(&self) -> Option<&Token> {
        self.lexed.tokens.get(self.position)
    }

    fn peek(&self) -> Peeked {
        match self.token() {
            Some(t) => Peeked {
                line_start: t.line_start,
                leading_indent: t.leading_indent,
            },
            None => {
                let last = self.lexed.tokens.last();
                Peeked {
                    line_start: last.map(|t| t.line_start).unwrap_or(0),
                    leading_indent: last.map(|t| t.leading_indent).unwrap_or(0),
                }
            }
        }
    }

    fn finished(&self) -> bool {
        self.position >= self.lexed.tokens.len()
    }

    fn next_is(&self, tok: Tok) -> bool {
        self.token().map(|t| t.tok) == Some(tok)
    }

    fn next_text(&self) -> String {
        match self.token() {
            Some(t) => self.lexed.token_text(t),
            None => String::new(),
        }
    }

    fn advance(&mut self) {
        self.position += 1;
    }

    fn try_consume(&mut self, tok: Tok) -> bool {
        if self.next_is(tok) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume_as_text(&mut self) -> String {
        let text = self.next_text();
        self.advance();
        text
    }

    fn consume_as_unquoted_text(&mut self) -> String {
        let text = match self.token() {
            Some(t) => self.lexed.token_unquoted_text(t),
            None => String::new(),
        };
        self.advance();
        text
    }

    fn skip_spaces(&mut self) {
        while self.next_is(Tok::Space) {
            self.advance();
        }
    }

    fn skip_newlines(&mut self) {
        while self.next_is(Tok::Space) || self.next_is(Tok::Newline) {
            self.advance();
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ParseContext {
    inside_paren: bool,
}

struct TagContext {
    starting_line: usize,
    starting_indent: usize,
    inside_paren: bool,
}

fn parse_tag_list_from_tokens(it: &mut TokenIterator) -> TagList {
    let query = parse_query_from_tokens(it, ParseContext { inside_paren: true });

    match query {
        None => TagList::default(),
        Some(query) => {
            // A Query is flattened back into a TagList by re-prepending its command.
            let mut tags = vec![Tag::new(&query.command)];
            tags.extend(query.tags);
            TagList { tags }
        }
    }
}

fn parse_query_tag_from_tokens(it: &mut TokenIterator) -> Tag {
    let mut result = Tag::default();

    it.try_consume(Tok::Space);

    let is_self_named_parameter = it.try_consume(Tok::Dollar);
    let skip_attribute = it.next_is(Tok::LParen);

    if !skip_attribute {
        if it.try_consume(Tok::DoubleDash) {
            // `--flag` syntax
            result.is_flag = true;
            result.value = Value::Bool(true);
        }
        result.attr = it.consume_as_unquoted_text();
        while it.next_is(Tok::PlainValue)
            || it.next_is(Tok::Dot)
            || it.next_is(Tok::Dash)
            || it.next_is(Tok::DoubleDash)
            || it.next_is(Tok::Integer)
            || it.next_is(Tok::Slash)
        {
            result.attr.push_str(&it.consume_as_unquoted_text());
        }
    }

    if is_self_named_parameter {
        result.param_name = Some(result.attr.clone());
    }

    if it.try_consume(Tok::Question) {
        result.is_attr_optional = true;
    }

    let before_paren_lookahead = it.position;
    it.try_consume(Tok::Space);

    if it.try_consume(Tok::LParen) {
        let tag_list = parse_tag_list_from_tokens(it);
        it.try_consume(Tok::RParen);
        result.value = Value::TagList(tag_list);
        return result;
    }

    it.position = before_paren_lookahead;

    if it.try_consume(Tok::Equals) || it.try_consume(Tok::Colon) {
        it.skip_spaces();

        if it.try_consume(Tok::Dollar) {
            result.param_name = Some(it.consume_as_unquoted_text());
        } else if it.try_consume(Tok::Question) {
            result.is_value_optional = true;
        } else if it.try_consume(Tok::Star) {
            result.value = Value::Star;
        } else if it.try_consume(Tok::LParen) {
            let tag_list = parse_tag_list_from_tokens(it);
            it.try_consume(Tok::RParen);
            result.value = Value::TagList(tag_list);
        } else {
            let first_token_type = it.token().map(|t| t.tok);
            let mut token_count = 0;

            if it.finished() || it.next_is(Tok::RParen) {
                // JS throws "Expected a value" here; an empty value is close
                // enough for config parsing and keeps the server from 500ing.
                return result;
            }

            let mut str_value = it.consume_as_unquoted_text();
            token_count += 1;

            while it.next_is(Tok::PlainValue)
                || it.next_is(Tok::Dot)
                || it.next_is(Tok::Slash)
                || it.next_is(Tok::Colon)
                || it.next_is(Tok::Integer)
            {
                str_value.push_str(&it.consume_as_unquoted_text());
                token_count += 1;
            }

            if token_count == 1 && first_token_type == Some(Tok::Integer) {
                result.value = match str_value.parse::<i64>() {
                    Ok(n) => Value::Int(n),
                    Err(_) => Value::Str(str_value),
                };
            } else {
                result.value = Value::Str(str_value);
            }
        }
    }

    result
}

fn parse_tags(it: &mut TokenIterator, ctx: &TagContext) -> Vec<Tag> {
    let mut tags = Vec::new();

    loop {
        if it.try_consume(Tok::Space)
            || it.try_consume(Tok::Newline)
            || it.try_consume(Tok::LineComment)
        {
            continue;
        }

        if it.finished()
            || it.next_is(Tok::Bar)
            || it.next_is(Tok::Slash)
            || it.next_is(Tok::RParen)
            || it.next_is(Tok::Semicolon)
        {
            break;
        }

        // Significant indentation: a query ends at the next line that isn't
        // indented further than the line the query started on.
        let peeked = it.peek();
        let is_on_following_line = peeked.line_start != ctx.starting_line;
        let indent_is_same_or_lower = peeked.leading_indent <= ctx.starting_indent;

        if !ctx.inside_paren && !tags.is_empty() && is_on_following_line && indent_is_same_or_lower
        {
            break;
        }

        let before = it.position;
        let tag = parse_query_tag_from_tokens(it);
        if it.position == before {
            // Defensive: never spin on a token the tag parser can't consume.
            it.advance();
            continue;
        }
        it.try_consume(Tok::Comma);
        tags.push(tag);
    }

    tags
}

/// `limit 10` / `last 5` special syntax.
fn maybe_parse_verb_with_count(it: &mut TokenIterator) -> Option<Vec<Tag>> {
    let start_pos = it.position;
    let peeked = it.peek();
    let text = it.next_text();
    if text != "limit" && text != "last" {
        return None;
    }
    let verb = text;
    it.advance();
    it.skip_newlines();
    if !it.next_is(Tok::Integer) {
        it.position = start_pos;
        return None;
    }
    let count = it.consume_as_text();

    let mut tags = vec![Tag::new(&verb), {
        let mut t = Tag::new("count");
        t.value = Value::Str(count);
        t
    }];
    tags.extend(parse_tags(
        it,
        &TagContext {
            starting_line: peeked.line_start,
            starting_indent: peeked.leading_indent,
            inside_paren: false,
        },
    ));
    Some(tags)
}

/// `rename from -> to` special syntax.
fn maybe_parse_rename(it: &mut TokenIterator) -> Option<Vec<Tag>> {
    let start_pos = it.position;
    let peeked = it.peek();
    if it.next_text() != "rename" {
        return None;
    }
    it.advance();
    it.skip_newlines();
    if !it.next_is(Tok::PlainValue) {
        it.position = start_pos;
        return None;
    }
    let from = it.consume_as_text();
    it.skip_newlines();
    if !it.next_is(Tok::RightArrow) {
        it.position = start_pos;
        return None;
    }
    it.advance();
    it.skip_newlines();
    if !it.next_is(Tok::PlainValue) {
        it.position = start_pos;
        return None;
    }
    let to = it.consume_as_text();

    let mut tags = vec![
        Tag::new("rename"),
        {
            let mut t = Tag::new("from");
            t.value = Value::Str(from);
            t
        },
        {
            let mut t = Tag::new("to");
            t.value = Value::Str(to);
            t
        },
    ];
    tags.extend(parse_tags(
        it,
        &TagContext {
            starting_line: peeked.line_start,
            starting_indent: peeked.leading_indent,
            inside_paren: false,
        },
    ));
    Some(tags)
}

/// `wait 100` special syntax.
fn maybe_parse_wait_verb(it: &mut TokenIterator) -> Option<Vec<Tag>> {
    let start_pos = it.position;
    let peeked = it.peek();
    if it.next_text() != "wait" {
        return None;
    }
    it.advance();
    it.skip_newlines();
    if !it.next_is(Tok::Integer) {
        it.position = start_pos;
        return None;
    }
    it.advance();

    let mut tags = vec![Tag::new("wait"), Tag::new("duration")];
    tags.extend(parse_tags(
        it,
        &TagContext {
            starting_line: peeked.line_start,
            starting_indent: peeked.leading_indent,
            inside_paren: false,
        },
    ));
    Some(tags)
}

fn parse_single_query_from_tokens(it: &mut TokenIterator, ctx: ParseContext) -> Option<Query> {
    it.skip_newlines();
    it.skip_spaces();

    let peeked = it.peek();
    let tag_ctx = TagContext {
        starting_line: peeked.line_start,
        starting_indent: peeked.leading_indent,
        inside_paren: ctx.inside_paren,
    };

    for handler in [
        maybe_parse_verb_with_count,
        maybe_parse_rename,
        maybe_parse_wait_verb,
    ] {
        if let Some(tags) = handler(it) {
            let command = tags[0].attr.clone();
            return Some(Query {
                command,
                tags: tags[1..].to_vec(),
            });
        }
    }

    let tags = parse_tags(it, &tag_ctx);
    if tags.is_empty() {
        return None;
    }

    let command = tags[0].attr.clone();
    Some(Query {
        command,
        tags: tags[1..].to_vec(),
    })
}

fn parse_query_from_tokens(it: &mut TokenIterator, ctx: ParseContext) -> Option<Query> {
    let mut steps: Vec<Query> = Vec::new();

    while !it.finished() {
        if it.try_consume(Tok::Space) || it.try_consume(Tok::LineComment) {
            continue;
        }
        if it.next_is(Tok::Bar) || it.next_is(Tok::Slash) {
            it.advance();
            continue;
        }

        let before = it.position;
        let step = parse_single_query_from_tokens(it, ctx);
        match step {
            None => {
                if it.position == before {
                    // JS loops forever here; stop instead.
                    break;
                }
                continue;
            }
            Some(step) => steps.push(step),
        }

        if !it.try_consume(Tok::Bar) && !it.try_consume(Tok::Slash) {
            break;
        }
    }

    // Multi-step queries aren't used by .deploy configs; keep the first step.
    steps.into_iter().next()
}

/// Port of qc's `parseFile`, which is how every `.deploy` config is read.
pub fn parse_file(contents: &str) -> Vec<Query> {
    let lexed = LexedText::new(
        contents,
        LexerSettings {
            bash_style_line_comments: true,
            ..Default::default()
        },
    );

    let mut it = TokenIterator::new(&lexed);
    let mut queries = Vec::new();

    while !it.finished() {
        if it.try_consume(Tok::Semicolon) {
            continue;
        }
        if it.try_consume(Tok::LineComment) {
            continue;
        }

        let before = it.position;
        match parse_query_from_tokens(&mut it, ParseContext::default()) {
            None => {
                if it.position == before {
                    break;
                }
            }
            Some(query) => {
                // parseFile drops queries that carry no arguments.
                if !query.tags.is_empty() {
                    queries.push(query);
                }
            }
        }
    }

    queries
}
