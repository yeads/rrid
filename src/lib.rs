//! # rrid — Fixed-Capacity Integer ID Pool
//!
//! A fixed-capacity integer ID allocator with **hierarchical bitmap** backend
//! and **round-robin** allocation strategy.
//!
//! - Manages IDs in `[0, n)`.
//! - Maintains a **round-robin** scan pointer `next_id` for allocation.
//! - Enforces a configurable free-ID watermark `m` (safety margin).
//! - Skips full blocks via a secondary bitmap for O(1) amortised scan.
//! - Reclaimed IDs are not immediately reused, reducing identity collision.

use core::fmt;

/// Each block in the hierarchical bitmap covers 64 IDs.
const BLOCK_BITS: usize = 64;

/// Error type for pool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Pool is at watermark — allocation denied.
    PoolFull,
    /// The requested ID is out of range.
    OutOfRange,
    /// The requested ID is already allocated.
    AlreadyAllocated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::PoolFull => write!(f, "pool is at watermark"),
            Error::OutOfRange => write!(f, "id out of range"),
            Error::AlreadyAllocated => write!(f, "id already allocated"),
        }
    }
}

/// Fixed-capacity ID pool backed by a hierarchical bitmap with **round-robin** allocation.
///
/// # Type Parameters
///
/// - `N` — total number of IDs managed by the pool (must be > 0).
/// - `WATERMARK` — minimum number of free IDs to keep (safety margin; must be < N).
///
/// The primary bitmap uses one bit per ID, grouped into 64-bit words.
/// The secondary bitmap is a single `u64` (supports up to 64 blocks = 4096 IDs).
/// `N` must therefore be in `1..=4096`; [`IdPool::new`] returns `None` otherwise.
///
/// # Examples
///
/// ```
/// use rrid::IdPool;
///
/// let mut pool = IdPool::<128, 16>::new().unwrap();
/// let id = pool.alloc().unwrap();
/// assert_eq!(id, 0);
/// pool.release(id).unwrap();
/// ```
pub struct IdPool<const N: usize, const WATERMARK: usize> {
    primary: Vec<u64>,
    secondary: u64,
    /// Round-robin scan pointer — the next ID position to start allocation from.
    next_id: usize,
    allocated: usize,
}

impl<const N: usize, const WATERMARK: usize> IdPool<N, WATERMARK> {
    /// Number of 64-bit words in the primary bitmap.
    const NUM_WORDS: usize = N.div_ceil(BLOCK_BITS);

    /// Upper bound on the number of 64-bit blocks supported by the single
    /// `u64` secondary bitmap.
    const MAX_BLOCKS: usize = 64;

    /// Create a new pool.
    ///
    /// Returns `None` if:
    /// - `N == 0`,
    /// - `N > 4096` (the secondary bitmap only supports up to 64 blocks of
    ///   64 IDs each — see type-level docs), or
    /// - `WATERMARK >= N`.
    pub fn new() -> Option<Self> {
        if N == 0 || WATERMARK >= N || Self::num_blocks() > Self::MAX_BLOCKS {
            return None;
        }
        let num_blocks = Self::num_blocks();
        let secondary_mask = if num_blocks == Self::MAX_BLOCKS {
            u64::MAX
        } else {
            (1u64 << num_blocks) - 1
        };
        Some(Self {
            primary: vec![0u64; Self::NUM_WORDS],
            secondary: secondary_mask,
            next_id: 0,
            allocated: 0,
        })
    }

    /// Number of currently allocated IDs.
    #[inline]
    pub fn allocated(&self) -> usize {
        self.allocated
    }

    /// Number of currently free IDs.
    #[inline]
    pub fn free(&self) -> usize {
        N - self.allocated
    }

    /// Watermark (safety margin).
    #[inline]
    pub fn watermark(&self) -> usize {
        WATERMARK
    }

