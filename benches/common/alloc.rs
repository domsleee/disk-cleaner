//! Tracking allocator shared by the memory-measuring benches.

use mimalloc::MiMalloc;
use std::alloc::{GlobalAlloc, Layout};
use std::sync::atomic::{AtomicUsize, Ordering};

struct TrackingAllocator;

pub static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { MiMalloc.alloc(layout) };
        if !ptr.is_null() {
            let current = ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(current, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        ALLOCATED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { MiMalloc.dealloc(ptr, layout) };
    }

    // Without this override the default realloc is alloc+copy+dealloc, which
    // both changes the program's allocation behavior vs. the real app and
    // double-counts every Vec/HashSet growth in the peak numbers.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { MiMalloc.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size >= layout.size() {
                let grow = new_size - layout.size();
                let current = ALLOCATED.fetch_add(grow, Ordering::Relaxed) + grow;
                PEAK.fetch_max(current, Ordering::Relaxed);
            } else {
                ALLOCATED.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_ptr
    }
}

#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator;

/// Reset the peak to the current live level. `ALLOCATED` is never zeroed:
/// it tracks live bytes process-wide, and zeroing it while allocations made
/// before the reset are still live would underflow (wrap) when they free.
/// Callers measure deltas against a `before` snapshot instead.
pub fn reset_tracking() {
    PEAK.store(ALLOCATED.load(Ordering::SeqCst), Ordering::SeqCst);
}
