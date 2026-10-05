//! 与独立实现（Node.js 内置 crypto / OpenSSL）算出的已知向量比对。
//! 向量由 `web-tests/gen-vectors.mjs` 生成，存在 `tests/vectors.json`。

use lanshare_proto::{
    ChannelKeys, confirm_tag, hello_tag, open, req_aad, resp_aad, seal, session_secret_from_key,
    session_secret_from_pake, verify_confirm, verify_hello,
};

fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors.json")).unwrap()
}

fn bytes(v: &serde_json::Value, name: &str) -> Vec<u8> {
    hex::decode(v[name].as_str().unwrap()).unwrap()
}

fn key(v: &serde_json::Value) -> [u8; 32] {
    bytes(v, "key").try_into().unwrap()
}

fn cid(v: &serde_json::Value) -> [u8; 16] {
    bytes(v, "cid").try_into().unwrap()
}

#[test]
fn hello_tag_matches_openssl() {
    let v = vectors();
    assert_eq!(hello_tag(&key(&v), &cid(&v)).to_vec(), bytes(&v, "hello_tag"));
}

#[test]
fn verify_hello_accepts_only_the_right_tag() {
    let v = vectors();
    let good = bytes(&v, "hello_tag");
    assert!(verify_hello(&key(&v), &cid(&v), &good));
    let mut bad = good.clone();
    bad[0] ^= 1;
    assert!(!verify_hello(&key(&v), &cid(&v), &bad));
    assert!(!verify_hello(&key(&v), &cid(&v), &good[..31]));
    assert!(!verify_hello(&key(&v), &cid(&v), &[]));
}

#[test]
fn session_secrets_match_openssl() {
    let v = vectors();
    assert_eq!(session_secret_from_key(&key(&v), &cid(&v)).to_vec(), bytes(&v, "session_from_key"));
    assert_eq!(session_secret_from_pake(&[0x55; 32], &cid(&v)).to_vec(), bytes(&v, "session_from_pake"));
}

#[test]
fn channel_keys_match_openssl() {
    let v = vectors();
    let ss: [u8; 32] = bytes(&v, "session_from_key").try_into().unwrap();
    let keys = ChannelKeys::derive(&ss);
    assert_eq!(keys.c2s.to_vec(), bytes(&v, "c2s"));
    assert_eq!(keys.s2c.to_vec(), bytes(&v, "s2c"));
    assert_ne!(keys.c2s, keys.s2c);
}

#[test]
fn confirm_tags_match_openssl_and_differ_by_role() {
    let v = vectors();
    let c = cid(&v);
    assert_eq!(confirm_tag(&[0x55; 32], "server", &c).to_vec(), bytes(&v, "confirm_server"));
    assert_eq!(confirm_tag(&[0x55; 32], "client", &c).to_vec(), bytes(&v, "confirm_client"));
    assert!(verify_confirm(&[0x55; 32], "client", &c, &bytes(&v, "confirm_client")));
    assert!(!verify_confirm(&[0x55; 32], "server", &c, &bytes(&v, "confirm_client")));
}

#[test]
fn aad_layout() {
    assert_eq!(req_aad("/api/rpc"), b"lanshare/2 req /api/rpc".to_vec());
    assert_eq!(resp_aad("/api/down/x/0"), b"lanshare/2 resp /api/down/x/0".to_vec());
}

#[test]
fn seal_matches_openssl() {
    let v = vectors();
    let k = key(&v);
    assert_eq!(seal(&k, 1, &req_aad("/api/rpc"), b"hello LanShare"), bytes(&v, "sealed_rpc"));
    assert_eq!(
        seal(&k, 0x0102030405060708, &resp_aad("/api/down/x/0"), "局域网快传".as_bytes()),
        bytes(&v, "sealed_big_ctr")
    );
}

#[test]
fn open_reverses_openssl_ciphertext() {
    let v = vectors();
    let plain = open(&key(&v), 1, &req_aad("/api/rpc"), &bytes(&v, "sealed_rpc")).unwrap();
    assert_eq!(plain, b"hello LanShare");
}
