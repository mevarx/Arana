//! URL security helpers: public-target normalization and host matching.
//!
//! Mirrors `agent_reach/utils/url.py`. Every rule that rejects a target is a
//! security control, not a formatting preference.

use std::net::{Ipv4Addr, Ipv6Addr};
use url::Url;

const BLOCKED_HOSTS: &[&str] = &[
    "home.arpa",
    "instance-data",
    "internal",
    "ip6-localhost",
    "ip6-loopback",
    "lan",
    "local",
    "localdomain",
    "localhost",
    "metadata.google.internal",
];

const BLOCKED_SUFFIXES: &[&str] = &[
    ".home.arpa",
    ".internal",
    ".lan",
    ".local",
    ".localdomain",
    ".localhost",
];

/// One failure mode for every rejected target; callers map it to `BAD_INPUT`.
#[derive(Debug, Clone, Copy)]
pub struct InvalidTarget;

impl std::fmt::Display for InvalidTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("only public HTTP(S) URLs are allowed")
    }
}

/// Normalize a URL or reject targets that are not clearly public HTTP(S).
pub fn normalize_public_http_url(raw: &str) -> Result<String, InvalidTarget> {
    let candidate = raw.trim();
    if candidate.is_empty()
        || candidate.contains('\\')
        || candidate
            .chars()
            .any(|c| c.is_whitespace() || (c as u32) < 0x20 || c as u32 == 0x7f)
    {
        return Err(InvalidTarget);
    }
    let candidate = if candidate.contains("://") {
        candidate.to_string()
    } else {
        format!("https://{candidate}")
    };
    let parsed = Url::parse(&candidate).map_err(|_| InvalidTarget)?;
    let authority = candidate
        .split_once("://")
        .ok_or(InvalidTarget)?
        .1
        .split(['/', '?', '#'])
        .next()
        .ok_or(InvalidTarget)?;
    if authority.contains('@') || authority.contains('%') {
        return Err(InvalidTarget);
    }
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(InvalidTarget);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(InvalidTarget);
    }
    let host = parsed
        .host_str()
        .ok_or(InvalidTarget)?
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_string();
    if host.is_empty() || host.contains('%') {
        return Err(InvalidTarget);
    }
    let literal = parse_literal_ip(&host);
    if BLOCKED_HOSTS.contains(&host.as_str())
        || BLOCKED_SUFFIXES.iter().any(|s| host.ends_with(s))
        || (!host.contains('.') && literal.is_none())
        || literal.is_some_and(|ip| !ip_is_global(&ip))
    {
        return Err(InvalidTarget);
    }
    // Validate with the WHATWG parser, but preserve the Python contract's
    // authority/path spelling (including the absence of a root slash).
    let rest = candidate.split_once("://").ok_or(InvalidTarget)?.1;
    Ok(format!("{scheme}://{rest}"))
}

/// Match a hostname/cookie domain exactly or as a real subdomain.
pub fn domain_matches(host: &str, domains: &[&str]) -> bool {
    let normalized = host
        .trim_start_matches('.')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    domains.iter().any(|domain| {
        let allowed = domain
            .trim_start_matches('.')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        normalized == allowed || normalized.ends_with(&format!(".{allowed}"))
    })
}

/// Whether a URL has an exact allowed host or a real subdomain.
///
/// Only HTTP(S) URLs without userinfo are accepted, so `x.com.evil.test` and
/// `x.com@evil.test` never match `x.com`.
pub fn host_matches(raw: &str, domains: &[&str]) -> bool {
    let Ok(parsed) = Url::parse(raw) else {
        return false;
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return false;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    match parsed.host_str() {
        Some(host) => domain_matches(host, domains),
        None => false,
    }
}

enum Literal {
    V4(u32),
    V6(Ipv6Addr),
}

fn parse_literal_ip(host: &str) -> Option<Literal> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Some(Literal::V4(u32::from(ip)));
    }
    if let Ok(ip) = host.parse::<Ipv6Addr>() {
        return Some(Literal::V6(ip));
    }
    // Legacy spellings `inet_aton` accepts: `127.1`, `2130706433`, `0177.0.0.1`.
    parse_legacy_ipv4(host).map(Literal::V4)
}

fn parse_legacy_ipv4(host: &str) -> Option<u32> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        if part.is_empty() {
            return None;
        }
        let value = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
            if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            u32::from_str_radix(hex, 16).ok()?
        } else if part.len() > 1 && part.starts_with('0') {
            if !part.chars().all(|c| ('0'..='7').contains(&c)) {
                return None;
            }
            u32::from_str_radix(part, 8).ok()?
        } else {
            if !part.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            part.parse::<u32>().ok()?
        };
        values.push(value);
    }
    let last_limit = 256u64.checked_pow(4 - values.len() as u32)?;
    if u64::from(*values.last()?) >= last_limit {
        return None;
    }
    let mut address = *values.last()?;
    for (index, value) in values.iter().enumerate().take(values.len() - 1) {
        if *value > 255 {
            return None;
        }
        address |= value << (8 * (3 - index));
    }
    Some(address)
}

fn ip_is_global(literal: &Literal) -> bool {
    match literal {
        Literal::V4(bits) => ipv4_is_global(Ipv4Addr::from(*bits)),
        Literal::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return ipv4_is_global(v4);
            }
            let segments = ip.segments();
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (segments[0] & 0xfe00) == 0xfc00 // unique local
                || (segments[0] & 0xffc0) == 0xfe80 // link local
                || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
                || (segments[0] == 0x0064 && segments[1] == 0xff9b)) // NAT64
        }
    }
}

fn ipv4_is_global(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(o[0] == 0
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || ip.is_unspecified()
        || (o[0] == 100 && (64..=127).contains(&o[1])) // shared address space
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
        || o[0] >= 240 // reserved, incl. 255.255.255.255
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)) // IETF protocol assignments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_targets() {
        assert_eq!(
            normalize_public_http_url("https://example.com/a?b=1").unwrap(),
            "https://example.com/a?b=1"
        );
        assert_eq!(
            normalize_public_http_url("example.com").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_public_http_url("http://8.8.8.8/x").unwrap(),
            "http://8.8.8.8/x"
        );
    }

    #[test]
    fn rejects_private_and_disguised_targets() {
        for target in [
            "http://127.0.0.1/",
            "http://127.1/",
            "http://2130706433/",
            "http://0177.0.0.1/",
            "http://localhost/",
            "http://metadata.google.internal/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "file:///etc/passwd",
            "https://user:pass@example.com/",
            "javascript:alert(1)",
            "http://[::1]/",
        ] {
            assert!(
                normalize_public_http_url(target).is_err(),
                "{target} must be rejected"
            );
        }
    }

    #[test]
    fn host_matching_is_exact() {
        assert!(host_matches("https://www.v2ex.com/t/1", &["v2ex.com"]));
        assert!(host_matches("https://x.com/a", &["x.com", "twitter.com"]));
        assert!(!host_matches("https://x.com.evil.test/a", &["x.com"]));
        assert!(!host_matches("https://x.com@evil.test/a", &["x.com"]));
        assert!(!host_matches("ftp://x.com/a", &["x.com"]));
        assert!(!host_matches("not a url", &["x.com"]));
    }
}
