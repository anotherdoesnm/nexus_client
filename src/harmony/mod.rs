//! Harmony's protocols, and the client that speaks them.
//!
//! Two transports sit behind [`transport::Transport`]:
//!
//! * [`bot`] — the documented bot protocol: `Authorization: Bot <token>` over
//!   `/bot-gateway/api/v1`, live events from the `/bot-gateway/gateway`
//!   WebSocket (opcodes 0/1/2/11, identify, heartbeat, reconnect).
//! * [`web`] — the protocol Harmony's own Vue app speaks: instance discovery,
//!   GoTrue sign-in, PostgREST reads and writes with the user's JWT.
//!
//! [`session`] maps the UI's [`crate::domain::Command`]s onto whichever
//! transport is signed in and turns its events into [`crate::domain::Event`]s.
//!
//! Nothing in here uses the old Nexus wire model: no `bincode`, no
//! `LengthDelimitedCodec`, no `ClientMessage`/`ServerMessage` frames.

pub mod bot;
pub mod error;
pub mod gateway;
pub mod model;
pub mod rest;
pub mod session;
pub mod transport;
pub mod web;

pub use error::HarmonyError;
pub use transport::{Transport, TransportEvent};

use url::Url;

/// Normalise what a person types into an instance root URL with a trailing
/// slash, so `Url::join` keeps the path.
///
/// Accepts `har.mony.lol`, `https://har.mony.lol/`, `http://localhost:8080`
/// and leaves local hosts on `http` when no scheme is given.
pub fn instance_root(instance: &str) -> Result<Url, HarmonyError> {
    let trimmed = instance.trim();
    if trimmed.is_empty() {
        return Err(HarmonyError::BadInstance("empty instance address".to_string()));
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        let host = trimmed.split(['/', '?', '#']).next().unwrap_or(trimmed);
        let hostname = host.split(':').next().unwrap_or(host);
        let scheme = if matches!(hostname, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1")
            || hostname.ends_with(".local")
        {
            "http"
        } else {
            "https"
        };
        format!("{scheme}://{trimmed}")
    };

    let mut url = Url::parse(&with_scheme).map_err(HarmonyError::from)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HarmonyError::BadInstance(format!(
            "unsupported scheme {}",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(HarmonyError::BadInstance(trimmed.to_string()));
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/').to_string() + "/";
    url.set_path(&path);
    Ok(url)
}

/// `wss://<instance>/bot-gateway/gateway` (plain `ws://` when the instance is
/// HTTP), the URL the docs give for a proxy-fronted gateway.
pub fn gateway_url(instance: &str) -> Result<Url, HarmonyError> {
    let root = instance_root(instance)?;
    let mut url = root
        .join("bot-gateway/gateway")
        .map_err(|e| HarmonyError::protocol(format!("bad gateway URL: {e}")))?;
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => {
            return Err(HarmonyError::BadInstance(format!(
                "unsupported scheme {other}"
            )))
        }
    };
    url.set_scheme(scheme)
        .map_err(|_| HarmonyError::protocol("could not set the gateway scheme"))?;
    Ok(url)
}

/// `https://<instance>/api/federation/instance-info`, the discovery document.
pub fn instance_info_url(instance: &str) -> Result<Url, HarmonyError> {
    let root = instance_root(instance)?;
    root.join("api/federation/instance-info")
        .map_err(|e| HarmonyError::protocol(format!("bad discovery URL: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_root_normalises_hosts() {
        assert_eq!(
            instance_root("har.mony.lol").unwrap().as_str(),
            "https://har.mony.lol/"
        );
        assert_eq!(
            instance_root("  https://har.mony.lol/  ").unwrap().as_str(),
            "https://har.mony.lol/"
        );
        assert_eq!(
            instance_root("http://localhost:3002").unwrap().as_str(),
            "http://localhost:3002/"
        );
        assert_eq!(
            instance_root("localhost:3002").unwrap().as_str(),
            "http://localhost:3002/"
        );
    }

    #[test]
    fn instance_root_rejects_nonsense() {
        assert!(instance_root("").is_err());
        assert!(instance_root("   ").is_err());
        assert!(instance_root("ftp://example.com").is_err());
    }

    #[test]
    fn gateway_urls_use_the_documented_path_and_scheme() {
        assert_eq!(
            gateway_url("har.mony.lol").unwrap().as_str(),
            "wss://har.mony.lol/bot-gateway/gateway"
        );
        assert_eq!(
            gateway_url("http://localhost:3002").unwrap().as_str(),
            "ws://localhost:3002/bot-gateway/gateway"
        );
    }

    #[test]
    fn discovery_url_is_the_app_endpoint() {
        assert_eq!(
            instance_info_url("har.mony.lol").unwrap().as_str(),
            "https://har.mony.lol/api/federation/instance-info"
        );
    }
}
