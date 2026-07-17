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
    /// The requested ID is not allocated.
    NotAllocated,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::PoolFull => write!(f, "pool is at watermark"),
            Error::OutOfRange => write!(f, "id out of range"),
            Error::NotAllocated => write!(f, "id not allocated"),
        }
    }
}

impl std::error::Error for Error {}

/// Fixed-capacity ID pool backed by a hierarchical bitmap with **round-robin** allocation.
///
/// Created with [`IdPool::new`] by passing the capacity `n` and watermark `m`
/// at runtime. The primary bitmap uses one bit per ID, grouped into 64-bit
/// words. The secondary bitmap uses one bit per 64-ID block (itself stored as
/// 64-bit words) so the pool can scale to arbitrary capacities.
///
/// # Examples
///
/// ```
/// use rrid::IdPool;
///
/// let mut pool = IdPool::new(128, 16).unwrap();
/// let id = pool.alloc().unwrap();
/// assert_eq!(id, 0);
/// pool.release(id).unwrap();
/// ```
pub struct IdPool {
    n: usize,
    watermark: usize,
    primary: Vec<u64>,
    /// Secondary bitmap: one bit per 64-ID block; grouped into 64-bit words.
    secondary: Vec<u64>,
    /// Round-robin scan pointer — the next ID position to start allocation from.
    next_id: usize,
    allocated: usize,
}

impl IdPool {
    /// Number of 64-bit words in the primary bitmap.
    #[inline]
    fn num_words(n: usize) -> usize {
        n.div_ceil(BLOCK_BITS)
    }

    /// Number of 64-ID blocks covered by a pool of capacity `n`.
    #[inline]
    fn num_blocks(n: usize) -> usize {
        n.div_ceil(BLOCK_BITS)
    }

    /// Number of 64-bit words in the secondary bitmap.
    #[inline]
    fn num_secondary_words(n: usize) -> usize {
        Self::num_blocks(n).div_ceil(BLOCK_BITS)
    }

