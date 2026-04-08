use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr};

/// Default DNS server (Cloudflare) as string.
pub const DEFAULT_DNS: &str = "1.1.1.1";

/// Default DNS server as `IpAddr`.
pub const DEFAULT_DNS_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

/// TCP port mapping from host to guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortMapping {
    /// Port on the host.
    pub host: u16,
    /// Port inside the guest.
    pub guest: u16,
}

/// Check if any CIDR in the list covers the given IP address.
pub fn cidrs_contain_ip(cidrs: &[String], ip: &str) -> bool {
    let ip: IpAddr = match ip.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    cidrs.iter().any(|cidr| {
        cidr.parse::<IpNet>()
            .or_else(|_| cidr.parse::<IpAddr>().map(IpNet::from))
            .is_ok_and(|net| net.contains(&ip))
    })
}

/// Ensure the default DNS server is reachable in a CIDR allowlist.
///
/// If none of the existing CIDRs cover the DNS IP, appends it as /32.
pub fn ensure_dns_in_cidrs(cidrs: &mut Vec<String>) {
    if !cidrs_contain_ip(cidrs, DEFAULT_DNS) {
        cidrs.push(format!("{}/32", DEFAULT_DNS));
    }
}

/// Allocate a free host port by binding to port 0 and extracting the assigned port.
pub fn allocate_host_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

impl PortMapping {
    /// Create a new port mapping.
    pub fn new(host: u16, guest: u16) -> Self {
        Self { host, guest }
    }

    /// Create a port mapping where host and guest ports are the same.
    pub fn same(port: u16) -> Self {
        Self {
            host: port,
            guest: port,
        }
    }

    /// Convert to `(host, guest)` tuple for storage.
    pub fn to_tuple(&self) -> (u16, u16) {
        (self.host, self.guest)
    }

    /// Batch convert port mappings to tuple format.
    pub fn to_tuples(ports: &[Self]) -> Vec<(u16, u16)> {
        ports.iter().map(|p| p.to_tuple()).collect()
    }

    /// Check for duplicate host ports in a list of mappings.
    pub fn check_duplicates(ports: &[Self]) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for p in ports {
            if !seen.insert(p.host) {
                return Err(format!(
                    "duplicate host port {}: each host port can only be mapped once",
                    p.host
                ));
            }
        }
        Ok(())
    }

    /// Parse a port mapping specification (`HOST:GUEST` or `PORT`).
    pub fn parse(spec: &str) -> Result<Self, String> {
        if let Some((host, guest)) = spec.split_once(':') {
            let host: u16 = host
                .parse()
                .map_err(|_| format!("invalid host port: {}", host))?;
            let guest: u16 = guest
                .parse()
                .map_err(|_| format!("invalid guest port: {}", guest))?;
            Ok(Self::new(host, guest))
        } else {
            let port: u16 = spec
                .parse()
                .map_err(|_| format!("invalid port: {}", spec))?;
            Ok(Self::same(port))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cidrs_contain_ip() {
        assert!(cidrs_contain_ip(&["1.1.1.1".into()], "1.1.1.1"));
        assert!(cidrs_contain_ip(&["1.1.1.1/32".into()], "1.1.1.1"));
        assert!(cidrs_contain_ip(&["0.0.0.0/0".into()], "8.8.8.8"));
        assert!(cidrs_contain_ip(&["10.0.0.0/8".into()], "10.5.3.1"));
        assert!(!cidrs_contain_ip(&["10.0.0.0/8".into()], "1.1.1.1"));
        assert!(cidrs_contain_ip(
            &["192.168.1.0/24".into()],
            "192.168.1.100"
        ));
        assert!(!cidrs_contain_ip(&["192.168.1.0/24".into()], "192.168.2.1"));
        assert!(cidrs_contain_ip(
            &["10.0.0.0/8".into(), "1.1.1.1/32".into()],
            "1.1.1.1"
        ));
        assert!(!cidrs_contain_ip(&[], "1.1.1.1"));
        assert!(!cidrs_contain_ip(&["not-a-cidr".into()], "1.1.1.1"));
    }

    #[test]
    fn test_ensure_dns_adds_when_missing() {
        let mut cidrs = vec!["10.0.0.0/8".to_string()];
        ensure_dns_in_cidrs(&mut cidrs);
        assert_eq!(cidrs.len(), 2);
        assert!(cidrs.contains(&"1.1.1.1/32".to_string()));
    }

    #[test]
    fn test_ensure_dns_skips_when_covered_by_subnet() {
        let mut cidrs = vec!["1.0.0.0/8".to_string()];
        ensure_dns_in_cidrs(&mut cidrs);
        assert_eq!(cidrs.len(), 1);
    }

    #[test]
    fn test_ensure_dns_skips_when_exact_match() {
        let mut cidrs = vec!["10.0.0.0/8".to_string(), "1.1.1.1/32".to_string()];
        ensure_dns_in_cidrs(&mut cidrs);
        assert_eq!(cidrs.len(), 2);
    }
}
