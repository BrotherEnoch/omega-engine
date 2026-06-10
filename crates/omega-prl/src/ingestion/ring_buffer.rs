// omega-prl/src/ingestion/ring_buffer.rs
//! Lock-free single-producer single-consumer ring buffer — §5.1
//!
//! Used by per-shard pipeline workers.  Each shard owns one producer end;
//! the drain task owns the consumer end.  Cache-line padding between head
//! and tail eliminates false sharing at high throughput.
//!
//! This module exposes `LockFreeRingBuffer` as the canonical ring buffer
//! type re-exported from `lib.rs`.  The `EventBus` uses `ShardQueue`
//! internally (simpler, correct under the tokio task model); this type is
//! available for callers that need a raw SPSC buffer.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// Cache-line size on x86-64 and ARM64.
const CACHE_LINE: usize = 64;

/// Cache-line-padded atomic counter — prevents false sharing between head
/// and tail when accessed by separate CPU cores.
#[repr(C, align(64))]
struct Padded(
    AtomicUsize,
    [u8; CACHE_LINE - std::mem::size_of::<AtomicUsize>()],
);

impl Padded {
    const fn new(v: usize) -> Self {
        Self(
            AtomicUsize::new(v),
            [0u8; CACHE_LINE - std::mem::size_of::<AtomicUsize>()],
        )
    }
}

/// Inner shared state for `LockFreeRingBuffer`.
struct RingInner<T> {
    head: Padded,
    tail: Padded,
    capacity: usize,
    /// Power-of-two mask for index wrapping without division.
    mask: usize,
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
}

// SAFETY: `RingInner` is only accessed through the single-producer /
// single-consumer split enforced by `Producer` and `Consumer` ownership.
unsafe impl<T: Send> Send for RingInner<T> {}
unsafe impl<T: Send> Sync for RingInner<T> {}

impl<T> RingInner<T> {
    fn new(capacity: usize) -> Self {
        // Round up to next power of two for mask-based wrapping.
        let capacity = capacity.next_power_of_two();
        let slots = (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            head: Padded::new(0),
            tail: Padded::new(0),
            capacity,
            mask: capacity - 1,
            slots,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        let head = self.head.0.load(Ordering::Relaxed);
        let tail = self.tail.0.load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    #[allow(dead_code)]
    fn is_full(&self) -> bool {
        self.len() == self.capacity
    }
}

impl<T> Drop for RingInner<T> {
    fn drop(&mut self) {
        // Drop any initialised but not-yet-consumed slots.
        let head = *self.head.0.get_mut();
        let tail = *self.tail.0.get_mut();
        for i in head..tail {
            let slot = &self.slots[i & self.mask];
            // SAFETY: slot in [head, tail) is initialised.
            unsafe {
                (*slot.get()).assume_init_drop();
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// SPSC lock-free ring buffer.
///
/// Split into `Producer` and `Consumer` halves via `LockFreeRingBuffer::split`.
/// The combined `LockFreeRingBuffer` handle is used for capacity queries and
/// re-export; actual push/pop goes through the split halves.
pub struct LockFreeRingBuffer<T> {
    inner: Arc<RingInner<T>>,
}

impl<T: Send> LockFreeRingBuffer<T> {
    /// Create a ring buffer with at least `capacity` slots (rounded to
    /// next power of two).
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RingInner::new(capacity.max(2))),
        }
    }

    /// Actual slot count (power-of-two ≥ requested capacity).
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Split into a single-producer and single-consumer half.
    ///
    /// Each half holds an `Arc` clone of the shared inner state.
    /// Callers must ensure exactly one `Producer` and one `Consumer`
    /// are live at any time — the type system enforces this via ownership.
    pub fn split(self) -> (Producer<T>, Consumer<T>) {
        let consumer_inner = Arc::clone(&self.inner);
        (
            Producer { inner: self.inner },
            Consumer {
                inner: consumer_inner,
            },
        )
    }
}

/// Single-producer write half.
pub struct Producer<T> {
    inner: Arc<RingInner<T>>,
}

impl<T: Send> Producer<T> {
    /// Push one item.  Returns `Err(item)` if the buffer is full.
    #[inline]
    pub fn push(&self, item: T) -> Result<(), T> {
        let tail = self.inner.tail.0.load(Ordering::Relaxed);
        let head = self.inner.head.0.load(Ordering::Acquire);
        if tail.wrapping_sub(head) == self.inner.capacity {
            return Err(item);
        }
        let slot = &self.inner.slots[tail & self.inner.mask];
        // SAFETY: tail slot is uninitialised; we are the only writer.
        unsafe {
            (*slot.get()).write(item);
        }
        self.inner
            .tail
            .0
            .store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

/// Single-consumer read half.
pub struct Consumer<T> {
    inner: Arc<RingInner<T>>,
}

impl<T: Send> Consumer<T> {
    /// Pop one item.  Returns `None` if the buffer is empty.
    #[inline]
    pub fn pop(&self) -> Option<T> {
        let head = self.inner.head.0.load(Ordering::Relaxed);
        let tail = self.inner.tail.0.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let slot = &self.inner.slots[head & self.inner.mask];
        // SAFETY: head slot is initialised; we are the only reader.
        let item = unsafe { (*slot.get()).assume_init_read() };
        self.inner
            .head
            .0
            .store(head.wrapping_add(1), Ordering::Release);
        Some(item)
    }

    /// Drain up to `limit` items into `out`.
    #[inline]
    pub fn drain_into(&self, out: &mut Vec<T>, limit: usize) {
        let mut drained = 0;
        while drained < limit {
            match self.pop() {
                Some(item) => {
                    out.push(item);
                    drained += 1;
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spsc_roundtrip() {
        let rb = LockFreeRingBuffer::new(8);
        let (tx, rx) = rb.split();
        assert!(tx.push(1u32).is_ok());
        assert!(tx.push(2u32).is_ok());
        assert_eq!(rx.pop(), Some(1));
        assert_eq!(rx.pop(), Some(2));
        assert_eq!(rx.pop(), None);
    }

    #[test]
    fn full_returns_err() {
        let rb = LockFreeRingBuffer::<u32>::new(2); // capacity rounded to 2
        let (tx, _rx) = rb.split();
        assert!(tx.push(1).is_ok());
        assert!(tx.push(2).is_ok());
        assert!(tx.push(3).is_err(), "must reject when full");
    }

    #[test]
    fn drain_into_correct() {
        let rb = LockFreeRingBuffer::new(8);
        let (tx, rx) = rb.split();
        for i in 0u32..5 {
            tx.push(i).unwrap();
        }
        let mut out = Vec::new();
        rx.drain_into(&mut out, 3);
        assert_eq!(out, vec![0, 1, 2]);
        out.clear();
        rx.drain_into(&mut out, 10);
        assert_eq!(out, vec![3, 4]);
    }

    #[test]
    fn capacity_is_power_of_two() {
        let rb = LockFreeRingBuffer::<u8>::new(5);
        assert_eq!(rb.capacity(), 8);
    }
}
