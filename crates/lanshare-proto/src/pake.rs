//! 口令配对：SPAKE2（对称模式，Ed25519 群）。
//!
//! 6 位口令只有一百万种可能，直接拿来派生密钥的话，抓包的人可以离线把一百万种全试一遍。
//! SPAKE2 让每一次猜测都必须和服务端真实交互一次，抓到的消息里没有可供离线验证的信息。

use rand_core::{CryptoRng, RngCore};
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::ProtoError;

/// 一方发出消息后、收到对方消息前的状态。只能用一次（[`pake_finish`] 按值拿走它）。
pub struct PakeState(Spake2<Ed25519Group>);

/// 开始配对：返回本方状态和要发给对方的消息。
///
/// `rng` 由调用方提供：服务端用操作系统随机源，浏览器里由 JS 的 `crypto.getRandomValues` 提供。
pub fn pake_start(
    pin: &str,
    server_id: &str,
    rng: impl CryptoRng + RngCore,
) -> (PakeState, Vec<u8>) {
    let identity = format!("lanshare/2|{server_id}");
    let (state, msg) = Spake2::<Ed25519Group>::start_symmetric_with_rng(
        &Password::new(pin.as_bytes()),
        &Identity::new(identity.as_bytes()),
        rng,
    );
    (PakeState(state), msg)
}

/// 用对方的消息完成配对，得到 32 字节共享密钥。口令不同时也会“成功”，但两边的密钥不一样，
/// 所以之后还要互相出示确认标签（见 `confirm_tag`）。
pub fn pake_finish(state: PakeState, peer_msg: &[u8]) -> Result<[u8; 32], ProtoError> {
    let key = state.0.finish(peer_msg).map_err(|_| ProtoError::Pake)?;
    key.try_into().map_err(|_| ProtoError::Pake)
}
