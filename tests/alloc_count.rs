//! The `alloc-count` measurement allocator (audit R12): it counts the
//! current thread's allocations only. Run with
//! `cargo test --features alloc-count --test alloc_count`.
#![cfg(feature = "alloc-count")]

use nora::trace::alloc_count::CountingAlloc;
use nora::trace::thread_allocations;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn counts_this_threads_allocations() {
    let before = thread_allocations().expect("feature enabled");
    let mut v: Vec<u64> = Vec::with_capacity(4);
    let after_alloc = thread_allocations().unwrap();
    assert_eq!(after_alloc - before, 1, "one allocation");
    v.extend(0..1000);
    let after_grow = thread_allocations().unwrap();
    assert!(after_grow > after_alloc, "growth reallocates");
    drop(v);
    assert_eq!(
        thread_allocations().unwrap(),
        after_grow,
        "frees are not counted"
    );
}

#[test]
fn other_threads_are_not_counted() {
    let before = thread_allocations().unwrap();
    let other = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let start = thread_allocations().unwrap();
                let strings: Vec<String> = (0..100).map(|i| i.to_string()).collect();
                (thread_allocations().unwrap() - start, strings.len())
            })
            .join()
            .unwrap()
    });
    assert!(
        other.0 >= 100,
        "the other thread counted its own: {}",
        other.0
    );
    let ours = thread_allocations().unwrap() - before;
    assert!(
        ours < 100,
        "this thread counted only its own spawn/join work: {ours}"
    );
}
