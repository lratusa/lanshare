"""访问口令、会话与防暴力猜口令。"""
import secrets
import threading
import time

COOKIE_NAME = "lanshare_session"
MAX_FAILURES = 5
LOCK_SECONDS = 60

OK = "ok"
WRONG = "wrong"
LOCKED = "locked"


def generate_pin():
    """随机 6 位数字口令。"""
    return "%06d" % secrets.randbelow(1000000)


class Auth:
    """口令只在本次运行有效；会话令牌存在内存里，程序重启后全部失效。"""

    def __init__(self, pin, clock=time.monotonic):
        self.pin = pin
        self._clock = clock
        self._sessions = set()
        self._failures = {}  # ip -> 连续输错次数
        self._locked_until = {}  # ip -> 解锁时刻
        self._lock = threading.Lock()

    def try_login(self, ip, pin):
        """校验口令，返回 (结果, 会话令牌)。结果是 OK / WRONG / LOCKED，只有 OK 时令牌非空。"""
        with self._lock:
            now = self._clock()
            if self._locked_until.get(ip, float("-inf")) > now:
                return LOCKED, None
            if secrets.compare_digest(str(pin).encode("utf-8"), self.pin.encode("utf-8")):
                self._failures.pop(ip, None)
                token = secrets.token_hex(32)
                self._sessions.add(token)
                return OK, token
            count = self._failures.get(ip, 0) + 1
            if count >= MAX_FAILURES:
                self._failures.pop(ip, None)
                self._locked_until[ip] = now + LOCK_SECONDS
            else:
                self._failures[ip] = count
            return WRONG, None

    def is_valid(self, token):
        with self._lock:
            return bool(token) and token in self._sessions
