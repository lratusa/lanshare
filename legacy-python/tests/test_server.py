import hashlib
import http.client
import json
import os
import socket
import tempfile
import threading
import time
import shutil
import unittest
from unittest import mock
from urllib.parse import quote

from lanshare.auth import MAX_FAILURES, Auth
from lanshare.server import AppState, content_disposition, make_server, parse_range
from lanshare.storage import TEMP_PREFIX, Storage
from lanshare.textboard import TextBoard

PIN = "123456"


class FakeDesktop:
    def __init__(self):
        self.calls = []

    def reveal(self, path):
        self.calls.append(("reveal", path))

    def open_folder(self, path):
        self.calls.append(("folder", path))


class ServerTestBase(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self._tmp.name, "share")
        self.logs = []
        self.desktop = FakeDesktop()
        self.state = AppState(
            Storage(self.root), Auth(PIN), TextBoard(), ["192.168.1.5"], log=self.logs.append,
            desktop=self.desktop,
        )
        self.server = make_server(self.state, host="127.0.0.1", port=0)
        self.port = self.server.server_address[1]
        self.state.port = self.port
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.cookie = None

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self._tmp.cleanup()

    def request(self, method, path, body=None, headers=None, login=True):
        if login and self.cookie is None:
            self.assertEqual(self.login(), 200)
        all_headers = dict(headers or {})
        if login:
            all_headers["Cookie"] = self.cookie
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=30)
        try:
            conn.request(method, path, body=body, headers=all_headers)
            resp = conn.getresponse()
            return resp.status, resp, resp.read()
        finally:
            conn.close()

    def login(self, pin=PIN):
        body = json.dumps({"pin": pin}).encode("utf-8")
        status, resp, _ = self.request(
            "POST", "/api/login", body, {"Content-Type": "application/json"}, login=False
        )
        if status == 200:
            self.cookie = resp.getheader("Set-Cookie").split(";")[0]
        return status


class AuthRoutesTest(ServerTestBase):
    def test_index_is_public(self):
        status, _, body = self.request("GET", "/", login=False)
        self.assertEqual(status, 200)
        self.assertIn("局域网快传", body.decode("utf-8"))

    def test_api_requires_login(self):
        for path in ["/api/files", "/api/text", "/api/info", "/qr.svg", "/api/files/a.txt"]:
            status, _, _ = self.request("GET", path, login=False)
            self.assertEqual(status, 401, path)
        status, _, _ = self.request("PUT", "/api/files/a.txt", b"data", login=False)
        self.assertEqual(status, 401)
        status, _, _ = self.request("POST", "/api/text", b'{"text": "x"}', login=False)
        self.assertEqual(status, 401)
        self.assertEqual(os.listdir(self.root), [])

    def test_forged_cookie_rejected(self):
        status, _, _ = self.request(
            "GET", "/api/info", headers={"Cookie": "lanshare_session=forged"}, login=False
        )
        self.assertEqual(status, 401)

    def test_login_sets_cookie_with_flags(self):
        body = json.dumps({"pin": PIN}).encode("utf-8")
        status, resp, _ = self.request("POST", "/api/login", body, login=False)
        self.assertEqual(status, 200)
        cookie = resp.getheader("Set-Cookie")
        self.assertTrue(cookie.startswith("lanshare_session="))
        self.assertIn("HttpOnly", cookie)
        self.assertIn("SameSite=Lax", cookie)

    def test_login_lockout(self):
        for _ in range(MAX_FAILURES):
            self.assertEqual(self.login("000000"), 403)
        self.assertEqual(self.login(PIN), 429)

    def test_query_pin_redirects_with_cookie(self):
        status, resp, _ = self.request("GET", "/?k=" + PIN, login=False)
        self.assertEqual(status, 303)
        self.assertEqual(resp.getheader("Location"), "/")
        self.assertIn("lanshare_session=", resp.getheader("Set-Cookie"))

    def test_wrong_query_pin_shows_page_and_counts_failure(self):
        for _ in range(MAX_FAILURES):
            status, _, _ = self.request("GET", "/?k=000000", login=False)
            self.assertEqual(status, 200)
        self.assertEqual(self.login(PIN), 429)

    def test_bad_login_bodies(self):
        status, _, _ = self.request("POST", "/api/login", b"not json", login=False)
        self.assertEqual(status, 400)
        status, _, _ = self.request("POST", "/api/login", b"x" * (300 * 1024), login=False)
        self.assertEqual(status, 413)


