"""HTTP 服务：路由、登录校验、上传下载、文字消息、二维码。"""
import email.utils
import errno
import io
import json
import os
import re
import socket
import socketserver
from http.cookies import CookieError, SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, quote, unquote, urlsplit

import qrcode
import qrcode.image.svg

from .auth import COOKIE_NAME, LOCKED, OK
from .storage import CHUNK_SIZE, IncompleteUpload

WEB_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")
JSON_BODY_LIMIT = 256 * 1024
# 出错时最多替客户端读掉这么多没读的请求体，避免直接关连接导致对方收到 RST、看不到错误码
DRAIN_LIMIT = 1024 * 1024
FILES_PREFIX = "/api/files/"
_RANGE_RE = re.compile(r"^bytes=(\d*)-(\d*)$")


class AppState:
    """一次运行中各请求共享的对象。port 在绑定端口后由调用方赋值。

    desktop 提供 reveal(path) / open_folder(path)，只给本机页面用；为 None 时这两个功能不可用。
    """

    def __init__(self, storage, auth, board, lan_ips, port=0, log=print, desktop=None):
        self.storage = storage
        self.auth = auth
        self.board = board
        self.lan_ips = list(lan_ips)
        self.port = port
        self.log = log
        self.desktop = desktop

    def base_urls(self):
        hosts = self.lan_ips or ["127.0.0.1"]
        return ["http://%s:%d/" % (host, self.port) for host in hosts]

    def entry_url(self):
        return "%s?k=%s" % (self.base_urls()[0], self.auth.pin)


class HttpError(Exception):
    def __init__(self, status, message):
        super().__init__(message)
        self.status = status
        self.message = message


def parse_range(header, size):
    """解析单段 Range 头。

    返回 (start, end)（含 end）；语法不认识或是多段时返回 None，表示按整个文件返回；
    范围落在文件之外时抛 ValueError（应答 416）。
    """
    match = _RANGE_RE.match(header.strip())
    if not match:
        return None
    first, last = match.groups()
    if not first and not last:
        return None
    if not first:
        suffix = int(last)
        if suffix == 0 or size == 0:
            raise ValueError("range not satisfiable")
        return max(0, size - suffix), size - 1
    start = int(first)
    if last and int(last) < start:
        return None  # 客户端写反了，按语法错误忽略
    if start >= size:
        raise ValueError("range not satisfiable")
    end = int(last) if last else size - 1
    return start, min(end, size - 1)


def content_disposition(name):
    """attachment 头：老浏览器看 ASCII 的 filename，其余看 UTF-8 的 filename*。"""
    fallback = "".join(c if 32 <= ord(c) < 127 and c not in '"\\' else "_" for c in name)
    return "attachment; filename=\"%s\"; filename*=UTF-8''%s" % (fallback, quote(name, safe=""))


def qr_svg(data):
    image = qrcode.make(data, image_factory=qrcode.image.svg.SvgPathImage, border=2)
    buffer = io.BytesIO()
    image.save(buffer)
    return buffer.getvalue()


def human_size(n):
    if n < 1024:
        return "%d B" % n
    for unit in ("KB", "MB", "GB"):
        n /= 1024.0
        if n < 1024:
            return "%.1f %s" % (n, unit)
    return "%.1f TB" % (n / 1024.0)


