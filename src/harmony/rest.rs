//! The bot REST API: `https://<instance>/bot-gateway/api/v1`.
//!
//! One typed method per route the client uses, plus the retry policy the docs
//! describe (`429` with `Retry-After`, `5xx` backoff) and the error mapping in
//! [`super::error`]. The client holds a [`RetryPolicy`] rather than sleeping
//! blindly, so tests can shrink it to zero.

use std::time::Duration;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;

use super::error::{ApiErrorBody, HarmonyError};
use super::model::{
    BotIdentity, Category, Channel, CreateMessage, Emoji, InvitePreview, Member, Message, Profile,
    Role, Server, Thread,
};

/// How many times one request is tried, and how long to wait between tries.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
        }
    }
}

impl RetryPolicy {
    /// No retries; used by tests and by callers that want a fast failure.
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    /// The delay before attempt `attempt` (1-based). A server-supplied
    /// `retry_after` wins over the exponential backoff; both are capped.
    pub fn delay(&self, attempt: u32, retry_after: Option<f64>) -> Duration {
        if let Some(secs) = retry_after {
            let requested = Duration::from_secs_f64(secs.max(0.0));
            return requested.min(self.max_delay);
        }
        let factor = 2u32.saturating_pow(attempt.saturating_sub(1).min(16));
        self.base_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

/// A bot-authenticated REST client. Cheap to clone: it is an `Arc` inside
/// `reqwest::Client` plus a token string.
#[derive(Clone)]
pub struct BotRest {
    base: Url,
    token: String,
    http: Client,
    retry: RetryPolicy,
}

impl std::fmt::Debug for BotRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token.
        f.debug_struct("BotRest")
            .field("base", &self.base.as_str())
            .field("token", &"<redacted>")
            .finish()
    }
}

impl BotRest {
    /// `base` is the API root, e.g. `https://har.mony.lol/bot-gateway/api/v1`.
    pub fn new(base: Url, token: impl Into<String>) -> Result<Self, HarmonyError> {
        Self::with_policy(base, token, RetryPolicy::default())
    }

