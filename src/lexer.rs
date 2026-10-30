use unicode_xid::UnicodeXID;

use crate::diagnose::{PpError, PpResult};
use crate::token::{Span, Token, TokenType};

const OPERATORS: &[(&str, TokenType)] = &[
    ("<<=", TokenType::ShlEq),
    (">>=", TokenType::ShrEq),
    ("...", TokenType::DotDotDot),
    ("..", TokenType::DotDot),
    ("->", TokenType::Arrow),
    ("=>", TokenType::FatArrow),
    ("==", TokenType::EqEq),
    ("!=", TokenType::Ne),
    ("<=", TokenType::Le),
    (">=", TokenType::Ge),
    ("&&", TokenType::And),
    ("||", TokenType::Or),
    ("<<", TokenType::Shl),
    (">>", TokenType::Shr),
    ("+=", TokenType::PlusEq),
    ("-=", TokenType::MinusEq),
    ("*=", TokenType::StarEq),
    ("/=", TokenType::SlashEq),
    ("%=", TokenType::PercentEq),
    ("&=", TokenType::AmpEq),
    ("|=", TokenType::PipeEq),
    ("^=", TokenType::CaretEq),
    ("+", TokenType::Plus),
    ("-", TokenType::Minus),
    ("*", TokenType::Star),
    ("/", TokenType::Slash),
    ("%", TokenType::Percent),
    ("&", TokenType::Amp),
    ("|", TokenType::Pipe),
    ("^", TokenType::Caret),
    ("!", TokenType::Not),
    ("~", TokenType::Tilde),
    ("=", TokenType::Eq),
    ("<", TokenType::Lt),
    (">", TokenType::Gt),
    (".", TokenType::Dot),
    ("(", TokenType::Lparen),
    (")", TokenType::Rparen),
    ("{", TokenType::Lbrace),
    ("}", TokenType::Rbrace),
    ("[", TokenType::Lbracket),
    ("]", TokenType::Rbracket),
    (",", TokenType::Comma),
    (":", TokenType::Colon),
    (";", TokenType::Semicolon),
    ("#", TokenType::Hash),
    ("@", TokenType::At),
    ("?", TokenType::Question),
    ("_", TokenType::Underline),
];

#[derive(Debug, Clone)]
pub struct LexOutput {
    pub tokens: Vec<Token>,
    pub errors: Vec<PpError>,
}

pub struct Lexer {
    code: Vec<char>,
    index: usize,
    byte_index: usize,
}

impl Lexer {
    pub fn new(text: &str) -> Self {
        Lexer {
            code: text.chars().collect(),
            index: 0,
            byte_index: 0,
        }
    }

    fn current_char(&self) -> Option<char> {
        self.code.get(self.index).copied()
    }

    fn peek_char(&self, n: usize) -> Option<char> {
        self.code.get(self.index + n).copied()
    }

    fn next_char(&mut self) {
        if let Some(ch) = self.current_char() {
            self.index += 1;
            self.byte_index += ch.len_utf8();
        }
    }

    fn offset(&self) -> usize {
        self.byte_index
    }

    fn try_skip_line_splice(&mut self) -> bool {
        if self.current_char() != Some('\\') {
            return false;
        }
        match self.peek_char(1) {
            Some('\n') => {
                self.next_char();
                self.next_char();
                true
            }
            Some('\r') if self.peek_char(2) == Some('\n') => {
                self.next_char();
                self.next_char();
                self.next_char();
                true
            }
            _ => false,
        }
    }

    fn span(&self, start: usize) -> Span {
        Span {
            start_off: start,
            end_off: self.byte_index,
        }
    }

