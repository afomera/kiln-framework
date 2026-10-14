//! Outbound HTTP for talking to URLs other people chose (webhooks, link previews), with the
//! guard rails that needs.
//!
//! ```ignore
//! let client = kiln::http::Client::new();
//! let response = client.post("https://example.com/hook").header("content-type", "application/json")
//!     .body(json).send().await?;
//! if response.status == 200 { .. }
//! ```
//!
//! - **No private networks.** Every address a host resolves to is checked, and the connection
//!   goes to the checked address, so a DNS answer can't swap in `127.0.0.1` (or the cloud
//!   metadata service at `169.254.169.254`) between the check and the connect. Loopback,
//!   private, link-local, carrier-grade NAT, multicast and reserved ranges are refused, as
//!   are IPv6 forms that embed them. `HTTP_ALLOW_PRIVATE_NETWORKS=1` lifts this (it's lifted
//!   by default in development, so a receiver on `localhost` works).
//! - **Bounded.** 5 s to connect, 15 s for the whole exchange, at most 64 KB of response
//!   body read (`Response::truncated` says when there was more).
//! - **No redirects, no proxies:** a redirect could point anywhere, and a proxy would do the
//!   resolving instead of us. Redirects come back as their 3xx response.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use crate::Error;

/// The most response body read by default.
pub const MAX_BODY: usize = 64 * 1024;

/// An outbound HTTP client. Cheap to clone; connections are pooled.
#[derive(Clone)]
pub struct Client {
    inner: reqwest::Client,
    allow_private: bool,
    max_body: usize,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("allow_private", &self.allow_private).finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether private addresses are allowed when nothing says: `HTTP_ALLOW_PRIVATE_NETWORKS`,
/// else only in development.
pub(crate) fn private_allowed_by_default() -> bool {
    match crate::config::value("HTTP_ALLOW_PRIVATE_NETWORKS").as_deref().map(str::trim) {
        Some("1" | "true" | "yes") => true,
        Some(_) => false,
        None => crate::config::env() == "development",
    }
}

impl Client {
    /// The default client: private networks refused (outside development), 5 s to connect,
    /// 15 s in all.
    pub fn new() -> Self {
        Self::builder().build()
    }

    pub fn builder() -> Builder {
        Builder {
            allow_private: private_allowed_by_default(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            max_body: MAX_BODY,
            user_agent: format!("Kiln/{}", env!("CARGO_PKG_VERSION")),
        }
    }

    pub fn get(&self, url: &str) -> Request {
        self.request(reqwest::Method::GET, url)
    }

    pub fn post(&self, url: &str) -> Request {
        self.request(reqwest::Method::POST, url)
    }

    pub fn request(&self, method: reqwest::Method, url: &str) -> Request {
        Request { client: self.clone(), method, url: url.to_owned(), headers: vec![], body: vec![] }
    }
}

/// Options for a [`Client`].
pub struct Builder {
    allow_private: bool,
    connect_timeout: Duration,
    timeout: Duration,
    max_body: usize,
    user_agent: String,
}

impl Builder {
    /// Allows (or refuses) loopback, private and link-local destinations. Leave it off for
    /// URLs users supply.
    pub fn allow_private_networks(mut self, allow: bool) -> Self {
        self.allow_private = allow;
        self
    }

