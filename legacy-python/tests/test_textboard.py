import unittest

from lanshare.textboard import MAX_MESSAGES, MAX_TEXT_LENGTH, TextBoard


class TextBoardTest(unittest.TestCase):
    def test_add_and_list_newest_first(self):
        board = TextBoard(clock=lambda: 42.0)
        board.add("first")
        board.add("second")
        items = board.list()
        self.assertEqual([i["text"] for i in items], ["second", "first"])
        self.assertEqual(items[0]["time"], 42.0)
        self.assertNotEqual(items[0]["id"], items[1]["id"])

    def test_keeps_only_latest_messages(self):
        board = TextBoard()
        for i in range(MAX_MESSAGES + 10):
            board.add("m%d" % i)
        items = board.list()
        self.assertEqual(len(items), MAX_MESSAGES)
        self.assertEqual(items[0]["text"], "m%d" % (MAX_MESSAGES + 9))

    def test_rejects_blank_too_long_and_non_string(self):
        board = TextBoard()
        for bad in ["", "   \n", "x" * (MAX_TEXT_LENGTH + 1), None, 123]:
            with self.assertRaises(ValueError):
                board.add(bad)
        board.add("x" * MAX_TEXT_LENGTH)

    def test_list_returns_copies(self):
        board = TextBoard()
        board.add("a")
        board.list()[0]["text"] = "changed"
        self.assertEqual(board.list()[0]["text"], "a")


if __name__ == "__main__":
    unittest.main()