    pub fn tokenize(mut self) -> LexOutput {
        let mut tokens: Vec<Token> = Vec::new();
        let mut errors: Vec<PpError> = Vec::new();

        loop {
            // skip whitespace (not newlines), and delete line splices
            while let Some(ch) = self.current_char() {
                if self.try_skip_line_splice() {
                    continue;
                }
                if ch == '\n' {
                    break;
                }
                if ch == '\r' || ch == ' ' || ch == '\t' {
                    self.next_char();
                    continue;
                }
                break;
            }

            let Some(ch) = self.current_char() else { break };

            if ch == '\n' {
                let start = self.offset();
                self.next_char();
                tokens.push(Token {
                    kind: TokenType::NewLine,
                    span: self.span(start),
                    text: "\n".to_string(),
                    hide: Default::default(),
                    file_id: 0,
                });
                continue;
            }

            // comments
            if ch == '/' && self.peek_char(1) == Some('/') {
                while let Some(c) = self.current_char() {
                    if c == '\n' {
                        break;
                    }
                    self.next_char();
                }
                continue;
            }
            if ch == '/' && self.peek_char(1) == Some('*') {
                let start = self.offset();
                self.next_char(); // /
                self.next_char(); // *
                let mut closed = false;
                while self.index < self.code.len() {
                    if self.current_char() == Some('*') && self.peek_char(1) == Some('/') {
                        self.next_char();
                        self.next_char();
                        closed = true;
                        break;
                    }
                    self.next_char();
                }
                if !closed {
                    errors.push(PpError::new(
                        Span {
                            start_off: start,
                            end_off: self.offset(),
                        },
                        "unterminated block comment",
                    ));
                }
                continue;
            }

            // (%
            if ch == '(' && self.peek_char(1) == Some('%') {
                let start = self.offset();
                self.next_char();
                self.next_char();
                tokens.push(Token {
                    kind: TokenType::PPStart,
                    span: self.span(start),
                    text: "(%".to_string(),
                    hide: Default::default(),
                    file_id: 0,
                });
                continue;
            }

            // %)
            if ch == '%' && self.peek_char(1) == Some(')') {
                let start = self.offset();
                self.next_char();
                self.next_char();
                tokens.push(Token {
                    kind: TokenType::PPEnd,
                    span: self.span(start),
                    text: "%)".to_string(),
                    hide: Default::default(),
                    file_id: 0,
                });
                continue;
            }

            // string
            if ch == '"' {
                match self.lex_string('"') {
                    Ok(tok) => tokens.push(tok),
                    Err(e) => errors.push(e),
                }
                continue;
            }

            // byte string prefix b"..."
            if ch == 'b' && self.peek_char(1) == Some('"') {
                let start = self.offset();
                self.next_char(); // b
                match self.lex_string('"') {
                    Ok(mut tok) => {
                        tok.kind = TokenType::Byte;
                        tok.span = self.span(start);
                        tokens.push(tok);
                    }
                    Err(e) => errors.push(e),
                }
                continue;
            }

            // char literal
            if ch == '\'' {
                match self.lex_char() {
                    Ok(tok) => tokens.push(tok),
                    Err(e) => errors.push(e),
                }
                continue;
            }

            // number
            if ch.is_ascii_digit() {
                let tok = self.lex_number();
                tokens.push(tok);
                continue;
            }

            // identifier
            if ch == '_' || ch.is_xid_start() {
                let start = self.offset();
                let mut text = String::new();
                while let Some(c) = self.current_char() {
                    if c == '_' || c.is_xid_continue() {
                        text.push(c);
                        self.next_char();
                    } else {
                        break;
                    }
                }
                tokens.push(Token {
                    kind: TokenType::Ident,
                    span: self.span(start),
                    text,
                    hide: Default::default(),
                    file_id: 0,
                });
                continue;
            }

            // operators (maximal munch)
            if let Some(tok) = self.lex_operator() {
                tokens.push(tok);
                continue;
            }

            // unknown character
            let start = self.offset();
            self.next_char();
            errors.push(PpError::new(
                self.span(start),
                format!("unexpected character `{ch}`"),
            ));
            tokens.push(Token {
                kind: TokenType::Error,
                span: self.span(start),
                text: ch.to_string(),
                hide: Default::default(),
                file_id: 0,
            });
        }

        // ensure trailing newline then EOF
        if let Some(last) = tokens.last() {
            if last.kind != TokenType::NewLine {
                let off = self.offset();
                tokens.push(Token {
                    kind: TokenType::NewLine,
                    span: Span {
                        start_off: off,
                        end_off: off,
                    },
                    text: "\n".to_string(),
                    hide: Default::default(),
                    file_id: 0,
                });
            }
        }
        let off = self.offset();
        tokens.push(Token {
            kind: TokenType::Eof,
            span: Span {
                start_off: off,
                end_off: off,
            },
            text: String::new(),
            hide: Default::default(),
            file_id: 0,
        });

        LexOutput { tokens, errors }
    }

