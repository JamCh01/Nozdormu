/// Lightweight thread-local xorshift64 random number generator.
use std::cell::Cell;
use std::time::SystemTime;

thread_local! {
    static STATE: Cell<u64> = Cell::new({
        let time_seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // Mix in thread ID to decorrelate threads starting at the same time
        let thread_id = std::thread::current().id();
        let tid = format!("{:?}", thread_id);
        let tid_hash = tid.bytes().fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
        let seed = time_seed ^ tid_hash;
        // Ensure seed is never zero (xorshift produces permanent zero output for zero seed)
        if seed == 0 { 0xdeadbeef_cafebabe } else { seed }
    });
}

fn next_u64() -> u64 {
    STATE.with(|s| {
        let mut state = s.get();
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        s.set(state);
        state
    })
}

pub fn u32(range: std::ops::RangeTo<u32>) -> u32 {
    if range.end == 0 {
        return 0;
    }
    (next_u64() as u32) % range.end
}

pub fn usize(range: std::ops::RangeTo<usize>) -> usize {
    if range.end == 0 {
        return 0;
    }
    (next_u64() as usize) % range.end
}

pub fn u64(range: std::ops::RangeTo<u64>) -> u64 {
    if range.end == 0 {
        return 0;
    }
    next_u64() % range.end
}