    /// Total capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        N
    }

    /// Returns `true` if the given ID is currently allocated.
    ///
    /// Returns `false` for IDs outside `[0, N)` so this never panics,
    /// unlike indexing the bitmap directly would.
    #[inline]
    pub fn is_allocated(&self, id: usize) -> bool {
        if id >= N {
            return false;
        }
        (self.primary[id / BLOCK_BITS] >> (id % BLOCK_BITS)) & 1 == 1
    }

    /// Allocate the next available ID using round-robin scanning.
    ///
    /// Scans from `next_id` using the hierarchical bitmap to skip full blocks.
    /// Returns `Err(PoolFull)` when the free count would drop to `WATERMARK`.
    pub fn alloc(&mut self) -> Result<usize, Error> {
        if self.free() <= WATERMARK {
            return Err(Error::PoolFull);
        }

        let total_blocks = Self::num_blocks();
        let start_block = self.next_id / BLOCK_BITS;
        let start_bit = self.next_id % BLOCK_BITS;

        // Phase 1: scan within the starting block from next_id onward.
        if let Some(id) = self.scan_block(start_block, start_bit) {
            self.advance_next(id);
            return Ok(id);
        }

        // Phase 2: scan subsequent blocks (with wrap-around).
        for offset in 1..=total_blocks {
            let block = (start_block + offset) % total_blocks;
            if self.secondary & (1u64 << block) == 0 {
                continue; // block is full
            }
            if let Some(id) = self.scan_block(block, 0) {
                self.advance_next(id);
                return Ok(id);
            }
        }

        Err(Error::PoolFull)
    }

    /// Release (free) a previously allocated ID.
    ///
    /// Does **not** modify `next_id`, so the ID will not be immediately reused.
    pub fn release(&mut self, id: usize) -> Result<(), Error> {
        if id >= N {
            return Err(Error::OutOfRange);
        }
        if !self.is_allocated(id) {
            return Err(Error::AlreadyAllocated);
        }
        self.primary[id / BLOCK_BITS] &= !(1u64 << (id % BLOCK_BITS));
        self.allocated -= 1;
        self.secondary |= 1u64 << (id / BLOCK_BITS);
        Ok(())
    }

    // ------------------------------------------------------------------
    //  Internal helpers
    // ------------------------------------------------------------------

    #[inline]
    fn num_blocks() -> usize {
        N.div_ceil(BLOCK_BITS)
    }

    /// Scan a single 64-bit block starting from `bit_offset` and allocate the
    /// first free bit. Returns the global ID if found.
    fn scan_block(&mut self, block: usize, bit_offset: usize) -> Option<usize> {
        debug_assert!(block < Self::num_blocks());

        if self.secondary & (1u64 << block) == 0 {
            return None;
        }

        let word = self.primary[block];
        let mask = if bit_offset == 0 {
            u64::MAX
        } else {
            u64::MAX << bit_offset
        };

        // Find the first free (zero) bit in `word` at or after `bit_offset`.
        let free = !word & mask;
        if free == 0 {
            return None;
        }
        let trailing = free.trailing_zeros() as usize;
        let id = block * BLOCK_BITS + trailing;
        if id >= N {
            return None;
        }
        self.mark_allocated(block, trailing);
        Some(id)
    }

    #[inline]
    fn mark_allocated(&mut self, block: usize, bit: usize) {
        self.primary[block] |= 1u64 << bit;
        self.allocated += 1;
        if self.primary[block] == u64::MAX {
            self.secondary &= !(1u64 << block);
        }
    }

    #[inline]
    fn advance_next(&mut self, id: usize) {
        self.next_id = (id + 1) % N;
    }
}

