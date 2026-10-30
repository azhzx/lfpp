mod diagnose;
mod lexer;
mod pp_keywords;
mod preprocessor;
mod render;
mod token;

use std::process::ExitCode;

use diagnose::{bold_error, format_error, format_running};
use preprocessor::Preprocessor;

fn usage() -> ! {
    eprintln!("usage: lfpp <file>");
    std::process::exit(2);
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 1 {
        usage();
    }
    let path = &args[0];

    if std::env::var_os("CARGO").is_none() {
        let argv0 = std::env::args().next().unwrap_or_else(|| "lfpp".into());
        eprintln!("{}", format_running(&format!("{argv0} {path}")));
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}: cannot read `{path}`: {e}", bold_error());
            return ExitCode::FAILURE;
        }
    };

    // lex
    let lex_out = lexer::tokenize(&source);
    for w in &lex_out.errors {
        eprintln!("{}", format_error(path, &source, w));
    }
    if !lex_out.errors.is_empty() {
        return ExitCode::FAILURE;
    }

    // preprocess
    let pp = Preprocessor::new(source.clone(), path.clone());
    let out = pp.pass(lex_out.tokens);

    for w in &out.warnings {
        eprintln!("{w}");
    }
    if let Some(err) = out.errors.first() {
        eprintln!("{}", format_error(path, &source, err));
        return ExitCode::FAILURE;
    }

    // render
    let text = render::render(&source, &out.included_sources, &out.tokens);
    print!("{text}");
    ExitCode::SUCCESS
}
