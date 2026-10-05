//! 把 `lanshare-proto` 导出给浏览器：裸 C ABI，不依赖 wasm-bindgen。
//!
//! 调用约定（由 `web/proto.js` 遵守）：
//! - 所有指针都指向本模块线性内存里、由 JS 通过 [`ls_alloc`] 申请的缓冲区；
//! - 输入缓冲区长度由参数给出，输出缓冲区的长度由函数文档规定，JS 必须先申请够；
//! - 64 位计数器拆成高低两个 u32 传，兼容不支持 BigInt↔i64 的老浏览器。
//!
//! 浏览器里的 WASM 拿不到操作系统随机源，随机数通过导入函数 `env.ls_random`
//! 由 JS 的 `crypto.getRandomValues` 提供（它在 http 页面里也可用）。

use std::cell::RefCell;

use lanshare_proto as proto;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    /// 由 JS 提供：往 `[ptr, ptr+len)` 写入密码学安全的随机字节。
    fn ls_random(ptr: *mut u8, len: usize);
}

#[cfg(target_arch = "wasm32")]
fn js_random(buf: &mut [u8]) -> Result<(), getrandom::Error> {
    // SAFETY: buf 是一段有效、可写、长度为 buf.len() 的线性内存；JS 端只在这个范围内写入。
    unsafe { ls_random(buf.as_mut_ptr(), buf.len()) };
    Ok(())
}

#[cfg(target_arch = "wasm32")]
getrandom::register_custom_getrandom!(js_random);

/// SAFETY: 调用方保证 `ptr` 指向至少 `len` 字节的有效内存（`len == 0` 时可以是任意值）。
unsafe fn input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: 由本函数的调用约定保证。
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// SAFETY: 调用方保证 `ptr` 指向至少 N 字节的有效内存。
unsafe fn input_array<const N: usize>(ptr: *const u8) -> [u8; N] {
    // SAFETY: 由本函数的调用约定保证；长度正好是 N，try_into 不会失败。
    unsafe { input(ptr, N) }.try_into().expect("长度正好是 N")
}

/// SAFETY: 调用方保证 `ptr` 指向至少 `data.len()` 字节的可写内存。
unsafe fn write_out(ptr: *mut u8, data: &[u8]) {
    if !data.is_empty() {
        // SAFETY: 由本函数的调用约定保证；源与目标不重叠（data 在 Rust 自己的分配里）。
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
    }
}

fn counter(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

/// 申请 `len` 字节，返回指针给 JS 写数据。用完必须用同样的 `len` 调 [`ls_free`]。
#[unsafe(no_mangle)]
pub extern "C" fn ls_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// 释放 [`ls_alloc`] 申请的内存。
///
/// # Safety
/// `ptr` 和 `len` 必须正好来自一次 `ls_alloc(len)`，且只释放一次。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_free(ptr: *mut u8, len: usize) {
    // SAFETY: ptr 来自 Vec::with_capacity(len.max(1))，长度按 0 重建即可，析构时只释放容量。
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}

/// 加密。输出 `plain_len + 16` 字节写到 `out`。
///
/// # Safety
/// `key` 32 字节；`aad`/`plain` 各自有效；`out` 至少 `plain_len + 16` 字节可写。
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ls_seal(
    key: *const u8,
    ctr_hi: u32,
    ctr_lo: u32,
    aad: *const u8,
    aad_len: usize,
    plain: *const u8,
    plain_len: usize,
    out: *mut u8,
) {
    // SAFETY: 由函数的调用约定保证。
    let (key, aad, plain) = unsafe { (input_array::<32>(key), input(aad, aad_len), input(plain, plain_len)) };
    let sealed = proto::seal(&key, counter(ctr_hi, ctr_lo), aad, plain);
    // SAFETY: out 至少 plain_len + 16 = sealed.len() 字节。
    unsafe { write_out(out, &sealed) };
}

/// 解密。成功返回 0 并把 `sealed_len - 16` 字节写到 `out`；失败返回 1，`out` 不变。
///
/// # Safety
/// `key` 32 字节；`aad`/`sealed` 各自有效；`out` 至少 `sealed_len - 16` 字节可写。
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ls_open(
    key: *const u8,
    ctr_hi: u32,
    ctr_lo: u32,
    aad: *const u8,
    aad_len: usize,
    sealed: *const u8,
    sealed_len: usize,
    out: *mut u8,
) -> i32 {
    // SAFETY: 由函数的调用约定保证。
    let (key, aad, sealed) = unsafe { (input_array::<32>(key), input(aad, aad_len), input(sealed, sealed_len)) };
    match proto::open(&key, counter(ctr_hi, ctr_lo), aad, sealed) {
        Ok(plain) => {
            // SAFETY: plain.len() == sealed_len - 16，调用方已按此申请 out。
            unsafe { write_out(out, &plain) };
            0
        }
        Err(_) => 1,
    }
}

/// 二维码握手标签，32 字节写到 `out`。
///
/// # Safety
/// `k` 32 字节，`cid` 16 字节，`out` 32 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_hello_tag(k: *const u8, cid: *const u8, out: *mut u8) {
    // SAFETY: 由函数的调用约定保证。
    unsafe { write_out(out, &proto::hello_tag(&input_array(k), &input_array(cid))) };
}

