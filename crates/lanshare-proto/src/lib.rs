//! LanShare 协议与加密核心：服务端（原生）和浏览器（WASM）共用同一份代码。
//!
//! 协议细节见 `docs/superpowers/specs/2026-10-02-lanshare-v2-rust-design.md` §3–§4。
//! 这里的每一个常量和标签字符串都是协议的一部分，改动会让两端互不相认。

mod channel;
mod filename;
mod handshake;
mod pake;
mod replay;

pub use channel::{open, req_aad, resp_aad, seal};
pub use filename::sanitize_filename;
pub use handshake::{
    ChannelKeys, confirm_tag, hello_tag, session_secret_from_key, session_secret_from_pake,
    verify_confirm, verify_hello,
};
pub use pake::{PakeState, pake_finish, pake_start};
pub use replay::{REPLAY_WINDOW, ReplayWindow};

/// 协议版本，出现在 HKDF 的 info 和 AEAD 的 aad 里。
pub const PROTOCOL: &str = "lanshare/2";
/// 分块上传/下载的块大小。
pub const CHUNK: usize = 1 << 20;
/// 一次 RPC 请求明文的上限。
pub const MAX_RPC: usize = 64 * 1024;
/// ChaCha20-Poly1305 认证标签长度：密文 = 明文 + 16 字节。
pub const TAG_LEN: usize = 16;

/// 协议层面的错误。刻意不区分“密钥错”“被篡改”“计数器错”：对攻击者一律只说解不开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoError {
    /// AEAD 认证失败：密钥、计数器、aad 或密文任意一处不对。
    Decrypt,
    /// PAKE 对端消息格式不对。
    Pake,
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Decrypt => write!(f, "解密失败"),
            ProtoError::Pake => write!(f, "口令配对消息无效"),
        }
    }
}

impl std::error::Error for ProtoError {}
