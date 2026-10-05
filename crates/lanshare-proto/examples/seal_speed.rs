//! 原生（服务端这一侧）加解密速度：按 1 MiB 一块，加密再解密 256 MiB。
//!
//! 运行：cargo run -p lanshare-proto --example seal_speed --release

use std::time::Instant;

use lanshare_proto::{CHUNK, open, req_aad, seal};

const TOTAL_MIB: usize = 256;

fn main() {
    let key = [7u8; 32];
    let aad = req_aad("/api/up/x/0");
    let plain = vec![0x5au8; CHUNK];

    // 先热身一轮，让 CPU 升频、缓存就位
    let warm = seal(&key, 0, &aad, &plain);
    assert_eq!(open(&key, 0, &aad, &warm).unwrap(), plain);

    let started = Instant::now();
    let mut sealed = Vec::with_capacity(TOTAL_MIB);
    for ctr in 0..TOTAL_MIB as u64 {
        sealed.push(seal(&key, ctr, &aad, &plain));
    }
    let seal_secs = started.elapsed().as_secs_f64();

    let started = Instant::now();
    for (ctr, block) in sealed.iter().enumerate() {
        let back = open(&key, ctr as u64, &aad, block).expect("解密失败");
        assert_eq!(back.len(), CHUNK);
    }
    let open_secs = started.elapsed().as_secs_f64();

    println!("seal {:.0} MiB/s, open {:.0} MiB/s ({TOTAL_MIB} MiB, 1 MiB chunks)", TOTAL_MIB as f64 / seal_secs, TOTAL_MIB as f64 / open_secs);
}
