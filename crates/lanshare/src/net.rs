//! 绑定监听端口：被占用就往后找，并且在 Windows 上独占端口。
//!
//! Windows 的坑（v1 Python 版两轮审查都踩到过）：
//! - 默认允许“通配地址 0.0.0.0:P”和别的程序的“127.0.0.1:P”同时存在，发往 127.0.0.1 的请求会进别人的程序；
//! - 开了 SO_REUSEADDR 时，第二个进程甚至能悄悄绑上同一个端口。
//!
//! 解决办法：绑定前设置 SO_EXCLUSIVEADDRUSE（实测不影响程序重启后立刻重绑同一端口）。

use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};

use socket2::{Domain, Protocol, Socket, Type};

/// 端口被占用时最多往后试几个。
pub const PORT_TRIES: u16 = 10;

#[cfg(windows)]
fn exclusive(socket: &Socket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SO_EXCLUSIVEADDRUSE, SOCKET, SOL_SOCKET, setsockopt};
    let on: i32 = 1;
    // SAFETY: socket 是一个有效的套接字句柄；optval 指向一个活着的 i32，长度正好 4 字节。
    let rc = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&on as *const i32).cast(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(not(windows))]
fn exclusive(_socket: &Socket) -> io::Result<()> {
    Ok(()) // 其他系统默认就不允许这种共存
}

fn bind_one(ip: Ipv4Addr, port: u16) -> io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    exclusive(&socket)?;
    socket.bind(&SocketAddr::from((ip, port)).into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?; // 交给 tokio 之前必须是非阻塞的
    Ok(socket.into())
}

/// 在 `ip` 上从 `port` 开始依次尝试，返回第一个绑定成功的监听器。`port == 0` 时由系统分配。
pub fn bind_with_fallback(ip: Ipv4Addr, port: u16) -> io::Result<TcpListener> {
    if port == 0 {
        return bind_one(ip, 0);
    }
    let mut last = None;
    for candidate in port..=port.saturating_add(PORT_TRIES - 1) {
        match bind_one(ip, candidate) {
            Ok(listener) => return Ok(listener),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("没有可用端口")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port_of(l: &TcpListener) -> u16 {
        l.local_addr().unwrap().port()
    }

    #[test]
    fn port_zero_gets_a_random_port() {
        let l = bind_with_fallback(Ipv4Addr::LOCALHOST, 0).unwrap();
        assert_ne!(port_of(&l), 0);
    }

    #[test]
    fn second_instance_moves_to_the_next_port() {
        let first = bind_with_fallback(Ipv4Addr::UNSPECIFIED, 0).unwrap();
        let p = port_of(&first);
        let second = bind_with_fallback(Ipv4Addr::UNSPECIFIED, p).unwrap();
        assert_ne!(port_of(&second), p, "不能和第一个实例共用端口");
    }

    #[test]
    fn port_taken_on_a_specific_address_is_skipped() {
        // 别的程序（比如 Django runserver）只占了 127.0.0.1:P —— 用的是普通绑定，没有独占
        let blocker = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let p = port_of(&blocker);
        let ours = bind_with_fallback(Ipv4Addr::UNSPECIFIED, p).unwrap();
        assert_ne!(port_of(&ours), p, "0.0.0.0:P 不能和别人的 127.0.0.1:P 共存");
    }

    #[test]
    fn listener_is_non_blocking_for_tokio() {
        let l = bind_with_fallback(Ipv4Addr::LOCALHOST, 0).unwrap();
        let err = l.accept().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }
}
