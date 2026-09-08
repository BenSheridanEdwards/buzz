//! Blossom (BUD-02/BUD-11) client helpers for the harness.
//!
//! The harness holds the agent key, so it is the only party that can sign the
//! kind-24242 authorization events the relay demands for media reads and
//! writes. This module owns that signing, the origin rule for which URLs the
//! harness will fetch, the bounded download and upload paths, and the NIP-11
//! probe for the `buzz-audio` extension. Both the inbound
//! ([`crate::attachments`]) and outbound ([`crate::media_publish`]) paths sit
//! on top of it.

use std::time::{Duration, Instant};

use base64::Engine as _;
use nostr::JsonUtil as _;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::relay::RestClient;

/// Longest a single attachment download may take, including connect time.
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest a single Blossom upload may take.
pub(crate) const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// Lifetime of a signed get/upload authorization.
const AUTH_LIFETIME_SECS: u64 = 600;
/// NIP-11 `supported_extensions` entry advertising native audio uploads.
pub(crate) const AUDIO_EXTENSION: &str = "buzz-audio";
/// How long a NIP-11 audio-support answer is reused before re-probing.
const AUDIO_SUPPORT_TTL: Duration = Duration::from_secs(600);
/// Bound on the error body echoed into logs and prompts.
const MAX_ERROR_BODY: usize = 200;

/// Errors from the Blossom client paths.
#[derive(Debug, thiserror::Error)]
pub enum BlossomError {
    /// A URL failed the origin or shape rule.
    #[error("attachment URL rejected: {0}")]
    UntrustedUrl(String),
    /// Transport or protocol failure talking to the relay.
    #[error("HTTP error: {0}")]
    Http(String),
    /// The relay answered with a non-success status.
    #[error("relay returned HTTP {status}: {body}")]
    Refused {
        /// HTTP status code.
        status: u16,
        /// Bounded response body.
        body: String,
    },
    /// The blob did not match the metadata that described it.
    #[error("integrity check failed: {0}")]
    Integrity(String),
    /// Signing the authorization event failed.
    #[error("signing failed: {0}")]
    Sign(String),
    /// A bounded operation ran out of time.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
}

impl BlossomError {
    /// True when the relay rejected the upload because of its media type or
    /// content (415 Unsupported Media Type, 422 Unprocessable Entity). These
    /// are the statuses `buzz-media` maps unsupported and non-canonical media
    /// to, and the only ones that justify an audio fallback.
    pub fn is_media_rejection(&self) -> bool {
        matches!(self, Self::Refused { status, .. } if is_media_rejection_status(*status))
    }
}

/// Whether an upload status means "this media is not accepted here".
pub(crate) fn is_media_rejection_status(status: u16) -> bool {
    matches!(status, 415 | 422)
}

/// Blossom BlobDescriptor as returned by `PUT /upload`.
///
/// Only the fields the harness echoes into `imeta` are modelled; unknown
/// fields are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BlobDescriptor {
    /// Full URL to the blob.
    pub url: String,
    /// SHA-256 hex of the stored bytes.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
    /// MIME type the relay recorded.
    #[serde(rename = "type")]
    pub mime_type: String,
    /// Pixel dimensions (`WxH`) for images and video.
    #[serde(default)]
    pub dim: Option<String>,
    /// Duration in seconds for audio and video.
    #[serde(default)]
    pub duration: Option<f64>,
}

