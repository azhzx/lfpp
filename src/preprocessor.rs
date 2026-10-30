use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::diagnose::{format_warning, PpError, PpResult};
use crate::pp_keywords as kw;
use crate::token::{Span, Token, TokenType};

const PRE_DEFINES: [(&str, isize); 3] = [
    if cfg!(target_os = "windows") {
        ("__windows", 1)
    } else if cfg!(target_os = "macos") {
        ("__mac", 1)
    } else if cfg!(target_os = "linux") {
        ("__linux", 1)
    } else {
        ("__unknown", 1)
    },
    if cfg!(target_pointer_width = "64") {
        ("__target_pointer_width", 64)
    } else {
        ("__target_pointer_width", 32)
    },
    ("version", 1),
];

const MAX_EXPAND_STEPS: usize = 1_000_000;

const MAX_INCLUDE_DEPTH: usize = 64;

type HideSet = BTreeSet<String>;


#[derive(Debug, Clone, Copy)]
struct Val {
    bits: u64,
    unsigned: bool,
}

#[derive(Debug, Clone, Copy)]
enum Compare {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Val {
    const ZERO: Val = Val {
        bits: 0,
        unsigned: false,
    };

    fn boolean(value: bool) -> Val {
        Val {
            bits: value as u64,
            unsigned: false,
        }
    }

    fn truthy(self) -> bool {
        self.bits != 0
    }

    fn signed(self) -> i64 {
        self.bits as i64
    }

    fn compare(left: Val, right: Val, cmp: Compare) -> Val {
        let result = if left.unsigned || right.unsigned {
            let (a, b) = (left.bits, right.bits);
            match cmp {
                Compare::Eq => a == b,
                Compare::Ne => a != b,
                Compare::Lt => a < b,
                Compare::Le => a <= b,
                Compare::Gt => a > b,
                Compare::Ge => a >= b,
            }
        } else {
            let (a, b) = (left.signed(), right.signed());
            match cmp {
                Compare::Eq => a == b,
                Compare::Ne => a != b,
                Compare::Lt => a < b,
                Compare::Le => a <= b,
                Compare::Gt => a > b,
                Compare::Ge => a >= b,
            }
        };
        Val::boolean(result)
    }
}

#[derive(Debug, Clone)]
struct PPDef {
    #[allow(dead_code)]
    name_token: Token,
    params: Vec<String>,
    has_rest_args: bool,
    is_function_like: bool,
    body: Vec<Token>,
}

pub struct Preprocessor {
    source_text: String,
    path: String,
    preprocessors: HashMap<String, PPDef>,
    counter: usize,
    warnings: Vec<String>,
    include_stack: Vec<PathBuf>,
    included_sources: Vec<String>,
}

impl Preprocessor {
    pub fn new(source_text: String, path: String) -> Self {
        Preprocessor {
            source_text,
            path,
            preprocessors: HashMap::new(),
            counter: 0,
            warnings: Vec::new(),
            include_stack: Vec::new(),
            included_sources: Vec::new(),
        }
    }

    fn err(&self, span: Span, msg: impl Into<String>) -> PpError {
        PpError::new(span, msg)
    }