impl<const N: usize, const WATERMARK: usize> fmt::Debug for IdPool<N, WATERMARK> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdPool")
            .field("capacity", &N)
            .field("watermark", &WATERMARK)
            .field("allocated", &self.allocated)
            .field("free", &(N - self.allocated))
            .field("next_id", &self.next_id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_alloc_release() {
        let mut pool = IdPool::<8, 2>::new().unwrap();
        assert_eq!(pool.capacity(), 8);
        assert_eq!(pool.watermark(), 2);
        assert_eq!(pool.free(), 8);

        let id0 = pool.alloc().unwrap();
        assert_eq!(id0, 0);
        assert_eq!(pool.allocated(), 1);

        let id1 = pool.alloc().unwrap();
        assert_eq!(id1, 1);

        pool.release(id0).unwrap();
        assert_eq!(pool.free(), 7);

        // next_id is past id1; next alloc should be id2 (not id0 reuse).
        let id2 = pool.alloc().unwrap();
        assert_eq!(id2, 2);
    }

    #[test]
    fn watermark_enforced() {
        let mut pool = IdPool::<4, 2>::new().unwrap();
        let _ = pool.alloc().unwrap(); // 0
        let _ = pool.alloc().unwrap(); // 1
        // free() == 2 == WATERMARK → next alloc should fail.
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
    }

    #[test]
    fn wrap_around() {
        let mut pool = IdPool::<4, 0>::new().unwrap();
        for i in 0..4 {
            assert_eq!(pool.alloc().unwrap(), i);
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));

        pool.release(1).unwrap();
        pool.release(3).unwrap();
        // next_id wrapped to 0, but 0 and 2 are still allocated → should find 1.
        let id = pool.alloc().unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn hierarchical_skip() {
        // 128 IDs → 2 blocks of 64. Fill block 0 completely.
        let mut pool = IdPool::<128, 0>::new().unwrap();
        for _ in 0..64 {
            pool.alloc().unwrap();
        }
        // Block 0 full. next_id = 64 → block 1.
        let id = pool.alloc().unwrap();
        assert_eq!(id, 64);
    }

    #[test]
    fn release_does_not_reuse_immediately() {
        let mut pool = IdPool::<8, 0>::new().unwrap();
        let id0 = pool.alloc().unwrap(); // 0
        let _id1 = pool.alloc().unwrap(); // 1
        pool.release(id0).unwrap();
        // next_id == 2 → alloc returns 2, not 0.
        let id2 = pool.alloc().unwrap();
        assert_eq!(id2, 2);
    }

    #[test]
    fn out_of_range() {
        let mut pool = IdPool::<8, 0>::new().unwrap();
        assert_eq!(pool.release(10), Err(Error::OutOfRange));
    }

    #[test]
    fn double_release() {
        let mut pool = IdPool::<8, 0>::new().unwrap();
        let id = pool.alloc().unwrap();
        pool.release(id).unwrap();
        assert_eq!(pool.release(id), Err(Error::AlreadyAllocated));
    }

    #[test]
    fn full_cycle() {
        let mut pool = IdPool::<64, 0>::new().unwrap();
        // Allocate all 64 IDs.
        for i in 0..64 {
            assert_eq!(pool.alloc().unwrap(), i);
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));

        // Release all.
        for i in 0..64 {
            pool.release(i).unwrap();
        }
        assert_eq!(pool.free(), 64);

        // Re-allocate: starts from next_id (0), finds 0 first.
        assert_eq!(pool.alloc().unwrap(), 0);
    }

    #[test]
    fn large_pool() {
        let mut pool = IdPool::<4096, 100>::new().unwrap();
        // Allocate up to watermark.
        for _ in 0..(4096 - 100) {
            pool.alloc().unwrap();
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
        assert_eq!(pool.free(), 100);
    }

    #[test]
    fn reject_pool_larger_than_4096() {
        // N exceeding the 64-block secondary-bitmap limit must be rejected,
        // otherwise `1u64 << block` would overflow and corrupt state.
        assert!(IdPool::<4097, 0>::new().is_none());
        assert!(IdPool::<8192, 0>::new().is_none());
        // 4096 is exactly 64 blocks and is the maximum allowed.
        assert!(IdPool::<4096, 0>::new().is_some());
    }

    #[test]
    fn boundary_pool_4096_allocates_all() {
        // Regression: 4096 IDs = 64 blocks; the last block must participate
        // in allocation, and filling it must not panic on `1u64 << 64`.
        let mut pool = IdPool::<4096, 0>::new().unwrap();
        for i in 0..4096 {
            assert_eq!(pool.alloc().unwrap(), i);
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
        assert_eq!(pool.free(), 0);
        // Release and realloc the very last ID (id 4095, block 63 high bit).
        pool.release(4095).unwrap();
        assert_eq!(pool.alloc().unwrap(), 4095);
    }

    #[test]
    fn is_allocated_out_of_range_is_false() {
        let pool = IdPool::<8, 0>::new().unwrap();
        // Must not panic; out-of-range IDs are reported as "not allocated".
        assert!(!pool.is_allocated(8));
        assert!(!pool.is_allocated(usize::MAX));
    }
}
