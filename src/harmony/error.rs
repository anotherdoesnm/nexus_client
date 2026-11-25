//! Errors the Harmony transports produce, mapped from the API's documented
//! status/`error` pairs so the UI can explain what happened.

use serde::Deserialize;

/// The JSON body the bot API answers with on failure.
///
/// `code`, `missing_permissions` and `max_position` are only present on some
/// errors; `retry_after` accompanies `429` on upload routes. See
/// `docs/build/rest.md#errors`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ApiErrorBody {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub code: Option<serde_json::Value>,
    #[serde(default)]
    pub missing_permissions: Option<serde_json::Value>,
    #[serde(default)]
    pub max_position: Option<serde_json::Value>,
    #[serde(default)]
    pub retry_after: Option<f64>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum HarmonyError {
    #[error("network error: {0}")]
    Network(String),

    #[error("instance not reachable: {0}")]
    Unreachable(String),

    #[error("not a Harmony instance: {0}")]
    NotHarmony(String),

    #[error("invalid instance address: {0}")]
    BadInstance(String),

    #[error("unauthorized: {message}")]
    Unauthorized { message: String },

    #[error("forbidden: {message}")]
    Forbidden { message: String },

    #[error("not found: {message}")]
    NotFound { message: String },

    #[error("bad request: {message}")]
    BadRequest { message: String },

    #[error("conflict: {message}")]
    Conflict { message: String },

    #[error("rate limited{}", retry_suffix(.retry_after))]
    RateLimited {
        message: String,
        retry_after: Option<f64>,
    },

    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("not supported: {0}")]
    Unsupported(String),

    #[error("gateway closed ({code:?}): {reason}")]
    GatewayClosed { code: Option<u16>, reason: String },
}

/// Render a JSON scalar without serde's quotes, so an error reads
/// `missing permissions: 2048` rather than `: "2048"`.
fn scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn retry_suffix(after: &Option<f64>) -> String {
    match after {
        Some(secs) => format!(", retry after {secs}s"),
        None => String::new(),
    }
}

impl HarmonyError {
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Unauthorized {
            message: message.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub fn network(err: impl std::fmt::Display) -> Self {
        Self::Network(err.to_string())
    }

    /// Build the typed error for an HTTP response. `body` is the raw response
    /// body; it is parsed as the API's error object when possible.
    pub fn from_response(status: u16, body: &str) -> Self {
        let parsed: ApiErrorBody = serde_json::from_str(body).unwrap_or_default();
        let message = if parsed.error.is_empty() {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                format!("HTTP {status}")
            } else {
                trimmed.chars().take(200).collect()
            }
        } else {
            match (&parsed.missing_permissions, &parsed.code) {
                (Some(bits), _) if !bits.is_null() => format!(
                    "{} (missing permissions: {})",
                    parsed.error,
                    scalar_text(bits)
                ),
                (_, Some(code)) if !code.is_null() => {
                    format!("{} (code: {})", parsed.error, scalar_text(code))
                }
                _ => parsed.error.clone(),
            }
        };

        match status {
            400 | 422 => Self::BadRequest { message },
            401 => Self::Unauthorized { message },
            403 => Self::Forbidden { message },
            404 => Self::NotFound { message },
            409 => Self::Conflict { message },
            429 => Self::RateLimited {
                message,
                retry_after: parsed.retry_after,
            },
            _ => Self::Server { status, message },
        }
    }

