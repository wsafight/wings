use std::env;
use std::fs;
use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "Usage: wings-compile check <file.wings>\n\nChecks the prototype language subset; does not generate machine code."
        );
        return ExitCode::SUCCESS;
    }
    if arguments.len() != 2 || arguments[0] != "check" {
        eprintln!("Usage: wings-compile check <file.wings>");
        return ExitCode::from(2);
    }
    let path = &arguments[1];
    let text = match read_source(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{path}: {error}");
            return ExitCode::from(2);
        }
    };
    let analysis = wings_compile::check(&text);
    if analysis.diagnostics.is_empty() {
        println!("{path}: check passed");
        ExitCode::SUCCESS
    } else {
        eprint!(
            "{}",
            wings_compile::render_diagnostics(path, &text, &analysis)
        );
        ExitCode::FAILURE
    }
}

fn read_source(path: &str) -> std::io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    Ok(text)
}
