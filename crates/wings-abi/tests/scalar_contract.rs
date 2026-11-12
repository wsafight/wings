use std::mem::{align_of, offset_of, size_of};
use std::sync::atomic::AtomicU32;

use wings_abi::{
    DEFAULT_MAX_STEPS, SCALAR_ABI_VERSION, STATUS_BUDGET_EXHAUSTED, STATUS_CANCELLED,
    STATUS_DIVISION_BY_ZERO, STATUS_INVALID_CONTROL, STATUS_OK, STATUS_OVERFLOW, ScalarContext,
    ScalarEntry, ScalarOutput, ScalarSlowPath, ValueType, host_target,
};

#[test]
fn integers_accept_the_entire_signed_scalar_range() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert!(ValueType::Int.accepts(value));
    }
}

#[test]
fn booleans_accept_only_zero_and_one() {
    for value in [i64::MIN, -1, 0, 1, 2, i64::MAX] {
        assert_eq!(ValueType::Bool.accepts(value), matches!(value, 0 | 1));
    }
}

#[test]
fn unit_accepts_only_zero() {
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(ValueType::Unit.accepts(value), value == 0);
    }
}

#[test]
fn scalar_output_is_two_contiguous_c_integers() {
    assert_eq!(offset_of!(ScalarOutput, state), 0);
    assert_eq!(offset_of!(ScalarOutput, reply), 8);
    assert_eq!(size_of::<ScalarOutput>(), 16);
    assert_eq!(align_of::<ScalarOutput>(), align_of::<i64>());
    let output = ScalarOutput::default();
    assert_eq!((output.state, output.reply), (0, 0));
}

#[test]
fn control_context_layout_matches_the_native_codegen_contract() {
    // The scalar backend is restricted to these 64-bit hosts; offsets are used
    // directly by generated code, so field reordering must change this test.
    if host_target().is_err() {
        return;
    }
    assert_eq!(offset_of!(ScalarContext, cancellation), 0);
    assert_eq!(offset_of!(ScalarContext, remaining_steps), 8);
    assert_eq!(offset_of!(ScalarContext, yield_request), 16);
    assert_eq!(offset_of!(ScalarContext, slice_remaining), 24);
    assert_eq!(offset_of!(ScalarContext, slow_path), 32);
    assert_eq!(offset_of!(ScalarContext, data), 40);
    assert_eq!(size_of::<ScalarContext>(), 48);
    assert_eq!(align_of::<ScalarContext>(), 8);
    assert_eq!(size_of::<AtomicU32>(), 4);
}

#[test]
fn optional_slow_path_has_the_nullable_function_pointer_layout() {
    assert_eq!(size_of::<Option<ScalarSlowPath>>(), size_of::<usize>());
    assert_eq!(size_of::<ScalarEntry>(), size_of::<usize>());
}

#[test]
fn scalar_status_codes_and_version_remain_wire_compatible() {
    assert_eq!(SCALAR_ABI_VERSION, 3);
    assert_eq!(STATUS_OK, 0);
    assert_eq!(STATUS_OVERFLOW, 1);
    assert_eq!(STATUS_DIVISION_BY_ZERO, 2);
    assert_eq!(STATUS_CANCELLED, 3);
    assert_eq!(STATUS_BUDGET_EXHAUSTED, 4);
    assert_eq!(STATUS_INVALID_CONTROL, 5);
    assert_eq!(DEFAULT_MAX_STEPS, 1_000_000);
}

#[test]
fn host_target_only_accepts_the_two_supported_platforms() {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => assert_eq!(host_target().unwrap(), "aarch64-apple-darwin"),
        ("linux", "x86_64") if cfg!(target_env = "gnu") => {
            assert_eq!(host_target().unwrap(), "x86_64-unknown-linux-gnu");
        }
        _ => assert!(host_target().unwrap_err().contains("requires")),
    }
}
