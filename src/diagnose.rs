use crate::token::Span;
use std::io::IsTerminal;

#[derive(Debug, Clone)]
pub struct PpError {
    pub span: Span,
    pub message: String,
    pub origin: Option<(String, usize, usize)>,
}

impl PpError {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        PpError {
            span,
            message: message.into(),
            origin: None,
        }
    }

    pub fn with_origin(mut self, path: &str, source: &str) -> Self {
        if self.origin.is_none() {
            let (line, col) = line_col(source, self.span.start_off);
            self.origin = Some((path.to_string(), line, col));
        }
        self
    }
}

pub type PpResult<T> = Result<T, PpError>;

const BOLD_RED: &str = "\x1b[1;31m";
const BOLD_YELLOW: &str = "\x1b[1;33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal()
}

fn bold_label(word: &str, color: &str) -> String {
    if color_enabled() {
        format!("{color}{word}{RESET}")
    } else {
        word.to_string()
    }
}

pub fn bold_error() -> String {
    bold_label("error", BOLD_RED)
}

pub fn bold_warning() -> String {
    bold_label("warning", BOLD_YELLOW)
}

pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut line_start = 0usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = i + 1;
        }
    }
    let col = source[line_start..offset.min(source.len())].chars().count() + 1;
    (line, col)
}

pub fn format_error(path: &str, source: &str, err: &PpError) -> String {
    let (err_path, line, col) = match &err.origin {
        Some((p, l, c)) => (p.as_str(), *l, *c),
        None => {
            let (l, c) = line_col(source, err.span.start_off);
            (path, l, c)
        }
    };
    format!(
        "{err_path}:{line}:{col}: {}: {}",
        bold_error(),
        err.message
    )
}

pub fn format_warning(path: &str, source: &str, span: &Span, msg: &str) -> String {
    let (line, col) = line_col(source, span.start_off);
    format!(
        "{path}:{line}:{col}: {}: {msg}",
        bold_warning()
    )
}

pub fn format_running(cmd: &str) -> String {
    if color_enabled() {
        format!("     {BOLD}Running{RESET} `{cmd}`")
    } else {
        format!("     Running `{cmd}`")
    }
}