class TextRoutesTest(ServerTestBase):
    def test_text_roundtrip_keeps_html_as_text(self):
        text = "你好 <img src=x onerror=alert(1)>"
        status, _, _ = self.request("POST", "/api/text", json.dumps({"text": text}).encode("utf-8"))
        self.assertEqual(status, 201)
        _, _, body = self.request("GET", "/api/text")
        self.assertEqual(json.loads(body)[0]["text"], text)

    def test_text_rejects_bad_input(self):
        for payload in [{"text": ""}, {"text": "x" * 10001}, {"nope": 1}, ["text"]]:
            status, _, _ = self.request("POST", "/api/text", json.dumps(payload).encode("utf-8"))
            self.assertEqual(status, 400, payload)


class MiscRoutesTest(ServerTestBase):
    def test_info(self):
        _, _, body = self.request("GET", "/api/info")
        info = json.loads(body)
        self.assertEqual(info["pin"], PIN)
        self.assertEqual(info["urls"], ["http://192.168.1.5:%d/" % self.port])
        self.assertEqual(info["entry_url"], "http://192.168.1.5:%d/?k=%s" % (self.port, PIN))

    def test_qr_svg(self):
        status, resp, body = self.request("GET", "/qr.svg")
        self.assertEqual(status, 200)
        self.assertEqual(resp.getheader("Content-Type"), "image/svg+xml")
        self.assertIn(b"<svg", body)

    def test_info_tells_page_it_runs_on_this_pc(self):
        _, _, body = self.request("GET", "/api/info")
        info = json.loads(body)
        self.assertTrue(info["local"])
        self.assertEqual(info["folder"], self.state.storage.root)

    def test_info_hides_folder_path_from_other_devices(self):
        with mock.patch("lanshare.server.Handler._is_local", return_value=False):
            _, _, body = self.request("GET", "/api/info")
        info = json.loads(body)
        self.assertFalse(info["local"])
        self.assertNotIn("folder", info)

    def test_favicon_is_public(self):
        status, resp, body = self.request("GET", "/favicon.ico", login=False)
        self.assertEqual(status, 200)
        self.assertEqual(resp.getheader("Content-Type"), "image/x-icon")
        self.assertEqual(body[:4], b"\x00\x00\x01\x00")

    def test_open_folder_on_this_pc(self):
        status, _, _ = self.request("POST", "/api/open-folder", b"{}")
        self.assertEqual(status, 200)
        self.assertEqual(self.desktop.calls, [("folder", self.state.storage.root)])

    def test_reveal_file_on_this_pc(self):
        with open(os.path.join(self.root, "a.txt"), "wb") as f:
            f.write(b"1")
        status, _, _ = self.request("POST", "/api/reveal", json.dumps({"name": "a.txt"}).encode("utf-8"))
        self.assertEqual(status, 200)
        self.assertEqual(self.desktop.calls, [("reveal", os.path.join(self.state.storage.root, "a.txt"))])
        status, _, _ = self.request("POST", "/api/reveal", json.dumps({"name": "../x"}).encode("utf-8"))
        self.assertEqual(status, 404)

    def test_desktop_actions_refused_for_other_devices(self):
        with mock.patch("lanshare.server.Handler._is_local", return_value=False):
            for path, body in [("/api/open-folder", b"{}"), ("/api/reveal", b'{"name": "a.txt"}')]:
                status, _, _ = self.request("POST", path, body)
                self.assertEqual(status, 403, path)
        self.assertEqual(self.desktop.calls, [])

    def test_desktop_actions_require_login(self):
        status, _, _ = self.request("POST", "/api/open-folder", b"{}", login=False)
        self.assertEqual(status, 401)
        self.assertEqual(self.desktop.calls, [])

    def test_unknown_route(self):
        status, _, _ = self.request("GET", "/api/nope")
        self.assertEqual(status, 404)

    def test_port_taken_on_specific_address_is_skipped(self):
        # 别的程序（比如 Django runserver）只占了 127.0.0.1:P 时，0.0.0.0:P 也不能要
        blocker = socket.socket()
        blocker.bind(("127.0.0.1", 0))
        blocker.listen()
        port = blocker.getsockname()[1]
        try:
            other = make_server(self.state, host="0.0.0.0", port=port)
            try:
                self.assertNotEqual(other.server_address[1], port)
            finally:
                other.server_close()
        finally:
            blocker.close()

    def test_second_server_gets_next_port(self):
        other = make_server(self.state, host="127.0.0.1", port=self.port)
        try:
            self.assertNotEqual(other.server_address[1], self.port)
        finally:
            other.server_close()


