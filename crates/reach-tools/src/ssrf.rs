//! Security helpers for untrusted URLs (port of `utils/url.py` + transcribe guards).
//!
//! Literal-IP tricks (`127.1`, `2130706433`, `0x7f000001`, octal forms,
//! full-width homoglyphs), userinfo disguises, blocked internal hostnames,
//! DNS-resolution checks and redirect re-validation for in-process fetches.

use reach_core::{ReachError, Result};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Hosts that never denote a public target (transcribe's blocked set).
const BLOCKED_HOSTS: &[&str] = &["localhost", "metadata.google.internal"];
const BLOCKED_SUFFIXES: &[&str] = &[".localhost"];

/// The wider `utils/url.py` blocked set for in-process public fetches.
const BLOCKED_FETCH_HOSTS: &[&str] = &[
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
const BLOCKED_FETCH_SUFFIXES: &[&str] = &[
    ".home.arpa",
    ".internal",
    ".lan",
    ".local",
    ".localdomain",
    ".localhost",
];

fn ssrf(message: &str) -> ReachError {
    ReachError::new("BAD_INPUT", message)
}

/// Parse a literal IPv4 host using the full `inet_aton` grammar, no DNS.
///
/// The C resolver behind child tools accepts more than the canonical
/// dotted-quad form: `127.1`, `2130706433`, `0x7f000001` and `0177.0.0.1`
/// all reach 127.0.0.1, and `0xA9FEA9FE` reaches the cloud metadata
/// endpoint. Parsing with the same grammar keeps those shorthands from
/// slipping past the private-address check. This is literal parsing only —
/// no name is resolved here.
pub fn literal_ip(host: &str) -> Option<IpAddr> {
    // Canonical IPv4 / IPv6 first.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(ip);
    }
    inet_aton(host).map(IpAddr::V4)
}

/// Legacy `inet_aton` grammar: 1–4 dot-separated parts, each decimal,
/// 0x-prefixed hex or leading-0 octal; the last part fills the remaining
/// width (24, 16 or 8 bits).
fn inet_aton(host: &str) -> Option<std::net::Ipv4Addr> {
    if host.is_empty() {
        return None;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut values: Vec<u64> = Vec::with_capacity(parts.len());
    for part in &parts {
        let (radix, digits) =
            if let Some(rest) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                (16, rest)
            } else if part.len() > 1 && part.starts_with('0') {
                (8, &part[1..])
            } else {
                (10, *part)
            };
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return None;
        }
        values.push(u64::from_str_radix(digits, radix).ok()?);
    }
    // The final part fills the remaining byte width; everything before it
    // must fit a single byte.
    let last_width = 8 * (5 - values.len()) as u64;
    let last = *values.last()?;
    if last >= 1u64 << last_width {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, v) in values[..values.len() - 1].iter().enumerate() {
        if *v > 0xFF {
            return None;
        }
        octets[i] = *v as u8;
    }
    let mut rest = last;
    for i in (values.len() - 1..4).rev() {
        octets[i] = (rest & 0xFF) as u8;
        rest >>= 8;
    }
    if rest != 0 {
        return None;
    }
    Some(std::net::Ipv4Addr::from(octets))
}

/// True when the (literal) host denotes a private/loopback/reserved address.
///
/// Mirrors the upstream check `is_private or is_loopback or is_link_local or
/// is_reserved or is_multicast or is_unspecified`, with the IANA
/// special-purpose ranges spelled out explicitly (Rust's `is_private` is
/// narrower than Python's).
pub fn is_private_ip(host: &str) -> bool {
    let Some(ip) = literal_ip(host) else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => {
            let o = u32::from_be_bytes(v4.octets());
            let in_range = |net: u32, bits: u8| {
                let mask = if bits == 0 {
                    0
                } else {
                    u32::MAX << (32 - bits)
                };
                (o & mask) == (net & mask)
            };
            v4.is_loopback() // 127/8
                || v4.is_link_local() // 169.254/16
                || v4.is_multicast() // 224/4
                || v4.is_unspecified() // 0.0.0.0
                || in_range(0xFF00_0000, 8) // 255/8 (broadcast)
                || in_range(0x0A00_0000, 8) // 10/8 private
                || in_range(0x6440_0000, 10) // 100.64/10 shared
                || in_range(0xAC10_0000, 12) // 172.16/12 private
                || in_range(0xC000_0000, 24) // 192.0.0/24
                || in_range(0xC000_0200, 24) // 192.0.2/24 TEST-NET-1
                || in_range(0xC058_6300, 24) // 192.88.99/24 6to4 relay
                || in_range(0xC0A8_0000, 16) // 192.168/16 private
                || in_range(0xC612_0000, 15) // 198.18/15 benchmarking
                || in_range(0xC633_6400, 24) // 198.51.100/24 TEST-NET-2
                || in_range(0xCB00_7100, 24) // 203.0.113/24 TEST-NET-3
                || in_range(0xF000_0000, 4) // 240/4 reserved
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (s[0] & 0xFE00) == 0xFC00 // unique local fc00::/7
                || (s[0] & 0xFFC0) == 0xFE80 // link local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0DB8) // documentation
        }
    }
}

