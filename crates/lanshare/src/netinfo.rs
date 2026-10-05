//! 找本机的局域网 IPv4 地址，排除代理 TUN（198.18.x）、CGNAT/组网软件（100.64/10）等非局域网地址。

use std::net::{Ipv4Addr, UdpSocket};

/// 只认 RFC 1918 私有网段；顺序即优先级：家用路由器最常见 192.168，其次 10，172.16/12 最后
/// （WSL、Hyper-V、Docker 的虚拟网卡通常在这个段）。
fn network_rank(ip: Ipv4Addr) -> Option<u8> {
    let [a, b, ..] = ip.octets();
    match (a, b) {
        (192, 168) => Some(0),
        (10, _) => Some(1),
        (172, 16..=31) => Some(2),
        _ => None,
    }
}

/// 从候选地址里挑出局域网 IPv4 并去重排序；`primary`（默认路由地址）合格时排第一。
/// `x.x.x.1` 一般是路由器或本机上的虚拟网卡（VirtualBox、VMware、移动热点、WSL），排到真实网卡后面。
pub fn pick_lan_ips(addresses: &[Ipv4Addr], primary: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut ranked: Vec<((bool, u8), Ipv4Addr)> = Vec::new();
    for &ip in addresses.iter().chain(primary.iter()) {
        let Some(rank) = network_rank(ip) else { continue };
        if ranked.iter().any(|(_, seen)| *seen == ip) {
            continue;
        }
        let suspect = ip.octets()[3] == 1;
        ranked.push(((suspect, rank), ip));
    }
    ranked.sort_by_key(|(key, _)| *key); // 稳定排序：同一档内保持原顺序
    let mut result: Vec<Ipv4Addr> = ranked.into_iter().map(|(_, ip)| ip).collect();
    if let Some(p) = primary
        && let Some(pos) = result.iter().position(|ip| *ip == p)
    {
        result.remove(pos);
        result.insert(0, p);
    }
    result
}

/// 默认路由所在网卡的地址（UDP connect 不会真的发包）；没有网络时返回 None。
pub fn default_route_ip() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) => Some(ip),
        std::net::IpAddr::V6(_) => None,
    }
}

/// 本机所有网卡的 IPv4 地址。
pub fn all_ipv4() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .map(|list| {
            list.into_iter()
                .filter_map(|iface| match iface.ip() {
                    std::net::IpAddr::V4(ip) => Some(ip),
                    std::net::IpAddr::V6(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn lan_ips() -> Vec<Ipv4Addr> {
    pick_lan_ips(&all_ipv4(), default_route_ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(list: &[&str]) -> Vec<Ipv4Addr> {
        list.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn filters_non_lan_addresses() {
        let input = ips(&["127.0.0.1", "169.254.3.4", "8.8.8.8", "198.18.0.1", "100.64.1.2", "0.0.0.0", "192.168.1.5"]);
        assert_eq!(pick_lan_ips(&input, None), ips(&["192.168.1.5"]));
    }

    #[test]
    fn orders_by_network_preference() {
        let input = ips(&["172.24.16.1", "10.0.0.8", "192.168.1.5"]);
        assert_eq!(pick_lan_ips(&input, None), ips(&["192.168.1.5", "10.0.0.8", "172.24.16.1"]));
    }

    #[test]
    fn primary_goes_first_when_it_is_lan() {
        let input = ips(&["192.168.1.5", "10.8.0.2"]);
        assert_eq!(pick_lan_ips(&input, Some("10.8.0.2".parse().unwrap())), ips(&["10.8.0.2", "192.168.1.5"]));
    }

    #[test]
    fn proxy_tun_primary_is_ignored() {
        let input = ips(&["198.18.0.1", "172.24.16.1", "192.168.1.5"]);
        assert_eq!(pick_lan_ips(&input, Some("198.18.0.1".parse().unwrap())), ips(&["192.168.1.5", "172.24.16.1"]));
    }

    #[test]
    fn virtual_host_adapters_rank_after_real_ones() {
        let input = ips(&["192.168.56.1", "192.168.137.1", "10.0.0.8", "192.168.40.1", "192.168.1.5"]);
        assert_eq!(
            pick_lan_ips(&input, Some("198.18.0.1".parse().unwrap())),
            ips(&["192.168.1.5", "10.0.0.8", "192.168.56.1", "192.168.137.1", "192.168.40.1"])
        );
    }

    #[test]
    fn primary_missing_from_list_is_added_and_deduped() {
        assert_eq!(pick_lan_ips(&[], Some("192.168.0.7".parse().unwrap())), ips(&["192.168.0.7"]));
        let input = ips(&["192.168.1.5", "192.168.1.5"]);
        assert_eq!(pick_lan_ips(&input, Some("192.168.1.5".parse().unwrap())), ips(&["192.168.1.5"]));
    }
}