class Handler(BaseHTTPRequestHandler):
    server_version = "LanShare/1.0"
    protocol_version = "HTTP/1.1"
    timeout = 60

    def log_message(self, format, *args):
        pass  # 默认每个请求打一行，页面轮询会刷屏

    @property
    def state(self):
        return self.server.state

    # ---------- 分发 ----------

    def do_GET(self):
        self._dispatch(self._get)

    def do_POST(self):
        self._dispatch(self._post)

    def do_PUT(self):
        self._dispatch(self._put)

    def send_response(self, code, message=None):
        self._response_started = True
        super().send_response(code, message)

    def _dispatch(self, route):
        self._body_started = False
        self._response_started = False
        try:
            try:
                route(urlsplit(self.path))
            except HttpError as exc:
                self._discard_unread_body()
                self._send_json(exc.status, {"error": exc.message})
            except Exception as exc:  # 兜底：只记一行日志，不把细节（比如本机路径）回给客户端
                self.state.log("出错：%s %s → %r" % (self.command, self.path, exc))
                if self._response_started:
                    self.close_connection = True
                else:
                    self._send_json(500, {"error": "internal error"})
        except (ConnectionError, socket.timeout):
            self.close_connection = True  # 对方已断开，什么也不用回

    def _get(self, url):
        if url.path == "/":
            return self._index(url.query)
        if url.path == "/favicon.ico":
            with open(os.path.join(WEB_DIR, "icon.ico"), "rb") as f:
                return self._send_bytes(200, f.read(), "image/x-icon")
        self._require_login()
        if url.path == "/api/info":
            info = {
                "urls": self.state.base_urls(),
                "pin": self.state.auth.pin,
                "entry_url": self.state.entry_url(),
                "local": self._is_local(),
            }
            if info["local"]:
                info["folder"] = self.state.storage.root  # 本机路径只告诉本机
            return self._send_json(200, info)
        if url.path == "/api/files":
            return self._send_json(200, self.state.storage.list_files())
        if url.path.startswith(FILES_PREFIX):
            return self._download(unquote(url.path[len(FILES_PREFIX):]))
        if url.path == "/api/text":
            return self._send_json(200, self.state.board.list())
        if url.path == "/qr.svg":
            return self._send_bytes(200, qr_svg(self.state.entry_url()), "image/svg+xml")
        raise HttpError(404, "not found")

    def _post(self, url):
        if url.path == "/api/login":
            return self._login()
        self._require_login()
        if url.path == "/api/text":
            body = self._read_json()
            try:
                item = self.state.board.add(body.get("text") if isinstance(body, dict) else None)
            except ValueError as exc:
                raise HttpError(400, str(exc))
            return self._send_json(201, item)
        if url.path in ("/api/open-folder", "/api/reveal"):
            return self._desktop_action(url.path)
        raise HttpError(404, "not found")

    def _put(self, url):
        self._require_login()
        if url.path.startswith(FILES_PREFIX):
            return self._upload(unquote(url.path[len(FILES_PREFIX):]))
        raise HttpError(404, "not found")

    # ---------- 登录 ----------

    def _index(self, query):
        pin = parse_qs(query).get("k")
        if pin:
            result, token = self.state.auth.try_login(self.client_address[0], pin[0])
            if result == OK:
                self.send_response(303)
                self.send_header("Location", "/")
                self._send_cookie(token)
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
        with open(os.path.join(WEB_DIR, "index.html"), "rb") as f:
            page = f.read()
        self._send_bytes(200, page, "text/html; charset=utf-8")

    def _login(self):
        body = self._read_json()
        pin = body.get("pin") if isinstance(body, dict) else None
        result, token = self.state.auth.try_login(
            self.client_address[0], "" if pin is None else str(pin)
        )
        if result == OK:
            return self._send_json(200, {"ok": True}, cookie=token)
        if result == LOCKED:
            raise HttpError(429, "too many attempts")
        raise HttpError(403, "wrong pin")

    def _require_login(self):
        if not self.state.auth.is_valid(self._session_token()):
            raise HttpError(401, "login required")

    def _session_token(self):
        raw = self.headers.get("Cookie")
        if not raw:
            return None
        cookie = SimpleCookie()
        try:
            cookie.load(raw)
        except CookieError:
            return None
        morsel = cookie.get(COOKIE_NAME)
        return morsel.value if morsel else None

    # ---------- 本机专用 ----------

    def _is_local(self):
        return self.client_address[0] in ("127.0.0.1", "::1")

    def _desktop_action(self, path):
        body = self._read_json()
        if not self._is_local():
            raise HttpError(403, "only on this pc")
        if self.state.desktop is None:
            raise HttpError(404, "not available")
        if path == "/api/open-folder":
            self.state.desktop.open_folder(self.state.storage.root)
        else:
            name = body.get("name") if isinstance(body, dict) else None
            target = self.state.storage.resolve(name) if isinstance(name, str) else None
            if target is None:
                raise HttpError(404, "not found")
            self.state.desktop.reveal(target)
        self._send_json(200, {"ok": True})

    # ---------- 文件 ----------

    def _upload(self, name):
        length = self._content_length()
        if length is None:
            raise HttpError(411, "Content-Length required")
        self._body_started = True
        try:
            saved = self.state.storage.save_stream(name, self.rfile, length)
        except IncompleteUpload:
            self.close_connection = True  # 对方中途断开，临时文件已删
            return
        except (ConnectionError, socket.timeout):
            raise
        except OSError as exc:
            if exc.errno == errno.ENOSPC:
                raise HttpError(507, "disk full")
            self.state.log("保存失败：%r" % exc)
            raise HttpError(500, "save failed")
        self.state.log("收到 %s（%s），来自 %s" % (saved, human_size(length), self.client_address[0]))
        self._send_json(201, {"name": saved})

    def _download(self, name):
        path = self.state.storage.resolve(name)
        if path is None:
            raise HttpError(404, "not found")
        try:
            f = open(path, "rb")
        except OSError:
            raise HttpError(404, "not found")
        with f:
            stat = os.fstat(f.fileno())
            size = stat.st_size
            # 浏览器要有校验器才肯断点续传；If-Range 对不上说明文件变了，整个重发
            etag = '"%x-%x"' % (size, stat.st_mtime_ns)
            last_modified = email.utils.formatdate(stat.st_mtime, usegmt=True)
            start, end, status = 0, size - 1, 200
            header = self.headers.get("Range")
            if_range = self.headers.get("If-Range")
            if header and if_range and if_range.strip() not in (etag, last_modified):
                header = None
            if header:
                try:
                    parsed = parse_range(header, size)
                except ValueError:
                    self.send_response(416)
                    self.send_header("Content-Range", "bytes */%d" % size)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                if parsed:
                    start, end = parsed
                    status = 206
            length = end - start + 1 if size else 0
            filename = os.path.basename(path)
            self.send_response(status)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(length))
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", last_modified)
            self.send_header("Content-Disposition", content_disposition(filename))
            if status == 206:
                self.send_header("Content-Range", "bytes %d-%d/%d" % (start, end, size))
            self.end_headers()
            if status == 200:
                self.state.log("发送 %s（%s）到 %s" % (filename, human_size(size), self.client_address[0]))
            f.seek(start)
            remaining = length
            while remaining > 0:
                chunk = f.read(min(CHUNK_SIZE, remaining))
                if not chunk:
                    break
                self.wfile.write(chunk)
                remaining -= len(chunk)
            if remaining:
                self.close_connection = True  # 文件在传输中变短了，只能断开

    # ---------- 请求体 ----------

    def _content_length(self):
        raw = self.headers.get("Content-Length")
        if raw is None:
            return None
        try:
            length = int(raw)
        except ValueError:
            raise HttpError(400, "bad Content-Length")
        if length < 0:
            raise HttpError(400, "bad Content-Length")
        return length

    def _read_json(self):
        length = self._content_length()
        if length is None:
            raise HttpError(411, "Content-Length required")
        if length > JSON_BODY_LIMIT:
            raise HttpError(413, "body too large")
        self._body_started = True
        try:
            return json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            raise HttpError(400, "bad json")

    def _discard_unread_body(self):
        if self._body_started:
            return
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return
        if 0 < length <= DRAIN_LIMIT:
            self._body_started = True
            self.rfile.read(length)

    # ---------- 应答 ----------

    def _send_bytes(self, status, body, content_type, cookie=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if cookie:
            self._send_cookie(cookie)
        if status >= 400:
            # 出错时请求体可能没读完，不能在这条连接上继续解析下一个请求
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        self.wfile.write(body)

    def _send_json(self, status, obj, cookie=None):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self._send_bytes(status, body, "application/json; charset=utf-8", cookie)

    def _send_cookie(self, token):
        self.send_header("Set-Cookie", "%s=%s; HttpOnly; SameSite=Lax; Path=/" % (COOKIE_NAME, token))


class LanShareServer(ThreadingHTTPServer):
    # Windows 上 SO_REUSEADDR 允许第二个进程绑同一端口并悄悄“抢”走请求，所以只在非 Windows 开
    allow_reuse_address = os.name != "nt"
    daemon_threads = True

    def __init__(self, address, state):
        self.state = state
        super().__init__(address, Handler)

    def server_bind(self):
        if os.name == "nt":
            # 否则别的程序只占了 127.0.0.1:P 时我们仍能绑上 0.0.0.0:P，发往 127.0.0.1 的请求会进别人的程序
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
        # HTTPServer.server_bind 会对监听地址做 getfqdn 反查，某些网络下要卡好几秒
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def make_server(state, host="0.0.0.0", port=8000, tries=10):
    """从 port 开始依次尝试绑定，返回第一个成功的服务器；都失败时抛出最后一个 OSError。"""
    if port == 0:
        return LanShareServer((host, 0), state)
    error = None
    for candidate in range(port, port + tries):
        try:
            return LanShareServer((host, candidate), state)
        except OSError as exc:
            error = exc
    raise error
