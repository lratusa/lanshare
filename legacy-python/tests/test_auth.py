import unittest

from lanshare.auth import LOCK_SECONDS, LOCKED, MAX_FAILURES, OK, WRONG, Auth, generate_pin


class FakeClock:
    def __init__(self):
        self.now = 1000.0

    def __call__(self):
        return self.now


class AuthTest(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        self.auth = Auth("123456", clock=self.clock)

    def test_generate_pin_is_six_digits(self):
        for _ in range(200):
            self.assertRegex(generate_pin(), r"^\d{6}$")

    def test_correct_pin_gives_valid_session(self):
        result, token = self.auth.try_login("1.1.1.1", "123456")
        self.assertEqual(result, OK)
        self.assertEqual(len(token), 64)
        self.assertTrue(self.auth.is_valid(token))

    def test_wrong_pin(self):
        self.assertEqual(self.auth.try_login("1.1.1.1", "000000"), (WRONG, None))

    def test_non_ascii_pin_is_just_wrong(self):
        self.assertEqual(self.auth.try_login("1.1.1.1", "一二三四五六")[0], WRONG)

    def test_unknown_tokens_are_invalid(self):
        for token in [None, "", "abc", "0" * 64]:
            self.assertFalse(self.auth.is_valid(token))

    def test_locks_after_five_failures_then_unlocks(self):
        ip = "192.168.1.9"
        for _ in range(MAX_FAILURES):
            self.assertEqual(self.auth.try_login(ip, "000000")[0], WRONG)
        self.assertEqual(self.auth.try_login(ip, "123456")[0], LOCKED)
        self.clock.now += LOCK_SECONDS - 1
        self.assertEqual(self.auth.try_login(ip, "123456")[0], LOCKED)
        self.clock.now += 2
        self.assertEqual(self.auth.try_login(ip, "123456")[0], OK)

    def test_lock_is_per_ip(self):
        for _ in range(MAX_FAILURES):
            self.auth.try_login("10.0.0.1", "000000")
        self.assertEqual(self.auth.try_login("10.0.0.2", "123456")[0], OK)

    def test_success_resets_failure_count(self):
        ip = "10.0.0.3"
        for _ in range(MAX_FAILURES - 1):
            self.auth.try_login(ip, "000000")
        self.assertEqual(self.auth.try_login(ip, "123456")[0], OK)
        for _ in range(MAX_FAILURES - 1):
            self.assertEqual(self.auth.try_login(ip, "000000")[0], WRONG)
        self.assertEqual(self.auth.try_login(ip, "123456")[0], OK)


if __name__ == "__main__":
    unittest.main()