    fn token_at<'b>(&self, tokens: &'b [Token], index: usize) -> PpResult<&'b Token> {
        tokens.get(index).ok_or_else(|| {
            self.err(
                Span {
                    start_off: 0,
                    end_off: 0,
                },
                "unexpected end of tokens",
            )
        })
    }

    fn warn(&mut self, span: Span, msg: impl Into<String>) {
        let formatted = format_warning(&self.path, &self.source_text, &span, &msg.into());
        self.warnings.push(formatted);
    }

    // expression evaluator

    pub fn eval(&self, tokens: &[Token]) -> PpResult<isize> {
        let mut index = 0usize;
        let value = self.eval_comma(tokens, &mut index, true)?;
        if index != tokens.len() {
            let tok = self.token_at(tokens, index)?;
            return Err(self.err(
                tok.span.clone(),
                format!("unexpected token '{}' in preprocessor expression", tok.text),
            ));
        }
        Ok(value.bits as isize)
    }

    fn eval_comma(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut value = self.eval_conditional(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Comma {
            *index += 1;
            value = self.eval_conditional(tokens, index, live)?;
        }
        Ok(value)
    }

    fn eval_conditional(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let cond = self.eval_logical_or(tokens, index, live)?;
        if *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Question {
            *index += 1;
            let then_value = self.eval_conditional(tokens, index, live && cond.truthy())?;
            let colon = self.token_at(tokens, *index)?;
            if colon.kind != TokenType::Colon {
                return Err(self.err(colon.span.clone(), "expected ':' in conditional expression"));
            }
            *index += 1;
            let else_value = self.eval_conditional(tokens, index, live && !cond.truthy())?;
            return Ok(if cond.truthy() { then_value } else { else_value });
        }
        Ok(cond)
    }

    fn eval_logical_or(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_logical_and(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Or {
            *index += 1;
            let right = self.eval_logical_and(tokens, index, live && !left.truthy())?;
            left = Val::boolean(left.truthy() || right.truthy());
        }
        Ok(left)
    }

    fn eval_logical_and(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_bit_or(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::And {
            *index += 1;
            let right = self.eval_bit_or(tokens, index, live && left.truthy())?;
            left = Val::boolean(left.truthy() && right.truthy());
        }
        Ok(left)
    }

    fn eval_bit_or(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_bit_xor(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Pipe {
            *index += 1;
            let right = self.eval_bit_xor(tokens, index, live)?;
            left = Val {
                bits: left.bits | right.bits,
                unsigned: left.unsigned || right.unsigned,
            };
        }
        Ok(left)
    }

    fn eval_bit_xor(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_bit_and(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Caret {
            *index += 1;
            let right = self.eval_bit_and(tokens, index, live)?;
            left = Val {
                bits: left.bits ^ right.bits,
                unsigned: left.unsigned || right.unsigned,
            };
        }
        Ok(left)
    }

    fn eval_bit_and(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_equality(tokens, index, live)?;
        while *index < tokens.len() && self.token_at(tokens, *index)?.kind == TokenType::Amp {
            *index += 1;
            let right = self.eval_equality(tokens, index, live)?;
            left = Val {
                bits: left.bits & right.bits,
                unsigned: left.unsigned || right.unsigned,
            };
        }
        Ok(left)
    }

    fn eval_equality(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_relational(tokens, index, live)?;
        loop {
            if *index >= tokens.len() {
                break;
            }
            let op = self.token_at(tokens, *index)?.kind.clone();
            let cmp = match op {
                TokenType::EqEq => Compare::Eq,
                TokenType::Ne => Compare::Ne,
                _ => break,
            };
            *index += 1;
            let right = self.eval_relational(tokens, index, live)?;
            left = Val::compare(left, right, cmp);
        }
        Ok(left)
    }

    fn eval_relational(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_shift(tokens, index, live)?;
        loop {
            if *index >= tokens.len() {
                break;
            }
            let op = self.token_at(tokens, *index)?.kind.clone();
            let cmp = match op {
                TokenType::Lt => Compare::Lt,
                TokenType::Le => Compare::Le,
                TokenType::Gt => Compare::Gt,
                TokenType::Ge => Compare::Ge,
                _ => break,
            };
            *index += 1;
            let right = self.eval_shift(tokens, index, live)?;
            left = Val::compare(left, right, cmp);
        }
        Ok(left)
    }

    fn eval_shift(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_add_sub(tokens, index, live)?;
        loop {
            if *index >= tokens.len() {
                break;
            }
            let op = self.token_at(tokens, *index)?.kind.clone();
            let shift_left = match op {
                TokenType::Shl => true,
                TokenType::Shr => false,
                _ => break,
            };
            *index += 1;
            let right = self.eval_add_sub(tokens, index, live)?;
            let amount = (right.bits & 63) as u32;
            left = if shift_left {
                Val {
                    bits: left.bits.wrapping_shl(amount),
                    unsigned: left.unsigned,
                }
            } else if left.unsigned {
                Val {
                    bits: left.bits >> amount,
                    unsigned: true,
                }
            } else {
                Val {
                    bits: (left.signed() >> amount) as u64,
                    unsigned: false,
                }
            };
        }
        Ok(left)
    }

    fn eval_add_sub(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_mul_div(tokens, index, live)?;
        loop {
            if *index >= tokens.len() {
                break;
            }
            let op = self.token_at(tokens, *index)?.kind.clone();
            let is_add = match op {
                TokenType::Plus => true,
                TokenType::Minus => false,
                _ => break,
            };
            *index += 1;
            let right = self.eval_mul_div(tokens, index, live)?;
            left = Val {
                bits: if is_add {
                    left.bits.wrapping_add(right.bits)
                } else {
                    left.bits.wrapping_sub(right.bits)
                },
                unsigned: left.unsigned || right.unsigned,
            };
        }
        Ok(left)
    }

    fn eval_mul_div(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let mut left = self.eval_unary(tokens, index, live)?;
        loop {
            if *index >= tokens.len() {
                break;
            }
            let op_tok = self.token_at(tokens, *index)?.clone();
            let op = match op_tok.kind {
                TokenType::Star => '*',
                TokenType::Slash => '/',
                TokenType::Percent => '%',
                _ => break,
            };
            *index += 1;
            let right = self.eval_unary(tokens, index, live)?;
            if (op == '/' || op == '%') && right.bits == 0 {
                if live {
                    let what = if op == '/' { "division" } else { "modulo" };
                    return Err(self.err(
                        op_tok.span.clone(),
                        format!("{what} by zero in preprocessor expression"),
                    ));
                }
                left = Val {
                    bits: 0,
                    unsigned: left.unsigned || right.unsigned,
                };
                continue;
            }
            let unsigned = left.unsigned || right.unsigned;
            let bits = match op {
                '*' => left.bits.wrapping_mul(right.bits),
                '/' => {
                    if unsigned {
                        left.bits / right.bits
                    } else {
                        left.signed().wrapping_div(right.signed()) as u64
                    }
                }
                '%' => {
                    if unsigned {
                        left.bits % right.bits
                    } else {
                        left.signed().wrapping_rem(right.signed()) as u64
                    }
                }
                _ => unreachable!(),
            };
            left = Val { bits, unsigned };
        }
        Ok(left)
    }

    fn eval_unary(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        if *index < tokens.len() {
            match self.token_at(tokens, *index)?.kind.clone() {
                TokenType::Not => {
                    *index += 1;
                    let value = self.eval_unary(tokens, index, live)?;
                    return Ok(Val::boolean(!value.truthy()));
                }
                TokenType::Tilde => {
                    *index += 1;
                    let value = self.eval_unary(tokens, index, live)?;
                    return Ok(Val {
                        bits: !value.bits,
                        unsigned: value.unsigned,
                    });
                }
                TokenType::Minus => {
                    *index += 1;
                    let value = self.eval_unary(tokens, index, live)?;
                    return Ok(Val {
                        bits: value.bits.wrapping_neg(),
                        unsigned: value.unsigned,
                    });
                }
                TokenType::Plus => {
                    *index += 1;
                    return self.eval_unary(tokens, index, live);
                }
                _ => {}
            }
        }
        self.eval_primary(tokens, index, live)
    }

    fn eval_primary(&self, tokens: &[Token], index: &mut usize, live: bool) -> PpResult<Val> {
        let token = self.token_at(tokens, *index)?.clone();
        match token.kind {
            TokenType::Int | TokenType::RadixInt => {
                *index += 1;
                let mut unsigned = false;
                while *index < tokens.len() {
                    let suffix = self.token_at(tokens, *index)?;
                    if suffix.kind == TokenType::Ident
                        && !suffix.text.is_empty()
                        && suffix.text.chars().all(|c| matches!(c, 'u' | 'U' | 'l' | 'L'))
                    {
                        unsigned |= suffix.text.chars().any(|c| c == 'u' || c == 'U');
                        *index += 1;
                    } else {
                        break;
                    }
                }
                let (bits, wide_unsigned) = parse_int_literal(&token.text).ok_or_else(|| {
                    self.err(token.span.clone(), format!("invalid integer '{}'", token.text))
                })?;
                Ok(Val {
                    bits,
                    unsigned: unsigned || wide_unsigned,
                })
            }
            TokenType::Char => {
                *index += 1;
                let value = token
                    .text
                    .chars()
                    .next()
                    .ok_or_else(|| self.err(token.span.clone(), "empty character constant"))?
                    as u32 as u64;
                Ok(Val {
                    bits: value,
                    unsigned: false,
                })
            }
            TokenType::Lparen => {
                *index += 1;
                let value = self.eval_comma(tokens, index, live)?;
                let close = self.token_at(tokens, *index)?;
                if close.kind != TokenType::Rparen {
                    return Err(self.err(token.span.clone(), "expected ')'"));
                }
                *index += 1;
                Ok(value)
            }

            TokenType::Ident => {
                *index += 1;
                Ok(Val::ZERO)
            }
            _ => Err(self.err(token.span.clone(), "invalid preprocessor expression")),
        }
    }

    // pp-block helpers
    fn collect_pp_block(
        &self,
        tokens: &[Token],
        index: &mut usize,
    ) -> PpResult<Vec<Token>> {
        let mut collected = Vec::new();
        let mut depth = 1usize;
        while *index < tokens.len() {
            let tok = self.token_at(tokens, *index)?;
            match tok.kind {
                TokenType::PPStart => {
                    depth += 1;
                    collected.push(tok.clone());
                }
                TokenType::PPEnd => {
                    depth -= 1;
                    if depth == 0 {
                        *index += 1;
                        return Ok(collected);
                    }
                    collected.push(tok.clone());
                }
                TokenType::Eof => break,
                _ => collected.push(tok.clone()),
            }
            *index += 1;
        }
        Err(self.err(
            Span {
                start_off: 0,
                end_off: 0,
            },
            "unterminated `(%` preprocessor block",
        ))
    }

    fn pp_keyword(
        &self,
        tokens: &[Token],
        index: &mut usize,
    ) -> PpResult<(Token, String)> {
        *index += 1; // skip PPStart
        let tok = self.token_at(tokens, *index)?.clone();
        if tok.kind != TokenType::Ident {
            return Err(self.err(tok.span.clone(), "expected preprocessor keyword after `(%`"));
        }
        *index += 1;
        Ok((tok.clone(), tok.text))
    }

    fn synthetic_int(&self, value: isize, at: usize) -> Token {
        Token {
            kind: TokenType::Int,
            span: Span::synthetic(at),
            text: value.to_string(),
            hide: HideSet::new(),
            file_id: 0,
        }
    }

    fn synthetic_string(&self, text: String, at: usize) -> Token {
        Token {
            kind: TokenType::String,
            span: Span::synthetic(at),
            text,
            hide: HideSet::new(),
            file_id: 0,
        }
    }

    fn synthetic_comma(&self, at: usize) -> Token {
        Token {
            kind: TokenType::Comma,
            span: Span::synthetic(at),
            text: ",".to_string(),
            hide: HideSet::new(),
            file_id: 0,
        }
    }

    // hide set
    fn add_hideset(toks: &[Token], hs: &HideSet) -> Vec<Token> {
        toks.iter()
            .map(|t| {
                let mut t = t.clone();
                t.hide.extend(hs.iter().cloned());
                t
            })
            .collect()
    }

    // macro expansion
    fn expand_tokens(&mut self, tokens: Vec<Token>) -> PpResult<Vec<Token>> {
        let mut work = tokens;
        let mut i = 0usize;
        let mut out: Vec<Token> = Vec::new();
        let mut steps = 0usize;

        while i < work.len() {
            steps += 1;
            if steps > MAX_EXPAND_STEPS {
                return Err(self.err(
                    Span {
                        start_off: 0,
                        end_off: 0,
                    },
                    "preprocessor expansion limit exceeded",
                ));
            }

            let tok = work[i].clone();

            if tok.kind == TokenType::Eof {
                out.push(tok);
                i += 1;
                continue;
            }

            if tok.kind == TokenType::PPStart && tok.hide.is_empty() {
                self.handle_directive(&mut work, &mut i, &mut out)?;
                continue;
            }

            if tok.kind == TokenType::Ident {
                if let Some((end, replacement)) = self.try_expand(&work, i)? {
                    work.splice(i..end, replacement);
                    continue;
                }
            }

            out.push(tok);
            i += 1;
        }

        Ok(out)
    }

    fn try_expand(
        &mut self,
        work: &[Token],
        i: usize,
    ) -> PpResult<Option<(usize, Vec<Token>)>> {
        let tok = &work[i];
        if tok.kind != TokenType::Ident {
            return Ok(None);
        }
        let name = tok.text.clone();
        if !self.preprocessors.contains_key(&name) {
            return Ok(None);
        }
        if tok.hide.contains(&name) {
            return Ok(None);
        }
        let def = self.preprocessors[&name].clone();

        if def.is_function_like {
            let mut lparen = i + 1;
            while lparen < work.len() && work[lparen].kind == TokenType::NewLine {
                lparen += 1;
            }
            if lparen >= work.len() || work[lparen].kind != TokenType::Lparen {
                return Ok(None);
            }
            self.try_expand_function(work, i, lparen, &tok.hide, &name, &def)
        } else {
            let mut hs = tok.hide.clone();
            hs.insert(name.clone());
            // Object-like replacement lists can also contain `(%concat%)`.
            let expanded_body = self.subst_body(&def.body, &def, &[], &[], &[], true, &name)?;
            let body = Self::add_hideset(&expanded_body, &hs);
            Ok(Some((i + 1, body)))
        }
    }

    fn try_expand_function(
        &mut self,
        work: &[Token],
        i: usize,
        lparen: usize,
        name_hide: &HideSet,
        name: &str,
        def: &PPDef,
    ) -> PpResult<Option<(usize, Vec<Token>)>> {
        let mut j = lparen;
        let mut depth = 0usize;
        let mut args: Vec<Vec<Token>> = Vec::new();
        let mut separators: Vec<Token> = Vec::new();
        let mut cur: Vec<Token> = Vec::new();
        let mut rparen_tok: Option<Token> = None;

        while j < work.len() {
            let t = work[j].clone();
            match t.kind {
                TokenType::Lparen => {
                    depth += 1;
                    if depth == 1 {
                        j += 1;
                        continue;
                    }
                    cur.push(t);
                }
                TokenType::Rparen => {
                    depth -= 1;
                    if depth == 0 {
                        rparen_tok = Some(t);
                        j += 1;
                        break;
                    }
                    cur.push(t);
                }
                TokenType::Comma if depth == 1 => {
                    args.push(std::mem::take(&mut cur));
                    separators.push(t);
                }
                TokenType::Eof => break,
                _ => cur.push(t),
            }
            j += 1;
        }

        let Some(rparen_tok) = rparen_tok else {
            return Err(self.err(
                work[i].span.clone(),
                "unterminated macro argument list",
            ));
        };

        if !args.is_empty() || !cur.is_empty() || !def.params.is_empty() {
            args.push(cur);
        }

        if def.has_rest_args {
            if args.len() < def.params.len() {
                return Err(self.err(
                    work[i].span.clone(),
                    format!(
                        "macro `{}` expects at least {} arguments, got {}",
                        name,
                        def.params.len(),
                        args.len()
                    ),
                ));
            }
        } else if def.params.len() != args.len() {
            return Err(self.err(
                work[i].span.clone(),
                format!(
                    "macro `{}` expects {} arguments, got {}",
                    name,
                    def.params.len(),
                    args.len()
                ),
            ));
        }

        let rest_raw_empty = if def.has_rest_args {
            let start = def.params.len().min(args.len());
            args[start..].iter().all(|a| a.is_empty())
        } else {
            true
        };

        let mut expanded_args: Vec<Vec<Token>> = Vec::with_capacity(args.len());
        for a in &args {
            expanded_args.push(self.expand_tokens(a.clone())?);
        }

        let mut hs: HideSet = name_hide.intersection(&rparen_tok.hide).cloned().collect();
        hs.insert(name.to_string());

        let body = self.subst_body(
            &def.body,
            def,
            &args,
            &separators,
            &expanded_args,
            rest_raw_empty,
            name,
        )?;
        let body = Self::add_hideset(&body, &hs);
        Ok(Some((j, body)))
    }


    fn subst_body(
        &self,
        body: &[Token],
        def: &PPDef,
        raw_args: &[Vec<Token>],
        separators: &[Token],
        expanded_args: &[Vec<Token>],
        rest_empty: bool,
        macro_name: &str,
    ) -> PpResult<Vec<Token>> {
        let mut result: Vec<Token> = Vec::new();
        let mut bi = 0usize;
        let at = def.name_token.span.start_off;
        let mut left_placemarker = false;

        while bi < body.len() {
            let t = &body[bi];

            // (%va_args%)
            if is_marker(body, bi, kw::VA_ARGS) {
                if def.has_rest_args {
                    let next_is_concat = is_marker(body, bi + 3, kw::CONCAT);
                    let src = if next_is_concat { raw_args } else { expanded_args };
                    let toks = self.variadic_tokens(def, src, separators, at);
                    if next_is_concat && toks.is_empty() {
                        left_placemarker = true;
                    } else {
                        result.extend(toks);
                        left_placemarker = false;
                    }
                } else {
                    left_placemarker = false;
                }
                bi += 3;
                continue;
            }

            // (%va_opt(...)%)
            if t.kind == TokenType::PPStart
                && bi + 3 < body.len()
                && body[bi + 1].kind == TokenType::Ident
                && body[bi + 1].text == kw::VA_OPT
                && body[bi + 2].kind == TokenType::Lparen
            {
                let content_start = bi + 3;
                let mut pdepth = 1usize;
                let mut content_end = content_start;
                while content_end < body.len() {
                    match body[content_end].kind {
                        TokenType::Lparen => pdepth += 1,
                        TokenType::Rparen => {
                            pdepth -= 1;
                            if pdepth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    content_end += 1;
                }
                if pdepth != 0 {
                    return Err(self.err(t.span.clone(), "unterminated '(%va_opt' content"));
                }
                if content_end + 1 >= body.len() || body[content_end + 1].kind != TokenType::PPEnd {
                    return Err(self.err(
                        t.span.clone(),
                        "expected '%)' after '(%va_opt(...%)'",
                    ));
                }
                if !rest_empty {
                    let nested = self.subst_body(
                        &body[content_start..content_end],
                        def,
                        raw_args,
                        separators,
                        expanded_args,
                        rest_empty,
                        macro_name,
                    )?;
                    result.extend(nested);
                }
                left_placemarker = false;
                bi = content_end + 2;
                continue;
            }

            // (%to_string%) OPERAND   --   C's '#' operator
            if is_marker(body, bi, kw::TO_STRING) {
                let operand_index = bi + 3;
                let operand = body.get(operand_index).ok_or_else(|| {
                    self.err(t.span.clone(), "'(%to_string%)' requires a following token")
                })?;
                let (text, consumed) = if let Some(idx) = param_index(def, operand) {
                    let raw = raw_args.get(idx).map(|v| v.as_slice()).unwrap_or(&[]);
                    (self.stringize_tokens(raw), 1usize)
                } else if is_marker(body, operand_index, kw::VA_ARGS) {
                    let text = if def.has_rest_args {
                        self.stringize_tokens(&self.variadic_tokens(
                            def,
                            raw_args,
                            separators,
                            at,
                        ))
                    } else {
                        String::new()
                    };
                    (text, 3usize)
                } else {
                    (self.token_spelling(operand), 1usize)
                };
                let literal = format!("\"{}\"", escape_string_body(&text));
                let span = operand.span.clone();
                result.push(self.synthetic_string(literal, span.start_off));
                left_placemarker = false;
                bi = operand_index + consumed;
                continue;
            }

            // (%concat%)   --   C's '##' operator (infix: left (%concat%) right)
            if is_marker(body, bi, kw::CONCAT) {
                let right = body.get(bi + 3);
                let left = if left_placemarker { None } else { result.pop() };
                let left_text = match &left {
                    Some(tok) => self.token_spelling(tok),
                    None => String::new(),
                };
                let left_hide: HideSet = left
                    .as_ref()
                    .map(|t| t.hide.clone())
                    .unwrap_or_default();

                let right_is_variadic = is_marker(body, bi + 3, kw::VA_ARGS);
                let (right_tokens, right_hide): (Vec<Token>, HideSet) = if right_is_variadic {
                    let toks = if def.has_rest_args {
                        self.variadic_tokens(def, raw_args, separators, at)
                    } else {
                        Vec::new()
                    };
                    let hide = toks.first().map(|t| t.hide.clone()).unwrap_or_default();
                    (toks, hide)
                } else {
                    match right {
                        Some(rtok) => {
                            if let Some(idx) = param_index(def, rtok) {
                                let raw = raw_args.get(idx).cloned().unwrap_or_default();
                                let hide = raw.first().map(|t| t.hide.clone()).unwrap_or_default();
                                (raw, hide)
                            } else {
                                (vec![rtok.clone()], rtok.hide.clone())
                            }
                        }
                        None => (Vec::new(), HideSet::new()),
                    }
                };

                if right_tokens.is_empty() {
                    // x ## placemarker  ->  x
                    if let Some(tok) = left {
                        result.push(tok);
                    }
                } else {
                    let first_right = &right_tokens[0];
                    let merged = format!("{left_text}{}", self.token_spelling(first_right));
                    let hide: HideSet = left_hide.intersection(&right_hide).cloned().collect();
                    let kind = classify_pasted(&merged, left.as_ref().map(|t| t.kind.clone()));
                    result.push(Token {
                        kind,
                        span: Span::synthetic(first_right.span.start_off.max(at)),
                        text: merged,
                        hide,
                        file_id: 0,
                    });
                    result.extend(right_tokens[1..].iter().cloned());
                }
                left_placemarker = false;
                bi += if right_is_variadic { 6 } else { 4 };
                continue;
            }

            if let Some(idx) = param_index(def, t) {
                if is_marker(body, bi + 1, kw::CONCAT) {
                    let raw = raw_args.get(idx).map(|v| v.as_slice()).unwrap_or(&[]);
                    if raw.is_empty() {
                        left_placemarker = true;
                    } else {
                        result.extend(raw.iter().cloned());
                        left_placemarker = false;
                    }
                } else if let Some(arg) = expanded_args.get(idx) {
                    result.extend(arg.iter().cloned());
                    left_placemarker = false;
                }
                bi += 1;
                continue;
            }

            result.push(t.clone());
            left_placemarker = false;
            bi += 1;
        }

        Ok(result)
    }


    fn variadic_tokens(
        &self,
        def: &PPDef,
        src_args: &[Vec<Token>],
        separators: &[Token],
        at: usize,
    ) -> Vec<Token> {
        let start = def.params.len().min(src_args.len());
        let mut out = Vec::new();
        for (k, arg) in src_args[start..].iter().enumerate() {
            if k > 0 {
                let sep = start + k - 1;
                if let Some(comma) = separators.get(sep) {
                    out.push(comma.clone());
                } else {
                    out.push(self.synthetic_comma(at));
                }
            }
            out.extend(arg.iter().cloned());
        }
        out
    }


    fn stringize_tokens(&self, toks: &[Token]) -> String {
        let mut out = String::new();
        for (i, tok) in toks.iter().enumerate() {
            if i > 0 {
                let prev = &toks[i - 1];
                let adjacent = !prev.is_synthetic()
                    && !tok.is_synthetic()
                    && prev.file_id == tok.file_id
                    && prev.span.end_off == tok.span.start_off;
                if !adjacent {
                    out.push(' ');
                }
            }
            out.push_str(&self.token_spelling(tok));
        }
        out
    }

    // directives

    fn take_string_path(
        &mut self,
        work: &[Token],
        index: &mut usize,
        kw_tok: &Token,
    ) -> PpResult<String> {
        let raw = self.collect_pp_block(work, index)?;
        let raw: Vec<Token> = raw
            .into_iter()
            .filter(|t| t.kind != TokenType::NewLine)
            .collect();
        let expanded = self.expand_tokens(raw)?;
        if expanded.len() != 1 || expanded[0].kind != TokenType::String {
            return Err(self.err(kw_tok.span.clone(), "expected string literal path"));
        }
        Ok(expanded[0].text.clone())
    }

    /// (%to_string%)
    fn token_spelling(&self, tok: &Token) -> String {
        let source = if tok.file_id == 0 {
            self.source_text.as_str()
        } else {
            self.included_sources
                .get((tok.file_id - 1) as usize)
                .map(|s| s.as_str())
                .unwrap_or(self.source_text.as_str())
        };
        if !tok.is_synthetic()
            && tok.span.start_off <= tok.span.end_off
            && tok.span.end_off <= source.len()
        {
            source[tok.span.start_off..tok.span.end_off].to_string()
        } else {
            tok.text.clone()
        }
    }

    fn resolve_include_path(&self, raw: &str) -> PathBuf {
        let p = Path::new(raw);
        if p.is_absolute() {
            return p.to_path_buf();
        }
        let base = Path::new(&self.path).parent().unwrap_or(Path::new("."));
        let cand = base.join(p);
        if cand.exists() {
            cand
        } else {
            p.to_path_buf()
        }
    }

    fn handle_include(
        &mut self,
        work: &[Token],
        index: &mut usize,
        out: &mut Vec<Token>,
        kw_tok: &Token,
    ) -> PpResult<()> {
        let raw = self.take_string_path(work, index, kw_tok)?;
        let resolved = self.resolve_include_path(&raw);
        let canonical = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());

        if self.include_stack.iter().any(|p| p == &canonical) {
            let mut chain: Vec<String> = self
                .include_stack
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            chain.push(canonical.display().to_string());
            return Err(self.err(
                kw_tok.span.clone(),
                format!("recursive include: {}", chain.join(" -> ")),
            ));
        }
        if self.include_stack.len() >= MAX_INCLUDE_DEPTH {
            return Err(self.err(
                kw_tok.span.clone(),
                format!("include depth limit exceeded ({MAX_INCLUDE_DEPTH})"),
            ));
        }

        let source = match std::fs::read_to_string(&resolved) {
            Ok(s) => s,
            Err(e) => {
                return Err(self.err(
                    kw_tok.span.clone(),
                    format!("cannot read `{}`: {e}", raw),
                ));
            }
        };

        let lex = crate::lexer::tokenize(&source);
        if let Some(le) = lex.errors.first() {
            return Err(le.clone().with_origin(&resolved.display().to_string(), &source));
        }

        let file_id = (self.included_sources.len() + 1) as u32;
        self.included_sources.push(source.clone());
        let mut inc_tokens = lex.tokens;
        for t in &mut inc_tokens {
            t.file_id = file_id;
        }

        self.include_stack.push(canonical);
        let saved_path = std::mem::replace(&mut self.path, resolved.display().to_string());
        let saved_source = std::mem::replace(&mut self.source_text, source.clone());
        let result = self.expand_tokens(inc_tokens);
        self.path = saved_path;
        self.source_text = saved_source;
        self.include_stack.pop();

        match result {
            Ok(toks) => {
                out.extend(toks.into_iter().filter(|t| t.kind != TokenType::Eof));
                Ok(())
            }
            Err(e) => Err(e.with_origin(&resolved.display().to_string(), &source)),
        }
    }

    fn handle_include_string(
        &mut self,
        work: &[Token],
        index: &mut usize,
        out: &mut Vec<Token>,
        kw_tok: &Token,
    ) -> PpResult<()> {
        let raw = self.take_string_path(work, index, kw_tok)?;
        let resolved = self.resolve_include_path(&raw);
        let content = match std::fs::read_to_string(&resolved) {
            Ok(s) => s,
            Err(e) => {
                return Err(self.err(
                    kw_tok.span.clone(),
                    format!("cannot read `{}`: {e}", raw),
                ));
            }
        };
        out.push(self.synthetic_string(
            format!("\"{}\"", escape_string_body(&content)),
            kw_tok.span.start_off,
        ));
        Ok(())
    }

    fn handle_include_bytes(
        &mut self,
        work: &[Token],
        index: &mut usize,
        out: &mut Vec<Token>,
        kw_tok: &Token,
    ) -> PpResult<()> {
        let raw = self.take_string_path(work, index, kw_tok)?;
        let resolved = self.resolve_include_path(&raw);
        let content = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => {
                return Err(self.err(
                    kw_tok.span.clone(),
                    format!("cannot read `{}`: {e}", raw),
                ));
            }
        };
        out.push(Token {
            kind: TokenType::Byte,
            span: Span::synthetic(kw_tok.span.start_off),
            text: format!("b\"{}\"", escape_bytes_body(&content)),
            hide: HideSet::new(),
            file_id: 0,
        });
        Ok(())
    }

    fn handle_directive(
        &mut self,
        work: &mut Vec<Token>,
        i: &mut usize,
        out: &mut Vec<Token>,
    ) -> PpResult<()> {
        let header_start = *i;
        let (kw_tok, name) = self.pp_keyword(work, i)?;
        match name.as_str() {
            kw::DEFINE_OBJECT_MACRO => {
                self.parse_object_macro(work, i, &kw_tok)?;
            }
            kw::DEFINE_FUNCTION_MACRO => {
                self.parse_function_macro(work, i, &kw_tok)?;
            }
            kw::UNDEFINE => {
                let target = self.token_at(work, *i)?.clone();
                *i += 1;
                self.expect_pp_end(work, i, &target)?;
                self.preprocessors.remove(&target.text);
            }
            kw::ERROR => {
                let msg_tokens = self.collect_pp_block(work, i)?;
                let msg = Self::join_text(&msg_tokens);
                return Err(self.err(kw_tok.span.clone(), msg));
            }
            kw::WARNING => {
                let msg_tokens = self.collect_pp_block(work, i)?;
                let msg = Self::join_text(&msg_tokens);
                self.warn(kw_tok.span.clone(), msg);
            }
            kw::GET_COUNTER => {
                let cur = self.counter;
                let span_tok = work[i.saturating_sub(1)].clone();
                let at = span_tok.span.start_off;
                self.expect_pp_end(work, i, &span_tok)?;
                out.push(self.synthetic_int(cur as isize, at));
                self.counter += 1;
            }
            kw::IS_DEFINED => {
                let first = self.token_at(work, *i)?.clone();
                let name_tok = if first.kind == TokenType::Lparen {
                    *i += 1;
                    let name = self.token_at(work, *i)?.clone();
                    if name.kind != TokenType::Ident {
                        return Err(self.err(
                            name.span.clone(),
                            "expected identifier in `(%is_defined(...)%)`",
                        ));
                    }
                    *i += 1;
                    let close = self.token_at(work, *i)?.clone();
                    if close.kind != TokenType::Rparen {
                        return Err(self.err(
                            close.span.clone(),
                            "expected `)` in `(%is_defined(...)%)`",
                        ));
                    }
                    *i += 1;
                    name
                } else {
                    *i += 1;
                    first
                };
                self.expect_pp_end(work, i, &name_tok)?;
                let defined = self.preprocessors.contains_key(&name_tok.text);
                let at = name_tok.span.start_off;
                out.push(self.synthetic_int(defined as isize, at));
            }
            kw::EVAL => {
                let expr = self.collect_pp_block(work, i)?;
                let expr: Vec<Token> = expr
                    .into_iter()
                    .filter(|t| t.kind != TokenType::NewLine)
                    .collect();
                let expanded = self.expand_tokens(expr)?;
                let val = self.eval(&expanded)?;
                let at = kw_tok.span.start_off;
                out.push(self.synthetic_int(val, at));
            }
            kw::REPEAT => {
                let body = self.collect_pp_block(work, i)?;
                let (times_tokens, body_tokens) = Self::split_first_comma(body);
                let expanded_times = self.expand_tokens(times_tokens)?;
                let times_val = self.eval(&expanded_times)?;
                if times_val < 0 {
                    return Err(self.err(kw_tok.span.clone(), "repeat count must be non-negative"));
                }
                for _ in 0..(times_val as usize) {
                    let expanded = self.expand_tokens(body_tokens.clone())?;
                    out.extend(expanded);
                }
            }
            kw::TO_STRING => {
                self.expect_pp_end(work, i, &kw_tok)?;
                while *i < work.len() && work[*i].kind == TokenType::NewLine {
                    *i += 1;
                }
                let next = self.token_at(work, *i)?.clone();
                if next.kind == TokenType::Eof {
                    return Err(self.err(
                        kw_tok.span.clone(),
                        "`(%to_string%)` requires a following token",
                    ));
                }
                *i += 1;
                let spelling = self.token_spelling(&next);
                let literal = format!("\"{}\"", escape_string_body(&spelling));
                out.push(self.synthetic_string(literal, next.span.start_off));
            }
            kw::CONCAT => {
                self.expect_pp_end(work, i, &kw_tok)?;
                let mut left = out.pop().ok_or_else(|| {
                    self.err(kw_tok.span.clone(), "`(%concat%)` requires a preceding token")
                })?;
                while *i < work.len() && work[*i].kind == TokenType::NewLine {
                    *i += 1;
                }
                let right = self.token_at(work, *i)?.clone();
                if right.kind == TokenType::Eof {
                    return Err(self.err(
                        kw_tok.span.clone(),
                        "`(%concat%)` requires a following token",
                    ));
                }
                *i += 1;
                let hide: HideSet = left.hide.intersection(&right.hide).cloned().collect();
                let merged = format!("{}{}", left.text, right.text);
                let at = left.span.start_off;
                left = Token {
                    kind: TokenType::Ident,
                    span: Span::synthetic(at),
                    text: merged,
                    hide,
                    file_id: 0,
                };
                out.push(left);
            }
            kw::INCLUDE => {
                self.handle_include(work, i, out, &kw_tok)?;
            }
            kw::INCLUDE_STRING => {
                self.handle_include_string(work, i, out, &kw_tok)?;
            }
            kw::INCLUDE_BYTES => {
                self.handle_include_bytes(work, i, out, &kw_tok)?;
            }
            kw::IF | kw::ELIF => {
                self.process_if(work, i, header_start)?;
            }
            kw::VA_ARGS | kw::VA_OPT => {
                return Err(self.err(
                    kw_tok.span.clone(),
                    format!("`({name}%)` is only valid inside a function macro body"),
                ));
            }
            other => {
                return Err(self.err(
                    kw_tok.span.clone(),
                    format!("unknown preprocessor keyword `{other}`"),
                ));
            }
        }
        Ok(())
    }

    fn join_text(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter(|t| t.kind != TokenType::NewLine)
            .map(|t| t.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn split_first_comma(tokens: Vec<Token>) -> (Vec<Token>, Vec<Token>) {
        let mut depth = 0usize;
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].kind {
                TokenType::Lparen | TokenType::Lbracket | TokenType::Lbrace => depth += 1,
                TokenType::Rparen | TokenType::Rbracket | TokenType::Rbrace => {
                    depth = depth.saturating_sub(1)
                }
                TokenType::Comma if depth == 0 => {
                    let left = tokens[..i].to_vec();
                    let right = tokens[i + 1..].to_vec();
                    return (left, right);
                }
                _ => {}
            }
            i += 1;
        }
        (tokens, Vec::new())
    }

    fn expect_pp_end(
        &self,
        tokens: &[Token],
        index: &mut usize,
        context: &Token,
    ) -> PpResult<()> {
        let tok = self.token_at(tokens, *index)?;
        if tok.kind != TokenType::PPEnd {
            return Err(self.err(
                context.span.clone(),
                format!("expected `%)`, found `{}`", tok.text),
            ));
        }
        *index += 1;
        Ok(())
    }

    fn parse_object_macro(
        &mut self,
        tokens: &[Token],
        index: &mut usize,
        kw_tok: &Token,
    ) -> PpResult<()> {
        let name_tok = self.token_at(tokens, *index)?.clone();
        if name_tok.kind != TokenType::Ident {
            return Err(self.err(name_tok.span.clone(), "expected macro name"));
        }
        *index += 1;
        let body = self.collect_pp_block(tokens, index)?;
        let def = PPDef {
            name_token: name_tok.clone(),
            params: Vec::new(),
            has_rest_args: false,
            is_function_like: false,
            body,
        };
        self.install_macro(&name_tok, def);
        let _ = kw_tok;
        Ok(())
    }

    fn install_macro(&mut self, name: &Token, def: PPDef) {
        match self.preprocessors.get(&name.text) {
            Some(old) if !same_definition(old, &def) => {
                self.warn(
                    name.span.clone(),
                    format!("macro `{}` redefined", name.text),
                );
                self.preprocessors.insert(name.text.clone(), def);
            }
            Some(_) => {}
            None => {
                self.preprocessors.insert(name.text.clone(), def);
            }
        }
    }

    fn parse_function_macro(
        &mut self,
        tokens: &[Token],
        index: &mut usize,
        kw_tok: &Token,
    ) -> PpResult<()> {
        let name_tok = self.token_at(tokens, *index)?.clone();
        if name_tok.kind != TokenType::Ident {
            return Err(self.err(name_tok.span.clone(), "expected macro name"));
        }
        *index += 1;

        let mut params: Vec<String> = Vec::new();
        let mut has_rest_args = false;

        if *index < tokens.len() && tokens[*index].kind == TokenType::Lparen {
            *index += 1; // (
            loop {
                let tok = self.token_at(tokens, *index)?;
                match tok.kind {
                    TokenType::Rparen => {
                        *index += 1;
                        break;
                    }
                    TokenType::DotDotDot => {
                        *index += 1;
                        has_rest_args = true;
                        // skip optional name after ...
                        if *index < tokens.len()
                            && tokens[*index].kind == TokenType::Ident
                            && tokens[*index].text != ")"
                        {
                            *index += 1;
                        }
                        let tok = self.token_at(tokens, *index)?;
                        if tok.kind != TokenType::Rparen {
                            return Err(self.err(tok.span.clone(), "expected `)`"));
                        }
                        *index += 1;
                        break;
                    }
                    TokenType::Ident => {
                        params.push(tok.text.clone());
                        *index += 1;
                    }
                    TokenType::Comma => {
                        *index += 1;
                    }
                    _ => {
                        return Err(self.err(
                            tok.span.clone(),
                            format!("unexpected token `{}` in parameter list", tok.text),
                        ));
                    }
                }
            }
        }

        if let Some(duplicate) = params
            .iter()
            .enumerate()
            .find_map(|(i, p)| params[..i].contains(p).then_some(p))
        {
            return Err(self.err(
                name_tok.span.clone(),
                format!("duplicate macro parameter `{duplicate}`"),
            ));
        }

        let body = self.collect_pp_block(tokens, index)?;
        let def = PPDef {
            name_token: name_tok.clone(),
            params,
            has_rest_args,
            is_function_like: true,
            body,
        };
        self.install_macro(&name_tok, def);
        let _ = kw_tok;
        Ok(())
    }

    // if / elif / else / endif

    fn process_if(
        &mut self,
        work: &mut Vec<Token>,
        i: &mut usize,
        header_start: usize,
    ) -> PpResult<()> {
        let cond_tokens = self.collect_pp_block(work, i)?;
        let cond_tokens: Vec<Token> = cond_tokens
            .into_iter()
            .filter(|t| t.kind != TokenType::NewLine)
            .collect();
        let cond_tokens = self.expand_tokens(cond_tokens)?;
        // In C any non-zero value is true (including negative values).
        let cond_true = self.eval(&cond_tokens)? != 0;

        const BRANCH_KWS: [&str; 4] = [kw::IF, kw::ELIF, kw::ELSE, kw::ENDIF];
        let (boundary, bkw) = Self::find_boundary(work, *i)?;

        if cond_true {
            Self::remove_dead_branch(work, boundary, &bkw)?;
            let _ = header_start;
            let _ = BRANCH_KWS;
            Ok(())
        } else {
            work.drain(header_start..boundary);
            *i = header_start;
            match bkw.as_str() {
                kw::ENDIF => {
                    if header_start + 3 > work.len() {
                        return Err(self.err(
                            Span {
                                start_off: 0,
                                end_off: 0,
                            },
                            "missing `(%endif%)`",
                        ));
                    }
                    work.drain(header_start..header_start + 3);
                    Ok(())
                }
                kw::ELIF => {
                    Ok(())
                }
                kw::ELSE => {
                    if header_start + 3 > work.len() {
                        return Err(self.err(
                            Span {
                                start_off: 0,
                                end_off: 0,
                            },
                            "missing `(%endif%)`",
                        ));
                    }
                    work.drain(header_start..header_start + 3);
                    *i = header_start;
                    Self::remove_trailing_endif(work, *i)
                }
                _ => unreachable!(),
            }
        }
    }

    fn find_boundary(work: &[Token], start: usize) -> PpResult<(usize, String)> {
        const KWS: [&str; 4] = [kw::IF, kw::ELIF, kw::ELSE, kw::ENDIF];
        let mut j = start;
        let mut depth = 0usize;
        while j < work.len() {
            if work[j].kind == TokenType::PPStart
                && j + 1 < work.len()
                && work[j + 1].kind == TokenType::Ident
            {
                let name = work[j + 1].text.as_str();
                if KWS.contains(&name) {
                    match name {
                        kw::IF => depth += 1,
                        kw::ENDIF => {
                            if depth == 0 {
                                return Ok((j, name.to_string()));
                            }
                            depth -= 1;
                        }
                        kw::ELIF if depth == 0 => return Ok((j, name.to_string())),
                        kw::ELSE if depth == 0 => return Ok((j, name.to_string())),
                        _ => {}
                    }
                }
            }
            j += 1;
        }
        Err(PpError::new(
            Span {
                start_off: 0,
                end_off: 0,
            },
            "missing `(%endif%)`",
        ))
    }

    fn remove_dead_branch(work: &mut Vec<Token>, boundary: usize, bkw: &str) -> PpResult<()> {
        if bkw == kw::ENDIF {
            if boundary + 3 > work.len() {
                return Err(PpError::new(
                    Span {
                        start_off: 0,
                        end_off: 0,
                    },
                    "missing `(%endif%)`",
                ));
            }
            work.drain(boundary..boundary + 3);
            return Ok(());
        }
        let mut j = boundary;
        let mut depth = 0usize;
        while j < work.len() {
            if work[j].kind == TokenType::PPStart
                && j + 1 < work.len()
                && work[j + 1].kind == TokenType::Ident
            {
                let name = work[j + 1].text.as_str();
                match name {
                    kw::IF => depth += 1,
                    kw::ENDIF => {
                        if depth == 0 {
                            if j + 3 > work.len() {
                                return Err(PpError::new(
                                    Span {
                                        start_off: 0,
                                        end_off: 0,
                                    },
                                    "missing `(%endif%)`",
                                ));
                            }
                            work.drain(boundary..j + 3);
                            return Ok(());
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        Err(PpError::new(
            Span {
                start_off: 0,
                end_off: 0,
            },
            "missing `(%endif%)`",
        ))
    }

    fn remove_trailing_endif(work: &mut Vec<Token>, start: usize) -> PpResult<()> {
        let mut j = start;
        let mut depth = 0usize;
        while j < work.len() {
            if work[j].kind == TokenType::PPStart
                && j + 1 < work.len()
                && work[j + 1].kind == TokenType::Ident
            {
                let name = work[j + 1].text.as_str();
                match name {
                    kw::IF => depth += 1,
                    kw::ENDIF => {
                        if depth == 0 {
                            if j + 3 > work.len() {
                                return Err(PpError::new(
                                    Span {
                                        start_off: 0,
                                        end_off: 0,
                                    },
                                    "missing `(%endif%)`",
                                ));
                            }
                            work.drain(j..j + 3);
                            return Ok(());
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            j += 1;
        }
        Err(PpError::new(
            Span {
                start_off: 0,
                end_off: 0,
            },
            "missing `(%endif%)`",
        ))
    }

    fn pre_definitions(&mut self) {
        for (name, value) in PRE_DEFINES {
            self.preprocessors
                .entry(name.to_string())
                .or_insert(PPDef {
                    name_token: Token {
                        kind: TokenType::Ident,
                        span: Span {
                            start_off: 0,
                            end_off: 0,
                        },
                        text: name.to_string(),
                        hide: HideSet::new(),
                        file_id: 0,
                    },
                    params: Vec::new(),
                    has_rest_args: false,
                    is_function_like: false,
                    body: vec![Token {
                        kind: TokenType::Int,
                        span: Span {
                            start_off: 0,
                            end_off: 0,
                        },
                        text: value.to_string(),
                        hide: HideSet::new(),
                        file_id: 0,
                    }],
                });
        }
    }

    pub fn pass(mut self, tokens: Vec<Token>) -> PreprocessOutput {
        self.pre_definitions();
        let warnings_start = self.warnings.len();
        match self.expand_tokens(tokens) {
            Ok(new_tokens) => PreprocessOutput {
                tokens: new_tokens,
                errors: Vec::new(),
                warnings: self.warnings.split_off(warnings_start),
                included_sources: self.included_sources,
            },
            Err(e) => PreprocessOutput {
                tokens: Vec::new(),
                errors: vec![e],
                warnings: self.warnings.split_off(warnings_start),
                included_sources: self.included_sources,
            },
        }
    }
}

pub struct PreprocessOutput {
    pub tokens: Vec<Token>,
    pub errors: Vec<PpError>,
    pub warnings: Vec<String>,
    pub included_sources: Vec<String>,
}


fn parse_int_literal(text: &str) -> Option<(u64, bool)> {
    let t = text.replace('_', "");
    let (digits, radix) = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (hex, 16)
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        (bin, 2)
    } else if let Some(oct) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        (oct, 8)
    } else if t.len() > 1 && t.starts_with('0') && t.chars().all(|c| ('0'..='7').contains(&c)) {
        (t.as_str(), 8)
    } else {
        (t.as_str(), 10)
    };
    if digits.is_empty() {
        return None;
    }
    let value = u128::from_str_radix(digits, radix).ok()?;
    Some((value as u64, value > i64::MAX as u128))
}

fn is_marker(body: &[Token], at: usize, keyword: &str) -> bool {
    at + 2 < body.len()
        && body[at].kind == TokenType::PPStart
        && body[at + 1].kind == TokenType::Ident
        && body[at + 1].text == keyword
        && body[at + 2].kind == TokenType::PPEnd
}

fn param_index(def: &PPDef, tok: &Token) -> Option<usize> {
    if tok.kind == TokenType::Ident {
        def.params.iter().position(|p| p == &tok.text)
    } else {
        None
    }
}

fn classify_pasted(text: &str, left_kind: Option<TokenType>) -> TokenType {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_digit()) {
        return TokenType::Int;
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 1 && chars[0] == '0' && matches!(chars[1], 'x' | 'X' | 'b' | 'B' | 'o' | 'O') {
        return TokenType::RadixInt;
    }
    left_kind.unwrap_or(TokenType::Ident)
}


fn same_definition(a: &PPDef, b: &PPDef) -> bool {
    a.params == b.params
        && a.has_rest_args == b.has_rest_args
        && a.is_function_like == b.is_function_like
        && a.body.len() == b.body.len()
        && a.body
            .iter()
            .zip(&b.body)
            .all(|(x, y)| x.kind == y.kind && x.text == y.text)
}

fn escape_string_body(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn escape_bytes_body(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0 => out.push_str("\\0"),
            b if b < 0x20 || b == 0x7f => {
                out.push_str(&format!("\\x{b:02x}"));
            }
            b if b.is_ascii_graphic() || b == b' ' => out.push(b as char),
            b => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use std::path::PathBuf;

    fn run(src: &str) -> PpResult<String> {
        let lex = tokenize(src);
        assert!(lex.errors.is_empty(), "lex errors: {:?}", lex.errors);
        let pp = Preprocessor::new(src.to_string(), "<test>".to_string());
        let out = pp.pass(lex.tokens);
        assert!(out.errors.is_empty(), "pp errors: {:?}", out.errors);
        Ok(out
            .tokens
            .iter()
            .filter(|t| t.kind != TokenType::Eof)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join(" "))
    }

    fn run_err(src: &str) -> String {
        let lex = tokenize(src);
        let pp = Preprocessor::new(src.to_string(), "<test>".to_string());
        let out = pp.pass(lex.tokens);
        assert!(!out.errors.is_empty(), "expected error");
        out.errors[0].message.clone()
    }

    #[test]
    fn passthrough() {
        let out = run("int x = 1;\n").unwrap();
        assert!(out.contains("int x = 1 ;"));
    }

    #[test]
    fn object_macro_define_and_use() {
        let out = run("(%object_macro MM 42%)\nMM\n").unwrap();
        assert!(out.contains('4'), "got: {out}");
        assert!(out.contains('2'), "got: {out}");
    }

    #[test]
    fn function_macro_call() {
        let out = run("(%function_macro KK(a) a + a%)\nKK(21)\n").unwrap();
        assert!(out.contains("21"), "got: {out}");
        assert!(out.contains('+'), "got: {out}");
        assert!(out.contains("21 + 21"), "got: {out}");
    }

    #[test]
    fn if_true_takes_branch() {
        let out = run("(%if 1%)\nAAA\n(%else%)\nBBB\n(%endif%)\n").unwrap();
        assert!(out.contains("AAA"), "got: {out}");
        assert!(!out.contains("BBB"), "got: {out}");
    }

    #[test]
    fn if_false_takes_else() {
        let out = run("(%if 0%)\nAAA\n(%else%)\nBBB\n(%endif%)\n").unwrap();
        assert!(!out.contains("AAA"), "got: {out}");
        assert!(out.contains("BBB"), "got: {out}");
    }

    #[test]
    fn elif_chain() {
        let src = "(%if 0%)\nAAA\n(%elif 1%)\nBBB\n(%elif 0%)\nCCC\n(%else%)\nDDD\n(%endif%)\n";
        let out = run(src).unwrap();
        assert!(!out.contains("AAA"), "got: {out}");
        assert!(out.contains("BBB"), "got: {out}");
        assert!(!out.contains("CCC"), "got: {out}");
        assert!(!out.contains("DDD"), "got: {out}");
    }

    #[test]
    fn elif_last_else() {
        let src = "(%if 0%)\nAAA\n(%elif 0%)\nBBB\n(%else%)\nCCC\n(%endif%)\n";
        let out = run(src).unwrap();
        assert!(out.contains("CCC"), "got: {out}");
        assert!(!out.contains("AAA"), "got: {out}");
        assert!(!out.contains("BBB"), "got: {out}");
    }

    #[test]
    fn nested_if() {
        let src = "(%if 1%)\n  (%if 0%)\n    NO\n  (%else%)\n    YES\n  (%endif%)\n(%endif%)\n";
        let out = run(src).unwrap();
        assert!(out.contains("YES"), "got: {out}");
        assert!(!out.contains("NO"), "got: {out}");
    }

    #[test]
    fn eval_expression() {
        let out = run("(%eval 1 + 2 * 3%)\n").unwrap();
        assert!(out.contains('7'), "got: {out}");
    }

    #[test]
    fn eval_parens() {
        let out = run("(%eval (1 + 2) * 3%)\n").unwrap();
        assert!(out.contains('9'), "got: {out}");
    }

    #[test]
    fn comparison_in_if() {
        let out = run("(%if 2 > 1%)\nOK\n(%endif%)\n").unwrap();
        assert!(out.contains("OK"), "got: {out}");
    }

    #[test]
    fn get_counter() {
        let out = run("(%get_counter%)\n(%get_counter%)\n").unwrap();
        assert!(out.contains('0'), "got: {out}");
        assert!(out.contains('1'), "got: {out}");
    }

    #[test]
    fn repeat() {
        let out = run("(%repeat 3, XYZ%)\n").unwrap();
        let count = out.matches("XYZ").count();
        assert_eq!(count, 3, "got: {out}");
    }

    #[test]
    fn is_defined() {
        let out = run("(%object_macro A 1%)\n(%is_defined A%)\n(%is_defined B%)\n").unwrap();
        assert!(out.contains('1'), "got: {out}");
        assert!(out.contains('0'), "got: {out}");
    }

    #[test]
    fn undefine() {
        let out = run("(%object_macro A 1%)\n(%undefine A%)\nA\n").unwrap();
        assert!(out.contains('A'), "got: {out}");
        assert!(!out.contains('1'), "got: {out}");
    }

    #[test]
    fn to_string_prefix() {
        let out = run("(%to_string%) hello\n").unwrap();
        assert!(out.contains("hello"), "got: {out}");
    }

    #[test]
    fn concat_infix() {
        let out = run("XX (%concat%) YY\n").unwrap();
        assert!(out.contains("XXYY"), "got: {out}");
    }

    #[test]
    fn error_aborts() {
        let msg = run_err("(%error \"boom\"%)\n");
        assert!(msg.contains("boom"), "got: {msg}");
    }

    #[test]
    fn warning_continues() {
        let lex = tokenize("(%warning \"careful\"%)\nOK\n");
        let pp = Preprocessor::new("x".into(), "<t>".into());
        let out = pp.pass(lex.tokens);
        assert!(out.errors.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("careful"));
    }

    #[test]
    fn va_args_and_va_opt() {
        let src = "(%function_macro M(a, ...) a (%va_args%) (%va_opt(+)%)\n%)\nM(1, 2, 3)\n";
        let out = run(src);
        match out {
            Ok(s) => {
                assert!(s.contains('1'), "got: {s}");
                assert!(s.contains('2'), "got: {s}");
            }
            Err(e) => panic!("error: {}", e.message),
        }
    }

    #[test]
    fn predef_version() {
        let out = run("(%if version == 1%)\nOK\n(%endif%)\n").unwrap();
        assert!(out.contains("OK"), "got: {out}");
    }

    #[test]
    fn if_missing_endif_errors() {
        let msg = run_err("(%if 1%)\nAAA\n");
        assert!(msg.contains("endif"), "got: {msg}");
    }


    #[test]
    fn self_referential_object_macro() {
        let out = run("(%object_macro FOO FOO%)\nFOO\n").unwrap();
        assert!(out.contains("FOO"), "got: {out}");
    }

    #[test]
    fn mutual_object_recursion() {
        let out = run("(%object_macro A B%)\n(%object_macro B A%)\nA\n").unwrap();
        assert!(out.contains("A") || out.contains("B"), "got: {out}");
    }

    #[test]
    fn function_body_self_reference() {
        let out = run("(%function_macro f(x) x f%)\nf(1)(2)\n").unwrap();
        assert!(out.contains("1 f ( 2 )") || out.contains("1 f(2)"), "got: {out}");
    }

    #[test]
    fn function_like_without_lparen_is_ident() {
        let out = run("(%function_macro f(x) x%)\nf\n").unwrap();
        assert!(out.contains("f"), "got: {out}");
        // must not error / must not treat as object macro
        assert!(!out.contains("(") || out.trim() == "f", "got: {out}");
    }

    #[test]
    fn function_like_zero_arg_call() {
        let out = run("(%function_macro QQ() QQ%)\nQQ()()()\n").unwrap();
        assert!(out.contains("QQ"), "got: {out}");
    }

    #[test]
    fn arg_prescan_expands_macros() {
        let out = run("(%function_macro id(x) x%)\n(%function_macro f() 2%)\nid(f())\n").unwrap();
        assert!(out.contains('2'), "got: {out}");
    }

    #[test]
    fn object_macro_body_paren_from_rest() {
        // g expands to `f`, then `(` from source forms f(1)
        let out = run("(%object_macro g f%)\n(%function_macro f(x) x%)\ng(1)\n").unwrap();
        assert!(out.contains('1'), "got: {out}");
    }

    #[test]
    fn param_substituted_every_occurrence() {
        let out = run("(%function_macro dup(a) a + a%)\ndup(3)\n").unwrap();
        assert!(out.contains("3 + 3"), "got: {out}");
    }

    #[test]
    fn blue_paint_via_arg_prescan() {
        // Classic: two expands in arg prescan and is painted; not re-expanded in body.
        let src = concat!(
            "(%object_macro two one, two%)\n",
            "(%function_macro a(x) b(x)%)\n",
            "(%function_macro b(x, y) x, y%)\n",
            "a(two)\n",
        );
        let out = run(src).unwrap();
        // `two` in the arg expands once to `one, two`, then the inner `two` stays.
        assert!(out.contains("one"), "got: {out}");
        assert!(out.contains("two"), "got: {out}");
    }

    #[test]
    fn expansion_result_not_treated_as_directive() {
        // Macro body contains (%if ...%); after expansion these must NOT run as directives.
        let src = "(%object_macro M (%if 0%) AAA (%endif%)%)\nM\n";
        let out = run(src).unwrap();
        // if wrongly processed, AAA would be dropped (cond false → skip to endif)
        assert!(out.contains("AAA"), "got: {out}");
    }

    #[test]
    fn object_macro_not_recursive_after_expand() {
        let out = run("(%object_macro A A%)\nA\n").unwrap();
        assert_eq!(out.matches('A').count(), 1, "got: {out}");
    }

    #[test]
    fn counter_still_works_with_hide_sets() {
        let out = run("(%get_counter%)(%get_counter%)(%get_counter%)\n").unwrap();
        assert!(out.contains('0') && out.contains('1') && out.contains('2'), "got: {out}");
    }

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!(
            "lfpp_test_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            n
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn run_file(main: &Path, src: &str) -> PpResult<String> {
        std::fs::write(main, src).unwrap();
        let text = std::fs::read_to_string(main).unwrap();
        let lex = tokenize(&text);
        assert!(lex.errors.is_empty(), "lex errors: {:?}", lex.errors);
        let pp = Preprocessor::new(text.clone(), main.display().to_string());
        let out = pp.pass(lex.tokens);
        if let Some(e) = out.errors.first() {
            return Err(e.clone());
        }
        Ok(out
            .tokens
            .iter()
            .filter(|t| t.kind != TokenType::Eof)
            .map(|t| t.text.clone())
            .collect::<Vec<_>>()
            .join(" "))
    }

    #[test]
    fn include_basic_and_macro_shared() {
        let d = temp_dir();
        std::fs::write(d.join("inc.h"), "(%object_macro MM 42%)\nMM\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include \"inc.h\"%)\nMM\n").unwrap();
        assert!(out.contains("42"), "got: {out}");
        assert!(!out.contains("MM"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_if_inside_included_file() {
        let d = temp_dir();
        std::fs::write(d.join("cond.h"), "(%if 1%)\nAAA\n(%else%)\nBBB\n(%endif%)\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include \"cond.h\"%)\n").unwrap();
        assert!(out.contains("AAA"), "got: {out}");
        assert!(!out.contains("BBB"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_path_macro_expanded() {
        let d = temp_dir();
        std::fs::write(d.join("target.h"), "INCLUDED_OK\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(
            &main,
            "(%object_macro P \"target.h\"%)\n(%include P%)\n",
        )
        .unwrap();
        assert!(out.contains("INCLUDED_OK"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_relative_to_current_file() {
        let d = temp_dir();
        let sub = d.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("leaf.h"), "SUB_OK\n").unwrap();
        std::fs::write(d.join("root.h"), "(%include \"sub/leaf.h\"%)\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include \"root.h\"%)\n").unwrap();
        assert!(out.contains("SUB_OK"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_recursive_errors() {
        let d = temp_dir();
        std::fs::write(d.join("a.h"), "(%include \"b.h\"%)\n").unwrap();
        std::fs::write(d.join("b.h"), "(%include \"a.h\"%)\n").unwrap();
        let main = d.join("main.pp");
        let err = run_file(&main, "(%include \"a.h\"%)\n").unwrap_err();
        assert!(err.message.contains("recursive"), "got: {}", err.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_missing_file_errors() {
        let d = temp_dir();
        let main = d.join("main.pp");
        let err = run_file(&main, "(%include \"nope.h\"%)\n").unwrap_err();
        assert!(err.message.contains("cannot read"), "got: {}", err.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_non_string_path_errors() {
        let d = temp_dir();
        let main = d.join("main.pp");
        let err = run_file(&main, "(%include 123%)\n").unwrap_err();
        assert!(
            err.message.contains("expected string literal path"),
            "got: {}",
            err.message
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_nested_error_has_origin() {
        let d = temp_dir();
        std::fs::write(d.join("bad.h"), "(%error \"boom\"%)\n").unwrap();
        let main = d.join("main.pp");
        let err = run_file(&main, "(%include \"bad.h\"%)\n").unwrap_err();
        let origin = err.origin.as_ref().expect("origin expected");
        assert!(origin.0.contains("bad.h"), "got: {origin:?}");
        assert!(err.message.contains("boom"), "got: {}", err.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_string_embeds_content() {
        let d = temp_dir();
        std::fs::write(d.join("data.txt"), "line1\n\"quoted\"\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include_string \"data.txt\"%)\n").unwrap();
        assert!(out.contains("line1"), "got: {out}");
        assert!(out.contains("\\n"), "got: {out}");
        assert!(out.contains("\\\"quoted\\\""), "got: {out}");
        assert!(out.contains('"'), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_bytes_embeds_binary() {
        let d = temp_dir();
        std::fs::write(d.join("bin.dat"), [0x00u8, 0xff, 0x41, 0x0a]).unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include_bytes \"bin.dat\"%)\n").unwrap();
        assert!(out.contains("b\""), "got: {out}");
        assert!(out.contains("\\0"), "got: {out}");
        assert!(out.contains("\\xff"), "got: {out}");
        assert!(out.contains('A'), "got: {out}");
        assert!(out.contains("\\n"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_string_on_non_utf8_errors() {
        let d = temp_dir();
        std::fs::write(d.join("bad.bin"), [0xff, 0xfe]).unwrap();
        let main = d.join("main.pp");
        let err = run_file(&main, "(%include_string \"bad.bin\"%)\n").unwrap_err();
        assert!(err.message.contains("cannot read"), "got: {}", err.message);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_bytes_on_non_utf8_ok() {
        let d = temp_dir();
        std::fs::write(d.join("bad.bin"), [0xff, 0xfe]).unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include_bytes \"bad.bin\"%)\n").unwrap();
        assert!(out.contains("b\""), "got: {out}");
        assert!(out.contains("\\xff"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn include_does_not_recurse_string() {
        let d = temp_dir();
        std::fs::write(d.join("a.txt"), "(%error \"should not run\"%)\n").unwrap();
        let main = d.join("main.pp");
        let out = run_file(&main, "(%include_string \"a.txt\"%)\n").unwrap();
        assert!(out.contains("error"), "got: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn to_string_produces_quoted_literal() {
        let out = run("(%to_string%) hello\n").unwrap();
        assert_eq!(out.trim(), format!("\"{}\"", "hello"));
        let out = run("(%to_string%) \"hi\"\n").unwrap();
        let text = out.trim();
        assert!(text.starts_with('"') && text.ends_with('"'), "got: {out}");
        assert!(text.contains("hi"), "got: {out}");
    }

    #[test]
    fn octal_literals_in_if() {
        assert_eq!(run("(%eval 010%)\n").unwrap().trim(), "8");
    }

    #[test]
    fn bitwise_shift_and_not_in_if() {
        assert_eq!(run("(%eval (1<<3)%)\n").unwrap().trim(), "8");
        assert_eq!(run("(%eval (1 & 0)%)\n").unwrap().trim(), "0");
        assert_eq!(run("(%eval ~0%)\n").unwrap().trim(), "-1");
    }

    #[test]
    fn short_circuit_avoids_division_by_zero() {
        let out = run("(%if 0 && 1/0%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('B'), "got: {out}");
        let out = run("(%if 1 || 1/0%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('A'), "got: {out}");
    }

    #[test]
    fn logical_precedence_and_unary_not() {
        let out = run("(%if 1 || 0 && 0%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('A'), "got: {out}");
        assert_eq!(run("(%eval !0 + 1%)\n").unwrap().trim(), "2");
    }

    #[test]
    fn chained_comparison_is_left_associative() {
        assert_eq!(run("(%eval 3 > 2 > 1%)\n").unwrap().trim(), "0");
    }

    #[test]
    fn undefined_identifier_is_zero() {
        let out = run("(%if FOO%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('B'), "got: {out}");
    }

    #[test]
    fn char_constant_in_if() {
        let out = run("(%if 'A' == 65%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('A'), "got: {out}");
    }

    #[test]
    fn ternary_and_comma_operator() {
        assert_eq!(run("(%eval 0 ? 0 : 1%)\n").unwrap().trim(), "1");
        assert_eq!(run("(%eval (1,2)%)\n").unwrap().trim(), "2");
    }

    #[test]
    fn trailing_tokens_are_rejected() {
        let msg = run_err("(%if 1 2%)\nA\n(%endif%)\n");
        assert!(msg.contains("unexpected token"), "got: {msg}");
    }

    #[test]
    fn function_call_across_newline() {
        let out = run("(%function_macro f(x) x%)\nf\n(1)\n").unwrap();
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn empty_call_for_one_parameter() {
        let out = run("(%function_macro f(x) [x]%)\nf()\n").unwrap();
        assert_eq!(out.trim().replace(' ', ""), "[]");
    }

    #[test]
    fn duplicate_parameter_errors() {
        let msg = run_err("(%function_macro f(a,a) a%)\nf(1,2)\n");
        assert!(msg.contains("duplicate"), "got: {msg}");
    }

    #[test]
    fn redefinition_uses_latest_definition() {
        let out = run("(%object_macro A 1%)\n(%object_macro A 2%)\nA\n").unwrap();
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn line_splice_joins_lines() {
        let mut src = String::from("(%object_macro A 1 ");
        src.push('\\');
        src.push('\n');
        src.push_str(" + 2%)\nA\n");
        let out = run(&src).unwrap();
        assert_eq!(out.trim(), "1 + 2");
    }

    #[test]
    fn unsigned_comparison_follows_c() {
        let out = run("(%if -1 < 1u%)\nA\n(%else%)\nB\n(%endif%)\n").unwrap();
        assert!(out.contains('B'), "got: {out}");
    }

    #[test]
    fn large_unsigned_literal_compares_equal() {
        let src = "(%if 0xFFFFFFFFFFFFFFFF == 18446744073709551615ULL%)\nA\n(%else%)\nB\n(%endif%)\n";
        let out = run(src).unwrap();
        assert!(out.contains('A'), "got: {out}");
    }

    #[test]
    fn is_defined_with_parentheses() {
        let out = run("(%object_macro A 1%)\n(%is_defined(A)%)\n(%is_defined(B)%)\n").unwrap();
        assert!(out.contains('1'), "got: {out}");
        assert!(out.contains('0'), "got: {out}");
    }

    #[test]
    fn paste_in_macro_body() {
        let out = run("(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(foo,bar)\n").unwrap();
        assert_eq!(out.trim(), "foobar");
    }

    #[test]
    fn paste_uses_unexpanded_arguments() {
        let src = "(%function_macro CAT(a,b) a (%concat%) b%)\n(%object_macro A foo%)\nCAT(A,bar)\n";
        let out = run(src).unwrap();
        assert_eq!(out.trim(), "Abar", "got: {out}");
    }

    #[test]
    fn paste_rescans_result() {
        let src = "(%function_macro CAT(a,b) a (%concat%) b%)\n(%object_macro xy 9%)\nCAT(x,y)\n";
        let out = run(src).unwrap();
        assert_eq!(out.trim(), "9", "got: {out}");
    }

    #[test]
    fn paste_placemarkers() {
        assert_eq!(run("(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(,x)\n").unwrap().trim(), "x");
        assert_eq!(run("(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(x,)\n").unwrap().trim(), "x");
        assert!(run("(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(,)\n").unwrap().trim().is_empty());
    }

    #[test]
    fn paste_in_object_macro_body() {
        let out = run("(%object_macro GLUE foo (%concat%) bar%)\nGLUE\n").unwrap();
        assert_eq!(out.trim(), "foobar");
    }

    #[test]
    fn paste_integer_tokens() {
        let out = run("(%function_macro CAT(a,b) a (%concat%) b%)\nCAT(1,2)\n").unwrap();
        assert_eq!(out.trim(), "12");
    }

    #[test]
    fn stringize_whole_argument() {
        let out = run("(%function_macro STR(x) (%to_string%) x%)\nSTR(a b c)\n").unwrap();
        assert_eq!(out.trim(), format!("\"{}\"", "a b c"));
    }

    #[test]
    fn stringize_keeps_adjacent_tokens_glued() {
        let out = run("(%function_macro STR(x) (%to_string%) x%)\nSTR(a+b)\n").unwrap();
        assert_eq!(out.trim(), format!("\"{}\"", "a+b"));
    }

    #[test]
    fn stringize_string_literal_argument() {
        let out = run("(%function_macro STR(x) (%to_string%) x%)\nSTR(\"hi\")\n").unwrap();
        let text = out.trim();
        assert!(text.contains("hi"), "got: {out}");
        assert_eq!(text.matches('\\').count(), 2, "got: {out}");
    }

    #[test]
    fn stringize_empty_argument() {
        let out = run("(%function_macro STR(x) (%to_string%) x%)\nSTR()\n").unwrap();
        assert_eq!(out.trim(), "\"\"");
    }

    #[test]
    fn stringize_variadic() {
        let out = run("(%function_macro STR(...) (%to_string%) (%va_args%)%)\nSTR(a,b)\n").unwrap();
        assert_eq!(out.trim(), format!("\"{}\"", "a,b"));
    }

    #[test]
    fn paste_empty_variadic() {
        let out = run("(%function_macro CAT(a,...) a (%concat%) (%va_args%)%)\nCAT(x,)\n").unwrap();
        assert_eq!(out.trim(), "x");
    }
}
