//! Test-only heap-allocation counter (ticket T-1.8 AC2 / T-1.5 AC5).
//!
//! `09-performance-60fps.md` §4 prescribes "a debug-build allocation counter (e.g. a
//! custom `GlobalAlloc` wrapper) asserting 0 allocations during a steady-state frame
//! in a test build". This is that wrapper. It is compiled ONLY under `cfg(test)`, so
//! it never ships in the binary's allocator path.
//!
//! It counts allocations only while *armed on the current thread* (a const-init
//! thread-local, so arming never itself allocates), which keeps cargo's parallel
//! test threads from polluting a measured region: other tests allocate freely through
//! the System allocator without being counted. [`count_allocs`] arms the counter
//! around a closure and returns the delta.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Whether allocations on this thread are currently being counted.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// Allocations counted on this thread since the last arm.
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

/// A `System`-delegating allocator that tallies allocations made while armed.
struct CountingAlloc;

// SAFETY: every method forwards to the `System` allocator unchanged; the only added
// behavior is incrementing a const-init thread-local counter when armed. `try_with`
// tolerates the TLS being torn down during thread exit (it returns `Err`, ignored).
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                let _ = COUNT.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                let _ = COUNT.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ARMED.try_with(|a| {
            if a.get() {
                let _ = COUNT.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Run `f` with allocation counting armed on this thread; return the number of
/// allocations (alloc / alloc_zeroed / realloc) it made.
pub(crate) fn count_allocs(f: impl FnOnce()) -> usize {
    COUNT.with(|n| n.set(0));
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_an_allocation_and_zero_when_warm() {
        // A fresh allocation is counted.
        let n = count_allocs(|| {
            let v: Vec<u8> = Vec::with_capacity(4096);
            std::hint::black_box(&v);
        });
        assert!(n >= 1, "allocating a Vec is counted (got {n})");

        // Reusing a warm buffer (clear + push within capacity) allocates nothing -
        // the steady-state property the renderer's frame build depends on.
        let mut warm: Vec<u8> = Vec::with_capacity(64);
        warm.push(1);
        let n = count_allocs(|| {
            warm.clear();
            for i in 0..32u8 {
                warm.push(i);
            }
            std::hint::black_box(&warm);
        });
        assert_eq!(n, 0, "reusing a warm buffer allocates nothing (got {n})");
    }
}
