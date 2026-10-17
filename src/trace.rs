//! Opt-in startup tracing (`NORA_TRACE_STARTUP=1`): milliseconds since
//! process start at each startup stage, on stderr. Used by
//! `scripts/perf.sh` and for diagnosing slow starts; off by default.

use std::sync::OnceLock;
use std::time::Instant;

static START: OnceLock<Option<Instant>> = OnceLock::new();

/// Record the process start; call first thing in `main`.
pub fn start() {
    START.get_or_init(|| std::env::var_os("NORA_TRACE_STARTUP").map(|_| Instant::now()));
}

/// Log `stage` with the elapsed time since [`start`], when tracing is on.
pub fn stage(stage: &str) {
    if let Some(Some(start)) = START.get() {
        eprintln!(
            "startup: {:8.1} ms {stage}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Heap allocations made by the current thread, when the binary was built
/// with the `alloc-count` feature (a measurement build: audit R12); `None`
/// otherwise. Differences between two reads on one thread count that
/// thread's allocations in between.
pub fn thread_allocations() -> Option<u64> {
    #[cfg(feature = "alloc-count")]
    {
        Some(alloc_count::THREAD_ALLOCS.with(std::cell::Cell::get))
    }
    #[cfg(not(feature = "alloc-count"))]
    {
        None
    }
}

/// The counting allocator behind [`thread_allocations`].
#[cfg(feature = "alloc-count")]
pub mod alloc_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        // Const-initialized and without a destructor, so reading it never
        // allocates (no recursion into the allocator).
        pub(super) static THREAD_ALLOCS: Cell<u64> = const { Cell::new(0) };
    }

    fn count() {
        // `try_with`: the slot is gone while the thread is being torn down.
        let _ = THREAD_ALLOCS.try_with(|n| n.set(n.get() + 1));
    }

    /// [`System`], counting `alloc`, `alloc_zeroed` and `realloc` calls
    /// per thread. Install with `#[global_allocator]`.
    pub struct CountingAlloc;

    // SAFETY: every method forwards its arguments unchanged to `System`,
    // which upholds the `GlobalAlloc` contract; the counter is a
    // thread-local integer that neither allocates nor touches the memory.
    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: the caller's contract is passed through to `System`.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count();
            // SAFETY: as above.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            count();
            // SAFETY: as above.
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: as above.
            unsafe { System.dealloc(ptr, layout) }
        }
    }
}