/// The exact origin the harness will fetch attachments from.
///
/// Attachments are Buzz media on the relay the agent is connected to. Anything
/// else is refused before a request is made: the get authorization would leak
/// the agent's signature to a third party, and the bytes could not be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl RelayOrigin {
    /// Derive the origin from the REST base URL (`http(s)://host[:port]`).
    pub fn from_base_url(base_url: &str) -> Option<Self> {
        let parsed = url::Url::parse(base_url).ok()?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if scheme != "https" && scheme != "http" {
            return None;
        }
        let host = parsed.host_str()?.to_ascii_lowercase();
        let port = parsed.port_or_known_default()?;
        Some(Self {
            scheme,
            host: host.trim_end_matches('.').to_string(),
            port,
        })
    }

    /// Host (with a non-default port) as the Blossom `server` tag expects.
    pub fn server_tag(&self) -> String {
        let default_port = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Check an attachment URL against the origin rule.
    ///
    /// The URL must be `https` (plain `http` is accepted only when the relay
    /// itself is reached over `http`, which is the loopback developer setup),
    /// carry no credentials or fragment, and resolve to exactly this host and
    /// port.
    pub fn permits(&self, candidate: &str) -> Result<url::Url, BlossomError> {
        let parsed = url::Url::parse(candidate)
            .map_err(|e| BlossomError::UntrustedUrl(format!("unparseable ({e})")))?;
        let scheme = parsed.scheme().to_ascii_lowercase();
        if scheme != "https" && !(scheme == "http" && self.scheme == "http") {
            return Err(BlossomError::UntrustedUrl(format!(
                "scheme {scheme} is not https"
            )));
        }
        if scheme != self.scheme {
            return Err(BlossomError::UntrustedUrl(format!(
                "scheme {scheme} does not match the relay origin scheme {}",
                self.scheme
            )));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(BlossomError::UntrustedUrl(
                "credentials embedded in URL".into(),
            ));
        }
        if parsed.fragment().is_some() {
            return Err(BlossomError::UntrustedUrl("fragment in URL".into()));
        }
        let host = parsed
            .host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
            .unwrap_or_default();
        let port = parsed.port_or_known_default().unwrap_or(0);
        if host != self.host || port != self.port {
            return Err(BlossomError::UntrustedUrl(format!(
                "{host}:{port} is not the relay origin {}:{}",
                self.host, self.port
            )));
        }
        Ok(parsed)
    }
}

/// Which Blossom verb an authorization covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlossomVerb {
    /// `t get` for reading a blob.
    Get,
    /// `t upload` for storing a blob.
    Upload,
}

impl BlossomVerb {
    fn tag_value(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Upload => "upload",
        }
    }

    fn content(self) -> &'static str {
        match self {
            Self::Get => "Get file",
            Self::Upload => "Upload file",
        }
    }
}

/// Sign a kind-24242 authorization event for one blob and return it as the
/// `Authorization` header value (`Nostr <base64url(event json)>`).
///
/// Tags follow BUD-02/BUD-11 and match what the relay verifies in
/// `buzz-media::auth`: `t <verb>`, `x <sha256>`, `expiration <unix>`, and a
/// `server <host>` tag scoping the token to this relay.
pub fn sign_blossom_auth(
    keys: &nostr::Keys,
    verb: BlossomVerb,
    sha256: &str,
    server: Option<&str>,
) -> Result<String, BlossomError> {
    let event = build_blossom_auth_event(keys, verb, sha256, server, unix_now())?;
    Ok(format!(
        "Nostr {}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(event.as_json().as_bytes())
    ))
}

/// Build (and sign) the kind-24242 event behind [`sign_blossom_auth`].
///
/// Split out so tests can inspect tags and verify the signature without
/// decoding the header.
pub fn build_blossom_auth_event(
    keys: &nostr::Keys,
    verb: BlossomVerb,
    sha256: &str,
    server: Option<&str>,
    now_secs: u64,
) -> Result<nostr::Event, BlossomError> {
    let expiration = (now_secs + AUTH_LIFETIME_SECS).to_string();
    let mut tags = vec![
        parse_tag(&["t", verb.tag_value()])?,
        parse_tag(&["x", sha256])?,
        parse_tag(&["expiration", &expiration])?,
    ];
    if let Some(server) = server {
        tags.push(parse_tag(&["server", server])?);
    }
    nostr::EventBuilder::new(nostr::Kind::Custom(24242), verb.content())
        .tags(tags)
        .custom_created_at(nostr::Timestamp::from(now_secs))
        .sign_with_keys(keys)
        .map_err(|e| BlossomError::Sign(e.to_string()))
}

fn parse_tag(parts: &[&str]) -> Result<nostr::Tag, BlossomError> {
    nostr::Tag::parse(parts.iter().copied()).map_err(|e| BlossomError::Sign(e.to_string()))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// SHA-256 of `bytes` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, BlossomError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| BlossomError::Http(format!("failed to build HTTP client: {e}")))
}

async fn bounded_body(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    let mut body: String = text.chars().take(MAX_ERROR_BODY).collect();
    if text.chars().count() > MAX_ERROR_BODY {
        body.push_str("...");
    }
    body
}

