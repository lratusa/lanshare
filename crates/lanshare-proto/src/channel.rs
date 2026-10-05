//! 加密信道：ChaCha20-Poly1305，nonce 由请求计数器构成。
//!
//! 每个方向一把密钥（c2s / s2c），每个请求的计数器唯一，所以 (密钥, nonce) 永不重复——
//! 这是 ChaCha20-Poly1305 安全性的前提。

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use crate::ProtoError;

/// 12 字节 nonce = 4 字节 0 + 8 字节大端计数器。
fn nonce(ctr: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[4..].copy_from_slice(&ctr.to_be_bytes());
    Nonce::from(bytes)
}

/// 加密并附上认证标签。返回 `明文长度 + 16` 字节。
pub fn seal(key: &[u8; 32], ctr: u64, aad: &[u8], plain: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(&nonce(ctr), Payload { msg: plain, aad })
        .expect("ChaCha20-Poly1305 只有在明文超过 256 GiB 时才会加密失败")
}

/// 验证并解密。任何一处不对（密钥、计数器、aad、密文）都返回 [`ProtoError::Decrypt`]。
pub fn open(key: &[u8; 32], ctr: u64, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, ProtoError> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(&nonce(ctr), Payload { msg: sealed, aad })
        .map_err(|_| ProtoError::Decrypt)
}

fn aad(prefix: &str, path: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + path.len());
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(path.as_bytes());
    out
}

/// 请求的附加认证数据：把密文绑定到具体接口，防止挪到别的接口重放。
pub fn req_aad(path: &str) -> Vec<u8> {
    aad("lanshare/2 req ", path)
}

/// 响应的附加认证数据：请求和响应用不同前缀，密文不能互换。
pub fn resp_aad(path: &str) -> Vec<u8> {
    aad("lanshare/2 resp ", path)
}
