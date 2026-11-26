//! Bounded HTTP client construction shared across outbound layers.
//!
//! Centralizes TLS provider installation and socket connect/read timeouts so no outbound call in
//! `core` or `app` can hang forever on a half-open TCP connection.

use crate::rt::supervise::End;
use reqwest::Client;
use std::time::Duration;

/// Socket connect timeout for outbound HTTP requests: matches lane `PATIENCE_MS` (`P1-18`).
pub const HTTP_CONNECT_TIMEOUT_MS: u64 = 10_000;
/// Idle socket read timeout for outbound HTTP requests: 60 s covers slow LLM first-token waits
/// while killing stalled connections; streaming uploads reset the read timer on each chunk.
pub const HTTP_READ_TIMEOUT_MS: u64 = 60_000;

/// Builds an outbound HTTP client with the standard connect and idle-read timeouts.
pub fn client() -> Result<Client, End> {
    client_with_timeouts(HTTP_CONNECT_TIMEOUT_MS, HTTP_READ_TIMEOUT_MS)
}

/// Builds an outbound HTTP client with explicit millisecond connect and idle-read timeouts.
pub fn client_with_timeouts(connect_ms: u64, read_ms: u64) -> Result<Client, End> {
    let _ = rustls::crypto::ring::default_provider().install_default(); // dropped: installing the same TLS provider again is harmless
    Client::builder()
        .connect_timeout(Duration::from_millis(connect_ms))
        .read_timeout(Duration::from_millis(read_ms))
        .build()
        .map_err(|_| End::Fatal("tls"))
}