/// Port of upstream `normalize_public_http_url` (utils/url.py): normalize a
/// URL or reject targets that are not clearly public HTTP(S).
pub fn normalize_public_http_url(url: &str) -> Result<String> {
    let candidate = url.trim();
    if candidate.is_empty()
        || candidate.contains('\\')
        || candidate
            .chars()
            .any(|c| c.is_whitespace() || (c as u32) < 0x20 || c as u32 == 0x7F)
    {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    let with_scheme = if candidate.contains("://") {
        candidate.to_string()
    } else {
        format!("https://{candidate}")
    };
    // Raw authority checks before any lenient parsing.
    let authority = raw_authority(&with_scheme);
    if authority.is_empty() || authority.contains('\\') || authority.contains('%') {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    let parsed = url::Url::parse(&with_scheme)
        .map_err(|_| ssrf("SSRF blocked: only public http(s) URLs are allowed"))?;
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_string();
    if host.is_empty() {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    let blocked = BLOCKED_FETCH_HOSTS.contains(&host.as_str())
        || BLOCKED_FETCH_SUFFIXES.iter().any(|s| host.ends_with(s))
        || is_private_ip(&host);
    let literal = literal_ip(&host);
    if blocked || (!host.contains('.') && literal.is_none()) {
        return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
    }
    // Reassemble a canonical URL with the normalized host.
    let hostport = match parsed.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    let mut out = format!("{}://{hostport}", parsed.scheme());
    out.push_str(parsed.path());
    if let Some(q) = parsed.query() {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = parsed.fragment() {
        out.push('#');
        out.push_str(f);
    }
    Ok(out)
}

/// Port of upstream `assert_safe_public_url` (transcribe.py): reject literal
/// local/internal URLs before handing a URL to a child process. Returns the
/// original URL string verbatim (child tools support bare public hosts).
pub fn assert_safe_public_url(url: &str) -> Result<String> {
    let normalized = if url.contains("://") {
        let scheme = url.split("://").next().unwrap_or("").to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
        }
        url.to_string()
    } else {
        // Bare form: a host[:port] before the first slash must be well-formed.
        let before_slash = url.split('/').next().unwrap_or("");
        if let Some((host_part, port_part)) = before_slash.rsplit_once(':') {
            if host_part.is_empty()
                || port_part.is_empty()
                || !port_part.chars().all(|c| c.is_ascii_digit())
            {
                return Err(ssrf("SSRF blocked: only public http(s) URLs are allowed"));
            }
        }
        format!("https://{url}")
    };
    let authority = raw_authority(&normalized);
    if authority.contains('\\') || authority.contains('%') || authority.is_empty() {
        return Err(ssrf("SSRF blocked: encoded or ambiguous URL host"));
    }
    let parsed = url::Url::parse(&normalized)
        .map_err(|_| ssrf("SSRF blocked: URL host is missing or invalid"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ssrf("SSRF blocked: URL host is invalid"));
    }
    let host = parsed
        .host_str()
        .unwrap_or("")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(ssrf("SSRF blocked: URL host is missing"));
    }
    if BLOCKED_HOSTS.contains(&host.as_str()) || BLOCKED_SUFFIXES.iter().any(|s| host.ends_with(s))
    {
        return Err(ssrf("SSRF blocked: internal host is not allowed"));
    }
    if is_private_ip(&host) {
        return Err(ssrf("SSRF blocked: private/internal IP is not allowed"));
    }
    Ok(url.to_string())
}

/// The authority portion of a URL string (between `://` and the first
/// `/`, `?` or `#`), mirroring upstream's manual extraction.
fn raw_authority(url: &str) -> &str {
    let rest = url.split_once("://").map(|x| x.1).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or("")
}

/// Match a hostname exactly or as a real subdomain of one of `domains`.
pub fn domain_matches(host: &str, domains: &[&str]) -> bool {
    let normalized = host.trim().to_ascii_lowercase();
    let normalized = normalized.trim_start_matches('.').trim_end_matches('.');
    if normalized.is_empty() {
        return false;
    }
    domains.iter().any(|domain| {
        let allowed = domain.trim().to_ascii_lowercase();
        let allowed = allowed.trim_start_matches('.').trim_end_matches('.');
        normalized == allowed || normalized.ends_with(&format!(".{allowed}"))
    })
}

/// Whether `url` is an http(s) URL with userinfo-free host matching `domains`.
///
/// Hostname-based (never substring) matching prevents lookalikes such as
/// `x.com.evil.test` and userinfo disguises such as `x.com@evil.test`;
/// malformed or out-of-range ports fail closed.
pub fn host_matches(url: &str, domains: &[&str]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let scheme = parsed.scheme().to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return false;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    domain_matches(host, domains)
}

/// Resolve `host` and require every address to be a public, unicast one.
/// Returns the verified socket addresses (used to pin connections, closing
/// the DNS-rebinding window between check and connect).
pub async fn check_host_public(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(host) {
            return Err(ssrf(
                "SSRF blocked: host resolves to a private/internal address",
            ));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| ReachError::new("UPSTREAM_BROKEN", format!("could not resolve {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(ReachError::new(
            "UPSTREAM_BROKEN",
            format!("could not resolve {host}: no addresses"),
        ));
    }
    for addr in &addrs {
        if is_private_ip(&addr.ip().to_string()) {
            return Err(ssrf(
                "SSRF blocked: host resolves to a private/internal address",
            ));
        }
    }
    Ok(addrs)
}

/// SSRF-guarded in-process GET: validates every URL (including each redirect
/// hop) as a public http(s) target, DNS-checks the host, pins the resolved
/// addresses, caps the body and honors the timeout.
pub async fn safe_fetch(url: &str, max_bytes: u64, timeout: Duration) -> Result<Vec<u8>> {
    let validator = |candidate: &str| -> Result<()> {
        assert_safe_public_url(candidate).map(|_| ())?;
        let authority = raw_authority(candidate);
        let host = authority_host(authority);
        if BLOCKED_FETCH_HOSTS.contains(&host.as_str())
            || BLOCKED_FETCH_SUFFIXES.iter().any(|s| host.ends_with(s))
        {
            return Err(ssrf("SSRF blocked: internal host is not allowed"));
        }
        Ok(())
    };
    fetch_following(url, max_bytes, timeout, &validator, true).await
}

/// Redirect-following GET with per-hop re-validation. `hop_validator` approves
/// every URL before the request (including each `Location` target); when
/// `dns_pin` is set the host is DNS-resolved, checked public and the
/// connection pinned to the verified addresses. Production code uses
/// [`safe_fetch`]; tests may inject a custom validator. The validator must be
/// `Sync` so the returned future stays `Send` for the dispatcher.
pub async fn fetch_following(
    url: &str,
    max_bytes: u64,
    timeout: Duration,
    hop_validator: &(dyn Fn(&str) -> Result<()> + Sync),
    dns_pin: bool,
) -> Result<Vec<u8>> {
    let fut = async {
        let mut current = url.to_string();
        for _hop in 0..5 {
            hop_validator(&current)?;
            let parsed = url::Url::parse(&current)
                .map_err(|_| ssrf("SSRF blocked: only public http(s) URLs are allowed"))?;
            let host = parsed
                .host_str()
                .ok_or_else(|| ssrf("SSRF blocked: URL host is missing"))?
                .to_string();
            let port = parsed.port_or_known_default().unwrap_or(443);
            let mut builder =
                reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
            if dns_pin {
                let addrs = check_host_public(&host, port).await?;
                if !addrs.is_empty() {
                    builder = builder.resolve_to_addrs(&host, &addrs);
                }
            }
            let client = builder.build().map_err(|e| {
                ReachError::new("UPSTREAM_BROKEN", format!("client build failed: {e}"))
            })?;
            let resp = client.get(&current).send().await?;
            if resp.status().is_redirection() {
                let location = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| {
                        ReachError::new("UPSTREAM_BROKEN", "redirect without a Location header")
                    })?
                    .to_string();
                current = parsed
                    .join(&location)
                    .map_err(|_| ssrf("SSRF blocked: invalid redirect target"))?
                    .to_string();
                continue;
            }
            let resp = resp.error_for_status()?;
            let mut body: Vec<u8> = Vec::new();
            let mut stream = resp.bytes_stream();
            use futures_util::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                if body.len() as u64 + chunk.len() as u64 > max_bytes {
                    return Err(ReachError::new(
                        "UPSTREAM_BROKEN",
                        format!("response exceeds {max_bytes} bytes"),
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(body);
        }
        Err(ReachError::new("UPSTREAM_BROKEN", "too many redirects"))
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(inner) => inner,
        Err(_) => Err(ReachError::new("TIMEOUT", "fetch did not finish in time")),
    }
}

/// Extract the lowercase host from a raw authority string.
fn authority_host(authority: &str) -> String {
    let without_userinfo = authority.rsplit('@').next().unwrap_or(authority);
    if let Some(stripped) = without_userinfo.strip_prefix('[') {
        // [ipv6]:port
        return stripped
            .split(']')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
    }
    let host_port = without_userinfo
        .rsplit_once(':')
        .map(|x| x.0)
        .unwrap_or(without_userinfo);
    host_port.trim_end_matches('.').to_ascii_lowercase()
}
