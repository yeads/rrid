# rrid

[English](./README.md) | [中文](./README_CN.md)

基于**两级 Bitmap** 和 **Round-Robin 分配策略**的固定容量整数 ID 池，适用于高频资源管理场景。

## 特性

- **固定容量** — 管理 `[0, N)` 范围内的整数 ID。
- **Round-Robin 循环分配** — 扫描指针（`next_id`）以 Round-Robin 方式在环形空间中递增，延迟复用已释放的 ID。
- **安全水位** — 可配置最低空闲 ID 数量（`WATERMARK`），拒绝低于水位的分配。
- **两级 Bitmap** — 一级 Bitmap 记录每个 ID 的占用状态，二级 Bitmap 按 64 个 ID 分组标记是否存在空闲位，实现 O(1) 均摊分配。
- **运行时配置** — 容量和水位通过 `new(n, watermark)` 在运行时指定，容量不再受 4096 限制。

## 快速开始

在 `Cargo.toml` 中添加：

```toml
[dependencies]
rrid = "0.1"
```

```rust
use rrid::IdPool;

fn main() {
    let mut pool = IdPool::new(1024, 64).expect("valid pool");

    let id = pool.alloc().expect("pool not full");
    println!("分配 ID = {id}");

    pool.release(id).expect("valid id");
    println!("释放 ID = {id}，空闲 = {}", pool.free());
}
```

## 工作原理

| 层级 | 作用 |
|------|------|
| **一级 Bitmap** | 每个 ID 对应一个 bit；1 = 已分配，0 = 空闲。按 64 bit 分组。 |
| **二级 Bitmap** | 每个 64-ID 块对应一个 bit；1 = 块内存在空闲位。 |
| **扫描指针** (`next_id`) | Round-Robin 循环索引；分配从此处开始，利用二级 Bitmap 跳过满块。 |

分配流程：

1. 若 `free() <= WATERMARK`，返回 `PoolFull`。
2. 从 `next_id` 开始检查当前块。
3. 若块已满（二级 bit = 0），跳至下一个有空闲的块。
4. 对一级 word 取反后使用 `trailing_zeros` 定位第一个空闲 bit。
5. 置位，若块变满则清除对应二级 bit，推进 `next_id`。

释放流程：

1. 清除一级 Bitmap 中对应 bit。
2. 设置对应二级 bit（块重新拥有空闲空间）。
3. **不修改** `next_id`，因此该 ID 不会被立即复用。

## API

| 方法 | 说明 |
|------|------|
| `IdPool::new(n, m)` | 创建容量为 `n`、水位为 `m` 的新池。约束不满足时返回 `None`。 |
| `alloc() -> Result<usize, Error>` | 分配下一个空闲 ID。 |
| `release(id) -> Result<(), Error>` | 释放已分配的 ID。 |
| `allocated() -> usize` | 当前已分配数量。 |
| `free() -> usize` | 当前空闲数量。 |
| `capacity() -> usize` | 总容量 `N`。 |
| `watermark() -> usize` | 安全水位 `W`。 |
| `is_allocated(id) -> bool` | 查询指定 ID 是否已分配。 |

## 许可证

[MIT](./LICENSE)