    /// Create a new pool with capacity `n` and watermark `m`.
    ///
    /// Returns `None` if:
    /// - `n == 0`,
    /// - `m >= n`, or
    /// - the required allocation overflowed `usize`.
    pub fn new(n: usize, watermark: usize) -> Option<Self> {
        if n == 0 || watermark >= n {
            return None;
        }
        let num_blocks = Self::num_blocks(n);
        let num_secondary_words = Self::num_secondary_words(n);
        // Guard against pathological usize overflow.
        let num_words = Self::num_words(n);
        num_words.checked_add(num_secondary_words)?;

        let primary = vec![0u64; num_words];

        // Mark every existing block as "has free" in the secondary bitmap.
        let mut secondary = vec![0u64; num_secondary_words];
        let full_secondary_words = num_blocks / BLOCK_BITS;
        for word in secondary.iter_mut().take(full_secondary_words) {
            *word = u64::MAX;
        }
        let rem = (num_blocks % BLOCK_BITS) as u32;
        if rem != 0 {
            secondary[full_secondary_words] = (1u64 << rem) - 1;
        }

        Some(Self {
            n,
            watermark,
            primary,
            secondary,
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
        self.n - self.allocated
    }

    /// Watermark (safety margin).
    #[inline]
    pub fn watermark(&self) -> usize {
        self.watermark
    }

    /// Total capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.n
    }

    /// Returns `true` if the given ID is currently allocated.
    ///
    /// Returns `false` for IDs outside `[0, n)` so this never panics,
    /// unlike indexing the bitmap directly would.
    #[inline]
    pub fn is_allocated(&self, id: usize) -> bool {
        if id >= self.n {
            return false;
        }
        (self.primary[id / BLOCK_BITS] >> (id % BLOCK_BITS)) & 1 == 1
    }

    /// Allocate the next available ID using round-robin scanning.
    ///
    /// Scans from `next_id` using the hierarchical bitmap to skip full blocks.
    /// Returns `Err(PoolFull)` when the free count would drop to `watermark`.
    pub fn alloc(&mut self) -> Result<usize, Error> {
        if self.free() <= self.watermark {
            return Err(Error::PoolFull);
        }

        let total_blocks = Self::num_blocks(self.n);
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
            if !self.block_has_free(block) {
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
        if id >= self.n {
            return Err(Error::OutOfRange);
        }
        if !self.is_allocated(id) {
            return Err(Error::NotAllocated);
        }
        self.primary[id / BLOCK_BITS] &= !(1u64 << (id % BLOCK_BITS));
        self.allocated -= 1;
        self.set_block_has_free(id / BLOCK_BITS, true);
        Ok(())
    }

    // ------------------------------------------------------------------
    //  Internal helpers — secondary bitmap access
    // ------------------------------------------------------------------

    #[inline]
    fn block_has_free(&self, block: usize) -> bool {
        (self.secondary[block / BLOCK_BITS] >> (block % BLOCK_BITS)) & 1 == 1
    }

    #[inline]
    fn set_block_has_free(&mut self, block: usize, value: bool) {
        let word = block / BLOCK_BITS;
        let bit = block % BLOCK_BITS;
        if value {
            self.secondary[word] |= 1u64 << bit;
        } else {
            self.secondary[word] &= !(1u64 << bit);
        }
    }

    // ------------------------------------------------------------------
    //  Internal helpers — scanning
    // ------------------------------------------------------------------

    /// Scan a single 64-bit block starting from `bit_offset` and allocate the
    /// first free bit. Returns the global ID if found.
    fn scan_block(&mut self, block: usize, bit_offset: usize) -> Option<usize> {
        debug_assert!(block < Self::num_blocks(self.n));

        if !self.block_has_free(block) {
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
        if id >= self.n {
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
            self.set_block_has_free(block, false);
        }
    }

    #[inline]
    fn advance_next(&mut self, id: usize) {
        self.next_id = (id + 1) % self.n;
    }
}

impl fmt::Debug for IdPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdPool")
            .field("capacity", &self.n)
            .field("watermark", &self.watermark)
            .field("allocated", &self.allocated)
            .field("free", &(self.n - self.allocated))
            .field("next_id", &self.next_id)
            .finish()
    }
}

impl Default for IdPool {
    fn default() -> Self {
        Self::new(1, 0).expect("default IdPool is well-formed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_alloc_release() {
        let mut pool = IdPool::new(8, 2).unwrap();
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
        let mut pool = IdPool::new(4, 2).unwrap();
        let _ = pool.alloc().unwrap(); // 0
        let _ = pool.alloc().unwrap(); // 1
        // free() == 2 == WATERMARK → next alloc should fail.
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
    }

    #[test]
    fn wrap_around() {
        let mut pool = IdPool::new(4, 0).unwrap();
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
        let mut pool = IdPool::new(128, 0).unwrap();
        for _ in 0..64 {
            pool.alloc().unwrap();
        }
        // Block 0 full. next_id = 64 → block 1.
        let id = pool.alloc().unwrap();
        assert_eq!(id, 64);
    }

    #[test]
    fn release_does_not_reuse_immediately() {
        let mut pool = IdPool::new(8, 0).unwrap();
        let id0 = pool.alloc().unwrap(); // 0
        let _id1 = pool.alloc().unwrap(); // 1
        pool.release(id0).unwrap();
        // next_id == 2 → alloc returns 2, not 0.
        let id2 = pool.alloc().unwrap();
        assert_eq!(id2, 2);
    }

    #[test]
    fn out_of_range() {
        let mut pool = IdPool::new(8, 0).unwrap();
        assert_eq!(pool.release(10), Err(Error::OutOfRange));
    }

    #[test]
    fn double_release() {
        let mut pool = IdPool::new(8, 0).unwrap();
        let id = pool.alloc().unwrap();
        pool.release(id).unwrap();
        assert_eq!(pool.release(id), Err(Error::NotAllocated));
    }

    #[test]
    fn full_cycle() {
        let mut pool = IdPool::new(64, 0).unwrap();
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
    fn large_pool_within_single_secondary_word() {
        let mut pool = IdPool::new(4096, 100).unwrap();
        // Allocate up to watermark.
        for _ in 0..(4096 - 100) {
            pool.alloc().unwrap();
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
        assert_eq!(pool.free(), 100);
    }

    #[test]
    fn rejects_invalid_params() {
        assert!(IdPool::new(0, 0).is_none());
        assert!(IdPool::new(8, 8).is_none());
        assert!(IdPool::new(8, 10).is_none());
        // Boundary: watermark just below capacity is valid.
        assert!(IdPool::new(8, 7).is_some());
    }

    #[test]
    fn boundary_pool_4096_allocates_all() {
        // 4096 IDs = 64 blocks; the last block must participate in allocation,
        // and filling it must not panic on secondary-bitmap access.
        let mut pool = IdPool::new(4096, 0).unwrap();
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
    fn pool_larger_than_4096_supported() {
        // Regression for the previous 4096 single-u64 secondary limit.
        // 8192 IDs => 128 blocks => 2 secondary words.
        let mut pool = IdPool::new(8192, 0).unwrap();
        for i in 0..8192 {
            assert_eq!(pool.alloc().unwrap(), i);
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
        // Release an ID in block 64 (which lives in secondary word 1).
        let id = 64 * BLOCK_BITS;
        pool.release(id).unwrap();
        assert_eq!(pool.alloc().unwrap(), id);
    }

    #[test]
    fn pool_crossing_secondary_word_boundary() {
        // 5000 IDs → 79 blocks → 2 secondary words. The high word holds 15 bits.
        let mut pool = IdPool::new(5000, 0).unwrap();
        for i in 0..5000 {
            assert_eq!(pool.alloc().unwrap(), i);
        }
        assert_eq!(pool.alloc(), Err(Error::PoolFull));
        //release the very last block's first id.
        let last_block = 5000 / BLOCK_BITS; // block 78
        let id = last_block * BLOCK_BITS; // 4992... actually 78*64 = 4992
        pool.release(id).unwrap();
        assert_eq!(pool.alloc().unwrap(), id);
    }

    #[test]
    fn is_allocated_out_of_range_is_false() {
        let pool = IdPool::new(8, 0).unwrap();
        // Must not panic; out-of-range IDs are reported as "not allocated".
        assert!(!pool.is_allocated(8));
        assert!(!pool.is_allocated(usize::MAX));
    }
}