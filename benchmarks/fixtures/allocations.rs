//! Standalone measurement binary, built and run by the Pico Cargo integration test.
//! The unsafe allocator adapter is confined to this harness (ADR 0003).

#![deny(unsafe_op_in_unsafe_fn)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: Every request is forwarded unchanged to System; pointers and layouts
// retain its guarantees. Counting uses non-allocating, non-panicking atomics.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies a valid layout, passed unchanged to System.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: This pointer was allocated by System with this same layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies a valid layout, passed unchanged to System.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: Caller guarantees pointer/layout validity and a valid new size.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn invoke<F: FnOnce() -> R, R>(run: F) -> R {
    #[cfg(feature = "clamp")]
    {
        rustclamp::Clamp::run(run)
    }
    #[cfg(not(feature = "clamp"))]
    {
        run()
    }
}

fn count<F: FnOnce()>(run: F) -> usize {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    run();
    ALLOCATIONS.load(Ordering::Relaxed) - before
}

fn main() {
    // Measure the empty closure separately from the first stdout call.
    let empty = count(|| {
        invoke(|| {
            black_box(42);
        })
    });
    let output = count(|| invoke(|| println!("Hello Clamp")));
    // Prove the counter detects real user work rather than always reporting zero.
    let user = count(|| {
        invoke(|| {
            black_box(Box::new(black_box(42)));
        })
    });
    let calibration = count(|| {
        let layout = Layout::from_size_align(16, 8).unwrap();
        // SAFETY: Nonzero valid layout; handle failure, access within bounds,
        // reallocate with a valid size, and free with the matching final layout.
        unsafe {
            let pointer = black_box(ALLOCATOR.alloc_zeroed(layout));
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            assert_eq!(*pointer, 0);
            let pointer = black_box(ALLOCATOR.realloc(pointer, layout, 32));
            if pointer.is_null() {
                std::alloc::handle_alloc_error(Layout::from_size_align(32, 8).unwrap());
            }
            ALLOCATOR.dealloc(pointer, Layout::from_size_align(32, 8).unwrap());
        }
    });
    assert_eq!(empty, 0);
    assert!(user >= 1, "allocation control was optimized away");
    assert_eq!(calibration, 2);
    println!(
        "{{\"entrypoint\":{empty},\"stdout\":{output},\"user_box\":{user},\"calibration\":{calibration}}}"
    );
}
