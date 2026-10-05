"""文字消息板：只存在内存里，保留最近的若干条。"""
import threading
import time
from collections import deque

MAX_MESSAGES = 50
MAX_TEXT_LENGTH = 10000


class TextBoard:
    def __init__(self, clock=time.time):
        self._items = deque(maxlen=MAX_MESSAGES)
        self._next_id = 1
        self._clock = clock
        self._lock = threading.Lock()

    def add(self, text):
        """新增一条消息并返回它；空白、超长或不是字符串时抛 ValueError。"""
        if not isinstance(text, str) or not text.strip():
            raise ValueError("文字不能为空")
        if len(text) > MAX_TEXT_LENGTH:
            raise ValueError("文字超过 %d 字" % MAX_TEXT_LENGTH)
        with self._lock:
            item = {"id": self._next_id, "text": text, "time": self._clock()}
            self._next_id += 1
            self._items.append(item)
            return dict(item)

    def list(self):
        """所有消息，新的在前。"""
        with self._lock:
            return [dict(item) for item in reversed(self._items)]
