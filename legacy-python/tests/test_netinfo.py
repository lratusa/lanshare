import unittest

from lanshare.netinfo import lan_ips, pick_lan_ips


class PickLanIpsTest(unittest.TestCase):
    def test_filters_non_lan_addresses(self):
        addresses = [
            "127.0.0.1", "169.254.3.4", "8.8.8.8", "198.18.0.1", "100.64.1.2",
            "0.0.0.0", "fe80::1", "garbage", "192.168.1.5",
        ]
        self.assertEqual(pick_lan_ips(addresses), ["192.168.1.5"])

    def test_orders_by_network_preference(self):
        self.assertEqual(
            pick_lan_ips(["172.24.16.1", "10.0.0.8", "192.168.1.5"]),
            ["192.168.1.5", "10.0.0.8", "172.24.16.1"],
        )

    def test_primary_goes_first_when_it_is_lan(self):
        self.assertEqual(
            pick_lan_ips(["192.168.1.5", "10.8.0.2"], primary="10.8.0.2"),
            ["10.8.0.2", "192.168.1.5"],
        )

    def test_proxy_tun_primary_is_ignored(self):
        # Clash 等代理开 TUN 模式时默认路由走 198.18.x，不能拿来当二维码地址
        self.assertEqual(
            pick_lan_ips(["198.18.0.1", "172.24.16.1", "192.168.1.5"], primary="198.18.0.1"),
            ["192.168.1.5", "172.24.16.1"],
        )

    def test_virtual_host_adapters_rank_after_real_ones(self):
        # VirtualBox host-only、Windows 移动热点、VMware VMnet、WSL 在本机一侧一般是 x.x.x.1
        self.assertEqual(
            pick_lan_ips(["192.168.56.1", "192.168.137.1", "10.0.0.8", "192.168.40.1", "192.168.1.5"],
                         primary="198.18.0.1"),
            ["192.168.1.5", "10.0.0.8", "192.168.56.1", "192.168.137.1", "192.168.40.1"],
        )

    def test_primary_missing_from_list_is_added(self):
        self.assertEqual(pick_lan_ips([], primary="192.168.0.7"), ["192.168.0.7"])

    def test_dedupes(self):
        self.assertEqual(
            pick_lan_ips(["192.168.1.5", "192.168.1.5"], primary="192.168.1.5"),
            ["192.168.1.5"],
        )

    def test_nothing_found(self):
        self.assertEqual(pick_lan_ips([], primary=None), [])


class LanIpsSmokeTest(unittest.TestCase):
    def test_returns_list_of_strings(self):
        result = lan_ips()
        self.assertIsInstance(result, list)
        for ip in result:
            self.assertIsInstance(ip, str)


if __name__ == "__main__":
    unittest.main()
