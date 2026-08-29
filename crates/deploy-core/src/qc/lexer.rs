//! Port of the lexer from @facetlayer/qc.
//!
//! Kept deliberately faithful to the JS implementation (including its quirks)
//! so that `.deploy` config files parse identically on both servers.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    GThan,
    GThanEq,
    LThan,
    LThanEq,
    Slash,
    Dot,
    Comma,
    Semicolon,
    Colon,
    Plus,
    Dash,
    DoubleDash,
    RightArrow,
    RightFatArrow,
    Star,
    Equals,
    DoubleEquals,
    TripleEquals,
    BangEquals,
    BangDoubleEquals,
    Hash,
    Percent,
    Dollar,
    Tilde,
    Exclaim,
    Bar,
    DoubleBar,
    Amp,
    DoubleAmp,
    Question,
    #[allow(dead_code)]
    Ident,
    PlainValue,
    Integer,
    Space,
    Tab,
    Newline,
    QuotedString,
    LineComment,
    BlockComment,
    Unrecognized,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Index into the char vector, not a byte offset.
    pub start: usize,
    pub end: usize,
    pub line_start: usize,
    pub leading_indent: usize,
}

#[derive(Clone, Copy, Default)]
pub struct LexerSettings {
    pub bash_style_line_comments: bool,
    pub c_style_line_comments: bool,
    pub c_style_block_comments: bool,
}