/// Download one blob from the relay with a signed get authorization.
///
/// `url` must already have passed [`RelayOrigin::permits`]. The response is
/// refused when `Content-Length` disagrees with `expected_size`, reading
/// stops the moment the body exceeds `expected_size`, and the bytes are
/// returned only when both the size and the SHA-256 match `expected_sha256`.
pub async fn download_blob(
    rest: &RestClient,
    origin: &RelayOrigin,
    url: &url::Url,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<Vec<u8>, BlossomError> {
    let auth = sign_blossom_auth(
        &rest.keys,
        BlossomVerb::Get,
        expected_sha256,
        Some(&origin.server_tag()),
    )?;
    let client = http_client(DOWNLOAD_TIMEOUT)?;
    let mut request = client
        .get(url.clone())
        .header("Authorization", auth)
        .header("Accept-Encoding", "identity");
    if let Some(tag) = &rest.auth_tag_json {
        request = request.header("x-auth-tag", tag);
    }

    let fetch = async {
        let mut resp = request
            .send()
            .await
            .map_err(|e| BlossomError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BlossomError::Refused {
                status: status.as_u16(),
                body: bounded_body(resp).await,
            });
        }
        if let Some(declared) = resp.content_length() {
            if declared != expected_size {
                return Err(BlossomError::Integrity(format!(
                    "Content-Length {declared} does not match imeta size {expected_size}"
                )));
            }
        }
        let capacity = usize::try_from(expected_size).unwrap_or(usize::MAX);
        let mut data = Vec::with_capacity(capacity.min(1 << 20));
        let mut hasher = Sha256::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| BlossomError::Http(e.to_string()))?
        {
            if (data.len() as u64).saturating_add(chunk.len() as u64) > expected_size {
                return Err(BlossomError::Integrity(
                    "body exceeded the declared imeta size".into(),
                ));
            }
            hasher.update(&chunk);
            data.extend_from_slice(&chunk);
        }
        if data.len() as u64 != expected_size {
            return Err(BlossomError::Integrity(format!(
                "body is {} bytes, imeta says {expected_size}",
                data.len()
            )));
        }
        let digest = hex::encode(hasher.finalize());
        if digest != expected_sha256 {
            return Err(BlossomError::Integrity(
                "SHA-256 does not match imeta x".into(),
            ));
        }
        Ok(data)
    };
    tokio::time::timeout(DOWNLOAD_TIMEOUT, fetch)
        .await
        .map_err(|_| BlossomError::Timeout(DOWNLOAD_TIMEOUT))?
}

/// Upload one blob with a signed upload authorization.
///
/// Tries `PUT /upload` first and falls back to the legacy `PUT /media/upload`
/// when the relay answers 404/405 for the standard route. Any other
/// non-success status is returned as [`BlossomError::Refused`] so the caller
/// can distinguish a media rejection (415/422) from a transport failure.
pub async fn upload_blob(
    rest: &RestClient,
    origin: &RelayOrigin,
    bytes: Vec<u8>,
    mime: &str,
) -> Result<BlobDescriptor, BlossomError> {
    let sha256 = sha256_hex(&bytes);
    let client = http_client(UPLOAD_TIMEOUT)?;
    let mut last = None;
    for path in ["/upload", "/media/upload"] {
        let auth = sign_blossom_auth(
            &rest.keys,
            BlossomVerb::Upload,
            &sha256,
            Some(&origin.server_tag()),
        )?;
        let mut request = client
            .put(format!("{}{path}", rest.base_url))
            .header("Authorization", auth)
            .header("Content-Type", mime)
            .header("X-SHA-256", &sha256);
        if let Some(tag) = &rest.auth_tag_json {
            request = request.header("x-auth-tag", tag);
        }
        // At most two attempts; the second only after a 404/405 on the
        // standard route, so the clone is paid once on legacy relays.
        let send = request.body(bytes.clone()).send();
        let resp = match tokio::time::timeout(UPLOAD_TIMEOUT, send).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => return Err(BlossomError::Http(e.to_string())),
            Err(_) => return Err(BlossomError::Timeout(UPLOAD_TIMEOUT)),
        };
        let status = resp.status();
        if matches!(status.as_u16(), 404 | 405) {
            last = Some(BlossomError::Refused {
                status: status.as_u16(),
                body: bounded_body(resp).await,
            });
            continue;
        }
        if !status.is_success() {
            return Err(BlossomError::Refused {
                status: status.as_u16(),
                body: bounded_body(resp).await,
            });
        }
        let descriptor: BlobDescriptor = resp
            .json()
            .await
            .map_err(|e| BlossomError::Http(format!("invalid blob descriptor: {e}")))?;
        if descriptor.url.is_empty() {
            return Err(BlossomError::Http("upload returned no url".into()));
        }
        return Ok(descriptor);
    }
    Err(last.unwrap_or_else(|| BlossomError::Http("relay has no Blossom upload endpoint".into())))
}

