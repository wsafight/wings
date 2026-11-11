use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "Usage: wings-lsp\n\nStarts the language server over stdio (UTF-16 positions, full document sync)."
        );
        return ExitCode::SUCCESS;
    }
    if !arguments.is_empty() {
        eprintln!("Usage: wings-lsp");
        return ExitCode::from(2);
    }
    match wings_lsp::run(io::stdin().lock(), io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("wings-lsp: {error}");
            ExitCode::FAILURE
        }
    }
}
