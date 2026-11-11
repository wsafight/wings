use wings_vm::{ResourceAccount, ResourceLevel, Resources};

fn limits(bytes: u64) -> Resources {
    Resources {
        bytes,
        processes: 8,
        messages: 16,
        requests: 4,
    }
}

fn bytes(amount: u64) -> Resources {
    Resources {
        bytes: amount,
        ..Resources::default()
    }
}

fn main() {
    let vm = ResourceAccount::vm(limits(100));
    assert_eq!(vm.level(), ResourceLevel::Vm);
    assert!(vm.process(limits(100)).is_err());
    let first_tenant = vm.tenant(limits(80)).expect("tenant");
    let second_tenant = vm.tenant(limits(40)).expect("tenant");
    assert!(first_tenant.tenant(limits(100)).is_err());
    let first = first_tenant.process(limits(60)).expect("process");
    let second = first_tenant.process(limits(60)).expect("process");
    let third = second_tenant.process(limits(40)).expect("process");
    assert!(first.process(limits(100)).is_err());
    let mut ticket = first.reserve(bytes(60)).expect("reserve");
    assert_eq!(
        (
            first.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (60, 60, 60)
    );
    assert!(
        first
            .reserve(bytes(1))
            .unwrap_err_string()
            .contains("Process")
    );
    assert!(
        second
            .reserve(bytes(21))
            .unwrap_err_string()
            .contains("Tenant")
    );
    assert_eq!(second.used().bytes, 0);
    let extra = third.reserve(bytes(40)).expect("fill VM");
    assert!(vm.reserve(bytes(1)).unwrap_err_string().contains("Vm"));
    ticket
        .transfer(&second)
        .expect("shared ancestor already full");
    assert_eq!(
        (
            first.used().bytes,
            second.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 60, 60, 100)
    );
    assert!(ticket.transfer(&third).is_err());
    assert_eq!(
        (second.used().bytes, third.used().bytes, vm.used().bytes),
        (60, 40, 100)
    );
    ticket.transfer(&second).expect("same account");
    assert!(ticket.transfer(&ResourceAccount::vm(limits(100))).is_err());
    drop(extra);
    ticket.transfer(&vm).expect("ancestor transfer");
    assert_eq!(
        (
            second.used().bytes,
            first_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 0, 60)
    );
    drop(ticket);
    assert_eq!(vm.used(), Resources::default());
    let mut ticket = first.reserve(bytes(30)).expect("cross tenant source");
    ticket.transfer(&third).expect("cross tenant");
    assert_eq!(
        (
            first_tenant.used().bytes,
            second_tenant.used().bytes,
            vm.used().bytes
        ),
        (0, 30, 30)
    );
    assert_eq!(ticket.amount(), bytes(30));
    drop(ticket);
    let amount = Resources {
        bytes: 7,
        processes: 1,
        messages: 2,
        requests: 1,
    };
    let ticket = first.reserve(amount).expect("four dimensions");
    assert_eq!(vm.used(), amount);
    drop(ticket);
    let zero = first.reserve(Resources::default()).expect("zero");
    assert_eq!(vm.used(), Resources::default());
    drop(zero);
    for dimension in [
        Resources {
            processes: 9,
            ..Resources::default()
        },
        Resources {
            messages: 17,
            ..Resources::default()
        },
        Resources {
            requests: 5,
            ..Resources::default()
        },
    ] {
        assert!(first.reserve(dimension).is_err());
        assert_eq!(vm.used(), Resources::default());
    }
    let unlimited = ResourceAccount::vm(Resources {
        bytes: u64::MAX,
        ..Resources::default()
    });
    let maximum = unlimited.reserve(bytes(u64::MAX)).expect("maximum");
    assert!(unlimited.reserve(bytes(1)).is_err());
    drop(maximum);
    assert_eq!(unlimited.used().bytes, 0);
    let observer = vm.clone();
    let ticket = first.reserve(bytes(11)).expect("survives handles");
    drop(first);
    drop(first_tenant);
    drop(vm);
    assert_eq!(observer.used().bytes, 11);
    drop(ticket);
    assert_eq!(observer.used(), Resources::default());
    println!(
        "Resource accounts verified: hierarchy, four limits, overflow, atomic rollback, net transfers and lifetime"
    );
}

trait ReservationError {
    fn unwrap_err_string(self) -> String;
}

impl ReservationError for Result<wings_vm::Reservation, String> {
    fn unwrap_err_string(self) -> String {
        match self {
            Err(error) => error,
            Ok(_) => panic!("expected reservation rejection"),
        }
    }
}
