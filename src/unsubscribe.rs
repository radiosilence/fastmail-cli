//! Leaving mailing lists: RFC 8058 one-click POSTs and RFC 2369 `mailto:`
//! links.
//!
//! The URL comes from a header anyone can write, and in a hosted deployment
//! the request leaves from the server, so a one-click POST is held to public
//! HTTPS addresses only: the host is resolved first, every address must be
//! globally routable, the connection is pinned to those addresses, and
//! redirects are not followed.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use percent_encoding::percent_decode_str;
use reqwest::Url;

use crate::jmap::lists::{ListEmail, UnsubscribeOptions};
use crate::jmap::{ComposeParams, JmapClient};
use crate::models::EmailAddress;

const POST_TIMEOUT: Duration = Duration::from_secs(20);

/// What a one-click POST got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OneClickOutcome {
    /// 2xx: the sender says it is done.
    Done(u16),
    /// 3xx: the sender wants a browser; `Location` if it gave one.
    Redirected(u16, Option<String>),
    /// 4xx/5xx.
    Refused(u16),
}

/// Why a link was not POSTed to.
#[derive(Debug, thiserror::Error)]
pub enum UnsubscribeError {
    #[error("not an HTTPS URL: {0}")]
    NotHttps(String),
    #[error("{0} resolves to a non-public address ({1}), refusing to POST to it")]
    PrivateAddress(String, IpAddr),
    #[error("could not resolve {0}: {1}")]
    Resolve(String, String),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
}

/// Whether an address is one the open internet can reach. Anything else
/// (loopback, private ranges, link-local, CGNAT, metadata endpoints, …) is
/// what a planted URL would aim at.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link-local
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || (seg[0] == 0x0064 && seg[1] == 0xff9b) // NAT64
                || seg[..6] == [0, 0, 0, 0, 0, 0]) // IPv4-compatible
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
        || o[0] >= 240)
}

/// Resolve `url`'s host and check every address is public.
pub async fn vet(url: &str) -> Result<(Url, Vec<SocketAddr>), UnsubscribeError> {
    let parsed = Url::parse(url).map_err(|_| UnsubscribeError::NotHttps(url.to_string()))?;
    if parsed.scheme() != "https" {
        return Err(UnsubscribeError::NotHttps(url.to_string()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| UnsubscribeError::NotHttps(url.to_string()))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<SocketAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|e| UnsubscribeError::Resolve(host.clone(), e.to_string()))?
            .collect(),
    };
    if addrs.is_empty() {
        return Err(UnsubscribeError::Resolve(host, "no addresses".into()));
    }
    if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
        return Err(UnsubscribeError::PrivateAddress(host, bad.ip()));
    }
    Ok((parsed, addrs))
}

/// RFC 8058 §3.2: POST `List-Unsubscribe=One-Click` as a form, without
/// cookies or credentials, to the link the header gave.
pub async fn one_click(url: &str) -> Result<OneClickOutcome, UnsubscribeError> {
    let (parsed, addrs) = vet(url).await?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(POST_TIMEOUT)
        .user_agent(concat!("fastmail-cli/", env!("CARGO_PKG_VERSION")));
    if let Some(host) = parsed.host_str()
        && host.parse::<IpAddr>().is_err()
    {
        // Pin the connection to the addresses just vetted, so a second
        // lookup cannot answer differently.
        builder = builder.resolve_to_addrs(host, &addrs);
    }
    let resp = builder
        .build()?
        .post(parsed)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("List-Unsubscribe=One-Click")
        .send()
        .await?;
    let status = resp.status();
    Ok(if status.is_success() {
        OneClickOutcome::Done(status.as_u16())
    } else if status.is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        OneClickOutcome::Redirected(status.as_u16(), location)
    } else {
        OneClickOutcome::Refused(status.as_u16())
    })
}

