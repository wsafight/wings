use std::path::Path;

use wings_vm::{
    CancellationToken, ExecutionStatus, PreemptionToken, ScalarExecution, ScalarProcess,
};

fn load(directory: &Path) -> ScalarProcess {
    unsafe { ScalarProcess::load_trusted(directory) }.expect("trusted scalar artifact")
}

fn finish(execution: &mut ScalarExecution<'_>, slice: u64) -> (Option<i64>, usize) {
    let mut suspensions = 0;
    loop {
        match execution.resume(slice).expect("resume") {
            ExecutionStatus::Suspended => {
                suspensions += 1;
                assert!(suspensions < 4096);
            }
            ExecutionStatus::Complete(reply) => return (reply, suspensions),
        }
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    assert!(arguments.len() == 3 && arguments[0] == "--trusted");
    let counter = Path::new(&arguments[1]);
    let arithmetic = Path::new(&arguments[2]);
    let cancellation = CancellationToken::default();
    let preemption = PreemptionToken::default();
    let mut process = load(counter);
    assert!(
        process
            .start_controlled("Missing", &[], 4, &cancellation, &preemption)
            .is_err()
    );
    assert!(
        process
            .start_controlled("Inc", &[], 4, &cancellation, &preemption)
            .is_err()
    );
    assert_eq!(process.invoke("Get", &[]), Ok(Some(0)));
    {
        let mut execution = process
            .start_controlled("Inc", &[3], 4, &cancellation, &preemption)
            .expect("start");
        assert!(execution.resume(0).unwrap_err().contains("at least one"));
        assert_eq!(finish(&mut execution, 1), (Some(3), 3));
        assert!(execution.resume(1).unwrap_err().contains("completed"));
    }
    assert_eq!(process.invoke("Get", &[]), Ok(Some(3)));
    for expected in 4..=131 {
        let mut execution = process
            .start_controlled("Inc", &[1], 4, &cancellation, &preemption)
            .expect("start");
        assert_eq!(finish(&mut execution, 1), (Some(expected), 3));
    }
    {
        let mut execution = process
            .start_controlled("Inc", &[1], 4, &cancellation, &preemption)
            .expect("start");
        preemption.request();
        assert_eq!(execution.resume(100), Ok(ExecutionStatus::Suspended));
        let remote = preemption.clone();
        std::thread::spawn(move || remote.request())
            .join()
            .expect("request thread");
        assert_eq!(execution.resume(100), Ok(ExecutionStatus::Suspended));
        assert_eq!(
            execution.resume(100),
            Ok(ExecutionStatus::Complete(Some(132)))
        );
    }
    let mut other = load(counter);
    {
        let mut first = process
            .start_controlled("Inc", &[8], 4, &cancellation, &preemption)
            .expect("start first");
        let mut second = other
            .start_controlled("Inc", &[9], 4, &cancellation, &preemption)
            .expect("start second");
        for _ in 0..3 {
            assert_eq!(first.resume(1), Ok(ExecutionStatus::Suspended));
            assert_eq!(second.resume(1), Ok(ExecutionStatus::Suspended));
        }
        assert_eq!(first.resume(1), Ok(ExecutionStatus::Complete(Some(140))));
        assert_eq!(second.resume(1), Ok(ExecutionStatus::Complete(Some(9))));
    }
    for start in [false, true] {
        let mut abandoned = load(counter);
        {
            let mut execution = abandoned
                .start_controlled("Inc", &[3], 4, &cancellation, &preemption)
                .expect("start");
            if start {
                for _ in 0..3 {
                    assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
                }
            }
        }
        assert!(
            abandoned
                .invoke("Get", &[])
                .unwrap_err()
                .contains("terminated")
        );
    }
    let mut cancelled = load(counter);
    let cancellation = CancellationToken::default();
    {
        let mut execution = cancelled
            .start_controlled("Inc", &[3], 4, &cancellation, &preemption)
            .expect("start");
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        let remote = cancellation.clone();
        std::thread::spawn(move || remote.cancel())
            .join()
            .expect("cancel thread");
        assert_eq!(
            execution.resume(1),
            Err("managed execution cancelled".into())
        );
    }
    assert!(
        cancelled
            .invoke("Get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    let cancellation = CancellationToken::default();
    let mut exhausted = load(counter);
    {
        let mut execution = exhausted
            .start_controlled("Inc", &[3], 3, &cancellation, &preemption)
            .expect("start");
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        assert_eq!(execution.resume(1), Ok(ExecutionStatus::Suspended));
        assert_eq!(
            execution.resume(1),
            Err("managed execution step budget exhausted".into())
        );
    }
    assert!(
        exhausted
            .invoke("Get", &[])
            .unwrap_err()
            .contains("terminated")
    );
    for cancelled in [false, true] {
        let mut process = load(counter);
        let cancellation = CancellationToken::default();
        if cancelled {
            cancellation.cancel();
        }
        let pending = PreemptionToken::default();
        pending.request();
        let mut execution = process
            .start_controlled("Inc", &[3], 0, &cancellation, &pending)
            .expect("start zero budget");
        let error = if cancelled {
            "managed execution cancelled"
        } else {
            "managed execution step budget exhausted"
        };
        assert_eq!(execution.resume(1), Err(error.into()));
        assert!(execution.resume(1).unwrap_err().contains("completed"));
    }
    let mut live = load(arithmetic);
    {
        let mut execution = live
            .start_controlled("Work", &[7], 147, &cancellation, &preemption)
            .expect("start live values");
        let (reply, suspensions) = finish(&mut execution, 1);
        assert_eq!(reply, Some(1464));
        assert_eq!(suspensions, 146);
    }
    assert_eq!(live.invoke("Get", &[]), Ok(Some(1464)));
    {
        let mut execution = live
            .start_controlled("Fail", &[], 100, &cancellation, &preemption)
            .expect("start failure");
        loop {
            match execution.resume(1) {
                Ok(ExecutionStatus::Suspended) => {}
                Err(error) => {
                    assert!(error.contains("division by zero"));
                    break;
                }
                result => panic!("unexpected result: {result:?}"),
            }
        }
    }
    assert!(live.invoke("Get", &[]).unwrap_err().contains("terminated"));
    println!("Stackful resume verified: slices, live values, interleaving, cancellation and drop");
}