/// Cached answer to "does this relay accept native audio uploads?".
///
/// The NIP-11 document is fetched at most once per [`AUDIO_SUPPORT_TTL`]; a
/// failed probe is cached as `false` so a relay that is down does not get
/// re-probed on every reply.
#[derive(Debug, Default)]
pub struct AudioSupportCache {
    state: std::sync::Mutex<Option<(bool, Instant)>>,
}

impl AudioSupportCache {
    /// Return the cached answer, re-probing the relay when it is stale.
    pub async fn relay_supports_audio(&self, rest: &RestClient) -> bool {
        if let Some(cached) = self.cached() {
            return cached;
        }
        let supported = probe_audio_support(rest).await;
        self.store(supported);
        supported
    }

    /// Seed the cache directly. Used by tests and by callers that already
    /// learned the answer from a rejection.
    pub fn store(&self, supported: bool) {
        if let Ok(mut guard) = self.state.lock() {
            *guard = Some((supported, Instant::now()));
        }
    }

    fn cached(&self) -> Option<bool> {
        let guard = self.state.lock().ok()?;
        let (supported, at) = (*guard)?;
        (at.elapsed() < AUDIO_SUPPORT_TTL).then_some(supported)
    }
}

async fn probe_audio_support(rest: &RestClient) -> bool {
    let url = format!("{}/", rest.base_url);
    let resp = match rest
        .http
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/nostr+json")
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(target: "acp::media", "NIP-11 probe failed: {e}");
            return false;
        }
    };
    if !resp.status().is_success() {
        tracing::warn!(target: "acp::media", "NIP-11 probe returned HTTP {}", resp.status());
        return false;
    }
    match resp.json::<serde_json::Value>().await {
        Ok(doc) => nip11_advertises_audio(&doc),
        Err(e) => {
            tracing::warn!(target: "acp::media", "NIP-11 probe returned invalid JSON: {e}");
            false
        }
    }
}

