"""找本机的局域网 IPv4 地址，排除代理 TUN、VPN 等常用的非局域网地址。"""
import ipaddress
import socket

# 只认 RFC 1918 私有网段。顺序即优先级：家用路由器最常见 192.168，其次 10；
# 172.16/12 放最后，因为 WSL、Hyper-V、Docker 的虚拟网卡通常在这个段。
_LAN_NETWORKS = [
    ipaddress.IPv4Network("192.168.0.0/16"),
    ipaddress.IPv4Network("10.0.0.0/8"),
    ipaddress.IPv4Network("172.16.0.0/12"),
]


def _rank(address):
    for index, network in enumerate(_LAN_NETWORKS):
        if address in network:
            return index
    return None


def pick_lan_ips(addresses, primary=None):
    """从候选地址里挑出局域网 IPv4 并去重排序；primary（默认路由地址）合格时排第一。"""
    candidates = list(addresses) + ([primary] if primary else [])
    ranked = []
    seen = set()
    for raw in candidates:
        try:
            address = ipaddress.IPv4Address(raw)
        except ValueError:
            continue
        rank = _rank(address)
        if rank is None or str(address) in seen:
            continue
        seen.add(str(address))
        # x.x.x.1 一般是路由器或本机上的虚拟网卡（VirtualBox、VMware、移动热点、WSL），排到真实网卡后面
        suspect = str(address).endswith(".1")
        ranked.append(((suspect, rank), str(address)))
    ranked.sort(key=lambda item: item[0])
    result = [ip for _, ip in ranked]
    if primary in result:
        result.remove(primary)
        result.insert(0, primary)
    return result


def default_route_ip():
    """默认路由所在网卡的地址（UDP connect 不会真的发包）；没有网络时返回 None。"""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        sock.connect(("8.8.8.8", 80))
        return sock.getsockname()[0]
    except OSError:
        return None
    finally:
        sock.close()


def all_ipv4_addresses():
    try:
        infos = socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET)
    except OSError:
        return []
    return [info[4][0] for info in infos]


def lan_ips():
    return pick_lan_ips(all_ipv4_addresses(), primary=default_route_ip())