    pub fn with_policy(
        base: Url,
        token: impl Into<String>,
        retry: RetryPolicy,
    ) -> Result<Self, HarmonyError> {
        let http = Client::builder()
            .user_agent(concat!("nexus-tui/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(HarmonyError::network)?;
        Ok(Self {
            base,
            token: token.into(),
            http,
            retry,
        })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    pub fn set_retry_policy(&mut self, retry: RetryPolicy) {
        self.retry = retry;
    }

    // -- plumbing ----------------------------------------------------------

    fn url(&self, path: &str) -> Result<Url, HarmonyError> {
        self.base
            .join(path.trim_start_matches('/'))
            .map_err(|e| HarmonyError::protocol(format!("bad API path {path}: {e}")))
    }

    fn auth(&self, builder: RequestBuilder) -> RequestBuilder {
        builder.header("Authorization", format!("Bot {}", self.token))
    }

    async fn execute(&self, method: Method, path: &str, body: Option<&serde_json::Value>) -> Result<Response, HarmonyError> {
        let url = self.url(path)?;
        let mut last_error: Option<HarmonyError> = None;

        for attempt in 1..=self.retry.max_attempts {
            let mut builder = self.http.request(method.clone(), url.clone());
            builder = self.auth(builder);
            if let Some(body) = body {
                builder = builder.json(body);
            }
            let result = builder.send().await;

            match result {
                Ok(response) => {
                    if response.status().is_success() {
                        return Ok(response);
                    }
                    let status = response.status();
                    let retry_after = parse_retry_after(&response);
                    let text = response.text().await.unwrap_or_default();
                    let error = HarmonyError::from_response(status.as_u16(), &text);
                    if error.is_retryable() && attempt < self.retry.max_attempts {
                        let delay = self.retry.delay(attempt, retry_after);
                        tracing::debug!(
                            "harmony: {} {} answered {}; retrying in {:?}",
                            method,
                            path,
                            status,
                            delay
                        );
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
                Err(err) => {
                    let error = HarmonyError::from(err);
                    if error.is_retryable() && attempt < self.retry.max_attempts {
                        let delay = self.retry.delay(attempt, None);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            HarmonyError::protocol(format!("{method} {path} exhausted its attempts"))
        }))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<T, HarmonyError> {
        let response = self.execute(method, path, body).await?;
        let status = response.status();
        let text = response.text().await.map_err(HarmonyError::from)?;
        if text.trim().is_empty() {
            return Err(HarmonyError::protocol(format!(
                "HTTP {status} carried an empty body where JSON was expected"
            )));
        }
        serde_json::from_str(&text).map_err(|err| {
            HarmonyError::protocol(format!(
                "unexpected JSON ({}): {err}",
                text.chars().take(160).collect::<String>()
            ))
        })
    }

    async fn empty(&self, method: Method, path: &str, body: Option<&serde_json::Value>) -> Result<(), HarmonyError> {
        self.execute(method, path, body).await?;
        Ok(())
    }

    // -- routes ------------------------------------------------------------

    /// `GET /users/@me`
    pub async fn me(&self) -> Result<BotIdentity, HarmonyError> {
        self.json(Method::GET, "users/@me", None).await
    }

    /// `GET /servers/{id}`. Needs an active installation.
    pub async fn server(&self, server_id: Uuid) -> Result<Server, HarmonyError> {
        self.json(Method::GET, &format!("servers/{server_id}"), None)
            .await
    }

    /// `GET /servers/{id}/channels`: every channel, including ones the bot
    /// cannot see.
    pub async fn channels(&self, server_id: Uuid) -> Result<Vec<Channel>, HarmonyError> {
        self.json(Method::GET, &format!("servers/{server_id}/channels"), None)
            .await
    }

    /// `GET /servers/{id}/categories`
    pub async fn categories(&self, server_id: Uuid) -> Result<Vec<Category>, HarmonyError> {
        self.json(Method::GET, &format!("servers/{server_id}/categories"), None)
            .await
    }

    /// `GET /servers/{id}/roles`
    pub async fn roles(&self, server_id: Uuid) -> Result<Vec<Role>, HarmonyError> {
        self.json(Method::GET, &format!("servers/{server_id}/roles"), None)
            .await
    }

    /// `GET /servers/{id}/emojis`
    pub async fn emojis(&self, server_id: Uuid) -> Result<Vec<Emoji>, HarmonyError> {
        self.json(Method::GET, &format!("servers/{server_id}/emojis"), None)
            .await
    }

    /// One page of `GET /servers/{id}/members`, ordered by user id.
    pub async fn member_page(
        &self,
        server_id: Uuid,
        after: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<Member>, HarmonyError> {
        let mut path = format!(
            "servers/{server_id}/members?limit={}",
            limit.clamp(1, 1000)
        );
        if let Some(after) = after {
            path.push_str(&format!("&after={after}"));
        }
        self.json(Method::GET, &path, None).await
    }

    /// Every member of a server, following `after` until a short page.
    pub async fn members(&self, server_id: Uuid) -> Result<Vec<Member>, HarmonyError> {
        const PAGE: u32 = 1000;
        let mut all = Vec::new();
        let mut after = None;
        loop {
            let page = self.member_page(server_id, after, PAGE).await?;
            let received = page.len() as u32;
            if let Some(last) = page.last() {
                after = Some(last.user.id);
            }
            all.extend(page);
            if received < PAGE {
                break;
            }
            // Guard against an instance that ignores `after`.
            if all.len() > 50_000 {
                break;
            }
        }
        Ok(all)
    }

    /// `GET /channels/{id}/messages`, newest first.
    pub async fn messages(
        &self,
        channel_id: Uuid,
        before: Option<i64>,
        after: Option<i64>,
        limit: u32,
    ) -> Result<Vec<Message>, HarmonyError> {
        let mut path = format!(
            "channels/{channel_id}/messages?limit={}",
            limit.clamp(1, 100)
        );
        if let Some(before) = before {
            path.push_str(&format!(
                "&before={}",
                super::model::epoch_to_iso8601(before)
            ));
        }
        if let Some(after) = after {
            path.push_str(&format!(
                "&after={}",
                super::model::epoch_to_iso8601(after)
            ));
        }
        self.json(Method::GET, &path, None).await
    }

    /// `POST /channels/{id}/messages`
    pub async fn send_message(
        &self,
        channel_id: Uuid,
        message: &CreateMessage,
    ) -> Result<Message, HarmonyError> {
        let body = serde_json::to_value(message).map_err(HarmonyError::from)?;
        self.json(
            Method::POST,
            &format!("channels/{channel_id}/messages"),
            Some(&body),
        )
        .await
    }

    /// `PATCH /messages/{id}`
    pub async fn edit_message(
        &self,
        message_id: Uuid,
        content: &str,
    ) -> Result<Message, HarmonyError> {
        let body = serde_json::json!({ "content": content });
        self.json(Method::PATCH, &format!("messages/{message_id}"), Some(&body))
            .await
    }

    /// `DELETE /messages/{id}`
    pub async fn delete_message(&self, message_id: Uuid) -> Result<(), HarmonyError> {
        self.empty(Method::DELETE, &format!("messages/{message_id}"), None)
            .await
    }

    /// `PUT /messages/{id}/reactions/{emoji}`
    pub async fn add_reaction(&self, message_id: Uuid, emoji: &str) -> Result<(), HarmonyError> {
        let encoded: String = utf8_percent_encode(emoji, NON_ALPHANUMERIC).to_string();
        self.empty(
            Method::PUT,
            &format!("messages/{message_id}/reactions/{encoded}"),
            None,
        )
        .await
    }

    /// `DELETE /messages/{id}/reactions/{emoji}`
    pub async fn remove_reaction(&self, message_id: Uuid, emoji: &str) -> Result<(), HarmonyError> {
        let encoded: String = utf8_percent_encode(emoji, NON_ALPHANUMERIC).to_string();
        self.empty(
            Method::DELETE,
            &format!("messages/{message_id}/reactions/{encoded}"),
            None,
        )
        .await
    }

    /// `POST /channels/{id}/threads`
    pub async fn create_thread(
        &self,
        channel_id: Uuid,
        name: &str,
        message_id: Option<Uuid>,
    ) -> Result<Thread, HarmonyError> {
        let mut body = serde_json::json!({ "name": name });
        if let Some(message_id) = message_id {
            body["message_id"] = serde_json::json!(message_id);
        }
        self.json(
            Method::POST,
            &format!("channels/{channel_id}/threads"),
            Some(&body),
        )
        .await
    }

    /// `GET /threads/{id}`
    pub async fn thread(&self, thread_id: Uuid) -> Result<Thread, HarmonyError> {
        self.json(Method::GET, &format!("threads/{thread_id}"), None)
            .await
    }

    /// `GET /users/{id}`
    pub async fn user(&self, user_id: Uuid) -> Result<Profile, HarmonyError> {
        self.json(Method::GET, &format!("users/{user_id}"), None)
            .await
    }

    /// `GET /invites/{code}/preview`
    pub async fn invite_preview(&self, code: &str) -> Result<InvitePreview, HarmonyError> {
        let encoded: String = utf8_percent_encode(code, NON_ALPHANUMERIC).to_string();
        self.json(Method::GET, &format!("invites/{encoded}/preview"), None)
            .await
    }

    /// Download an image the instance serves (avatars, icons). Capped so a
    /// hostile URL cannot exhaust memory.
    pub async fn download(&self, url: &str) -> Result<Vec<u8>, HarmonyError> {
        const MAX: usize = 4 * 1024 * 1024;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(HarmonyError::from)?;
        if !response.status().is_success() {
            return Err(HarmonyError::from_response(
                response.status().as_u16(),
                "<image>",
            ));
        }
        if let Some(length) = response.content_length() {
            if length as usize > MAX {
                return Err(HarmonyError::protocol(format!(
                    "image at {url} is {length} bytes, over the {MAX}-byte cap"
                )));
            }
        }
        let bytes = response.bytes().await.map_err(HarmonyError::from)?;
        if bytes.len() > MAX {
            return Err(HarmonyError::protocol(format!(
                "image at {url} is {} bytes, over the {MAX}-byte cap",
                bytes.len()
            )));
        }
        Ok(bytes.to_vec())
    }
}

fn parse_retry_after(response: &Response) -> Option<f64> {
    if response.status() != StatusCode::TOO_MANY_REQUESTS {
        return None;
    }
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<f64>().ok())
        .or_else(|| {
            response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok())
        })
}

/// The API root for an instance host.
pub fn api_base(instance: &str) -> Result<Url, HarmonyError> {
    let root = super::instance_root(instance)?;
    root.join("bot-gateway/api/v1/")
        .map_err(|e| HarmonyError::protocol(format!("bad API base: {e}")))
}

/// Parse an `error` body without a response, for unit tests.
pub fn parse_error_body(body: &str) -> Option<ApiErrorBody> {
    serde_json::from_str(body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_grows_and_caps() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        assert_eq!(policy.delay(1, None), Duration::from_millis(100));
        assert_eq!(policy.delay(2, None), Duration::from_millis(200));
        assert_eq!(policy.delay(3, None), Duration::from_millis(400));
        assert_eq!(policy.delay(4, None), Duration::from_millis(500));
    }

    #[test]
    fn retry_after_wins_over_backoff() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay(1, Some(2.0)), Duration::from_secs(2));
        // Capped by max_delay.
        assert_eq!(policy.delay(1, Some(600.0)), policy.max_delay);
    }

    #[test]
    fn api_base_builds_the_documented_prefix() {
        assert_eq!(
            api_base("har.mony.lol").unwrap().as_str(),
            "https://har.mony.lol/bot-gateway/api/v1/"
        );
        assert_eq!(
            api_base("https://har.mony.lol").unwrap().as_str(),
            "https://har.mony.lol/bot-gateway/api/v1/"
        );
        assert_eq!(
            api_base("http://localhost:8080").unwrap().as_str(),
            "http://localhost:8080/bot-gateway/api/v1/"
        );
    }

    #[test]
    fn api_base_rejects_garbage() {
        assert!(api_base("").is_err());
        assert!(api_base("not a host").is_err());
    }

    #[test]
    fn token_never_appears_in_debug() {
        let rest = BotRest::new(api_base("har.mony.lol").unwrap(), "harmony_bot_secret").unwrap();
        let printed = format!("{rest:?}");
        assert!(!printed.contains("secret"));
        assert!(printed.contains("redacted"));
    }
}
