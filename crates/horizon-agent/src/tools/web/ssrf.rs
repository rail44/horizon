use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Url;

const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_URL_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug, Default)]
pub(super) struct SafeResolver;

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        Box::pin(async move {
            reject_hostname(&host).map_err(boxed)?;
            let resolved = tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((&*host, 0)))
                .await
                .map_err(|_| {
                    boxed(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "DNS lookup timed out",
                    ))
                })?
                .map_err(boxed)?
                .collect::<Vec<_>>();
            let resolved = validate_resolved_addresses(resolved).map_err(boxed)?;
            Ok(Box::new(resolved.into_iter()) as Addrs)
        })
    }
}

fn boxed(error: io::Error) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(error)
}

fn validate_resolved_addresses(resolved: Vec<SocketAddr>) -> io::Result<Vec<SocketAddr>> {
    if resolved.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "DNS returned no addresses",
        ));
    }
    // Fail the whole lookup when even one answer is unsafe. Selecting only
    // the public subset would make the policy depend on resolver ordering
    // and leave a mixed public/private answer usable for rebinding probes.
    if resolved.iter().any(|addr| is_forbidden_ip(addr.ip())) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "DNS returned a non-public address",
        ));
    }
    Ok(resolved)
}

pub(super) fn validate_url(url: &Url) -> Result<String, String> {
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(format!(
            "web_fetch URL exceeds the {MAX_URL_BYTES} byte limit"
        ));
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err("web_fetch only supports http:// and https:// URLs".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("web_fetch URLs must not contain credentials".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "web_fetch URL has no host".to_string())?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    reject_hostname(&host).map_err(|error| error.to_string())?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err("web_fetch refuses non-public IP addresses".to_string());
        }
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "web_fetch URL has no usable port".to_string())?;
    let expected_port = if url.scheme() == "https" { 443 } else { 80 };
    if port != expected_port {
        return Err("web_fetch only permits the standard HTTP and HTTPS ports".to_string());
    }
    Ok(host)
}

fn reject_hostname(host: &str) -> io::Result<()> {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    if normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized == "metadata.google.internal"
        || normalized.ends_with(".metadata.google.internal")
        || normalized == "instance-data"
        || normalized.ends_with(".instance-data")
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "metadata and localhost names are not fetchable",
        ));
    }
    Ok(())
}

pub(super) fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => forbidden_v4(ip),
        IpAddr::V6(ip) => forbidden_v6(ip),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let value = u32::from(ip);
    let in_net = |base: [u8; 4], prefix: u8| {
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - u32::from(prefix))
        };
        value & mask == u32::from(Ipv4Addr::from(base)) & mask
    };

    // Deny every special-use range that cannot be an ordinary public web
    // destination. The explicit table also covers cloud metadata and
    // carrier-grade NAT, which `Ipv4Addr::is_private` alone does not.
    [
        ([0, 0, 0, 0], 8),
        ([10, 0, 0, 0], 8),
        ([100, 64, 0, 0], 10),
        ([127, 0, 0, 0], 8),
        ([169, 254, 0, 0], 16),
        ([172, 16, 0, 0], 12),
        ([192, 0, 0, 0], 24),
        ([192, 0, 2, 0], 24),
        ([192, 168, 0, 0], 16),
        ([198, 18, 0, 0], 15),
        ([198, 51, 100, 0], 24),
        ([203, 0, 113, 0], 24),
        ([224, 0, 0, 0], 4),
        ([240, 0, 0, 0], 4),
    ]
    .into_iter()
    .any(|(base, prefix)| in_net(base, prefix))
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let value = u128::from(ip);
    let in_net = |base: Ipv6Addr, prefix: u8| {
        let mask = if prefix == 0 {
            0
        } else {
            u128::MAX << (128 - u32::from(prefix))
        };
        value & mask == u128::from(base) & mask
    };

    ip.is_unspecified()
        || ip.is_loopback()
        // Public IPv6 unicast allocations currently live in 2000::/3.
        // Refusing everything else closes IPv4-compatible and other
        // special-purpose forms by default instead of chasing aliases.
        || !in_net(Ipv6Addr::from(0x2000_u128 << 112), 3)
        // Translation and transition mechanisms can carry an otherwise
        // forbidden IPv4 destination inside a globally-shaped IPv6
        // literal. v1 does not need them, so refuse each family wholesale.
        || in_net("64:ff9b::".parse().expect("static NAT64 prefix"), 96)
        || in_net(
            "64:ff9b:1::"
                .parse()
                .expect("static local-use NAT64 prefix"),
            48,
        )
        || in_net("2001::".parse().expect("static IETF protocol prefix"), 23)
        || in_net("2002::".parse().expect("static 6to4 prefix"), 16)
        || in_net(Ipv6Addr::from(0xfc00_u128 << 112), 7)
        || in_net(Ipv6Addr::from(0xfe80_u128 << 112), 10)
        || in_net(Ipv6Addr::from(0xfec0_u128 << 112), 10)
        || in_net(Ipv6Addr::from(0xff00_u128 << 112), 8)
        || in_net(
            "2001:db8::"
                .parse()
                .expect("static IPv6 documentation prefix"),
            32,
        )
        || ip
            == "fd00:ec2::254"
                .parse::<Ipv6Addr>()
                .expect("static IPv6 metadata address")
}

pub(super) fn remote_addr_is_safe(remote: Option<SocketAddr>) -> bool {
    remote.is_some_and(|addr| !is_forbidden_ip(addr.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_metadata_loopback_and_documentation_addresses() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "100.100.100.200",
            "169.254.169.254",
            "192.0.2.1",
            "::1",
            "fd00:ec2::254",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::a9fe:a9fe",
            "2002:a9fe:a9fe::1",
            "2001::1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn validates_scheme_credentials_and_static_hostnames_before_dns() {
        for url in [
            "file:///etc/passwd",
            "http://user:pass@example.com/",
            "http://localhost/",
            "http://metadata.google.internal/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://example.com:22/",
        ] {
            assert!(validate_url(&Url::parse(url).unwrap()).is_err(), "{url}");
        }
        assert_eq!(
            validate_url(&Url::parse("https://Example.COM./docs").unwrap()).unwrap(),
            "example.com"
        );
        assert_eq!(
            validate_url(&Url::parse("https://[2606:4700:4700::1111]/").unwrap()).unwrap(),
            "2606:4700:4700::1111"
        );
    }

    #[test]
    fn rejects_an_entire_mixed_public_private_dns_answer() {
        let mixed = vec![
            "1.1.1.1:443".parse().unwrap(),
            "127.0.0.1:443".parse().unwrap(),
        ];
        let error = validate_resolved_addresses(mixed).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let public = vec!["1.1.1.1:443".parse().unwrap()];
        assert_eq!(validate_resolved_addresses(public.clone()).unwrap(), public);
        assert_eq!(
            validate_resolved_addresses(Vec::new()).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
}
