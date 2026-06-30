#![allow(dead_code, unused_imports)]
use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::LazyLock;
use url::Url;

static UNSPECIFIED_NETWORKS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        "0.0.0.0/8".parse().unwrap(),
        "::/128".parse().unwrap(),
    ]
});

static LOOPBACK_NETWORKS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        "127.0.0.0/8".parse().unwrap(),
        "::1/128".parse().unwrap(),
    ]
});

static PRIVATE_NETWORKS: LazyLock<Vec<IpNet>> = LazyLock::new(|| {
    vec![
        "10.0.0.0/8".parse().unwrap(),
        "172.16.0.0/12".parse().unwrap(),
        "192.168.0.0/16".parse().unwrap(),
        "192.0.0.0/24".parse().unwrap(),
        "192.0.2.0/24".parse().unwrap(),
        "198.51.100.0/24".parse().unwrap(),
        "203.0.113.0/24".parse().unwrap(),
        "192.88.99.0/24".parse().unwrap(),
        "198.18.0.0/15".parse().unwrap(),
        "240.0.0.0/4".parse().unwrap(),
        "100.64.0.0/10".parse().unwrap(),
        "100::/64".parse().unwrap(),
        "2001::/23".parse().unwrap(),
        "2001:2::/48".parse().unwrap(),
        "2001:db8::/32".parse().unwrap(),
        "2001::/32".parse().unwrap(),
        "fc00::/7".parse().unwrap(),
        "fe80::/10".parse().unwrap(),
        "fec0::/10".parse().unwrap(),
        "ff00::/8".parse().unwrap(),
        "2002::/16".parse().unwrap(),
        "64:ff9b::/96".parse().unwrap(),
        "2001:10::/28".parse().unwrap(),
        "2001:20::/28".parse().unwrap(),
    ]
});

#[derive(Debug, Clone)]
pub struct AllowedIPError {
    pub ip: IpAddr,
    pub message: String,
}

impl fmt::Display for AllowedIPError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IP {} is not allowed: {}", self.ip, self.message)
    }
}

impl std::error::Error for AllowedIPError {}

pub fn is_safe_ip(ip: IpAddr, allow_localhost: bool) -> Result<(), AllowedIPError> {
    if ip == IpAddr::V4(Ipv4Addr::BROADCAST) {
        return Err(AllowedIPError {
            ip,
            message: "limited broadcast IPs are not allowed".to_string(),
        });
    }

    for network in PRIVATE_NETWORKS.iter() {
        if network.contains(&ip) {
            return Err(AllowedIPError {
                ip,
                message: "private IPs are not allowed".to_string(),
            });
        }
    }

    if !allow_localhost {
        for network in LOOPBACK_NETWORKS.iter() {
            if network.contains(&ip) {
                return Err(AllowedIPError {
                    ip,
                    message: "loopback IPs are not allowed".to_string(),
                });
            }
        }

        for network in UNSPECIFIED_NETWORKS.iter() {
            if network.contains(&ip) {
                return Err(AllowedIPError {
                    ip,
                    message: "unspecified IPs are not allowed".to_string(),
                });
            }
        }
    }

    if ip.is_multicast() {
        return Err(AllowedIPError {
            ip,
            message: "multicast IPs are not allowed".to_string(),
        });
    }

    if is_link_local(&ip) {
        return Err(AllowedIPError {
            ip,
            message: "link-local unicast and multicast IPs are not allowed".to_string(),
        });
    }

    Ok(())
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            segments[0] & 0xffc0 == 0xfe80
        }
    }
}

pub fn parse_socket_addr(address: &str) -> Option<SocketAddr> {
    SocketAddr::from_str(address).ok()
}

