import json
import os
import socket
import tempfile
import threading
import unittest
from unittest import mock

from lanshare.auth import Auth
from lanshare.main import (
    banner,
    default_share_dir,
    find_running_instance,
    main,
    parse_args,
    remove_instance_file,
    write_instance_file,
)
from lanshare.server import AppState, make_server
from lanshare.storage import Storage
from lanshare.textboard import TextBoard


class ParseArgsTest(unittest.TestCase):
    def test_defaults(self):
        args = parse_args([])
        self.assertEqual(args.port, 8000)
        self.assertFalse(args.no_browser)
        self.assertEqual(args.dir, os.path.join(os.path.expanduser("~"), "Downloads", "LanShare"))
        self.assertEqual(args.dir, default_share_dir())
        self.assertTrue(args.single_instance)
        self.assertFalse(args.no_tray)
        self.assertIsNone(args.info_file)

    def test_automation_flags(self):
        args = parse_args(["--no-tray", "--info-file", "D:/x/info.json"])
        self.assertTrue(args.no_tray)
        self.assertEqual(args.info_file, "D:/x/info.json")

    def test_custom(self):
        args = parse_args(["--port", "9000", "--dir", "D:/x", "--no-browser"])
        self.assertEqual((args.port, args.dir, args.no_browser), (9000, "D:/x", True))
        self.assertFalse(args.single_instance)

    def test_explicit_port_or_dir_disables_single_instance(self):
        self.assertFalse(parse_args(["--port", "9000"]).single_instance)
        self.assertFalse(parse_args(["--dir", "D:/x"]).single_instance)
        self.assertTrue(parse_args(["--no-browser"]).single_instance)


class SingleInstanceTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.path = os.path.join(self._tmp.name, "LanShare", "instance.json")
        self.servers = []

    def tearDown(self):
        for server in self.servers:
            server.shutdown()
            server.server_close()
        self._tmp.cleanup()

    def start_server(self, pin):
        state = AppState(Storage(os.path.join(self._tmp.name, "share")), Auth(pin), TextBoard(), [],
                         log=lambda *a: None)
        server = make_server(state, host="127.0.0.1", port=0)
        state.port = server.server_address[1]
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.servers.append(server)
        return state.port

    def free_port(self):
        sock = socket.socket()
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
        sock.close()
        return port

    def test_no_file_means_not_running(self):
        self.assertIsNone(find_running_instance(self.path))

    def test_garbage_file_means_not_running(self):
        os.makedirs(os.path.dirname(self.path))
        for content in ["not json", '{"port": "x", "pin": "123456"}', '{"port": 1}', '[]',
                        '{"port": 8000, "pin": "12\\r\\nX"}']:
            with open(self.path, "w", encoding="utf-8") as f:
                f.write(content)
            self.assertIsNone(find_running_instance(self.path), content)

    def test_stale_file_means_not_running(self):
        write_instance_file(self.path, self.free_port(), "123456")
        self.assertIsNone(find_running_instance(self.path))

    def test_live_instance_is_found(self):
        port = self.start_server("123456")
        write_instance_file(self.path, port, "123456")
        self.assertEqual(find_running_instance(self.path), "http://127.0.0.1:%d/?k=123456" % port)

    def test_other_lanshare_on_that_port_is_not_ours(self):
        port = self.start_server("654321")
        write_instance_file(self.path, port, "123456")
        self.assertIsNone(find_running_instance(self.path))

    def test_remove_only_own_file(self):
        write_instance_file(self.path, 8000, "123456")
        with open(self.path, encoding="utf-8") as f:
            self.assertEqual(json.load(f)["pid"], os.getpid())
        with open(self.path, "w", encoding="utf-8") as f:
            json.dump({"port": 8001, "pin": "000000", "pid": os.getpid() + 1}, f)
        remove_instance_file(self.path)
        self.assertTrue(os.path.exists(self.path))  # 别的实例写的，不删
        write_instance_file(self.path, 8000, "123456")
        remove_instance_file(self.path)
        self.assertFalse(os.path.exists(self.path))
        remove_instance_file(self.path)  # 不存在时也不报错

    def test_main_opens_existing_instance_instead_of_starting_another(self):
        port = self.start_server("123456")
        write_instance_file(self.path, port, "123456")
        with mock.patch("lanshare.main.instance_file", return_value=self.path), \
                mock.patch("lanshare.main.data_dir", return_value=self._tmp.name), \
                mock.patch("lanshare.main.background_log", side_effect=lambda write: write), \
                mock.patch("lanshare.main.make_server") as make, \
                mock.patch("lanshare.main.find_app_browser", return_value="C:/edge/msedge.exe"), \
                mock.patch("lanshare.main.open_app_window") as opened:
            self.assertEqual(main([]), 0)
        make.assert_not_called()
        opened.assert_called_once_with("http://127.0.0.1:%d/?k=123456" % port, "C:/edge/msedge.exe")

    def run_main_with_busy_ports(self, argv):
        with mock.patch("lanshare.main.instance_file", return_value=self.path), \
                mock.patch("lanshare.main.data_dir", return_value=self._tmp.name), \
                mock.patch("lanshare.main.background_log", side_effect=lambda write: write), \
                mock.patch("lanshare.main.make_server", side_effect=OSError("busy")), \
                mock.patch("lanshare.main.show_error") as shown:
            code = main(argv + ["--dir", os.path.join(self._tmp.name, "share")])
        return code, shown

    def test_startup_failure_shows_dialog_when_no_console(self):
        code, shown = self.run_main_with_busy_ports([])
        self.assertEqual(code, 1)
        self.assertIn("端口", shown.call_args[0][0])

    def test_startup_failure_stays_quiet_in_automation(self):
        code, shown = self.run_main_with_busy_ports(["--no-tray"])
        self.assertEqual(code, 1)
        shown.assert_not_called()


class BannerTest(unittest.TestCase):
    def test_contains_url_pin_alternatives_and_firewall_hint(self):
        state = AppState(None, Auth("654321"), None, ["192.168.1.5", "10.0.0.2"], port=8001)
        text = banner(state, "C:/share")
        self.assertIn("手机访问：http://192.168.1.5:8001/", text)
        self.assertIn("口令：654321", text)
        self.assertIn("http://10.0.0.2:8001/", text)
        self.assertIn("C:/share", text)
        self.assertIn("专用网络", text)
        self.assertNotIn("警告", text)

    def test_warns_without_lan_address(self):
        state = AppState(None, Auth("654321"), None, [], port=8001)
        text = banner(state, "C:/share")
        self.assertIn("http://127.0.0.1:8001/", text)
        self.assertIn("警告", text)


if __name__ == "__main__":
    unittest.main()