class ParseRangeTest(unittest.TestCase):
    def test_ranges(self):
        self.assertEqual(parse_range("bytes=0-99", 1000), (0, 99))
        self.assertEqual(parse_range("bytes=100-", 1000), (100, 999))
        self.assertEqual(parse_range("bytes=-10", 1000), (990, 999))
        self.assertEqual(parse_range("bytes=900-5000", 1000), (900, 999))
        self.assertEqual(parse_range("bytes=-5000", 1000), (0, 999))

    def test_ignored_headers_mean_whole_file(self):
        for header in ["items=0-1", "bytes=0-1,5-6", "bytes=-", "bytes=9-3", "garbage"]:
            self.assertIsNone(parse_range(header, 1000), header)

    def test_unsatisfiable(self):
        for header, size in [("bytes=1000-", 1000), ("bytes=-0", 1000), ("bytes=0-", 0), ("bytes=-1", 0)]:
            with self.assertRaises(ValueError):
                parse_range(header, size)


class ContentDispositionTest(unittest.TestCase):
    def test_utf8_name_is_latin1_safe(self):
        name = '报告 "终版".pdf'
        value = content_disposition(name)
        value.encode("latin-1")  # http.server 用 latin-1 编码头部，必须能编码
        self.assertIn("filename*=UTF-8''" + quote(name, safe=""), value)
        self.assertTrue(value.startswith("attachment; "))