    /// The whole exchange, from connecting to the last byte read.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// The most response body read; the rest is dropped.
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn build(self) -> Client {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent(self.user_agent);
        if !self.allow_private {
            builder = builder.dns_resolver(Arc::new(PublicOnly));
        }
        // Building fails only when the TLS backend can't initialize, which is a broken build.
        let inner = builder.build().expect("the HTTP client builds");
        Client { inner, allow_private: self.allow_private, max_body: self.max_body }
    }
}

/// A request being built: `client.post(url).header(..).body(..).send().await`.
pub struct Request {
    client: Client,
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Sends it. Any status is a response (check `status`); a refused destination, a
    /// timeout or a connection failure is an error ([`SendError`]).
    pub async fn send(self) -> std::result::Result<Response, SendError> {
        let url = reqwest::Url::parse(&self.url).map_err(|e| SendError::InvalidUrl(e.to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(SendError::InvalidUrl(format!("`{}` URLs aren't allowed, only http and https", url.scheme())));
        }
        // IP literals skip the resolver, so they're checked here.
        // (The URL parser has already normalized forms like `http://2130706433/`.)
        let Some(host) = url.host_str() else { return Err(SendError::InvalidUrl("the URL has no host".into())) };
        let literal = host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>().ok();
        if let Some(ip) = literal
            && !self.client.allow_private
            && !is_public(ip)
        {
            return Err(SendError::Blocked(format!("{ip} is a private or reserved address")));
        }
        let mut request = self.client.inner.request(self.method, url);
        for (name, value) in self.headers {
            request = request.header(name, value);
        }
        let mut response = request.body(self.body).send().await.map_err(SendError::from_reqwest)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let mut body = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await.map_err(SendError::from_reqwest)? {
            let room = self.client.max_body - body.len();
            if chunk.len() > room {
                body.extend_from_slice(&chunk[..room]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response { status, headers, body, truncated })
    }
}

/// A response, its body read up to the client's limit.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Names are lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Whether the body was longer than the limit (and cut there).
    pub truncated: bool,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Why a request got no response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// The destination is a private or reserved address (or resolves to one).
    #[error("refused: {0}")]
    Blocked(String),
    #[error("timed out")]
    Timeout,
    #[error("{0}")]
    Failed(String),
}

impl SendError {
    fn from_reqwest(e: reqwest::Error) -> Self {
        // The resolver's refusal arrives wrapped in connect errors.
        let mut source: Option<&dyn std::error::Error> = Some(&e);
        while let Some(err) = source {
            if let Some(Refused(why)) = err.downcast_ref::<Refused>() {
                return SendError::Blocked(why.clone());
            }
            source = err.source();
        }
        if e.is_timeout() {
            return SendError::Timeout;
        }
        // Without the URL: it can carry credentials, and callers know it already.
        SendError::Failed(e.without_url().to_string())
    }

    /// Whether trying again could help (a refused or malformed URL never will).
    pub fn is_retryable(&self) -> bool {
        matches!(self, SendError::Timeout | SendError::Failed(_))
    }
}

impl From<SendError> for Error {
    fn from(e: SendError) -> Self {
        match e {
            SendError::InvalidUrl(_) | SendError::Blocked(_) => Error::BadRequest(e.to_string()),
            _ => Error::Job(format!("HTTP request failed: {e}")),
        }
    }
}

/// The resolver's refusal, found again in `SendError::from_reqwest`.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Resolves names, refusing any whose addresses aren't all public. The client connects to
/// exactly the addresses returned here, which is what pins the check to the connection.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            check_resolved(&host, &addrs).map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Refuses a name when any of its addresses isn't public: a name answering with both is
/// someone probing.
fn check_resolved(host: &str, addrs: &[SocketAddr]) -> std::result::Result<(), Refused> {
    if addrs.is_empty() {
        return Err(Refused(format!("{host} has no addresses")));
    }
    if let Some(bad) = addrs.iter().find(|a| !is_public(a.ip())) {
        return Err(Refused(format!("{host} resolves to {}, a private or reserved address", bad.ip())));
    }
    Ok(())
}

/// Whether `ip` is a globally routable address that's safe to send user-chosen requests to.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0                                   // "this network"
        || (a == 100 && (64..128).contains(&b))     // carrier-grade NAT
        || (a == 192 && b == 0 && c == 0)           // IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19))       // benchmarking
        || a >= 240) // reserved
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Forms that carry an IPv4 address are judged by it: mapped (::ffff:a.b.c.d),
    // NAT64 (64:ff9b::/96) and 6to4 (2002::/16).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_v4(Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
    }
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || s[0..6] == [0, 0, 0, 0, 0, 0]        // IPv4-compatible (deprecated)
        || (s[0] & 0xfe00) == 0xfc00            // unique local
        || (s[0] & 0xffc0) == 0xfe80            // link-local
        || (s[0] & 0xffc0) == 0xfec0            // site-local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0db8)   // documentation
        || (s[0] == 0x2001 && s[1] == 0)        // Teredo
        || s[0] == 0x0100) // discard-only
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn private_and_reserved_addresses_are_not_public() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "240.0.0.1",
            "198.18.0.1",
            "192.0.0.8",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2001:db8::1",
            "::127.0.0.1",
        ] {
            assert!(!is_public(ip(bad)), "{bad} should be refused");
        }
        for good in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111", "::ffff:93.184.216.34", "2002:5db8:d822::"] {
            assert!(is_public(ip(good)), "{good} should be allowed");
        }
    }

    #[test]
    fn a_name_with_any_private_address_is_refused() {
        let public: SocketAddr = "93.184.216.34:0".parse().unwrap();
        let private: SocketAddr = "10.0.0.1:0".parse().unwrap();
        assert!(check_resolved("ok.example", &[public]).is_ok());
        assert!(check_resolved("mixed.example", &[public, private]).is_err());
        assert!(check_resolved("none.example", &[]).is_err());
    }

    #[tokio::test]
    async fn private_destinations_are_refused_before_connecting() {
        let client = Client::builder().allow_private_networks(false).build();
        for url in ["http://127.0.0.1:9/hook", "http://[::1]:9/", "http://169.254.169.254/latest/meta-data"] {
            let err = client.post(url).send().await.unwrap_err();
            assert!(matches!(err, SendError::Blocked(_)), "{url}: {err:?}");
            assert!(!err.is_retryable());
        }
        // `localhost` goes through the resolver, which refuses it.
        let err = client.get("http://localhost:9/").send().await.unwrap_err();
        assert!(matches!(err, SendError::Blocked(_)), "{err:?}");
        let err = client.get("file:///etc/passwd").send().await.unwrap_err();
        assert!(matches!(err, SendError::InvalidUrl(_)), "{err:?}");
    }

    #[tokio::test]
    async fn responses_are_read_up_to_the_limit_and_slow_servers_time_out() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // First connection: a large body. Second: never answers.
            let (mut first, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = first.read(&mut buf).await;
            let body = "x".repeat(10_000);
            let _ = first
                .write_all(
                    format!("HTTP/1.1 418 I'm a teapot\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes(),
                )
                .await;
            let (_second, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let client =
            Client::builder().allow_private_networks(true).max_body(100).timeout(Duration::from_millis(300)).build();
        let response = client.post(&format!("http://{addr}/")).body("hi").send().await.unwrap();
        assert_eq!(response.status, 418);
        assert_eq!(response.body.len(), 100);
        assert!(response.truncated);
        let err = client.post(&format!("http://{addr}/")).send().await.unwrap_err();
        assert_eq!(err, SendError::Timeout);
        assert!(err.is_retryable());
    }
}
