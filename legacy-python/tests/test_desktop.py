import os
import tempfile
import threading
import time
import unittest

from lanshare.desktop import (
    allow_session_end,
    background_log,
    find_app_browser,
    make_logger,
    open_app_window,
    tray_menu_spec,
)


class FindAppBrowserTest(unittest.TestCase):
    def test_prefers_first_existing_candidate(self):
        found = find_app_browser(["C:/no/msedge.exe", "C:/yes/msedge.exe", "C:/yes/chrome.exe"],
                                 exists=lambda p: p.startswith("C:/yes"))
        self.assertEqual(found, "C:/yes/msedge.exe")

    def test_none_when_nothing_installed(self):
        self.assertIsNone(find_app_browser(["C:/no/msedge.exe"], exists=lambda p: False))

    def test_default_candidates_put_edge_first(self):
        found = find_app_browser(exists=lambda p: p.lower().endswith(("msedge.exe", "chrome.exe")))
        self.assertTrue(found.lower().endswith("msedge.exe"), found)


class OpenAppWindowTest(unittest.TestCase):
    def test_opens_chromeless_app_window(self):
        calls = []
        open_app_window("http://127.0.0.1:8000/?k=1", browser="C:/edge/msedge.exe",
                        popen=lambda args, **kw: calls.append((args, kw)), fallback=self.fail)
        args, kw = calls[0]
        self.assertEqual(args[0], "C:/edge/msedge.exe")
        self.assertIn("--app=http://127.0.0.1:8000/?k=1", args)
        self.assertIn("stdin", kw)  # 无控制台进程里起子进程，三个标准句柄都要显式给

    def test_falls_back_to_default_browser(self):
        opened = []
        open_app_window("http://x/", browser=None, popen=self.fail, fallback=opened.append)
        self.assertEqual(opened, ["http://x/"])

    def test_falls_back_when_launch_fails(self):
        opened = []

        def broken(args, **kw):
            raise OSError("gone")

        open_app_window("http://x/", browser="C:/edge/msedge.exe", popen=broken, fallback=opened.append)
        self.assertEqual(opened, ["http://x/"])


class TrayMenuTest(unittest.TestCase):
    def test_menu_items_and_default(self):
        hits = []
        spec = tray_menu_spec(lambda: hits.append("open"), lambda: hits.append("folder"),
                              lambda: hits.append("quit"))
        labels = [label for label, _, _ in spec]
        self.assertEqual(labels, ["打开局域网快传", "打开共享文件夹", None, "退出"])
        self.assertTrue(spec[0][2])  # 左键单击托盘图标 = 打开窗口
        for _, action, _ in spec:
            if action:
                action()
        self.assertEqual(hits, ["open", "folder", "quit"])


class LoggerTest(unittest.TestCase):
    def test_writes_lines_and_trims_big_old_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "sub", "lanshare.log")
            os.makedirs(os.path.dirname(path))
            with open(path, "w", encoding="utf-8") as f:
                f.write("x" * 2000)
            log = make_logger(path, echo=False, max_bytes=1000)
            log("收到 a.txt")
            with open(path, encoding="utf-8") as f:
                content = f.read()
            self.assertNotIn("xxxx", content)
            self.assertIn("收到 a.txt", content)

    def test_background_log_never_blocks_caller(self):
        gate = threading.Event()
        seen = []

        def slow_write(message):
            gate.wait(5)
            seen.append(message)

        log = background_log(slow_write)
        start = time.time()
        for i in range(3):
            log("m%d" % i)
        self.assertLess(time.time() - start, 0.5)
        gate.set()
        deadline = time.time() + 5
        while len(seen) < 3 and time.time() < deadline:
            time.sleep(0.01)
        self.assertEqual(seen, ["m0", "m1", "m2"])


    def test_background_log_flush_waits_for_pending_messages(self):
        seen = []

        def slow_write(message):
            time.sleep(0.1)
            seen.append(message)

        log = background_log(slow_write)
        for i in range(3):
            log("m%d" % i)
        log.flush(timeout=5)
        self.assertEqual(seen, ["m0", "m1", "m2"])


class SessionEndTest(unittest.TestCase):
    class FakeIcon:
        def __init__(self):
            self._message_handlers = {}

    def test_tray_window_allows_windows_shutdown(self):
        icon = self.FakeIcon()
        ended = []
        allow_session_end(icon, lambda: ended.append(True))
        self.assertEqual(icon._message_handlers[0x0011](0, 0), 1)  # WM_QUERYENDSESSION：同意关机
        icon._message_handlers[0x0016](0, 0)  # WM_ENDSESSION，被取消了
        self.assertEqual(ended, [])
        icon._message_handlers[0x0016](1, 0)  # WM_ENDSESSION，真的要关了
        self.assertEqual(ended, [True])

    def test_ignores_icons_without_message_table(self):
        allow_session_end(object(), lambda: None)  # 不报错即可

if __name__ == "__main__":
    unittest.main()
