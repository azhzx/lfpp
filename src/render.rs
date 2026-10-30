use crate::token::{Token, TokenType};

pub fn render(source: &str, included: &[String], tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut last_ends: Vec<usize> = vec![0; included.len() + 1];
    let mut prev: Option<&Token> = None;

    for tok in tokens {
        if tok.kind == TokenType::Eof {
            continue;
        }

        let file_src = file_source(source, included, tok.file_id);
        let fi = tok.file_id as usize;
        if fi >= last_ends.len() {
            last_ends.resize(fi + 1, 0);
        }
        let last_end = last_ends[fi];

        let is_source = !tok.is_synthetic()
            && tok.span.end_off <= file_src.len()
            && tok.span.start_off <= tok.span.end_off
            && tok.span.start_off >= last_end;

        if is_source {
            let gap = &file_src[last_end..tok.span.start_off];
            if is_transparent_gap(gap) {
                out.push_str(gap);
                out.push_str(&file_src[tok.span.start_off..tok.span.end_off]);
                last_ends[fi] = tok.span.end_off;
                prev = Some(tok);
                continue;
            }
        }

        if let Some(p) = prev {
            let space = if !p.is_synthetic() && !tok.is_synthetic() {
                needs_space_lexical(p, tok)
            } else {
                needs_space(p, tok)
            };
            if space {
                out.push(' ');
            }
        }

        if tok.is_synthetic() {
            out.push_str(&tok.text);
        } else if tok.span.end_off <= file_src.len() && tok.span.start_off < tok.span.end_off {
            out.push_str(&file_src[tok.span.start_off..tok.span.end_off]);
            last_ends[fi] = tok.span.end_off.max(last_end);
        } else {
            out.push_str(&tok.text);
        }

        prev = Some(tok);
    }

    out
}

fn file_source<'a>(main: &'a str, included: &'a [String], file_id: u32) -> &'a str {
    if file_id == 0 {
        main
    } else {
        included
            .get((file_id - 1) as usize)
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

fn is_transparent_gap(gap: &str) -> bool {
    let bytes = gap.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // line comment: skip to newline
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            // block comment
            let mut j = i + 2;
            let mut closed = false;
            while j + 1 < bytes.len() {
                if bytes[j] == b'*' && bytes[j + 1] == b'/' {
                    j += 2;
                    closed = true;
                    break;
                }
                j += 1;
            }
            if !closed {
                return false;
            }
            i = j;
            continue;
        }
        return false;
    }
    true
}


fn needs_space_lexical(prev: &Token, next: &Token) -> bool {
    let is_word = |t: &TokenType| {
        matches!(
            t,
            TokenType::Ident
                | TokenType::Int
                | TokenType::RadixInt
                | TokenType::Float
                | TokenType::ENotationFloat
                | TokenType::Byte
                | TokenType::Char
                | TokenType::String
        )
    };

    // `a` `b` would become `ab`; `1` `e5` etc.
    if is_word(&prev.kind) && is_word(&next.kind) {
        return true;
    }

    // `1` `.` `5` would become `1.5`
    if matches!(
        prev.kind,
        TokenType::Int | TokenType::RadixInt | TokenType::Float | TokenType::ENotationFloat
    ) && next.kind == TokenType::Dot
    {
        return true;
    }

    let prev_ch = prev.text.chars().last();
    let next_ch = next.text.chars().next();
    let (Some(pc), Some(nc)) = (prev_ch, next_ch) else {
        return false;
    };

    if is_word(&prev.kind) != is_word(&next.kind) {
        return false;
    }

    // both non-word: glue only if concatenation forms a longer operator / comment
    if !is_word(&prev.kind) && !is_word(&next.kind) {
        return operator_glues(pc, nc);
    }

    false
}

fn operator_glues(a: char, b: char) -> bool {
    // comment starters
    if a == '/' && (b == '/' || b == '*') {
        return true;
    }
    // inc/dec
    if (a == '+' || a == '-') && (b == '+' || b == '-') {
        return true;
    }
    // `==` `!=` `<=` `>=` `&&` `||` `<<` `>>` `+=` ...
    if (a == '=' || a == '!' || a == '<' || a == '>') && b == '=' {
        return true;
    }
    if (a == '&' || a == '|') && (b == '&' || b == '|' || b == '=') {
        return true;
    }
    if a == '-' && b == '>' {
        return true;
    }
    // compound assign second char
    if (a == '+' || a == '-' || a == '*' || a == '/' || a == '%' || a == '^') && b == '=' {
        return true;
    }
    // `##` and `...` / `..`
    if a == '#' && b == '#' {
        return true;
    }
    if a == '.' && b == '.' {
        return true;
    }
    // `*/` (end of block comment) — if somehow adjacent
    if a == '*' && b == '/' {
        return true;
    }
    false
}

fn needs_space(prev: &Token, next: &Token) -> bool {
    // no space before these
    match next.kind {
        TokenType::Comma
        | TokenType::Semicolon
        | TokenType::Rparen
        | TokenType::Rbracket
        | TokenType::Rbrace
        | TokenType::Dot
        | TokenType::Arrow
        | TokenType::PPEnd
        | TokenType::NewLine => return false,
        _ => {}
    }
    // no space after these
    match prev.kind {
        TokenType::Lparen
        | TokenType::Lbracket
        | TokenType::Lbrace
        | TokenType::Dot
        | TokenType::PPStart
        | TokenType::NewLine => return false,
        _ => {}
    }
    // no space between adjacent operators/operands handled simply:
    match prev.kind {
        TokenType::Ident
        | TokenType::Int
        | TokenType::RadixInt
        | TokenType::Float
        | TokenType::ENotationFloat
        | TokenType::String
        | TokenType::Byte
        | TokenType::Char => {
            // ident followed by ( is a call — no space
            if next.kind == TokenType::Lparen {
                return false;
            }
            return true;
        }
        _ => {}
    }
    match next.kind {
        TokenType::Ident
        | TokenType::Int
        | TokenType::RadixInt
        | TokenType::Float
        | TokenType::ENotationFloat
        | TokenType::String
        | TokenType::Byte
        | TokenType::Char => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::preprocessor::Preprocessor;

    fn render_src(src: &str) -> String {
        let lex = tokenize(src);
        assert!(lex.errors.is_empty(), "lex errors: {:?}", lex.errors);
        let pp = Preprocessor::new(src.to_string(), "<test>".to_string());
        let out = pp.pass(lex.tokens);
        assert!(out.errors.is_empty(), "pp errors: {:?}", out.errors);
        render(src, &out.included_sources, &out.tokens)
    }

    #[test]
    fn expanded_object_macro_keeps_body_spacing() {
        let src = "(%object_macro A B*B*B%)\n(%object_macro B A*A*A%)\nA\n";
        let out = render_src(src);
        let line = out.lines().find(|l| l.contains('A') && !l.contains("object_macro")).unwrap();
        assert!(
            line.contains("A*A*A*A*A*A*A*A*A"),
            "expected tight A*A*A*... expansion, got: {line:?}"
        );
        assert!(!line.contains(" * "), "no spaced stars, got: {line:?}");
    }

    #[test]
    fn plain_source_preserves_spacing() {
        let src = "x = a*b*c;\n";
        let out = render_src(src);
        assert!(out.contains("a*b*c"), "got: {out:?}");
        assert!(out.contains("x = "), "got: {out:?}");
    }

    #[test]
    fn word_tokens_still_get_space_when_not_from_source_order() {
        let src = "XX (%concat%) YY\n";
        let out = render_src(src);
        assert!(out.contains("XXYY"), "got: {out:?}");
    }
}