pub fn resolve_endpoint_match(
    ip: IpAddr,
    allowed_endpoints: &[String],
) -> Result<bool, String> {
    for endpoint in allowed_endpoints {
        if endpoint.contains("://") {
            if let Ok(url) = Url::parse(endpoint) {
                if let Some(host) = url.host_str() {
                    if endpoint_matches_ip(ip, host)? {
                        return Ok(true);
                    }
                }
            }
        } else if endpoint.contains(':') {
            let host_part = if let Ok(addr) = SocketAddr::from_str(endpoint) {
                addr.ip()
            } else {
                continue;
            };
            if host_part == ip {
                return Ok(true);
            }
        } else {
            if endpoint_matches_ip(ip, endpoint)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn endpoint_matches_ip(ip: IpAddr, hostname: &str) -> Result<bool, String> {
    if let Ok(parsed_ip) = IpAddr::from_str(hostname) {
        return Ok(parsed_ip == ip);
    }

    if let Ok(network) = hostname.parse::<IpNet>() {
        return Ok(network.contains(&ip));
    }

    Ok(false)
}

pub fn validate_address(
    address: &str,
    allow_localhost: bool,
    allowed_endpoints: &[String],
) -> Result<(), AllowedIPError> {
    let ip = extract_ip_from_address(address)?;

    if !allowed_endpoints.is_empty() {
        match resolve_endpoint_match(ip, allowed_endpoints) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(_) => {}
        }
    }

    is_safe_ip(ip, allow_localhost)
}

fn extract_ip_from_address(address: &str) -> Result<IpAddr, AllowedIPError> {
    if let Ok(addr) = SocketAddr::from_str(address) {
        return Ok(addr.ip());
    }

    let host = address
        .trim_start_matches('[')
        .trim_end_matches(']');

    IpAddr::from_str(host).map_err(|_| AllowedIPError {
        ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        message: format!("invalid IP address: {}", address),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_safe_public_ip() {
        assert!(is_safe_ip("93.184.215.14".parse().unwrap(), false).is_ok());
        assert!(is_safe_ip("8.8.8.8".parse().unwrap(), false).is_ok());
    }

    #[test]
    fn test_is_safe_private_ip() {
        let err = is_safe_ip("192.168.0.1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("private"));

        let err = is_safe_ip("172.16.0.1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("private"));

        let err = is_safe_ip("10.0.0.1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("private"));
    }

    #[test]
    fn test_is_safe_loopback() {
        let err = is_safe_ip("127.0.0.1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("loopback"));

        assert!(is_safe_ip("127.0.0.1".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn test_is_safe_unspecified() {
        let err = is_safe_ip("0.0.0.0".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("unspecified"));

        assert!(is_safe_ip("0.0.0.0".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn test_is_safe_link_local() {
        let err = is_safe_ip("169.254.0.1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("link-local"));
    }

    #[test]
    fn test_is_safe_broadcast() {
        let err = is_safe_ip("255.255.255.255".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("broadcast"));
    }

    #[test]
    fn test_is_safe_multicast() {
        let err = is_safe_ip("224.0.0.0".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("multicast"));
    }

    #[test]
    fn test_private_ipv6() {
        let err = is_safe_ip("fd00::1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("private"));
    }

    #[test]
    fn test_loopback_ipv6() {
        let err = is_safe_ip("::1".parse().unwrap(), false).unwrap_err();
        assert!(err.message.contains("loopback"));
    }

    #[test]
    fn test_allowed_endpoint_ip() {
        let ip: IpAddr = "172.16.123.1".parse().unwrap();
        let allowed = vec!["172.16.123.1".to_string()];
        assert!(resolve_endpoint_match(ip, &allowed).unwrap());
    }

    #[test]
    fn test_allowed_endpoint_url() {
        let ip: IpAddr = "172.16.123.1".parse().unwrap();
        let allowed = vec!["http://172.16.123.1:9000".to_string()];
        assert!(resolve_endpoint_match(ip, &allowed).unwrap());
    }

    #[test]
    fn test_allowed_endpoint_cidr() {
        let ip: IpAddr = "10.12.24.1".parse().unwrap();
        let allowed = vec!["10.12.24.0/23".to_string()];
        assert!(resolve_endpoint_match(ip, &allowed).unwrap());
    }

    #[test]
    fn test_allowed_endpoint_no_match() {
        let ip: IpAddr = "10.13.24.1".parse().unwrap();
        let allowed = vec!["10.12.24.0/23".to_string()];
        assert!(!resolve_endpoint_match(ip, &allowed).unwrap());
    }

    #[test]
    fn test_extract_ip_from_address() {
        let ip = extract_ip_from_address("93.184.215.14:80").unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(93, 184, 215, 14)));
    }

    #[test]
    fn test_extract_ipv6_from_address() {
        let ip = extract_ip_from_address("[::1]:80").unwrap();
        assert_eq!(ip, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn test_validate_address_public() {
        assert!(validate_address("93.184.215.14:80", false, &[]).is_ok());
    }

    #[test]
    fn test_validate_address_private() {
        let err = validate_address("192.168.0.0:80", false, &[]).unwrap_err();
        assert!(err.message.contains("private"));
    }

    #[test]
    fn test_validate_address_allowed() {
        assert!(validate_address("172.16.123.1:9000", false, &["http://172.16.123.1:9000".to_string()]).is_ok());
    }
}
