//! Basic usage example for rrid.

use rrid::IdPool;

fn main() {
    // Create a pool with 256 IDs and a safety watermark of 32.
    let mut pool = IdPool::new(256, 32).expect("valid pool");

    println!("capacity:  {}, watermark: {}", pool.capacity(), pool.watermark());

    // Allocate some IDs.
    let mut ids = Vec::new();
    for _ in 0..10 {
        match pool.alloc() {
            Ok(id) => {
                println!("allocated id = {id}   (free = {})", pool.free());
                ids.push(id);
            }
            Err(e) => println!("alloc error: {e}"),
        }
    }

    // Release half of them.
    let half = ids.len() / 2;
    for &id in &ids[..half] {
        pool.release(id).unwrap();
        println!("released  id = {id}   (free = {})", pool.free());
    }

    // Allocate again — newly freed IDs are not immediately reused.
    for _ in 0..5 {
        match pool.alloc() {
            Ok(id) => println!("re-allocated id = {id}"),
            Err(e) => println!("alloc error: {e}"),
        }
    }
}
