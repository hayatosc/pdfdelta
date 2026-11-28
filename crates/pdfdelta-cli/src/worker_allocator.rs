//! Process-wide allocation ceiling for bounded worker subprocesses on macOS.
//!
//! # Safety
//!
//! This module is the only place in the workspace allowed to use unsafe code.
//! It implements [`GlobalAlloc`] by forwarding to [`System`] while accounting
//! live bytes. The implementation never dereferences a pointer: it only adds
//! and subtracts the layout sizes required by the trait contract and forwards
//! the same pointer and layout to `System`. `_exit` is an allocation-free,
//! async-signal-safe libc call used after the ceiling is crossed.
//!
//! Linux enforces a worker's 2 GiB address-space ceiling with `RLIMIT_AS`.
//! macOS accepts the same limit but does not enforce it, so worker mode arms
//! this counting allocator instead. An allocation that would take live bytes
//! above the ceiling terminates the worker with exit code 3, which the parent
//! maps to a typed `resource_limit` outcome. The allocator is inert until
//! [`enforce`] is called; the comparison process never arms it.
#![allow(unsafe_code)] // The allocator contract is the whole subject of this module.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Mirrors the 2 GiB ceiling passed to `RLIMIT_AS`.
pub(crate) const WORKER_MEMORY_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Exit code the parent maps to a typed resource-limit outcome.
const LIMIT_EXIT_CODE: i32 = 3;

static ENFORCED: AtomicBool = AtomicBool::new(false);
static LIMIT: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);

/// Arms the allocator for the current worker process.
///
/// Must run before the worker reads untrusted input. Allocations made before
/// arming are not counted; their later frees saturate at zero so accounting
/// never wraps.
pub(crate) fn enforce(limit: u64) {
    LIMIT.store(limit, Ordering::Relaxed);
    ENFORCED.store(true, Ordering::Release);
}

/// The system allocator with a live-byte ceiling while worker mode is armed.
pub(crate) struct WorkerAllocator;

unsafe impl GlobalAlloc for WorkerAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENFORCED.load(Ordering::Acquire) {
            reserve(layout.size() as u64);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENFORCED.load(Ordering::Acquire) {
            let _ = LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
                Some(live.saturating_sub(layout.size() as u64))
            });
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn reserve(size: u64) {
    let limit = LIMIT.load(Ordering::Relaxed);
    let mut live = LIVE.load(Ordering::Relaxed);
    loop {
        let next = match live.checked_add(size) {
            Some(next) if next <= limit => next,
            _ => terminate_over_budget(),
        };
        match LIVE.compare_exchange_weak(live, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return,
            Err(observed) => live = observed,
        }
    }
}

fn terminate_over_budget() -> ! {
    // `_exit` is async-signal-safe and allocation-free. The parent classifies
    // any worker exit status 3 as a typed resource-limit outcome.
    unsafe extern "C" {
        fn _exit(status: i32) -> !;
    }
    unsafe { _exit(LIMIT_EXIT_CODE) }
}