/// Matches `tokenFromSingleCharCode` in the JS lexer: every token whose `str`
/// is exactly one character.
fn single_char_token(c: char) -> Option<Tok> {
    Some(match c {
        '(' => Tok::LParen,
        ')' => Tok::RParen,
        '[' => Tok::LBracket,
        ']' => Tok::RBracket,
        '{' => Tok::LBrace,
        '}' => Tok::RBrace,
        '>' => Tok::GThan,
        '<' => Tok::LThan,
        '/' => Tok::Slash,
        '.' => Tok::Dot,
        ',' => Tok::Comma,
        ';' => Tok::Semicolon,
        ':' => Tok::Colon,
        '+' => Tok::Plus,
        '-' => Tok::Dash,
        '*' => Tok::Star,
        '=' => Tok::Equals,
        '#' => Tok::Hash,
        '%' => Tok::Percent,
        '$' => Tok::Dollar,
        '~' => Tok::Tilde,
        '!' => Tok::Exclaim,
        '|' => Tok::Bar,
        '&' => Tok::Amp,
        '?' => Tok::Question,
        '\t' => Tok::Tab,
        '\n' => Tok::Newline,
        _ => return None,
    })
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

fn can_start_plain_value(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn can_continue_plain_value(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '!'
}

struct LexContext<'a> {
    chars: &'a [char],
    settings: LexerSettings,
    index: usize,
    line_number: usize,
    column_number: usize,
    leading_indent: usize,
    tokens: Vec<Token>,
}

impl<'a> LexContext<'a> {
    fn finished(&self, lookahead: usize) -> bool {
        self.index + lookahead >= self.chars.len()
    }

    /// Mirrors `next()` in the JS lexer, which returns char code 0 past the end.
    fn next(&self, lookahead: usize) -> char {
        match self.chars.get(self.index + lookahead) {
            Some(c) => *c,
            None => '\0',
        }
    }

    fn consume(&mut self, tok: Tok, len: usize) {
        if tok == Tok::Space && self.column_number == 1 {
            self.leading_indent = len;
        }

        self.tokens.push(Token {
            tok,
            start: self.index,
            end: self.index + len,
            line_start: self.line_number,
            leading_indent: self.leading_indent,
        });

        if tok == Tok::Newline {
            self.line_number += 1;
            self.column_number = 1;
            self.leading_indent = 0;
        } else {
            self.column_number += len;
        }

        self.index += len;
    }

    fn consume_while(&mut self, tok: Tok, matcher: impl Fn(char) -> bool) {
        let mut len = 0;
        while self.next(len) != '\0' && matcher(self.next(len)) {
            len += 1;
        }
        self.consume(tok, len);
    }

    fn consume_quoted_string(&mut self, looking_for: char) {
        let mut lookahead = 1;
        let mut new_line_number = self.line_number;
        let mut new_column_number = self.column_number + 1;

        while !self.finished(lookahead) {
            let c = self.next(lookahead);
            if c == '\\' {
                lookahead += 2;
                new_column_number += 2;
                continue;
            }
            if c == looking_for {
                lookahead += 1;
                new_column_number += 1;
                break;
            }
            if c == '\n' {
                new_line_number += 1;
                new_column_number = 1;
            } else {
                new_column_number += 1;
            }
            lookahead += 1;
        }

        self.consume(Tok::QuotedString, lookahead);
        self.line_number = new_line_number;
        self.column_number = new_column_number;
    }

    fn consume_multiline_comment(&mut self) {
        let mut lookahead = 1;
        let mut new_line_number = self.line_number;
        let mut new_column_number = self.column_number;

        while !self.finished(lookahead) {
            if self.next(lookahead) == '*' && self.next(lookahead + 1) == '/' {
                lookahead += 2;
                break;
            }
            if self.next(lookahead) == '\n' {
                new_line_number += 1;
                new_column_number = 1;
            } else {
                new_column_number += 1;
            }
            lookahead += 1;
        }

        self.consume(Tok::BlockComment, lookahead);
        self.line_number = new_line_number;
        self.column_number = new_column_number;
    }

    fn consume_plain_value(&mut self) {
        let mut lookahead = 0;
        let mut is_all_numbers = true;
        while can_continue_plain_value(self.next(lookahead)) {
            if !is_digit(self.next(lookahead)) {
                is_all_numbers = false;
            }
            lookahead += 1;
        }
        if is_all_numbers {
            self.consume(Tok::Integer, lookahead);
        } else {
            self.consume(Tok::PlainValue, lookahead);
        }
    }

    fn consume_next(&mut self) {
        let c = self.next(0);

        if c == '=' && self.next(1) == '=' {
            if self.next(2) == '=' {
                return self.consume(Tok::TripleEquals, 3);
            }
            return self.consume(Tok::DoubleEquals, 2);
        }
        if c == '=' && self.next(1) == '>' {
            return self.consume(Tok::RightFatArrow, 2);
        }
        if c == '!' && self.next(1) == '=' {
            if self.next(2) == '=' {
                return self.consume(Tok::BangDoubleEquals, 3);
            }
            return self.consume(Tok::BangEquals, 2);
        }
        if c == '-' && self.next(1) == '-' {
            return self.consume(Tok::DoubleDash, 2);
        }
        if c == '-' && self.next(1) == '>' {
            return self.consume(Tok::RightArrow, 2);
        }
        if c == '|' && self.next(1) == '|' {
            return self.consume(Tok::DoubleBar, 2);
        }
        if c == '&' && self.next(1) == '&' {
            return self.consume(Tok::DoubleAmp, 2);
        }
        // Note: the JS lexer has these two swapped ('>' produces gthaneq via the
        // `c_gthan` branch). Kept identical on purpose.
        if c == '>' && self.next(1) == '=' {
            return self.consume(Tok::GThanEq, 2);
        }
        if c == '<' && self.next(1) == '=' {
            return self.consume(Tok::LThanEq, 2);
        }
        if c == '/' && self.next(1) == '/' && self.settings.c_style_line_comments {
            return self.consume_while(Tok::LineComment, |c| c != '\n');
        }
        if c == '/' && self.next(1) == '*' && self.settings.c_style_block_comments {
            return self.consume_multiline_comment();
        }

        // rqePlainValues defaults to true, so plain values shadow identifiers.
        if can_start_plain_value(c) {
            return self.consume_plain_value();
        }
        if c == '#' && self.settings.bash_style_line_comments {
            return self.consume_while(Tok::LineComment, |c| c != '\n');
        }
        if c == '\'' || c == '"' || c == '`' {
            return self.consume_quoted_string(c);
        }
        if c == ' ' {
            return self.consume_while(Tok::Space, |c| c == ' ');
        }
        if let Some(tok) = single_char_token(c) {
            return self.consume(tok, 1);
        }
        self.consume(Tok::Unrecognized, 1);
    }
}

pub struct LexedText {
    pub chars: Vec<char>,
    pub tokens: Vec<Token>,
}

impl LexedText {
    pub fn new(text: &str, settings: LexerSettings) -> LexedText {
        let chars: Vec<char> = text.chars().collect();
        let mut ctx = LexContext {
            chars: &chars,
            settings,
            index: 0,
            line_number: 1,
            column_number: 1,
            leading_indent: 0,
            tokens: Vec::new(),
        };

        while !ctx.finished(0) {
            let pos = ctx.index;
            ctx.consume_next();
            if ctx.index == pos {
                // The JS lexer throws here; a stalled lexer would otherwise hang.
                break;
            }
        }

        let tokens = std::mem::take(&mut ctx.tokens);
        LexedText { chars, tokens }
    }

    pub fn token_text(&self, token: &Token) -> String {
        self.chars[token.start..token.end].iter().collect()
    }

    pub fn token_unquoted_text(&self, token: &Token) -> String {
        if token.tok == Tok::QuotedString {
            // Mirrors JS `slice(textStart + 1, textEnd - 1)`, which yields an
            // empty string when the two bounds cross (unterminated quote).
            let lo = token.start + 1;
            let hi = token.end.saturating_sub(1);
            let inner: String = if hi > lo {
                self.chars[lo..hi].iter().collect()
            } else {
                String::new()
            };
            return unescape(&inner);
        }
        self.token_text(token)
    }
}

/// Port of qc's `unescape`, which simply drops every backslash (it does not
/// treat `\\` as an escaped backslash).
fn unescape(s: &str) -> String {
    s.chars().filter(|c| *c != '\\').collect()
}
