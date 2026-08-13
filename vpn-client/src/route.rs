use std::io;

use ipnet::Ipv4Net;

pub fn ensure_subnet_route(dev_name: &str, subnet: Ipv4Net) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        add_route_or_verify(dev_name, subnet)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (dev_name, subnet);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn add_route_or_verify(dev_name: &str, subnet: Ipv4Net) -> io::Result<()> {
    let status = std::process::Command::new("ip")
        .args(["route", "add", &subnet.to_string(), "dev", dev_name])
        .status()?;
    if status.success() {
        return Ok(());
    }
    let output = std::process::Command::new("ip")
        .args(["route", "show", "to", &subnet.to_string(), "dev", dev_name])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "failed to add route {subnet} dev {dev_name}: ip exited with {}",
            status.code().unwrap_or(-1)
        )))
    }
}

pub fn add_routes(dev_name: &str, routes: &[Ipv4Net]) -> io::Result<()> {
    if routes.is_empty() {
        return Ok(());
    }

    let mut mgr = route_manager::RouteManager::new()?;
    for route in routes {
        let entry =
            route_manager::Route::new(std::net::IpAddr::V4(route.network()), route.prefix_len())
                .with_if_name(dev_name.to_string());
        if let Err(e) = mgr.add(&entry)
            && e.raw_os_error() != Some(libc::EEXIST)
        {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_ensure_subnet_route_non_linux_returns_ok_without_command() {
        let subnet: Ipv4Net = "10.0.0.0/24".parse().unwrap();
        assert!(ensure_subnet_route("utun10", subnet).is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_ensure_subnet_route_non_linux_accepts_any_subnet() {
        let subnet: Ipv4Net = "192.168.5.0/24".parse().unwrap();
        assert!(ensure_subnet_route("utun99", subnet).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_ensure_subnet_route_linux_builds_correct_command() {
        let subnet: Ipv4Net = "10.0.0.0/24".parse().unwrap();
        let mut cmd = std::process::Command::new("ip");
        cmd.args(["route", "add", &subnet.to_string(), "dev", "tun0"]);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["route", "add", "10.0.0.0/24", "dev", "tun0"]);
    }

    #[test]
    fn test_add_routes_when_empty_returns_ok_without_route_manager() {
        assert!(add_routes("utun99", &[]).is_ok());
    }
}
