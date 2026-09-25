//! Regression tests for the vendored pool macro used by frb_generated.rs.
#![allow(dead_code)]

use flutter_rust_bridge::for_generated::BaseArc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

flutter_rust_bridge::frb_generated_moi_arc_def!();

#[derive(Debug)]
struct Value;

impl MoiArcValue for Value {
    fn get_pool() -> &'static MoiArcPool<Self> {
        static POOL: OnceLock<MoiArcPool<Value>> = OnceLock::new();
        POOL.get_or_init(Default::default)
    }
}

#[test]
fn drop_releases_cached_arc_before_decrementing_pool_count() {
    let remaining = MoiArc::new(Value);
    let dropping = remaining.clone();
    // Hold the pool lock to stop the destructor at its count decrement. The
    // cached Arc must already be gone at that point, or another owner can see
    // ref_count == 1 while Arc::into_inner still sees two references.
    let pool = moi_arc_pool_write(Value::get_pool()).unwrap();
    let thread = std::thread::spawn(move || drop(dropping));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Arc::strong_count(remaining.value.as_ref().unwrap()) != 2 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let cached_arc_released = Arc::strong_count(remaining.value.as_ref().unwrap()) == 2;
    // Always unblock and join before asserting, including on the old code.
    drop(pool);
    thread.join().unwrap();
    assert!(
        cached_arc_released,
        "drop still holds its cached Arc at the pool decrement"
    );
    assert!(remaining.try_unwrap().is_ok());
}

#[test]
fn raw_handle_keeps_value_alive_until_last_owner() {
    let owner = MoiArc::new(Value);
    let raw = owner.clone().into_raw();
    let owner = owner.try_unwrap().unwrap_err();
    drop(MoiArc::<Value>::from_raw(raw));
    assert!(owner.try_unwrap().is_ok());
}

#[test]
fn invalid_release_identifies_the_handle_and_still_panics() {
    // Separate pool: the deliberately invalid release poisons its lock.
    struct InvalidReleaseValue;
    impl MoiArcValue for InvalidReleaseValue {
        fn get_pool() -> &'static MoiArcPool<Self> {
            static POOL: OnceLock<MoiArcPool<InvalidReleaseValue>> = OnceLock::new();
            POOL.get_or_init(Default::default)
        }
    }
    let raw = MoiArc::new(InvalidReleaseValue).into_raw();
    MoiArc::<InvalidReleaseValue>::decrement_strong_count(raw);
    let error = std::panic::catch_unwind(|| {
        MoiArc::<InvalidReleaseValue>::decrement_strong_count(raw);
    })
    .expect_err("invalid releases must not be silently ignored");
    let message = error.downcast_ref::<String>().unwrap();
    assert!(message.contains("MOI missing handle on release:"));
    assert!(message.contains("InvalidReleaseValue"));
    assert!(message.contains(&format!("id={raw}")));
}
