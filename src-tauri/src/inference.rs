//! Shared ONNX Runtime execution-provider probing.
//!
//! Priority per handoff spec: CUDA -> CoreML -> DirectML -> OpenVINO -> CPU.
//! `with_execution_providers` tries them in order and falls back to CPU
//! automatically; a failure never crashes.
//!
//! The list is deliberately the same on every platform even though the EP
//! *features* in Cargo.toml are per-target. An EP that was not compiled into
//! this build fails to register and is skipped, which is the same path an EP
//! whose hardware is absent takes — so there is one code path to reason
//! about instead of one per operating system.

use std::sync::OnceLock;

use ort::ep::ExecutionProviderDispatch;
use ort::session::Session;

/// Ordered EP list. ort attempts each in order, silently falling back to CPU.
pub fn ordered_providers() -> Vec<ExecutionProviderDispatch> {
    vec![
        ort::ep::CUDA::default().with_device_id(0).build(),
        // NOTE: struct names verified for ort 2.0.0-rc.12 (`ort::ep::*`).
        // If a name drifts, adjust here only.
        ort::ep::CoreML::default().build(),
        ort::ep::DirectML::default().build(),
        ort::ep::OpenVINO::default().build(),
        ort::ep::CPU::default().build(),
    ]
}

/// Probe one EP by registering it (with `error_on_failure`) on a fresh
/// builder. Returns true when ORT accepts the EP registration.
/// NOTE: this is a registration check only — no session is committed, so
/// it cannot prove the hardware is present (e.g. `cuda` may be reported
/// on a CUDA-less machine if the build registers). Do not present the
/// result as a hardware inventory; it answers "which EP will ORT pick".
fn probe_dispatch(dispatch: ExecutionProviderDispatch) -> bool {
    let builder = match Session::builder() {
        Ok(b) => b,
        Err(_) => return false,
    };
    builder.with_execution_providers([dispatch]).is_ok()
}

/// Select the first working EP in CUDA→CoreML→DirectML→OpenVINO→CPU order.
fn select_ep() -> String {
    const ORDER: &str = "cuda→coreml→directml→openvino→cpu";
    let candidates: Vec<(&str, ExecutionProviderDispatch)> = vec![
        (
            "cuda",
            ort::ep::CUDA::default()
                .with_device_id(0)
                .build()
                .error_on_failure(),
        ),
        (
            "coreml",
            ort::ep::CoreML::default().build().error_on_failure(),
        ),
        (
            "directml",
            ort::ep::DirectML::default().build().error_on_failure(),
        ),
        (
            "openvino",
            ort::ep::OpenVINO::default().build().error_on_failure(),
        ),
        ("cpu", ort::ep::CPU::default().build().error_on_failure()),
    ];
    for (name, dispatch) in candidates {
        if probe_dispatch(dispatch) {
            if name == "cpu" {
                eprintln!("inference: falling back to CPU execution provider (probed: {ORDER})");
            }
            return format!("{name} (probed: {ORDER})");
        }
    }
    eprintln!("inference: no EP probe succeeded, falling back to cpu (probed: {ORDER})");
    format!("cpu (probed: {ORDER}; fallback)")
}

/// Human-readable name of the EP ort actually selected (first available).
///
/// Probed once at startup via tiny per-EP builder probes; the result is
/// cached. Output stays a human-readable string for `ep_report`.
pub fn describe_providers() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(select_ep).as_str()
}