/// A `mailto:` unsubscribe link, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailto {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// `mailto:leave@x.test?subject=unsubscribe` → its address, subject and
/// body, with "unsubscribe" filling whichever the link leaves out.
pub fn parse_mailto(link: &str) -> Option<Mailto> {
    let rest = link
        .get(..7)
        .filter(|p| p.eq_ignore_ascii_case("mailto:"))
        .map(|_| &link[7..])?;
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    let decode = |s: &str| {
        percent_decode_str(&s.replace('+', " "))
            .decode_utf8_lossy()
            .trim()
            .to_string()
    };
    let to = percent_decode_str(addr)
        .decode_utf8_lossy()
        .trim()
        .to_string();
    if !to.contains('@') || to.contains(',') || to.contains(char::is_whitespace) {
        return None;
    }
    let mut subject = String::new();
    let mut body = String::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k.to_ascii_lowercase().as_str() {
            "subject" => subject = decode(v),
            "body" => body = decode(v),
            _ => {}
        }
    }
    if subject.is_empty() {
        subject = "unsubscribe".into();
    }
    if body.is_empty() {
        body = "unsubscribe".into();
    }
    Some(Mailto { to, subject, body })
}

/// How to leave a list, picked from what its headers offer: one-click
/// first (no mail sent, no page to load), then `mailto:`, else a link only a
/// browser can finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    OneClick(String),
    Mailto(Mailto),
    Manual(Vec<String>),
}

impl Plan {
    pub fn for_options(options: &UnsubscribeOptions) -> Self {
        if let Some(url) = options.one_click_url() {
            return Plan::OneClick(url.to_string());
        }
        if let Some(m) = options.mailto.iter().find_map(|l| parse_mailto(l)) {
            return Plan::Mailto(m);
        }
        Plan::Manual(options.http.clone())
    }

    /// `ONE_CLICK`, `MAILTO` or `MANUAL`.
    pub fn method(&self) -> &'static str {
        match self {
            Plan::OneClick(_) => "ONE_CLICK",
            Plan::Mailto(_) => "MAILTO",
            Plan::Manual(_) => "MANUAL",
        }
    }

    /// What CONFIRM would act on, for the confirmation fingerprint.
    pub fn target(&self) -> String {
        match self {
            Plan::OneClick(url) => url.clone(),
            Plan::Mailto(m) => format!("{}\n{}\n{}", m.to, m.subject, m.body),
            Plan::Manual(urls) => urls.join("\n"),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Plan::OneClick(url) => {
                format!("POST List-Unsubscribe=One-Click (RFC 8058) to {url}")
            }
            Plan::Mailto(m) => format!(
                "Send an email to {} with subject \"{}\" and body \"{}\"",
                m.to, m.subject, m.body
            ),
            Plan::Manual(urls) if urls.is_empty() => {
                "Nothing: this email has no unsubscribe link".to_string()
            }
            Plan::Manual(urls) => format!(
                "Nothing automatic: the sender only offers a web page, which needs a browser:\n{}",
                urls.join("\n")
            ),
        }
    }
}

/// What carrying out a [`Plan`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    pub message: String,
    /// Links left for a browser, when it could not be finished here.
    pub manual_urls: Vec<String>,
}

