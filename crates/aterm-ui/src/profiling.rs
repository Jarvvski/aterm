//! Frame profiling hooks (ticket T-1.8 AC4).
//!
//! The renderer's frame path is instrumented with `tracing` spans
//! ([`crate::gpu`]'s `frame` / `build` / `encode` / `present` zones). `tracing` is
//! always compiled and costs nothing when no subscriber is installed, so the zones
//! ride along in every build.
//!
//! [`init`] installs a Tracy subscriber so those zones stream to the Tracy profiler
//! (`09-performance-60fps.md` §8 names `tracing-tracy` as the primary live dev tool;
//! the convention is a frame marker with sub-zones for input/parse/build/encode).
//! It is gated behind the `tracy` cargo feature so the default build never links the
//! C Tracy client; without the feature it is a no-op. Capturing and reading a frame
//! breakdown requires a running Tracy server on real hardware - that on-device pass
//! is EPIC-7 (the renderer zones + the subscriber wiring are what land here).

/// Install the Tracy `tracing` subscriber (feature `tracy`); otherwise a no-op.
/// Idempotent: a second call is ignored (`try_init`). Call once at startup.
#[cfg(feature = "tracy")]
pub fn init() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let _ = tracing_subscriber::registry()
        .with(tracing_tracy::TracyLayer::default())
        .try_init();
}

/// No-op profiling init for the default (non-`tracy`) build.
#[cfg(not(feature = "tracy"))]
pub fn init() {}