class FileRoutesTest(ServerTestBase):
    def upload(self, name, data):
        status, _, body = self.request("PUT", "/api/files/" + quote(name, safe=""), data)
        self.assertEqual(status, 201, body)
        return json.loads(body)["name"]

    def test_roundtrip_20mb(self):
        data = os.urandom(20 * 1024 * 1024)
        self.assertEqual(self.upload("video.mp4", data), "video.mp4")
        status, resp, body = self.request("GET", "/api/files/video.mp4")
        self.assertEqual(status, 200)
        self.assertEqual(resp.getheader("Content-Length"), str(len(data)))
        self.assertEqual(resp.getheader("Accept-Ranges"), "bytes")
        self.assertEqual(hashlib.sha256(body).hexdigest(), hashlib.sha256(data).hexdigest())

    def test_empty_file(self):
        self.assertEqual(self.upload("empty.txt", b""), "empty.txt")
        status, resp, body = self.request("GET", "/api/files/empty.txt")
        self.assertEqual((status, body), (200, b""))

    def test_list_files(self):
        self.upload("a.txt", b"hello")
        status, _, body = self.request("GET", "/api/files")
        self.assertEqual(status, 200)
        files = json.loads(body)
        self.assertEqual([f["name"] for f in files], ["a.txt"])
        self.assertEqual(files[0]["size"], 5)

    def test_range_requests(self):
        data = bytes(range(256)) * 4
        self.upload("r.bin", data)
        status, resp, body = self.request("GET", "/api/files/r.bin", headers={"Range": "bytes=100-199"})
        self.assertEqual(status, 206)
        self.assertEqual(body, data[100:200])
        self.assertEqual(resp.getheader("Content-Range"), "bytes 100-199/1024")
        status, _, body = self.request("GET", "/api/files/r.bin", headers={"Range": "bytes=-10"})
        self.assertEqual((status, body), (206, data[-10:]))
        status, resp, _ = self.request("GET", "/api/files/r.bin", headers={"Range": "bytes=5000-"})
        self.assertEqual(status, 416)
        self.assertEqual(resp.getheader("Content-Range"), "bytes */1024")

    def test_download_has_validators_so_browsers_can_resume(self):
        self.upload("r.bin", b"x" * 100)
        _, resp, _ = self.request("GET", "/api/files/r.bin")
        etag, modified = resp.getheader("ETag"), resp.getheader("Last-Modified")
        self.assertTrue(etag and etag.startswith('"') and etag.endswith('"'), etag)
        self.assertTrue(modified and modified.endswith("GMT"), modified)
        for validator in (etag, modified):
            status, _, body = self.request("GET", "/api/files/r.bin",
                                           headers={"Range": "bytes=10-19", "If-Range": validator})
            self.assertEqual((status, body), (206, b"x" * 10), validator)
        status, _, body = self.request("GET", "/api/files/r.bin",
                                       headers={"Range": "bytes=10-19", "If-Range": '"stale"'})
        self.assertEqual((status, len(body)), (200, 100))

    def test_chinese_and_special_filename(self):
        name = "测试 文件#1.txt"
        self.assertEqual(self.upload(name, b"x"), name)
        status, resp, body = self.request("GET", "/api/files/" + quote(name, safe=""))
        self.assertEqual((status, body), (200, b"x"))
        self.assertIn("filename*=UTF-8''" + quote(name, safe=""), resp.getheader("Content-Disposition"))

    def test_duplicate_upload_renamed(self):
        self.assertEqual(self.upload("a.txt", b"1"), "a.txt")
        self.assertEqual(self.upload("a.txt", b"2"), "a (1).txt")

    def test_upload_cannot_escape_share_dir(self):
        self.assertEqual(self.upload("../../evil.txt", b"x"), "evil.txt")
        self.assertEqual(sorted(os.listdir(self._tmp.name)), ["share"])

    def test_download_cannot_escape_share_dir(self):
        with open(os.path.join(self._tmp.name, "secret.txt"), "wb") as f:
            f.write(b"secret")
        paths = [
            "/api/files/..%2Fsecret.txt", "/api/files/..%5Csecret.txt",
            "/api/files/../secret.txt", "/api/files/nope.txt",
        ]
        for path in paths:
            status, _, body = self.request("GET", path)
            self.assertEqual(status, 404, path)
            self.assertNotIn(b"secret", body)

    def test_temp_files_hidden_and_not_downloadable(self):
        open(os.path.join(self.root, TEMP_PREFIX + "x.part"), "wb").close()
        _, _, body = self.request("GET", "/api/files")
        self.assertEqual(json.loads(body), [])
        status, _, _ = self.request("GET", "/api/files/" + TEMP_PREFIX + "x.part")
        self.assertEqual(status, 404)

    def test_missing_content_length(self):
        self.login()
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        try:
            conn.putrequest("PUT", "/api/files/a.txt")
            conn.putheader("Cookie", self.cookie)
            conn.endheaders()
            self.assertEqual(conn.getresponse().status, 411)
        finally:
            conn.close()

    def test_interrupted_upload_leaves_no_file(self):
        self.login()
        sock = socket.create_connection(("127.0.0.1", self.port))
        head = (
            "PUT /api/files/big.bin HTTP/1.1\r\nHost: x\r\nCookie: %s\r\n"
            "Content-Length: 1000000\r\n\r\n" % self.cookie
        )
        sock.sendall(head.encode("ascii") + b"x" * 1000)
        deadline = time.time() + 5
        while time.time() < deadline and not os.listdir(self.root):
            time.sleep(0.02)
        self.assertEqual(len(os.listdir(self.root)), 1)  # 临时文件已经建好
        sock.close()
        deadline = time.time() + 5
        while time.time() < deadline and os.listdir(self.root):
            time.sleep(0.02)
        self.assertEqual(os.listdir(self.root), [])

    def test_share_dir_deleted_while_running(self):
        shutil.rmtree(self.root)
        status, _, body = self.request("GET", "/api/files")
        self.assertEqual((status, json.loads(body)), (200, []))
        shutil.rmtree(self.root)
        self.assertEqual(self.upload("a.txt", b"x"), "a.txt")

    def test_unexpected_error_returns_500_json_without_details(self):
        with mock.patch.object(self.state.storage, "list_files", side_effect=RuntimeError(r"C:\secret\boom")):
            status, _, body = self.request("GET", "/api/files")
        self.assertEqual(status, 500)
        self.assertEqual(json.loads(body), {"error": "internal error"})
        self.assertTrue(any("boom" in line for line in self.logs), self.logs)

    def test_upload_and_download_are_logged(self):
        self.upload("a.txt", b"hello")
        self.request("GET", "/api/files/a.txt")
        self.assertTrue(any("收到 a.txt" in line for line in self.logs), self.logs)
        self.assertTrue(any("发送 a.txt" in line for line in self.logs), self.logs)


if __name__ == "__main__":
    unittest.main()