/// Carry out `plan` for `email`. A `mailto:` is sent from the identity the
/// list wrote to when there is one, since many lists only act on a request
/// from the subscribed address (a masked one, say).
pub async fn carry_out(client: &JmapClient, email: &ListEmail, plan: &Plan) -> Outcome {
    match plan {
        Plan::OneClick(url) => match one_click(url).await {
            Ok(OneClickOutcome::Done(code)) => Outcome {
                success: true,
                message: format!("Unsubscribed by one-click POST (HTTP {code})"),
                manual_urls: vec![],
            },
            Ok(OneClickOutcome::Redirected(code, location)) => Outcome {
                success: false,
                message: format!(
                    "The one-click POST was redirected (HTTP {code}); finish it in a browser"
                ),
                manual_urls: location.into_iter().chain([url.clone()]).collect(),
            },
            Ok(OneClickOutcome::Refused(code)) => Outcome {
                success: false,
                message: format!("The one-click POST was refused (HTTP {code})"),
                manual_urls: email.unsubscribe().http,
            },
            Err(e) => Outcome {
                success: false,
                message: e.to_string(),
                manual_urls: email.unsubscribe().http,
            },
        },
        Plan::Mailto(m) => {
            let from = subscribed_identity(client, email).await;
            let to = vec![EmailAddress {
                name: None,
                email: m.to.clone(),
            }];
            let params = ComposeParams {
                cc: vec![],
                bcc: vec![],
                from: from.as_deref(),
                draft: false,
                html_body: None,
                attachments: vec![],
            };
            match client
                .send_email(to, &m.subject, &m.body, None, params)
                .await
            {
                Ok(_) => Outcome {
                    success: true,
                    message: format!(
                        "Sent an unsubscribe request to {}{}",
                        m.to,
                        from.map(|f| format!(" from {f}")).unwrap_or_default()
                    ),
                    manual_urls: vec![],
                },
                Err(e) => Outcome {
                    success: false,
                    message: format!("Sending the unsubscribe request failed: {e}"),
                    manual_urls: email.unsubscribe().http,
                },
            }
        }
        Plan::Manual(urls) => Outcome {
            success: false,
            message: plan.describe(),
            manual_urls: urls.clone(),
        },
    }
}

/// The identity among the addresses the list wrote to, if any.
async fn subscribed_identity(client: &JmapClient, email: &ListEmail) -> Option<String> {
    let identities = client.list_identities().await.ok()?;
    email.to.iter().flatten().find_map(|addr| {
        identities
            .iter()
            .find(|i| i.email.eq_ignore_ascii_case(&addr.email))
            .map(|i| i.email.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_special_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "151.101.1.1", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn vet_refuses_plain_http_and_private_literals() {
        assert!(matches!(
            vet("http://example.com/u").await,
            Err(UnsubscribeError::NotHttps(_))
        ));
        assert!(matches!(
            vet("https://169.254.169.254/latest").await,
            Err(UnsubscribeError::PrivateAddress(..))
        ));
        assert!(matches!(
            vet("https://[::1]/u").await,
            Err(UnsubscribeError::PrivateAddress(..))
        ));
        assert!(matches!(
            vet("https://localhost/u").await,
            Err(UnsubscribeError::PrivateAddress(..) | UnsubscribeError::Resolve(..))
        ));
    }

    #[test]
    fn mailto_links_come_apart() {
        assert_eq!(
            parse_mailto("mailto:leave%2Babc@list.test?subject=Remove%20me&body=bye+now"),
            Some(Mailto {
                to: "leave+abc@list.test".into(),
                subject: "Remove me".into(),
                body: "bye now".into(),
            })
        );
        assert_eq!(
            parse_mailto("MAILTO:u@x.test").unwrap().subject,
            "unsubscribe"
        );
        assert_eq!(parse_mailto("mailto:a@x.test,b@y.test"), None);
        assert_eq!(parse_mailto("https://x.test"), None);
        assert_eq!(parse_mailto("mailto:nobody"), None);
    }

    #[test]
    fn plans_prefer_one_click_then_mailto() {
        let opts = |urls: &[&str], post: Option<&str>| {
            let urls: Vec<String> = urls.iter().map(|s| s.to_string()).collect();
            UnsubscribeOptions::from_headers("e", &urls, post)
        };
        let both = ["mailto:u@x.test", "https://x.test/u"];
        assert_eq!(
            Plan::for_options(&opts(&both, Some("List-Unsubscribe=One-Click"))),
            Plan::OneClick("https://x.test/u".into())
        );
        assert_eq!(Plan::for_options(&opts(&both, None)).method(), "MAILTO");
        assert_eq!(
            Plan::for_options(&opts(&["https://x.test/u"], None)),
            Plan::Manual(vec!["https://x.test/u".into()])
        );
    }
}
