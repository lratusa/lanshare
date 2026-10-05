import io
import os
import shutil
import time
import tempfile
import threading
import unittest

from lanshare.storage import (
    CHUNK_SIZE,
    TEMP_PREFIX,
    IncompleteUpload,
    Storage,
    sanitize_filename,
)


class SanitizeFilenameTest(unittest.TestCase):
    def test_keeps_normal_names(self):
        self.assertEqual(sanitize_filename("照片 2026.jpg"), "照片 2026.jpg")

    def test_drops_directories(self):
        self.assertEqual(sanitize_filename("../../etc/passwd"), "passwd")
        self.assertEqual(sanitize_filename("C:\\Windows\\win.ini"), "win.ini")

    def test_removes_illegal_characters(self):
        self.assertEqual(sanitize_filename('a<b>c:d"e|f?g*h.txt'), "abcdefgh.txt")
        self.assertEqual(sanitize_filename("tab\there\n.txt"), "tabhere.txt")

    def test_strips_dots_and_spaces_at_both_ends(self):
        self.assertEqual(sanitize_filename("  report.pdf. . "), "report.pdf")
        self.assertEqual(sanitize_filename(".hidden"), "hidden")
        self.assertEqual(sanitize_filename(".lanshare-abc.part"), "lanshare-abc.part")

    def test_empty_becomes_unnamed(self):
        for raw in ["", "..", "...", "/", "  ", "<>"]:
            self.assertEqual(sanitize_filename(raw), "unnamed", repr(raw))

    def test_prefixes_windows_reserved_names(self):
        self.assertEqual(sanitize_filename("CON"), "_CON")
        self.assertEqual(sanitize_filename("nul.txt"), "_nul.txt")
        self.assertEqual(sanitize_filename("com1.tar.gz"), "_com1.tar.gz")
        self.assertEqual(sanitize_filename("console.txt"), "console.txt")

    def test_truncates_long_names_keeping_extension(self):
        result = sanitize_filename("中" * 150 + ".mp4")
        self.assertTrue(result.endswith(".mp4"))
        self.assertTrue(result.startswith("中"))
        self.assertLessEqual(len(result.encode("utf-8")), 200)


class StorageTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self._tmp.name, "share")
        self.storage = Storage(self.root)

    def tearDown(self):
        self._tmp.cleanup()

    def save(self, name, data):
        return self.storage.save_stream(name, io.BytesIO(data), len(data))

    def test_creates_root(self):
        self.assertTrue(os.path.isdir(self.root))

    def test_save_and_list(self):
        self.assertEqual(self.save("a.txt", b"hello"), "a.txt")
        files = self.storage.list_files()
        self.assertEqual([f["name"] for f in files], ["a.txt"])
        self.assertEqual(files[0]["size"], 5)
        with open(os.path.join(self.root, "a.txt"), "rb") as f:
            self.assertEqual(f.read(), b"hello")

    def test_duplicate_names_get_numbered(self):
        self.assertEqual(self.save("a.txt", b"1"), "a.txt")
        self.assertEqual(self.save("a.txt", b"2"), "a (1).txt")
        self.assertEqual(self.save("a.txt", b"3"), "a (2).txt")

    def test_save_sanitizes_name(self):
        self.assertEqual(self.save("../evil.txt", b"x"), "evil.txt")
        self.assertEqual(os.listdir(self._tmp.name), ["share"])

    def test_incomplete_upload_leaves_nothing(self):
        with self.assertRaises(IncompleteUpload):
            self.storage.save_stream("big.bin", io.BytesIO(b"x" * 10), 100)
        self.assertEqual(os.listdir(self.root), [])

    def test_stream_error_leaves_nothing(self):
        class Broken:
            def read(self, n):
                raise ConnectionResetError("gone")

        with self.assertRaises(ConnectionResetError):
            self.storage.save_stream("big.bin", Broken(), 100)
        self.assertEqual(os.listdir(self.root), [])

    def test_reads_in_bounded_chunks(self):
        sizes = []
        data = io.BytesIO(b"x" * (CHUNK_SIZE * 2 + 5))

        class Recorder:
            def read(self, n):
                sizes.append(n)
                return data.read(n)

        self.storage.save_stream("big.bin", Recorder(), CHUNK_SIZE * 2 + 5)
        self.assertEqual(sizes, [CHUNK_SIZE, CHUNK_SIZE, 5])

    def test_concurrent_same_name_uploads_get_distinct_files(self):
        payloads = [bytes([i]) * 50000 for i in range(8)]
        names = []
        threads = [
            threading.Thread(target=lambda p=p: names.append(self.save("same.bin", p)))
            for p in payloads
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(len(set(names)), 8)
        contents = set()
        for name in names:
            with open(os.path.join(self.root, name), "rb") as f:
                contents.add(f.read())
        self.assertEqual(contents, set(payloads))

    def test_list_hides_temp_hidden_files_and_dirs(self):
        for name in [TEMP_PREFIX + "abc.part", ".hidden"]:
            open(os.path.join(self.root, name), "wb").close()
        os.mkdir(os.path.join(self.root, "folder"))
        self.save("ok.txt", b"1")
        self.assertEqual([f["name"] for f in self.storage.list_files()], ["ok.txt"])

    def test_list_sorted_newest_first(self):
        self.save("old.txt", b"1")
        self.save("new.txt", b"2")
        os.utime(os.path.join(self.root, "old.txt"), (1000, 1000))
        self.assertEqual([f["name"] for f in self.storage.list_files()], ["new.txt", "old.txt"])

    def test_recovers_when_root_deleted_while_running(self):
        shutil.rmtree(self.root)
        self.assertEqual(self.storage.list_files(), [])
        self.assertTrue(os.path.isdir(self.root))
        shutil.rmtree(self.root)
        self.assertEqual(self.save("a.txt", b"1"), "a.txt")

    def test_cleanup_removes_only_stale_temp_files(self):
        stale = os.path.join(self.root, TEMP_PREFIX + "old.part")
        fresh = os.path.join(self.root, TEMP_PREFIX + "new.part")
        for path in (stale, fresh):
            open(path, "wb").close()
        old = time.time() - 3600
        os.utime(stale, (old, old))
        self.save("keep.txt", b"1")
        os.utime(os.path.join(self.root, "keep.txt"), (old, old))
        self.assertEqual(self.storage.cleanup_stale_temp(), 1)
        self.assertEqual(sorted(os.listdir(self.root)), sorted([TEMP_PREFIX + "new.part", "keep.txt"]))

    def test_resolve(self):
        self.save("a.txt", b"1")
        self.assertEqual(self.storage.resolve("a.txt"), os.path.join(self.storage.root, "a.txt"))
        open(os.path.join(self._tmp.name, "secret.txt"), "wb").close()
        open(os.path.join(self.root, TEMP_PREFIX + "x.part"), "wb").close()
        bad_names = [
            "", "missing.txt", "../secret.txt", "..\\secret.txt", "..", ".",
            TEMP_PREFIX + "x.part", "a.txt:stream", "CON",
        ]
        for bad in bad_names:
            self.assertIsNone(self.storage.resolve(bad), repr(bad))


if __name__ == "__main__":
    unittest.main()
