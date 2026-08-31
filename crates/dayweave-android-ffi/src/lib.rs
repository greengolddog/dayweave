//! Bounded JNI boundary for on-device deterministic schedule composition.
//!
//! The Java surface is deliberately one byte-array-in/byte-array-out method.
//! Input and output use the scheduler helper's strict versioned JSON protocol,
//! so this crate does not maintain a second scheduling schema. The bridge has
//! no file, network, environment, credential, clock, or logging I/O.
//!
//! The [`jni::jni_mangle`] macro supplies the required exported JNI symbol
//! without weakening the workspace's `unsafe_code = "forbid"` policy. There
//! are no handwritten unsafe operations in this crate.

// Every JNI panic boundary below relies on catch_unwind. Refuse to produce an Android library if
// a Cargo profile or ambient build control attempts to replace that contract with panic=abort.
#[cfg(not(panic = "unwind"))]
compile_error!("dayweave-android-ffi requires panic=unwind for JNI containment");

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::Once;

use dayweave_scheduler_helper::{
    MAX_INPUT_BYTES, ProcessOutput, internal_failure_output, process_bytes,
    request_too_large_output,
};
use jni::errors::ErrorPolicy;
use jni::objects::{JByteArray, JObject};
use jni::{Env, EnvUnowned};

static INSTALL_PRIVATE_PANIC_HOOK: Once = Once::new();

/// Processes one helper-protocol request with full Rust panic containment.
///
/// This pure entry point exists so host tests can exercise exactly the same
/// helper call used by JNI. Every returned value is bounded by the scheduler
/// helper's output limit and contains only its fixed protocol messages.
#[must_use]
pub fn process_request_bytes(input: &[u8]) -> Vec<u8> {
    process_contained(|| {
        install_private_panic_hook();
        process_bytes(input)
    })
}

fn install_private_panic_hook() {
    // Android's default Rust panic hook writes the payload to stderr/logcat.
    // A request may be present in a panic payload, so this native library
    // permanently installs a no-op hook before processing any request. This is
    // the only Rust library loaded by DayWeave's Android process.
    INSTALL_PRIVATE_PANIC_HOOK.call_once(|| std::panic::set_hook(Box::new(|_| {})));
}

fn process_contained(process: impl FnOnce() -> ProcessOutput) -> Vec<u8> {
    catch_unwind(AssertUnwindSafe(process)).map_or_else(
        |payload| {
            // Do not inspect, format, log, or run a potentially hostile panic
            // payload destructor while handling the boundary failure.
            std::mem::forget(payload);
            internal_failure_output().stdout
        },
        |output| output.stdout,
    )
}

fn response_for_java_array(
    env: &Env<'_>,
    request: &JByteArray<'_>,
) -> jni::errors::Result<Vec<u8>> {
    if request.len(env)? > MAX_INPUT_BYTES {
        return Ok(request_too_large_output().stdout);
    }

    let input = env.convert_byte_array(request)?;
    let response = process_request_bytes(&input);
    drop(input);
    Ok(response)
}

fn internal_response_array(env: &mut Env<'_>) -> jni::errors::Result<jni::sys::jbyteArray> {
    env.byte_array_from_slice(&internal_failure_output().stdout)
        .map(JByteArray::into_raw)
}

/// Resolves an unexpected JNI error or panic without exposing its contents.
struct PrivateInternalResponse;

impl ErrorPolicy<jni::sys::jbyteArray, jni::errors::Error> for PrivateInternalResponse {
    type Captures<'unowned_env_local: 'native_method, 'native_method> = ();

    fn on_error<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _captures: &mut Self::Captures<'unowned_env_local, 'native_method>,
        _error: jni::errors::Error,
    ) -> jni::errors::Result<jni::sys::jbyteArray> {
        if env.exception_check() {
            return Ok(ptr::null_mut());
        }
        internal_response_array(env)
    }

    fn on_panic<'unowned_env_local: 'native_method, 'native_method>(
        env: &mut Env<'unowned_env_local>,
        _captures: &mut Self::Captures<'unowned_env_local, 'native_method>,
        payload: Box<dyn std::any::Any + Send + 'static>,
    ) -> jni::errors::Result<jni::sys::jbyteArray> {
        std::mem::forget(payload);
        if env.exception_check() {
            return Ok(ptr::null_mut());
        }
        internal_response_array(env)
    }

    fn on_internal_jni_error<'unowned_env_local: 'native_method, 'native_method>(
        _captures: &mut Self::Captures<'unowned_env_local, 'native_method>,
        _error: jni::errors::Error,
    ) -> jni::sys::jbyteArray {
        ptr::null_mut()
    }

    fn on_internal_panic<'unowned_env_local: 'native_method, 'native_method>(
        _captures: &mut Self::Captures<'unowned_env_local, 'native_method>,
        payload: Box<dyn std::any::Any + Send + 'static>,
    ) -> jni::sys::jbyteArray {
        std::mem::forget(payload);
        ptr::null_mut()
    }
}

/// JNI implementation of
/// `com.greengolddog.dayweave.scheduler.RustSchedulerNative.process(byte[])`.
///
/// JNI infrastructure failures retain any pending Java exception and return
/// `null`; all scheduler/protocol failures return a non-null, sanitized helper
/// response. Kotlin must treat a `null` result as an internal native failure.
#[jni::jni_mangle("com.greengolddog.dayweave.scheduler.RustSchedulerNative", "process")]
pub extern "system" fn process<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _receiver: JObject<'caller>,
    request: JByteArray<'caller>,
) -> jni::sys::jbyteArray {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(install_private_panic_hook)) {
        std::mem::forget(payload);
        return ptr::null_mut();
    }
    catch_unwind(AssertUnwindSafe(|| {
        unowned_env
            .with_env(|env| -> jni::errors::Result<_> {
                let response = response_for_java_array(env, &request)?;
                env.byte_array_from_slice(&response)
                    .map(JByteArray::into_raw)
            })
            .resolve::<PrivateInternalResponse>()
    }))
    .unwrap_or_else(|payload| {
        std::mem::forget(payload);
        ptr::null_mut()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSE_REQUEST: &[u8] =
        include_bytes!("../../dayweave-scheduler-helper/tests/fixtures/compose-request-v1.json");
    const COMPOSE_SUCCESS: &[u8] =
        include_bytes!("../../dayweave-scheduler-helper/tests/fixtures/compose-success-v1.json");

    #[test]
    fn pure_bridge_preserves_the_exact_compose_protocol() {
        assert_eq!(process_request_bytes(COMPOSE_REQUEST), COMPOSE_SUCCESS);
    }

    #[test]
    fn pure_bridge_is_deterministic() {
        let first = process_request_bytes(COMPOSE_REQUEST);
        let second = process_request_bytes(COMPOSE_REQUEST);
        assert_eq!(first, second);
    }

    #[test]
    fn outer_panic_is_sanitized_and_does_not_echo_its_payload() {
        install_private_panic_hook();
        let response = process_contained(|| -> ProcessOutput {
            panic!("private Android bridge panic payload")
        });
        assert_eq!(response, internal_failure_output().stdout);
        assert!(!response.windows(7).any(|window| window == b"private"));
    }

    #[test]
    fn pre_copy_size_rejection_matches_the_helper_contract() {
        let direct = request_too_large_output().stdout;
        let oversized = vec![b' '; MAX_INPUT_BYTES + 1];
        assert_eq!(process_request_bytes(&oversized), direct);
    }
}
