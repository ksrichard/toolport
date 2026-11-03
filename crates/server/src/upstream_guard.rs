//! Blocks upstream calls to private, loopback, and link-local addresses (SSRF).

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use reqwest::{
    Url,
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
};

const MAX_REDIRECTS: usize = 10;

#[derive(Debug, thiserror::Error)]
#[error("the upstream address is not allowed")]
pub struct BlockedAddress;

/// Loopback is the only range that `allow_loopback` opens. It is for local dev and tests.
pub fn is_allowed(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_allowed(v4, allow_loopback),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4_allowed(v4, allow_loopback),
            None => v6_allowed(v6, allow_loopback),
        },
    }
}

fn v4_allowed(ip: Ipv4Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return allow_loopback;
    }
    let [a, b, ..] = ip.octets();
    let shared = a == 100 && (64..128).contains(&b);
    !(ip.is_unspecified()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || shared)
}

fn v6_allowed(ip: Ipv6Addr, allow_loopback: bool) -> bool {
    if ip.is_loopback() {
        return allow_loopback;
    }
    let first = ip.segments()[0];
    let unique_local = first & 0xfe00 == 0xfc00;
    let link_local = first & 0xffc0 == 0xfe80;
    !(ip.is_unspecified() || ip.is_multicast() || unique_local || link_local)
}

/// Checks the scheme and an IP-literal host. Host names are checked later, by the resolver.
pub fn check_url(url: &Url, allow_loopback: bool) -> Result<(), BlockedAddress> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BlockedAddress);
    }
    let host = url.host_str().ok_or(BlockedAddress)?;
    match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(ip) if !is_allowed(ip, allow_loopback) => Err(BlockedAddress),
        _ => Ok(()),
    }
}

struct GuardedResolver {
    allow_loopback: bool,
}

impl Resolve for GuardedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let allow_loopback = self.allow_loopback;
        Box::pin(async move {
            let found = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let allowed: Vec<SocketAddr> = found
                .filter(|addr| is_allowed(addr.ip(), allow_loopback))
                .collect();
            if allowed.is_empty() {
                return Err(BlockedAddress.into());
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// The client checks every resolved address and every redirect target.
/// IP literals skip the resolver, so callers must call `check_url` on the first URL.
pub fn upstream_client(allow_loopback: bool) -> Result<reqwest::Client, reqwest::Error> {
    let policy = Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if check_url(attempt.url(), allow_loopback).is_err() {
            attempt.error(BlockedAddress)
        } else {
            attempt.follow()
        }
    });
    reqwest::Client::builder()
        .user_agent("toolport")
        // A proxy resolves names itself, so the guarded resolver would not run.
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .dns_resolver(Arc::new(GuardedResolver { allow_loopback }))
        .redirect(policy)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn public_addresses_are_allowed() {
        assert!(is_allowed(ip("1.1.1.1"), false));
        assert!(is_allowed(ip("2606:4700::1111"), false));
    }

    #[test]
    fn private_ranges_are_blocked() {
        for text in [
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "169.254.169.254",
            "fc00::1",
            "fe80::1",
            "::",
            "224.0.0.1",
        ] {
            assert!(!is_allowed(ip(text), true), "{text}");
        }
    }

    #[test]
    fn loopback_follows_the_setting() {
        for text in ["127.0.0.1", "::1"] {
            assert!(!is_allowed(ip(text), false), "{text}");
            assert!(is_allowed(ip(text), true), "{text}");
        }
    }

    #[test]
    fn mapped_ipv6_is_checked_as_ipv4() {
        assert!(!is_allowed(ip("::ffff:10.0.0.1"), false));
        assert!(is_allowed(ip("::ffff:127.0.0.1"), true));
    }

    #[test]
    fn the_setting_allows_only_loopback() {
        assert!(!is_allowed(ip("10.0.0.1"), true));
    }

    #[test]
    fn check_url_refuses_ip_literals() {
        let metadata = Url::parse("http://169.254.169.254/").unwrap();
        assert!(check_url(&metadata, true).is_err());
        let loopback = Url::parse("http://[::1]/").unwrap();
        assert!(check_url(&loopback, false).is_err());
        assert!(check_url(&loopback, true).is_ok());
    }
}
