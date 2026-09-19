//! Waiting, on a platform that has a reactor and on one that has an event loop.
//!
//! The engine's backoff is the only place it needs a clock, and `tokio::time`
//! needs a tokio runtime driving it — which a browser tab does not have and
//! cannot be given. This is the one line of the engine that differs between the
//! two, so it is the only thing behind a `cfg` rather than the loop that uses it.

use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn sleep(how_long: Duration) {
    tokio::time::sleep(how_long).await
}

/// `setTimeout`, awaited.
///
/// A timeout longer than `i32::MAX` milliseconds is not something this asks for
/// — the backoff ceiling is eight seconds — but saturating is still the right
/// answer to it, because the alternative on overflow is a timer that fires
/// immediately and a reconnect loop that spins.
#[cfg(target_arch = "wasm32")]
pub(crate) async fn sleep(how_long: Duration) {
    use wasm_bindgen::JsCast;
    let millis = i32::try_from(how_long.as_millis()).unwrap_or(i32::MAX);
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let global = js_sys::global();
        // Whichever global this thread has; a worker's `setTimeout` is a
        // different binding from a window's and does the same thing.
        let set = if let Some(window) = global.dyn_ref::<web_sys::Window>() {
            window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis)
        } else if let Some(worker) = global.dyn_ref::<web_sys::WorkerGlobalScope>() {
            worker.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis)
        } else {
            Err(wasm_bindgen::JsValue::NULL)
        };
        // Nothing to wait on if the timer could not be set. Resolving now turns
        // that into a busy reconnect loop rather than an engine that never wakes
        // again, and an engine that never wakes is the worse of the two: the app
        // would sit there saying "offline" with a relay that is right there.
        if set.is_err() {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
