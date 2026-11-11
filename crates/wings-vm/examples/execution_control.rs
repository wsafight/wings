use std::path::Path;

use wings_vm::{CancellationToken, ScalarProcess};

fn load(directory: &Path) -> ScalarProcess {
    unsafe { ScalarProcess::load_trusted(directory) }.expect("trusted Counter artifact")
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 2 && arguments[0] == "--trusted");
    let directory = Path::new(&arguments[1]);
    let cancellation = CancellationToken::default();
    let mut process = load(directory);
    assert!(process.invoke("Missing", &[]).is_err());
    assert!(process.invoke("Inc", &[]).is_err());
    assert_eq!(process.invoke("Get", &[]), Ok(Some(0)));
    assert_eq!(
        process.invoke_controlled("Inc", &[3], 4, &cancellation),
        Ok(Some(3))
    );
    assert_eq!(
        process.invoke_controlled("Inc", &[4], 4, &cancellation),
        Ok(Some(7))
    );
    let remote = cancellation.clone();
    std::thread::spawn(move || remote.cancel())
        .join()
        .expect("cancel thread");
    assert_eq!(
        process.invoke_controlled("Inc", &[3], 0, &cancellation),
        Err("managed execution cancelled".into())
    );
    assert!(
        process
            .invoke("Get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    let mut process = load(directory);
    assert_eq!(process.invoke("Get", &[]), Ok(Some(0)));
    assert_eq!(
        process.invoke_controlled("Inc", &[3], 3, &CancellationToken::default()),
        Err("managed execution step budget exhausted".into())
    );
    assert!(
        process
            .invoke("Get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    println!(
        "Execution control API verified: per-call budget, shared cancellation, failure termination"
    );
}