    /// Whether the same request may be tried again later without changing it.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) | Self::Unreachable(_) | Self::RateLimited { .. } => true,
            Self::Server { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// The HTTP status, when the error came from a response.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::BadRequest { .. } => Some(400),
            Self::Unauthorized { .. } => Some(401),
            Self::Forbidden { .. } => Some(403),
            Self::NotFound { .. } => Some(404),
            Self::Conflict { .. } => Some(409),
            Self::RateLimited { .. } => Some(429),
            Self::Server { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// A sentence for the notification line.
    pub fn user_message(&self) -> String {
        match self {
            Self::Network(err) => format!("Network error: {err}"),
            Self::Unreachable(host) => format!("Cannot reach {host}"),
            Self::NotHarmony(host) => format!("{host} does not answer as a Harmony instance"),
            Self::BadInstance(host) => format!("Invalid instance address: {host}"),
            Self::Unauthorized { message } => format!("Sign-in rejected: {message}"),
            Self::Forbidden { message } => format!("Not allowed: {message}"),
            Self::NotFound { message } => format!("Not found: {message}"),
            Self::BadRequest { message } => format!("Rejected: {message}"),
            Self::Conflict { message } => format!("Conflict: {message}"),
            Self::RateLimited { retry_after, .. } => match retry_after {
                Some(secs) => format!("Rate limited; retry in {secs}s"),
                None => "Rate limited by the instance".to_string(),
            },
            Self::Server { status, message } => format!("Instance error {status}: {message}"),
            Self::Protocol(message) => format!("Protocol error: {message}"),
            Self::Unsupported(what) => format!("Not supported: {what}"),
            Self::GatewayClosed { code, reason } => match code {
                Some(code) => format!("Gateway closed ({code}): {reason}"),
                None => format!("Gateway closed: {reason}"),
            },
        }
    }
}

impl From<reqwest::Error> for HarmonyError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_decode() {
            Self::Protocol(err.to_string())
        } else if err.is_timeout() || err.is_connect() {
            Self::Unreachable(err.to_string())
        } else {
            Self::Network(err.to_string())
        }
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for HarmonyError {
    fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
        use tokio_tungstenite::tungstenite::Error as WsError;
        match err {
            WsError::ConnectionClosed | WsError::AlreadyClosed => Self::GatewayClosed {
                code: None,
                reason: "connection closed".to_string(),
            },
            WsError::Protocol(proto) => Self::Protocol(proto.to_string()),
            WsError::Io(io) => Self::Network(io.to_string()),
            other => Self::Protocol(other.to_string()),
        }
    }
}

impl From<url::ParseError> for HarmonyError {
    fn from(err: url::ParseError) -> Self {
        Self::BadInstance(err.to_string())
    }
}

impl From<serde_json::Error> for HarmonyError {
    fn from(err: serde_json::Error) -> Self {
        Self::Protocol(format!("invalid JSON: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_documented_statuses() {
        let err = HarmonyError::from_response(401, r#"{"error":"Invalid or expired token"}"#);
        assert!(matches!(err, HarmonyError::Unauthorized { .. }));
        assert_eq!(err.status(), Some(401));

        let err = HarmonyError::from_response(
            403,
            r#"{"error":"Missing permission: send_messages","missing_permissions":"2048"}"#,
        );
        assert!(matches!(err, HarmonyError::Forbidden { .. }));
        assert!(err.to_string().contains("missing permissions: 2048"));

        let err = HarmonyError::from_response(429, r#"{"error":"Rate limit exceeded"}"#);
        assert!(matches!(err, HarmonyError::RateLimited { .. }));
        assert!(err.is_retryable());

        let err = HarmonyError::from_response(500, "boom");
        assert!(matches!(err, HarmonyError::Server { status: 500, .. }));
        assert!(err.is_retryable());

        let err = HarmonyError::from_response(404, "");
        assert_eq!(err.to_string(), "not found: HTTP 404");
        assert!(!err.is_retryable());
    }

    #[test]
    fn parses_retry_after_from_upload_errors() {
        let err = HarmonyError::from_response(
            429,
            r#"{"error":"Too many uploads at once","retry_after":2}"#,
        );
        match err {
            HarmonyError::RateLimited { retry_after, .. } => assert_eq!(retry_after, Some(2.0)),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(err.to_string().contains("retry after 2s"));
    }

    #[test]
    fn code_is_included_when_present() {
        let err = HarmonyError::from_response(
            403,
            r#"{"error":"Blocked by the server's AutoMod","code":"AUTOMOD_BLOCKED"}"#,
        );
        assert!(err.to_string().contains("AUTOMOD_BLOCKED"));
    }
}
