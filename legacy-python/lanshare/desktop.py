"""桌面端体验：应用窗口、托盘图标、资源管理器、日志、错误弹窗。

服务端（server.py）不依赖这里；main.py 把这些接到 AppState 上。
"""
import os
import queue
import subprocess
import sys
import threading
import time
import webbrowser

ICON_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web", "icon.ico")
TITLE = "局域网快传"
WINDOW_SIZE = "1280,880"


def _default_browser_candidates():
    program_files = [os.environ.get(k) for k in ("ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA")]
    candidates = []
    for base in filter(None, program_files):
        candidates.append(os.path.join(base, "Microsoft", "Edge", "Application", "msedge.exe"))
    for base in filter(None, program_files):
        candidates.append(os.path.join(base, "Google", "Chrome", "Application", "chrome.exe"))
    return candidates


def find_app_browser(candidates=None, exists=os.path.isfile):
    """找一个支持 --app 应用模式的浏览器（Edge 优先，Windows 10/11 都自带），找不到返回 None。"""
    for path in candidates if candidates is not None else _default_browser_candidates():
        if exists(path):
            return path
    return None


def open_app_window(url, browser=None, popen=subprocess.Popen, fallback=webbrowser.open):
    """用没有地址栏和标签页的应用窗口打开 url；不行就退回默认浏览器。"""
    if browser:
        try:
            # 无控制台的进程里起子进程，三个标准句柄都要显式给，否则可能报“句柄无效”
            popen([browser, "--app=" + url, "--window-size=" + WINDOW_SIZE],
                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                  close_fds=True)
            return
        except OSError:
            pass
    fallback(url)


class Explorer:
    """“在资源管理器中显示”和“打开文件夹”，供 AppState.desktop 使用。"""

    def reveal(self, path):
        subprocess.Popen('explorer /select,"%s"' % path,
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def open_folder(self, path):
        os.startfile(path)


def tray_menu_spec(on_open, on_folder, on_quit):
    """托盘菜单：[(文字, 动作, 是否默认项)]，None 表示分隔线。默认项就是左键单击图标的动作。"""
    return [
        ("打开局域网快传", on_open, True),
        ("打开共享文件夹", on_folder, False),
        (None, None, False),
        ("退出", on_quit, False),
    ]


WM_QUERYENDSESSION = 0x0011
WM_ENDSESSION = 0x0016


def allow_session_end(icon, on_end):
    """pystray 对没登记的消息一律返回 0，WM_QUERYENDSESSION 返回 0 等于“拒绝关机”。

    这里显式同意关机，并在真的要关机时先做收尾。
    """
    handlers = getattr(icon, "_message_handlers", None)
    if not isinstance(handlers, dict):
        return
    handlers[WM_QUERYENDSESSION] = lambda wparam, lparam: 1

    def ended(wparam, lparam):
        if wparam:
            on_end()
        return 0

    handlers[WM_ENDSESSION] = ended


def run_tray(tooltip, on_open, on_folder, on_quit, greeting=None):
    """显示托盘图标并阻塞，直到选了“退出”。需要 pystray + Pillow。"""
    import pystray
    from PIL import Image

    items = []
    for label, action, default in tray_menu_spec(on_open, on_folder, on_quit):
        if label is None:
            items.append(pystray.Menu.SEPARATOR)
        elif label == "退出":
            items.append(pystray.MenuItem(label, lambda icon, item: (on_quit(), icon.stop())))
        else:
            items.append(pystray.MenuItem(label, (lambda a: lambda icon, item: a())(action), default=default))
    icon = pystray.Icon("LanShare", Image.open(ICON_PATH), tooltip[:120], pystray.Menu(*items))
    allow_session_end(icon, lambda: (on_quit(), icon.stop()))

    def setup(icon):
        icon.visible = True
        if greeting:
            try:
                icon.notify(greeting[1], greeting[0])
            except Exception:  # 通知被系统关掉之类，不影响使用
                pass

    icon.run(setup=setup)


def make_logger(path, echo=True, max_bytes=1024 * 1024):
    """写日志文件（超过 max_bytes 就从头开始），有控制台时同时打印。线程安全。"""
    lock = threading.Lock()
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        if os.path.getsize(path) > max_bytes:
            os.remove(path)
    except OSError:
        pass

    def log(message):
        line = "%s %s\n" % (time.strftime("%Y-%m-%d %H:%M:%S"), message)
        with lock:
            try:
                with open(path, "a", encoding="utf-8") as f:
                    f.write(line)
            except OSError:
                pass
        if echo and sys.stdout is not None:
            try:
                print(message)
            except (OSError, ValueError):
                pass

    return log


def background_log(write):
    """把日志交给后台线程写，处理请求的线程永远不会被卡住（比如 Win10 控制台被选中文字时 print 会阻塞）。

    返回的函数带 flush(timeout)：进程退出前调用，免得最后几条日志随守护线程一起丢掉。
    """
    pending = queue.Queue()

    def worker():
        while True:
            message = pending.get()
            try:
                write(message)
            finally:
                pending.task_done()

    threading.Thread(target=worker, name="lanshare-log", daemon=True).start()

    def log(message):
        pending.put(message)

    def flush(timeout=2.0):
        deadline = time.time() + timeout
        while pending.unfinished_tasks and time.time() < deadline:
            time.sleep(0.01)

    log.flush = flush
    return log


def show_error(message):
    """没有控制台时用弹窗告诉用户出了什么事。"""
    if os.name == "nt":
        try:
            import ctypes
            ctypes.windll.user32.MessageBoxW(None, message, TITLE, 0x10)
            return
        except (AttributeError, OSError):
            pass
    print(message, file=sys.stderr)