/// 二维码配对的会话秘密，32 字节写到 `out`。
///
/// # Safety
/// `k` 32 字节，`cid` 16 字节，`out` 32 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_session_from_key(k: *const u8, cid: *const u8, out: *mut u8) {
    // SAFETY: 由函数的调用约定保证。
    unsafe { write_out(out, &proto::session_secret_from_key(&input_array(k), &input_array(cid))) };
}

/// 口令配对的会话秘密，32 字节写到 `out`。
///
/// # Safety
/// `pk` 有 `pk_len` 字节，`cid` 16 字节，`out` 32 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_session_from_pake(pk: *const u8, pk_len: usize, cid: *const u8, out: *mut u8) {
    // SAFETY: 由函数的调用约定保证。
    unsafe { write_out(out, &proto::session_secret_from_pake(input(pk, pk_len), &input_array(cid))) };
}

/// 两把方向密钥：`out` 前 32 字节是 c2s，后 32 字节是 s2c。
///
/// # Safety
/// `ss` 32 字节，`out` 64 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_channel_keys(ss: *const u8, out: *mut u8) {
    // SAFETY: 由函数的调用约定保证。
    let keys = proto::ChannelKeys::derive(&unsafe { input_array(ss) });
    // SAFETY: out 至少 64 字节。
    unsafe {
        write_out(out, &keys.c2s);
        write_out(out.add(32), &keys.s2c);
    }
}

/// PAKE 确认标签，32 字节写到 `out`。
///
/// # Safety
/// 各输入按长度有效，`cid` 16 字节，`out` 32 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_confirm_tag(
    pk: *const u8,
    pk_len: usize,
    role: *const u8,
    role_len: usize,
    cid: *const u8,
    out: *mut u8,
) {
    // SAFETY: 由函数的调用约定保证。
    let (pk, role, cid) = unsafe { (input(pk, pk_len), input(role, role_len), input_array::<16>(cid)) };
    let role = std::str::from_utf8(role).unwrap_or("");
    // SAFETY: out 32 字节可写。
    unsafe { write_out(out, &proto::confirm_tag(pk, role, &cid)) };
}

/// 常量时间校验 PAKE 确认标签：通过返回 1，否则 0。
///
/// # Safety
/// 各输入按长度有效，`cid` 16 字节。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_verify_confirm(
    pk: *const u8,
    pk_len: usize,
    role: *const u8,
    role_len: usize,
    cid: *const u8,
    tag: *const u8,
    tag_len: usize,
) -> i32 {
    // SAFETY: 由函数的调用约定保证。
    let (pk, role, cid, tag) =
        unsafe { (input(pk, pk_len), input(role, role_len), input_array::<16>(cid), input(tag, tag_len)) };
    let role = std::str::from_utf8(role).unwrap_or("");
    i32::from(proto::verify_confirm(pk, role, &cid, tag))
}

thread_local! {
    /// 进行中的 PAKE 状态。WASM 是单线程的，thread_local 就是全局表。
    static PAKES: RefCell<Vec<Option<proto::PakeState>>> = const { RefCell::new(Vec::new()) };
}

/// SPAKE2 消息长度（Ed25519 对称模式：1 字节标识 + 32 字节群元素）。
#[unsafe(no_mangle)]
pub extern "C" fn ls_pake_msg_len() -> usize {
    33
}

/// 开始口令配对：消息写到 `msg_out`（[`ls_pake_msg_len`] 字节），返回句柄（≥1）；输入不是 UTF-8 时返回 0。
///
/// # Safety
/// `pin`/`sid` 按长度有效，`msg_out` 至少 33 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_pake_start(
    pin: *const u8,
    pin_len: usize,
    sid: *const u8,
    sid_len: usize,
    msg_out: *mut u8,
) -> u32 {
    // SAFETY: 由函数的调用约定保证。
    let (pin, sid) = unsafe { (input(pin, pin_len), input(sid, sid_len)) };
    let (Ok(pin), Ok(sid)) = (std::str::from_utf8(pin), std::str::from_utf8(sid)) else {
        return 0;
    };
    let (state, msg) = proto::pake_start(pin, sid, rand_core::OsRng);
    // SAFETY: msg_out 至少 33 字节，msg 正好 33 字节。
    unsafe { write_out(msg_out, &msg) };
    PAKES.with_borrow_mut(|table| {
        table.push(Some(state));
        u32::try_from(table.len()).unwrap_or(0)
    })
}

/// 完成口令配对：成功返回 0 并把 32 字节密钥写到 `key_out`；句柄无效、已用过或消息不对返回 1。
///
/// # Safety
/// `peer` 按长度有效，`key_out` 32 字节可写。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_pake_finish(handle: u32, peer: *const u8, peer_len: usize, key_out: *mut u8) -> i32 {
    let state = PAKES.with_borrow_mut(|table| {
        let index = (handle as usize).checked_sub(1)?;
        table.get_mut(index)?.take()
    });
    let Some(state) = state else { return 1 };
    // SAFETY: 由函数的调用约定保证。
    match proto::pake_finish(state, unsafe { input(peer, peer_len) }) {
        Ok(key) => {
            // SAFETY: key_out 32 字节可写。
            unsafe { write_out(key_out, &key) };
            0
        }
        Err(_) => 1,
    }
}
