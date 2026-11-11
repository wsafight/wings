mod lower;

use std::path::Path;

use wings_sema::{Analysis, analyze};
use wings_source::{SourceFile, SourceId};

pub fn check(text: &str) -> Analysis {
    analyze(text)
}

pub fn build(text: &str, directory: &Path) -> Result<(), String> {
    wings_codegen::build(&lower::lower(text)?, directory)
}

pub fn render_diagnostics(path: &str, text: &str, analysis: &Analysis) -> String {
    let source = SourceFile::new(SourceId(0), text);
    let mut output = String::new();
    for diagnostic in &analysis.diagnostics {
        let position = source.position(diagnostic.span.start).unwrap_or_default();
        output.push_str(&format!(
            "{}:{}:{}: error[{}]: {}\n",
            path,
            position.line + 1,
            position.character + 1,
            diagnostic.code,
            diagnostic.message,
        ));
    }
    output
}