/// Whether a NIP-11 document lists the `buzz-audio` extension.
pub fn nip11_advertises_audio(document: &serde_json::Value) -> bool {
    document["supported_extensions"]
        .as_array()
        .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(AUDIO_EXTENSION)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_values(event: &nostr::Event, name: &str) -> Vec<String> {
        event
            .tags
            .iter()
            .map(|t| t.as_slice())
            .filter(|parts| parts.first().map(String::as_str) == Some(name))
            .filter_map(|parts| parts.get(1).cloned())
            .collect()
    }

    #[test]
    fn origin_from_base_url_normalises_host_and_port() {
        let https = RelayOrigin::from_base_url("https://Relay.Example.:443").unwrap();
        assert_eq!(https.server_tag(), "relay.example");
        let custom = RelayOrigin::from_base_url("https://relay.example:3100").unwrap();
        assert_eq!(custom.server_tag(), "relay.example:3100");
        let http = RelayOrigin::from_base_url("http://localhost:3000").unwrap();
        assert_eq!(http.server_tag(), "localhost:3000");
        assert!(RelayOrigin::from_base_url("ftp://relay.example").is_none());
        assert!(RelayOrigin::from_base_url("not a url").is_none());
    }

    #[test]
    fn origin_permits_only_https_on_the_relay_host() {
        let origin = RelayOrigin::from_base_url("https://relay.example").unwrap();
        let sha = "a".repeat(64);
        assert!(origin
            .permits(&format!("https://relay.example/media/{sha}.mp3"))
            .is_ok());
        assert!(origin
            .permits(&format!("https://RELAY.example:443/media/{sha}"))
            .is_ok());
        for bad in [
            format!("http://relay.example/media/{sha}"),
            format!("https://relay.example:3100/media/{sha}"),
            format!("https://other.example/media/{sha}"),
            format!("https://user:pw@relay.example/media/{sha}"),
            format!("https://relay.example/media/{sha}#frag"),
            "file:///etc/passwd".to_string(),
        ] {
            assert!(
                matches!(origin.permits(&bad), Err(BlossomError::UntrustedUrl(_))),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn origin_allows_plain_http_only_for_http_relays() {
        let dev = RelayOrigin::from_base_url("http://127.0.0.1:3000").unwrap();
        assert!(dev.permits("http://127.0.0.1:3000/media/abc").is_ok());
        assert!(dev.permits("http://127.0.0.1:3001/media/abc").is_err());
        assert!(dev.permits("https://127.0.0.1:3000/media/abc").is_err());
    }

    #[test]
    fn upload_auth_event_matches_relay_expectations() {
        let keys = nostr::Keys::generate();
        let sha = "b".repeat(64);
        let event = build_blossom_auth_event(
            &keys,
            BlossomVerb::Upload,
            &sha,
            Some("relay.example"),
            1_700_000_000,
        )
        .unwrap();
        assert_eq!(event.kind.as_u16(), 24242);
        assert_eq!(event.content, "Upload file");
        assert_eq!(event.pubkey, keys.public_key());
        assert!(event.verify().is_ok(), "signature must verify");
        assert_eq!(tag_values(&event, "t"), vec!["upload"]);
        assert_eq!(tag_values(&event, "x"), vec![sha]);
        assert_eq!(tag_values(&event, "server"), vec!["relay.example"]);
        assert_eq!(
            tag_values(&event, "expiration"),
            vec![(1_700_000_000 + AUTH_LIFETIME_SECS).to_string()]
        );
    }

    #[test]
    fn get_auth_event_uses_get_verb_and_blob_hash() {
        let keys = nostr::Keys::generate();
        let sha = "c".repeat(64);
        let event =
            build_blossom_auth_event(&keys, BlossomVerb::Get, &sha, None, 1_700_000_000).unwrap();
        assert_eq!(tag_values(&event, "t"), vec!["get"]);
        assert_eq!(tag_values(&event, "x"), vec![sha]);
        assert!(tag_values(&event, "server").is_empty());
        assert_eq!(event.content, "Get file");
        assert!(event.verify().is_ok());
    }

    #[test]
    fn auth_header_is_nostr_base64url_of_the_event() {
        let keys = nostr::Keys::generate();
        let header = sign_blossom_auth(&keys, BlossomVerb::Upload, &"d".repeat(64), None).unwrap();
        let token = header.strip_prefix("Nostr ").expect("Nostr scheme");
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(token)
            .expect("base64url without padding");
        let event = nostr::Event::from_json(&json).expect("event json");
        assert_eq!(event.kind.as_u16(), 24242);
        assert!(event.verify().is_ok());
    }

    #[test]
    fn media_rejection_statuses() {
        assert!(is_media_rejection_status(415));
        assert!(is_media_rejection_status(422));
        assert!(!is_media_rejection_status(413));
        assert!(!is_media_rejection_status(500));
        assert!(BlossomError::Refused {
            status: 422,
            body: String::new()
        }
        .is_media_rejection());
        assert!(!BlossomError::Http("x".into()).is_media_rejection());
    }

    #[test]
    fn nip11_audio_extension_detection() {
        assert!(nip11_advertises_audio(&serde_json::json!({
            "supported_extensions": ["buzz-audio", "other"]
        })));
        assert!(!nip11_advertises_audio(&serde_json::json!({
            "supported_extensions": ["other"]
        })));
        assert!(!nip11_advertises_audio(&serde_json::json!({})));
        assert!(!nip11_advertises_audio(&serde_json::json!({
            "supported_extensions": "buzz-audio"
        })));
    }

    #[test]
    fn audio_support_cache_reuses_stored_answer() {
        let cache = AudioSupportCache::default();
        assert_eq!(cache.cached(), None);
        cache.store(true);
        assert_eq!(cache.cached(), Some(true));
    }

    #[test]
    fn blob_descriptor_parses_relay_shape() {
        let desc: BlobDescriptor = serde_json::from_str(
            r#"{"url":"https://r/media/ab.mp3","sha256":"ab","size":12,"type":"audio/mpeg","uploaded":1,"duration":2.5}"#,
        )
        .unwrap();
        assert_eq!(desc.mime_type, "audio/mpeg");
        assert_eq!(desc.duration, Some(2.5));
        assert_eq!(desc.dim, None);
    }
}
