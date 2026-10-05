//! 加密信道的性质：任何一处不对都必须解密失败；防重放窗口的边界。

use lanshare_proto::{ProtoError, ReplayWindow, open, req_aad, resp_aad, seal};

const K: [u8; 32] = [7; 32];

#[test]
fn roundtrip_including_empty_and_large() {
    for len in [0usize, 1, 1000, (1 << 20) + 3] {
        let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let sealed = seal(&K, 42, b"aad", &plain);
        assert_eq!(sealed.len(), len + 16, "密文 = 明文 + 16 字节认证标签");
        assert_eq!(open(&K, 42, b"aad", &sealed).unwrap(), plain);
    }
}

#[test]
fn any_mismatch_fails_to_open() {
    let sealed = seal(&K, 5, &req_aad("/api/rpc"), b"secret");
    assert_eq!(open(&[8; 32], 5, &req_aad("/api/rpc"), &sealed), Err(ProtoError::Decrypt), "错 key");
    assert_eq!(open(&K, 6, &req_aad("/api/rpc"), &sealed), Err(ProtoError::Decrypt), "错计数器");
    assert_eq!(open(&K, 5, &req_aad("/api/up/x/0"), &sealed), Err(ProtoError::Decrypt), "挪到别的接口");
    assert_eq!(open(&K, 5, &resp_aad("/api/rpc"), &sealed), Err(ProtoError::Decrypt), "请求当响应");
    for i in 0..sealed.len() {
        let mut tampered = sealed.clone();
        tampered[i] ^= 0x80;
        assert_eq!(open(&K, 5, &req_aad("/api/rpc"), &tampered), Err(ProtoError::Decrypt), "篡改第 {i} 字节");
    }
    assert_eq!(open(&K, 5, &req_aad("/api/rpc"), &sealed[..10]), Err(ProtoError::Decrypt), "截断");
}

#[test]
fn replay_window_rejects_duplicates_and_too_old() {
    let mut w = ReplayWindow::new();
    assert!(!w.check(0), "计数器从 1 开始");
    assert!(w.check(1));
    w.commit(1);
    assert!(!w.check(1), "重复");
    assert!(w.check(3));
    w.commit(3);
    assert!(w.check(2), "乱序但没见过，接受");
    w.commit(2);
    assert!(!w.check(2));
    w.commit(10_000);
    assert!(!w.check(10_000 - 4096), "落在窗口下沿之外");
    assert!(w.check(10_000 - 4095), "窗口内没见过");
    assert!(w.check(u64::MAX - 1));
}

#[test]
fn replay_window_check_does_not_reserve() {
    let mut w = ReplayWindow::new();
    assert!(w.check(7));
    assert!(w.check(7), "只检查不登记：解密失败的请求不能占位");
    w.commit(7);
    assert!(!w.check(7));
}

#[test]
fn replay_window_memory_stays_bounded() {
    let mut w = ReplayWindow::new();
    for ctr in 1..=100_000u64 {
        assert!(w.check(ctr));
        w.commit(ctr);
    }
    assert!(w.tracked() <= 4096, "只记住窗口内的计数器，实际 {}", w.tracked());
}
