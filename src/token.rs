use std::collections::BTreeSet;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum TokenType {
    Ident,
    Int,
    RadixInt,
    Float,
    String,
    Byte,
    Char,
    ENotationFloat,

    Plus,      // +
    Minus,     // -
    Star,      // *
    Slash,     // /
    Percent,   // %
    Amp,       // &
    Pipe,      // |
    Caret,     // ^
    Not,       // !
    Tilde,     // ~
    Eq,        // =
    EqEq,      // ==
    Ne,        // !=
    Lt,        // <
    Gt,        // >
    Le,        // <=
    Ge,        // >=
    And,       // &&
    Or,        // ||
    Shl,       // <<
    Shr,       // >>
    PlusEq,    // +=
    MinusEq,   // -=
    StarEq,    // *=
    SlashEq,   // /=
    PercentEq, // %=
    AmpEq,     // &=
    PipeEq,    // |=
    CaretEq,   // ^=
    ShlEq,     // <<=
    ShrEq,     // >>=
    Arrow,     // ->
    FatArrow,  // =>
    Dot,       // .
    DotDot,    // ..
    DotDotDot, // ...
    Lparen,    // (
    Rparen,    // )
    Lbrace,    // {
    Rbrace,    // }
    Lbracket,  // [
    Rbracket,  // ]
    Comma,     // ,
    Colon,     // :
    Semicolon, // ;
    Hash,      // #
    At,        // @
    Question,  // ?
    Underline, // _

    PPStart, // (%
    PPEnd,   // %)

    Eof,
    NewLine,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenType,
    pub span: Span,
    pub text: String,
    pub hide: BTreeSet<String>,
    pub file_id: u32,
}

impl Token {
    pub fn is_synthetic(&self) -> bool {
        self.span.start_off == self.span.end_off
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Span {
    pub start_off: usize,
    pub end_off: usize,
}

#[allow(dead_code)]
impl Span {
    pub fn len(&self) -> usize {
        self.end_off - self.start_off
    }

    pub fn is_empty(&self) -> bool {
        self.start_off == self.end_off
    }

    pub fn synthetic(at: usize) -> Self {
        Span {
            start_off: at,
            end_off: at,
        }
    }
}
