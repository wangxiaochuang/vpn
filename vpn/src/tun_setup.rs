use std::io;
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use tun_rs::DeviceBuilder;

pub fn create_tun(subnet: Ipv4Net, mtu: u16) -> io::Result<tun_rs::AsyncDevice> {
    let gateway = gateway_addr(subnet);
    DeviceBuilder::new()
        .ipv4(gateway, subnet.prefix_len(), None)
        .mtu(mtu)
        .build_async()
}

pub fn create_client_tun(
    assigned_ip: Ipv4Addr,
    subnet: Ipv4Net,
    mtu: u16,
) -> io::Result<tun_rs::AsyncDevice> {
    build_client_device(assigned_ip, subnet, mtu).build_async()
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn build_client_device(assigned_ip: Ipv4Addr, subnet: Ipv4Net, mtu: u16) -> DeviceBuilder {
    let gateway = gateway_addr(subnet);
    DeviceBuilder::new()
        .ipv4(assigned_ip, subnet.prefix_len(), Some(gateway))
        .mtu(mtu)
        .associate_route(true)
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
fn build_client_device(assigned_ip: Ipv4Addr, subnet: Ipv4Net, mtu: u16) -> DeviceBuilder {
    let gateway = gateway_addr(subnet);
    DeviceBuilder::new()
        .ipv4(assigned_ip, subnet.prefix_len(), Some(gateway))
        .mtu(mtu)
}

pub fn gateway_addr(subnet: Ipv4Net) -> Ipv4Addr {
    let net = u32::from(subnet.network());
    Ipv4Addr::from(net + 1)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_addr_for_24_returns_network_plus_one() {
        let subnet: Ipv4Net = "10.0.0.0/24".parse().unwrap();
        assert_eq!(gateway_addr(subnet), Ipv4Addr::new(10, 0, 0, 1));
    }

    #[test]
    fn test_gateway_addr_for_30_returns_network_plus_one() {
        let subnet: Ipv4Net = "10.0.0.0/30".parse().unwrap();
        assert_eq!(gateway_addr(subnet), Ipv4Addr::new(10, 0, 0, 1));
    }

    #[test]
    fn test_gateway_addr_for_16_returns_network_plus_one() {
        let subnet: Ipv4Net = "172.16.0.0/16".parse().unwrap();
        assert_eq!(gateway_addr(subnet), Ipv4Addr::new(172, 16, 0, 1));
    }

    #[test]
    fn test_create_client_tun_creates_device_with_mtu() {
        if !has_tun_permission() {
            tracing::warn!("skipping: TUN device creation requires root");
            return;
        }
        let subnet: Ipv4Net = "10.99.0.0/24".parse().unwrap();
        let dev = create_client_tun(Ipv4Addr::new(10, 99, 0, 2), subnet, 1280)
            .expect("create client tun");
        assert_eq!(dev.mtu().expect("device has mtu"), 1280);
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    fn has_tun_permission() -> bool {
        tun_rs::DeviceBuilder::new().build_async().is_ok()
    }

    #[cfg(not(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    )))]
    fn has_tun_permission() -> bool {
        false
    }
}
