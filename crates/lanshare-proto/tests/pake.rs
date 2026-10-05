//! SPAKE2 口令配对：同口令两端得到同一把钥匙，错口令得到不同的钥匙。

use lanshare_proto::{pake_finish, pake_start};
use rand_core::OsRng;

#[test]
fn same_pin_same_key() {
    let (a, msg_a) = pake_start("123456", "server-1", OsRng);
    let (b, msg_b) = pake_start("123456", "server-1", OsRng);
    let ka = pake_finish(a, &msg_b).unwrap();
    let kb = pake_finish(b, &msg_a).unwrap();
    assert_eq!(ka, kb);
}

#[test]
fn wrong_pin_different_key() {
    let (a, msg_a) = pake_start("123456", "server-1", OsRng);
    let (b, msg_b) = pake_start("654321", "server-1", OsRng);
    assert_ne!(pake_finish(a, &msg_b).unwrap(), pake_finish(b, &msg_a).unwrap());
}

#[test]
fn identity_binds_the_server() {
    let (a, msg_a) = pake_start("123456", "server-1", OsRng);
    let (b, msg_b) = pake_start("123456", "server-2", OsRng);
    assert_ne!(pake_finish(a, &msg_b).unwrap(), pake_finish(b, &msg_a).unwrap());
}

#[test]
fn messages_do_not_reveal_the_pin_and_are_fresh() {
    let (_, m1) = pake_start("123456", "s", OsRng);
    let (_, m2) = pake_start("123456", "s", OsRng);
    assert_ne!(m1, m2, "每次随机，抓到的消息不能复用");
    assert!(!m1.windows(6).any(|w| w == b"123456"));
}

#[test]
fn garbage_peer_message_is_an_error() {
    let (a, _) = pake_start("123456", "s", OsRng);
    assert!(pake_finish(a, b"short").is_err());
}
