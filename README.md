# rrid

[English](./README.md) | [中文](./README_CN.md)

A fixed-capacity integer ID pool with a **hierarchical bitmap** backend and **round-robin** allocation strategy, designed for high-frequency resource management scenarios.

## Features

- **Fixed capacity** — manages IDs in the range `[0, N)`.
- **Round-robin allocation** — a scan pointer (`next_id`) wraps around the ring in round-robin fashion, delaying reuse of recently freed IDs.
- **Safety watermark** — enforces a configurable minimum number of free IDs (`WATERMARK`).
- **Hierarchical bitmap** — two-level bitmap enables O(1) amortised allocation by skipping full blocks.
- **Zero-cost generics** — capacity and watermark are const-generic parameters resolved at compile time.

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
rrid = "0.1"
```

```rust
use rrid::IdPool;

fn main() {
    let mut pool = IdPool::<1024, 64>::new().expect("valid pool");

    let id = pool.alloc().expect("pool not full");
    println!("allocated id = {id}");

    pool.release(id).expect("valid id");
    println!("released id = {id}, free = {}", pool.free());
}
```

## How It Works

| Layer | Purpose |
|-------|---------|
| **Primary bitmap** | One bit per ID; 1 = allocated, 0 = free. Grouped into 64-bit words. |
| **Secondary bitmap** | One bit per 64-ID block; 1 = block has ≥1 free bit. |
| **Scan pointer** (`next_id`) | Round-robin index; allocation starts here and skips full blocks via the secondary bitmap. |

Allocation:

1. If `free() <= WATERMARK`, return `PoolFull`.
2. Starting from `next_id`, check the current block.
3. If the block is full (secondary bit = 0), skip to the next block with a free bit.
4. Use `trailing_zeros` on the inverted primary word to locate the first free bit.
5. Set the bit, update the secondary bitmap if the block becomes full, advance `next_id`.

Release:

1. Clear the bit in the primary bitmap.
2. Set the corresponding secondary bit (block now has free space).
3. `next_id` is **not** modified, so the ID is not reused immediately.

## API

| Method | Description |
|--------|-------------|
| `IdPool::<N, W>::new()` | Create a new pool. Returns `None` if constraints are unsatisfiable. |
| `alloc() -> Result<usize, Error>` | Allocate the next free ID. |
| `release(id) -> Result<(), Error>` | Free a previously allocated ID. |
| `allocated() -> usize` | Number of allocated IDs. |
| `free() -> usize` | Number of free IDs. |
| `capacity() -> usize` | Total capacity `N`. |
| `watermark() -> usize` | Safety margin `W`. |
| `is_allocated(id) -> bool` | Check if an ID is currently allocated. |

## License

[MIT](./LICENSE)
