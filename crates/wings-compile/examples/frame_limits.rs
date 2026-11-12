use std::path::Path;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(arguments.len(), 1);
    let directory = Path::new(&arguments[0]);
    let source = "process Counter { state count: Int = 0; on Inc(by: Int) -> Int { count = count + by; count } }";
    for limit in [0, wings_codegen::MAX_SCALAR_FRAME_BYTES + 1, u32::MAX] {
        let error = wings_compile::build_with_frame_limit(source, directory, limit).unwrap_err();
        assert!(error.contains("frame limit must be within"));
        assert!(!directory.exists());
    }
    let error = wings_compile::build_with_frame_limit(source, directory, 1).unwrap_err();
    assert!(error.contains("scalar stack frame") && error.contains("limit is 1"));
    assert!(!directory.exists());
    let bindings = (0..128)
        .map(|index| format!("let value_{index} = seed + {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let total = (0..128)
        .map(|index| format!("value_{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    let pressure = format!(
        "process Pressure {{ state count: Int = 0; on Work(seed: Int) -> Int {{ {bindings}; {total} }} }}"
    );
    let error = wings_compile::build_with_frame_limit(&pressure, directory, 512).unwrap_err();
    assert!(error.contains("wings_scalar_handler_0") && error.contains("limit is 512"));
    assert!(!directory.exists());
    wings_compile::build(source, directory).expect("default frame admission");
    assert!(directory.join("manifest.json").is_file());
    println!(
        "Scalar frame admission verified: invalid limits and backend frame rejection before artifact writes"
    );
}
