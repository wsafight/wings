use std::env;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        println!(
            "Usage: wings-compile check <file.wings>\n       wings-compile build <file.wings> -o <new-directory>\n\ncheck only analyzes source; build emits a trusted host-native scalar AOT prototype."
        );
        return ExitCode::SUCCESS;
    }
    let is_check = arguments.len() == 2 && arguments[0] == "check";
    let is_build = arguments.len() == 4 && arguments[0] == "build" && arguments[2] == "-o";
    if !is_check && !is_build {
        eprintln!(
            "Usage: wings-compile check <file.wings> | build <file.wings> -o <new-directory>"
        );
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
        if is_build {
            if let Err(error) = wings_compile::build(&text, Path::new(&arguments[3])) {
                eprintln!("{path}: build failed: {error}");
                return ExitCode::FAILURE;
            }
            println!("{}: scalar AOT artifact built", arguments[3]);
        } else {
            println!("{path}: check passed");
        }
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
