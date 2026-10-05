"""冒烟测试：真实启动 lanshare（源码或 exe），走一遍登录、上传、下载、文字、二维码。

用法：
  python tools/smoke_test.py python -m lanshare
  python tools/smoke_test.py dist/LanShare.exe
"""
import hashlib
import http.client
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote

STARTUP_TIMEOUT = 60  # onefile exe 首次启动要解压，给足时间


def read_info(proc, info_path):
    """等程序把端口和口令写进 --info-file（exe 没有控制台，不能靠读输出）。"""
    deadline = time.time() + STARTUP_TIMEOUT
    while time.time() < deadline:
        try:
            with open(info_path, encoding="utf-8") as f:
                info = json.load(f)
            return info["port"], info["pin"]
        except (OSError, ValueError, KeyError):
            pass
        if proc.poll() is not None:
            break
        time.sleep(0.1)
    raise RuntimeError("程序没有写出 %s（退出码 %s）" % (info_path, proc.poll()))


def call(port, method, path, body=None, cookie=None, headers=None):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    try:
        h = dict(headers or {})
        if cookie:
            h["Cookie"] = cookie
        conn.request(method, path, body=body, headers=h)
        resp = conn.getresponse()
        return resp.status, resp, resp.read()
    finally:
        conn.close()


def check(condition, message):
    if not condition:
        raise AssertionError(message)
    print("  OK  " + message)


def run_checks(port, pin):
    status, _, body = call(port, "GET", "/")
    check(status == 200 and "局域网快传" in body.decode("utf-8"), "首页可访问（页面文件已打包）")

    status, resp, _ = call(port, "POST", "/api/login", json.dumps({"pin": pin}).encode("utf-8"))
    check(status == 200, "口令登录")
    cookie = resp.getheader("Set-Cookie").split(";")[0]

    data = os.urandom(5 * 1024 * 1024)
    name = "冒烟 测试.bin"
    status, _, body = call(port, "PUT", "/api/files/" + quote(name, safe=""), data, cookie)
    check(status == 201 and json.loads(body)["name"] == name, "上传 5MB 中文名文件")

    status, _, body = call(port, "GET", "/api/files/" + quote(name, safe=""), cookie=cookie)
    check(status == 200 and hashlib.sha256(body).digest() == hashlib.sha256(data).digest(), "下载内容一致")

    status, _, body = call(port, "GET", "/api/files/" + quote(name, safe=""), cookie=cookie,
                           headers={"Range": "bytes=0-9"})
    check(status == 206 and body == data[:10], "断点续传 Range")

    status, _, _ = call(port, "POST", "/api/text", json.dumps({"text": "你好"}).encode("utf-8"), cookie)
    _, _, body = call(port, "GET", "/api/text", cookie=cookie)
    check(status == 201 and json.loads(body)[0]["text"] == "你好", "文字收发")

    status, _, body = call(port, "GET", "/qr.svg", cookie=cookie)
    check(status == 200 and b"<svg" in body, "二维码生成（qrcode 已打包）")


def kill_tree(proc):
    if os.name == "nt":
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True)
    else:
        proc.kill()
    proc.wait(timeout=10)


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    command = sys.argv[1:]
    if not command:
        print(__doc__)
        return 2
    if os.path.isfile(command[0]):
        # Windows 的 CreateProcess 不认 "dist/LanShare.exe" 这种正斜杠相对路径
        command[0] = os.path.abspath(command[0])
    work = tempfile.mkdtemp(prefix="lanshare-smoke-")
    share = os.path.join(work, "share")
    info_path = os.path.join(work, "info.json")
    # 日志和单实例记录写到临时目录，别碰本机真实的 %LOCALAPPDATA%\LanShare
    env = dict(os.environ, PYTHONIOENCODING="utf-8", LOCALAPPDATA=work)
    proc = subprocess.Popen(
        command + ["--no-browser", "--no-tray", "--port", "18765", "--dir", share, "--info-file", info_path],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env,
    )
    try:
        port, pin = read_info(proc, info_path)
        print("已启动：端口 %d" % port)
        run_checks(port, pin)
        print("SMOKE OK")
        return 0
    except Exception as exc:
        print("SMOKE FAILED: %s" % exc)
        return 1
    finally:
        kill_tree(proc)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