    fn lex_string(&mut self, quote: char) -> PpResult<Token> {
        let start = self.offset();
        self.next_char(); // opening quote
        let mut text = String::new();
        let mut closed = false;
        while let Some(c) = self.current_char() {
            if self.try_skip_line_splice() {
                continue;
            }
            if c == '\\' {
                self.next_char();
                let esc = self.current_char().unwrap_or('\\');
                let mapped = match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    '"' => '"',
                    '\'' => '\'',
                    '0' => '\0',
                    other => other,
                };
                text.push(mapped);
                self.next_char();
                continue;
            }
            if c == quote {
                self.next_char();
                closed = true;
                break;
            }
            if c == '\n' {
                break;
            }
            text.push(c);
            self.next_char();
        }
        if !closed {
            return Err(PpError::new(
                Span {
                    start_off: start,
                    end_off: self.offset(),
                },
                "unterminated string literal",
            ));
        }
        Ok(Token {
            kind: TokenType::String,
            span: self.span(start),
            text,
            hide: Default::default(),
            file_id: 0,
        })
    }

    fn lex_char(&mut self) -> PpResult<Token> {
        let start = self.offset();
        self.next_char(); // '
        let mut text = String::new();
        let mut closed = false;
        while let Some(c) = self.current_char() {
            if self.try_skip_line_splice() {
                continue;
            }
            if c == '\\' {
                self.next_char();
                let esc = self.current_char().unwrap_or('\\');
                let mapped = match esc {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    '0' => '\0',
                    other => other,
                };
                text.push(mapped);
                self.next_char();
                continue;
            }
            if c == '\'' {
                self.next_char();
                closed = true;
                break;
            }
            if c == '\n' {
                break;
            }
            text.push(c);
            self.next_char();
        }
        if !closed {
            return Err(PpError::new(
                Span {
                    start_off: start,
                    end_off: self.offset(),
                },
                "unterminated character literal",
            ));
        }
        Ok(Token {
            kind: TokenType::Char,
            span: self.span(start),
            text,
            hide: Default::default(),
            file_id: 0,
        })
    }

    fn lex_number(&mut self) -> Token {
        let start = self.offset();
        let mut text = String::new();
        let mut base: u32 = 0;
        let mut is_float = false;
        let mut has_exponent = false;

        // radix prefix
        if self.current_char() == Some('0') {
            let base_val = match self.peek_char(1) {
                Some('b') | Some('B') => 2,
                Some('o') | Some('O') => 8,
                Some('x') | Some('X') => 16,
                _ => 0,
            };
            if base_val != 0 && self.peek_char(2).is_some_and(|c| c.to_digit(base_val).is_some())
            {
                base = base_val;
                text.push('0');
                text.push(self.peek_char(1).unwrap());
                self.next_char();
                self.next_char();
            }
        }

        while let Some(c) = self.current_char() {
            if base != 0 {
                if c.to_digit(base).is_some() || c == '_' {
                    text.push(c);
                    self.next_char();
                } else {
                    break;
                }
            } else if c.is_ascii_digit() {
                text.push(c);
                self.next_char();
            } else if c == '_' {
                text.push(c);
                self.next_char();
            } else if c == '.' && !is_float && !has_exponent && self.peek_char(1) != Some('.') {
                is_float = true;
                text.push(c);
                self.next_char();
            } else if (c == 'e' || c == 'E') && !has_exponent {
                // require digit after e[+-]?
                let mut n = 1;
                if matches!(self.peek_char(1), Some('+') | Some('-')) {
                    n = 2;
                }
                if self
                    .peek_char(n)
                    .is_some_and(|d| d.is_ascii_digit())
                {
                    has_exponent = true;
                    text.push(c);
                    self.next_char();
                    if matches!(self.current_char(), Some('+') | Some('-')) {
                        text.push(self.current_char().unwrap());
                        self.next_char();
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        let kind = if base != 0 {
            TokenType::RadixInt
        } else if has_exponent {
            TokenType::ENotationFloat
        } else if is_float {
            TokenType::Float
        } else {
            TokenType::Int
        };

        Token {
            kind,
            span: self.span(start),
            text,
            hide: Default::default(),
            file_id: 0,
        }
    }

    fn lex_operator(&mut self) -> Option<Token> {
        // try longest match from OPERATORS
        let mut best: Option<(usize, TokenType, &str)> = None;
        for (text, kind) in OPERATORS {
            let len = text.chars().count();
            // check match at current position
            let mut ok = true;
            for (i, expected) in text.chars().enumerate() {
                if self.peek_char(i) != Some(expected) {
                    ok = false;
                    break;
                }
            }
            if ok {
                match best {
                    Some((best_len, _, _)) if best_len >= len => {}
                    _ => best = Some((len, kind.clone(), text)),
                }
            }
        }

        let (len, kind, text) = best?;
        let start = self.offset();
        for _ in 0..len {
            self.next_char();
        }
        Some(Token {
            kind,
            span: self.span(start),
            text: text.to_string(),
            hide: Default::default(),
            file_id: 0,
        })
    }
}

pub fn tokenize(text: &str) -> LexOutput {
    Lexer::new(text).tokenize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenType> {
        tokenize(text)
            .tokens
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn pp_delimiters() {
        let out = tokenize("(%if 1%)");
        assert_eq!(
            out.tokens.iter().map(|t| t.kind.clone()).collect::<Vec<_>>(),
            vec![
                TokenType::PPStart,
                TokenType::Ident,
                TokenType::Int,
                TokenType::PPEnd,
                TokenType::NewLine,
                TokenType::Eof,
            ]
        );
        assert!(out.errors.is_empty());
    }

    #[test]
    fn percent_not_followed_by_rparen_is_operator() {
        let out = tokenize("10 % 3");
        assert_eq!(out.tokens[1].kind, TokenType::Percent);
    }

    #[test]
    fn comments_skipped() {
        let out = tokenize("a // line\n/* block */ b");
        let idents: Vec<_> = out
            .tokens
            .iter()
            .filter(|t| t.kind == TokenType::Ident)
            .map(|t| t.text.clone())
            .collect();
        assert_eq!(idents, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn string_and_char() {
        let out = tokenize(r#"'a' "hello""#);
        assert_eq!(out.tokens[0].kind, TokenType::Char);
        assert_eq!(out.tokens[0].text, "a");
        assert_eq!(out.tokens[1].kind, TokenType::String);
        assert_eq!(out.tokens[1].text, "hello");
    }

    #[test]
    fn operators_maximal_munch() {
        let ks = kinds("<<= -> == &&");
        assert_eq!(ks[0], TokenType::ShlEq);
        assert_eq!(ks[1], TokenType::Arrow);
        assert_eq!(ks[2], TokenType::EqEq);
        assert_eq!(ks[3], TokenType::And);
    }

    #[test]
    fn numbers() {
        let out = tokenize("123 0xFF 45.6 1e10");
        assert_eq!(out.tokens[0].kind, TokenType::Int);
        assert_eq!(out.tokens[1].kind, TokenType::RadixInt);
        assert_eq!(out.tokens[2].kind, TokenType::Float);
        assert_eq!(out.tokens[3].kind, TokenType::ENotationFloat);
    }

    #[test]
    fn hash_include_passthrough() {
        let out = tokenize("#include <stdio.h>");
        assert_eq!(out.tokens[0].kind, TokenType::Hash);
        assert_eq!(out.tokens[1].kind, TokenType::Ident);
        assert_eq!(out.tokens[1].text, "include");
    }
}
