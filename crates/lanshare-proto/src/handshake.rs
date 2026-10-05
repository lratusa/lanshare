//! 配对握手与密钥派生（HKDF-SHA256 / HMAC-SHA256）。

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// HKDF-SHA256 派生 32 字节。`salt = None` 等价于 32 字节全 0 的盐（RFC 5869）。
fn hkdf32(ikm: &[u8], salt: Option<&[u8]>, info: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(salt, ikm)
        .expand(info.as_bytes(), &mut out)
        .expect("32 字节远小于 HKDF-SHA256 的输出上限");
    out
}

fn mac(key: &[u8], parts: &[&[u8]]) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度的密钥");
    for part in parts {
        m.update(part);
    }
    m
}

fn hello_key(k: &[u8; 32]) -> [u8; 32] {
    hkdf32(k, None, "lanshare/2 hello")
}

/// 二维码配对的握手标签：证明“我知道二维码里的密钥 K”，但不暴露 K。
pub fn hello_tag(k: &[u8; 32], cid: &[u8; 16]) -> [u8; 32] {
    mac(&hello_key(k), &[b"hello", cid]).finalize().into_bytes().into()
}

/// 常量时间校验握手标签（长度不对直接不通过）。
pub fn verify_hello(k: &[u8; 32], cid: &[u8; 16], tag: &[u8]) -> bool {
    mac(&hello_key(k), &[b"hello", cid]).verify_slice(tag).is_ok()
}

/// 二维码配对后的会话秘密：每个会话（cid）一份，由 K 派生。
pub fn session_secret_from_key(k: &[u8; 32], cid: &[u8; 16]) -> [u8; 32] {
    hkdf32(k, Some(cid), "lanshare/2 session")
}

/// 口令（SPAKE2）配对后的会话秘密。
pub fn session_secret_from_pake(pake_key: &[u8], cid: &[u8; 16]) -> [u8; 32] {
    hkdf32(pake_key, Some(cid), "lanshare/2 session")
}

/// 一个会话的两把方向密钥。
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelKeys {
    /// 客户端 → 服务端
    pub c2s: [u8; 32],
    /// 服务端 → 客户端
    pub s2c: [u8; 32],
}

impl ChannelKeys {
    pub fn derive(session_secret: &[u8; 32]) -> Self {
        Self {
            c2s: hkdf32(session_secret, None, "lanshare/2 c2s"),
            s2c: hkdf32(session_secret, None, "lanshare/2 s2c"),
        }
    }
}

// 故意不派生 Debug：免得哪天被 {:?} 打进日志。
impl std::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelKeys(<redacted>)")
    }
}

/// PAKE 的确认标签：双方证明自己算出了同一把钥匙。`role` 为 "server" 或 "client"。
pub fn confirm_tag(pake_key: &[u8], role: &str, cid: &[u8; 16]) -> [u8; 32] {
    mac(pake_key, &[role.as_bytes(), cid]).finalize().into_bytes().into()
}

/// 常量时间校验确认标签。
pub fn verify_confirm(pake_key: &[u8], role: &str, cid: &[u8; 16], tag: &[u8]) -> bool {
    mac(pake_key, &[role.as_bytes(), cid]).verify_slice(tag).is_ok()
}
