"""程序入口：lanshare [--port 8000] [--dir 共享目录] [--no-browser] [--no-tray] [--info-file 路径]

默认（双击、开始菜单、任务栏）：没有命令行窗口，主界面是一个应用窗口，程序常驻托盘。
--no-tray：前台运行、Ctrl+C 退出，供命令行和自动化测试使用。
"""
import argparse
import http.client
import json
import os
import re
import sys
import threading

from . import __version__
from .auth import Auth, generate_pin
from .desktop import (
    Explorer,
    background_log,
    find_app_browser,
    make_logger,
    open_app_window,
    run_tray,
    show_error,
)
from .netinfo import lan_ips
from .server import AppState, make_server
from .storage import Storage
from .textboard import TextBoard

DEFAULT_PORT = 8000
TITLE = "局域网快传 LanShare"


def default_share_dir():
    return os.path.join(os.path.expanduser("~"), "Downloads", "LanShare")


def data_dir():
    base = os.environ.get("LOCALAPPDATA") or os.path.join(os.path.expanduser("~"), ".local", "share")
    return os.path.join(base, "LanShare")


def parse_args(argv=None):
    parser = argparse.ArgumentParser(
        prog="lanshare", description="局域网快传：手机和电脑用浏览器互传文件和文字"
    )
    parser.add_argument("--port", type=int, help="起始端口，被占用时自动往后找（默认 %d）" % DEFAULT_PORT)
    parser.add_argument("--dir", help="共享文件夹（默认 %s）" % default_share_dir())
    parser.add_argument("--no-browser", action="store_true", help="启动后不打开主界面窗口")
    parser.add_argument("--no-tray", action="store_true", help="不用托盘，前台运行（Ctrl+C 退出）")
    parser.add_argument("--info-file", help="启动后把端口和口令写进这个 JSON 文件（给自动化用）")
    args = parser.parse_args(argv)
    # 只有默认启动（双击、开始菜单、任务栏）才保持单实例；显式指定端口或目录时照常另起一个
    args.single_instance = args.port is None and args.dir is None
    if args.port is None:
        args.port = DEFAULT_PORT
    if args.dir is None:
        args.dir = default_share_dir()
    return args


# ---------- 单实例 ----------

def instance_file():
    return os.path.join(data_dir(), "instance.json")


def write_instance_file(path, port, pin):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"port": port, "pin": pin, "pid": os.getpid()}, f)


def remove_instance_file(path):
    """只删本进程写的记录；被强制结束时来不及删，所以读的一方还会再验活。"""
    try:
        with open(path, encoding="utf-8") as f:
            if json.load(f).get("pid") != os.getpid():
                return
        os.remove(path)
    except (OSError, ValueError, AttributeError):
        pass


def find_running_instance(path, timeout=1.0):
    """记录里的实例还活着、且口令对得上时，返回它的本机入口 URL；否则返回 None。"""
    try:
        with open(path, encoding="utf-8") as f:
            info = json.load(f)
        port, pin = int(info["port"]), str(info["pin"])
    except (OSError, ValueError, KeyError, TypeError):
        return None
    if not re.match(r"^\d{6}$", pin) or not 0 < port < 65536:
        return None
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    try:
        conn.request("GET", "/?k=" + pin)
        response = conn.getresponse()
        response.read()
        if response.status == 303:
            return "http://127.0.0.1:%d/?k=%s" % (port, pin)
    except (OSError, http.client.HTTPException):
        pass
    finally:
        conn.close()
    return None


# ---------- 启动 ----------

def banner(state, share_dir):
    urls = state.base_urls()
    lines = [
        "局域网快传 LanShare %s 已启动" % __version__,
        "  手机访问：%s" % urls[0],
        "  口令：%s" % state.auth.pin,
    ]
    for url in urls[1:]:
        lines.append("  其他地址：%s" % url)
    lines += [
        "  共享文件夹：%s" % share_dir,
        "  手机和电脑要连同一个 WiFi / 路由器，用手机相机扫主界面上的二维码即可进入。",
        "  手机连不上时：Windows 防火墙要允许“专用网络”，当前网络也不能是“公用网络”。",
    ]
    if not state.lan_ips:
        lines.append("  [警告] 没找到局域网地址，目前只能在本机访问。请检查是否连上了 WiFi 或网线。")
    return "\n".join(lines)


def _setup_console():
    # 有控制台时（源码运行）按 UTF-8 输出中文；按行刷新，输出被管道读取时也能实时看到
    for stream in (sys.stdout, sys.stderr):
        if stream is not None and hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", line_buffering=True)


def _tray_available():
    try:
        import pystray  # noqa: F401
        import PIL  # noqa: F401
        return True
    except ImportError:
        return False


def _write_info_file(path, state):
    with open(path, "w", encoding="utf-8") as f:
        json.dump({"port": state.port, "pin": state.auth.pin, "urls": state.base_urls()}, f)


def main(argv=None):
    _setup_console()
    args = parse_args(argv)
    log = background_log(make_logger(os.path.join(data_dir(), "lanshare.log")))
    try:
        return _run(args, log)
    finally:
        flush = getattr(log, "flush", None)
        if flush:
            flush()


def _run(args, log):
    interactive = not args.no_tray
    browser = find_app_browser()

    record = instance_file() if args.single_instance else None
    if record:
        url = find_running_instance(record)
        if url:
            log("已经在运行，打开已有实例的主界面")
            if not args.no_browser:
                open_app_window(url, browser)
            return 0

    storage = Storage(args.dir)
    removed = storage.cleanup_stale_temp()
    if removed:
        log("清理了 %d 个上次没传完的临时文件" % removed)
    auth = Auth(generate_pin())
    state = AppState(storage, auth, TextBoard(), lan_ips(), log=log, desktop=Explorer())
    try:
        server = make_server(state, port=args.port)
    except OSError as exc:
        message = "启动失败：从 %d 开始的 10 个端口都用不了（%s）。" % (args.port, exc)
        log(message)
        if interactive:
            show_error(message + "\n\n请关掉占用这些端口的程序后再试。")
        return 1
    state.port = server.server_address[1]
    local_url = "http://127.0.0.1:%d/?k=%s" % (state.port, auth.pin)
    if record:
        write_instance_file(record, state.port, auth.pin)
    if args.info_file:
        _write_info_file(args.info_file, state)
    log(banner(state, storage.root))
    if not args.no_browser:
        open_app_window(local_url, browser)

    try:
        if interactive and _tray_available():
            threading.Thread(target=server.serve_forever, name="lanshare-http", daemon=True).start()
            run_tray(
                "局域网快传 · 口令 %s · %s" % (auth.pin, state.base_urls()[0]),
                on_open=lambda: open_app_window(local_url, browser),
                on_folder=lambda: Explorer().open_folder(storage.root),
                on_quit=lambda: log("从托盘退出"),
                greeting=("局域网快传已启动", "用手机扫主界面上的二维码就能互传文件。关掉窗口后，它会继续在托盘里运行。"),
            )
            server.shutdown()
        else:
            if interactive:
                log("没有安装 pystray / Pillow，改为前台运行（Ctrl+C 退出）")
            server.serve_forever()
    except KeyboardInterrupt:
        log("已退出。")
    finally:
        server.server_close()
        if record:
            remove_instance_file(record)
    return 0
