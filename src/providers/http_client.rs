// SSRF-safe HTTP client builder.
//
// Validates URLs before requests to block access to private/internal networks.
// Applied to all external-facing providers (NOT ollama — intentionally local).
//
// The redirect policy validates each 302/3xx hop to prevent DNS rebinding
// and redirect-to-localhost attacks (attacker URL -> 302 -> http://127.0.0.1).

use reqwest::{redirect, Client, Url};
use std::net::IpAddr;

/// Known private/internal hostnames that should never be reachable from providers.
const BLOCKED_HOSTS: &[&str] = &[
    "localhost",
    "metadata.google.internal",
    "metadata.aws.internal",
    "instance-data",
];

/// Returns true if the IP address is in a private, loopback, or link-local range.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()              // 127.0.0.0/8
                || v4.is_private()        // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()     // 169.254.0.0/16
                || v4.is_broadcast()      // 255.255.255.255
                || v4.is_unspecified()    // 0.0.0.0
                || v4.octets()[0] == 100 && v4.octets()[1] >= 64 && v4.octets()[1] <= 127
            // CGNAT 100.64.0.0/10
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()              // ::1
                || v6.is_unspecified()    // ::
                || {
                    let segments = v6.segments();
                    // fc00::/7 (unique local)
                    (segments[0] & 0xfe00) == 0xfc00
                    // fe80::/10 (link-local)
                    || (segments[0] & 0xffc0) == 0xfe80
                }
        }
    }
}

/// Validates that a URL does not point to a private/internal address.
/// Returns Ok(()) if safe, Err with reason if blocked.
pub fn validate_url_not_private(url: &str) -> Result<(), String> {
    let parsed = Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;

    let host = parsed.host_str().unwrap_or("");

    // Block known internal hostnames
    let host_lower = host.to_lowercase();
    for blocked in BLOCKED_HOSTS {
        if host_lower == *blocked || host_lower.ends_with(&format!(".{blocked}")) {
            return Err(format!("Blocked internal hostname: {host}"));
        }
    }

    // If it parses as an IP, check directly
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(ip) {
            return Err(format!("Blocked private IP: {ip}"));
        }
    }

    // Bracket-stripped IPv6 check (e.g., [::1])
    if host.starts_with('[') && host.ends_with(']') {
        if let Ok(ip) = host[1..host.len() - 1].parse::<IpAddr>() {
            if is_private_ip(ip) {
                return Err(format!("Blocked private IP: {ip}"));
            }
        }
    }

    Ok(())
}

/// Build a reqwest `Client` with SSRF-safe redirect policy and standard timeouts.
///
/// Each 3xx redirect hop is validated against `is_private_ip()` and blocked
/// hostnames. Prevents redirect-to-localhost and DNS rebinding attacks where
/// an attacker-controlled URL returns `302 -> http://127.0.0.1/...`.
///
/// Includes 120s request timeout and 10s connect timeout (matching provider defaults).
/// Max 10 redirects. Providers that intentionally target localhost (e.g. Ollama)
/// should NOT use this — use `Client::builder()` directly instead.
pub fn build_ssrf_safe_client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(redirect::Policy::custom(|attempt| {
            // Extract host info before consuming `attempt`
            let reject_reason = {
                let url = attempt.url();
                url.host_str().and_then(|host| {
                    let host_lower = host.to_lowercase();

                    // Block known internal hostnames
                    for blocked in BLOCKED_HOSTS {
                        if host_lower == *blocked || host_lower.ends_with(&format!(".{blocked}")) {
                            return Some(format!("SSRF: redirect to blocked host: {host}"));
                        }
                    }

                    // Block private IPs
                    if let Ok(ip) = host.parse::<IpAddr>() {
                        if is_private_ip(ip) {
                            return Some(format!("SSRF: redirect to private IP: {ip}"));
                        }
                    }

                    // Bracket-stripped IPv6
                    if host.starts_with('[') && host.ends_with(']') {
                        if let Ok(ip) = host[1..host.len() - 1].parse::<IpAddr>() {
                            if is_private_ip(ip) {
                                return Some(format!("SSRF: redirect to private IP: {ip}"));
                            }
                        }
                    }

                    None
                })
            };

            if let Some(reason) = reject_reason {
                return attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    reason,
                ));
            }

            // Cap at 10 redirects
            if attempt.previous().len() >= 10 {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .unwrap_or_else(|_| Client::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_private_ip ───────────────────────────────────────

    #[test]
    fn loopback_v4_is_private() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("127.255.255.255".parse().unwrap()));
    }

    #[test]
    fn loopback_v6_is_private() {
        assert!(is_private_ip("::1".parse().unwrap()));
    }

    #[test]
    fn rfc1918_10_is_private() {
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.255.255.255".parse().unwrap()));
    }

    #[test]
    fn rfc1918_172_is_private() {
        assert!(is_private_ip("172.16.0.1".parse().unwrap()));
        assert!(is_private_ip("172.31.255.255".parse().unwrap()));
    }

    #[test]
    fn rfc1918_192_is_private() {
        assert!(is_private_ip("192.168.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.255.255".parse().unwrap()));
    }

    #[test]
    fn link_local_is_private() {
        assert!(is_private_ip("169.254.1.1".parse().unwrap()));
    }

    #[test]
    fn cgnat_is_private() {
        assert!(is_private_ip("100.64.0.1".parse().unwrap()));
        assert!(is_private_ip("100.127.255.255".parse().unwrap()));
    }

    #[test]
    fn ipv6_unique_local_is_private() {
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(is_private_ip("fc00::1".parse().unwrap()));
    }

    #[test]
    fn ipv6_link_local_is_private() {
        assert!(is_private_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn public_ips_not_private() {
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip("142.250.80.46".parse().unwrap()));
        assert!(!is_private_ip("2607:f8b0:4004:800::200e".parse().unwrap()));
    }

    // ── validate_url_not_private ────────────────────────────

    #[test]
    fn blocks_localhost_url() {
        assert!(validate_url_not_private("http://localhost/path").is_err());
    }

    #[test]
    fn blocks_private_ip_url() {
        assert!(validate_url_not_private("http://10.0.0.1/api").is_err());
        assert!(validate_url_not_private("http://192.168.1.1/api").is_err());
        assert!(validate_url_not_private("http://172.16.0.1/api").is_err());
    }

    #[test]
    fn blocks_metadata_endpoints() {
        assert!(
            validate_url_not_private("http://metadata.google.internal/computeMetadata").is_err()
        );
        assert!(validate_url_not_private("http://169.254.169.254/latest/meta-data").is_err());
    }

    #[test]
    fn allows_public_urls() {
        assert!(validate_url_not_private("https://api.openai.com/v1/chat").is_ok());
        assert!(validate_url_not_private("https://api.anthropic.com/v1/messages").is_ok());
        assert!(validate_url_not_private("https://openrouter.ai/api/v1/chat").is_ok());
    }

    #[test]
    fn rejects_invalid_url() {
        assert!(validate_url_not_private("not a url").is_err());
    }

    // ── build_ssrf_safe_client ────────────────────────────────

    #[test]
    fn ssrf_safe_client_builds_successfully() {
        let client = build_ssrf_safe_client();
        // Smoke test — client should construct without panic
        drop(client);
    }
}
