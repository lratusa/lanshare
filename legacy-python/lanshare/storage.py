"""共享文件夹：文件名清理、重名改名、列文件、流式写入。"""
import os
import re
import secrets
import threading
import time

TEMP_PREFIX = ".lanshare-"
TEMP_SUFFIX = ".part"
MAX_NAME_BYTES = 200
CHUNK_SIZE = 1024 * 1024
STALE_TEMP_SECONDS = 600

_ILLEGAL_CHARS = re.compile(r'[\\/:*?"<>|\x00-\x1f\x7f]')
_RESERVED_NAMES = (
    {"CON", "PRN", "AUX", "NUL"}
    | {"COM%d" % i for i in range(1, 10)}
    | {"LPT%d" % i for i in range(1, 10)}
)


class IncompleteUpload(Exception):
    """客户端发送的字节数少于它声明的 Content-Length。"""


def sanitize_filename(name):
    """把客户端给的文件名变成能安全放进共享目录的名字。

    开头的点也去掉：既不会产生隐藏文件，也不会和临时文件前缀撞上。
    """
    name = name.replace("\\", "/").split("/")[-1]
    name = _ILLEGAL_CHARS.sub("", name)
    name = name.strip().strip(". ")
    if not name:
        return "unnamed"
    if name.split(".")[0].strip().upper() in _RESERVED_NAMES:
        name = "_" + name
    return _truncate_utf8(name, MAX_NAME_BYTES)


def _truncate_utf8(name, limit):
    if len(name.encode("utf-8")) <= limit:
        return name
    stem, ext = os.path.splitext(name)
    if len(ext.encode("utf-8")) > limit // 4:
        stem, ext = name, ""
    budget = limit - len(ext.encode("utf-8"))
    stem = stem.encode("utf-8")[:budget].decode("utf-8", "ignore")
    return stem + ext


def _candidate_names(name):
    stem, ext = os.path.splitext(name)
    yield name
    n = 1
    while True:
        yield "%s (%d)%s" % (stem, n, ext)
        n += 1


def _remove_quietly(path):
    try:
        os.remove(path)
    except OSError:
        pass


class Storage:
    """共享目录。所有方法都可以被多个线程同时调用。"""

    def __init__(self, root):
        os.makedirs(root, exist_ok=True)
        self.root = os.path.realpath(root)
        self._lock = threading.Lock()

    def list_files(self):
        """返回 [{"name", "size", "mtime"}]，从新到旧；不含隐藏文件、临时文件和子目录。"""
        items = []
        try:
            entries = os.scandir(self.root)
        except FileNotFoundError:
            os.makedirs(self.root, exist_ok=True)  # 运行中被人删了，重建
            return items
        with entries:
            for entry in entries:
                if entry.name.startswith("."):
                    continue
                try:
                    if not entry.is_file():
                        continue
                    stat = entry.stat()
                except OSError:
                    continue
                items.append({"name": entry.name, "size": stat.st_size, "mtime": stat.st_mtime})
        items.sort(key=lambda item: item["mtime"], reverse=True)
        return items

    def resolve(self, name):
        """返回共享目录里名为 name 的文件的绝对路径；不存在、越界、隐藏时返回 None。"""
        if not name or name.startswith(".") or any(c in name for c in "/\\:\x00"):
            return None
        path = os.path.realpath(os.path.join(self.root, name))
        if os.path.normcase(os.path.dirname(path)) != os.path.normcase(self.root):
            return None
        if not os.path.isfile(path):
            return None
        return path

    def save_stream(self, name, stream, length):
        """从 stream 读取正好 length 字节存成文件，返回实际保存的文件名。

        先写隐藏临时文件，读完整后才改成正式名字；字节数不够抛 IncompleteUpload，
        其他错误原样抛出，两种情况都会删掉临时文件。
        """
        safe = sanitize_filename(name)
        os.makedirs(self.root, exist_ok=True)  # 运行中被人删了，重建
        tmp = os.path.join(self.root, TEMP_PREFIX + secrets.token_hex(8) + TEMP_SUFFIX)
        try:
            with open(tmp, "wb") as f:
                remaining = length
                while remaining > 0:
                    chunk = stream.read(min(CHUNK_SIZE, remaining))
                    if not chunk:
                        raise IncompleteUpload("还差 %d 字节" % remaining)
                    f.write(chunk)
                    remaining -= len(chunk)
            with self._lock:
                final = self._free_path(safe)
                os.rename(tmp, final)
            return os.path.basename(final)
        except BaseException:
            _remove_quietly(tmp)
            raise

    def cleanup_stale_temp(self, max_age=STALE_TEMP_SECONDS):
        """删掉上次异常退出留下的临时文件，返回删除个数。

        正在写的临时文件修改时间一直在更新，所以只删很久没动过的；在用的删不掉也不要紧。
        """
        removed = 0
        cutoff = time.time() - max_age
        try:
            entries = list(os.scandir(self.root))
        except OSError:
            return 0
        for entry in entries:
            if not (entry.name.startswith(TEMP_PREFIX) and entry.name.endswith(TEMP_SUFFIX)):
                continue
            try:
                if entry.is_file() and entry.stat().st_mtime < cutoff:
                    os.remove(entry.path)
                    removed += 1
            except OSError:
                pass
        return removed

    def _free_path(self, name):
        for candidate in _candidate_names(name):
            path = os.path.join(self.root, candidate)
            if not os.path.exists(path):
                return path
