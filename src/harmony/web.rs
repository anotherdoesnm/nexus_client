//! The web-app transport: the protocol Harmony's own Vue app speaks.
//!
//! Discovery (`/api/federation/instance-info`) hands out the Supabase URL and
//! anon key; GoTrue authenticates an email/password; PostgREST reads and writes
//! the tables the app reads, with the user's own JWT so row level security
//! applies. This transport is REST-only for now: Realtime's Phoenix protocol is
//! not implemented, so the session reports `web` capabilities without presence,
//! typing or live updates and the bot gateway remains the transport for live
//! events.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use url::Url;
use uuid::Uuid;

use crate::domain::{Capabilities, ChannelKind, TransportKind};

use super::error::HarmonyError;
use super::model::parse_timestamp;
use super::transport::{
    Account, AttachmentInfo, AuthorInfo, ChannelInfo, MemberInfo, MessageInfo, ProfileInfo,
    ReactionInfo, ServerInfo, Transport, TransportEvent,
};

/// What `GET /api/federation/instance-info` answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    pub supabase_url: String,
    pub supabase_anon_key: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl InstanceInfo {
    pub fn display_name(&self, fallback: &str) -> String {
        self.name.clone().unwrap_or_else(|| fallback.to_string())
    }
}

/// One authenticated web session: tokens, keys and the identity behind them.
#[derive(Debug, Clone)]
pub struct WebSession {
    pub instance: String,
    pub supabase_url: Url,
    pub anon_key: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub auth_user_id: Uuid,
    pub profile_id: Uuid,
    pub username: String,
}

#[derive(Debug, Deserialize)]
struct GoTrueSession {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    user: GoTrueUser,
}

#[derive(Debug, Deserialize)]
struct GoTrueUser {
    id: Uuid,
}

pub struct WebTransport {
    instance: String,
    email: String,
    password: String,
    http: reqwest::Client,
    info: Mutex<Option<InstanceInfo>>,
    session: Mutex<Option<WebSession>>,
    profiles: Mutex<HashMap<Uuid, ProfileRow>>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<TransportEvent>>>,
    events_tx: mpsc::UnboundedSender<TransportEvent>,
    closed: AtomicBool,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProfileRow {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    auth_user_id: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    is_admin: bool,
    #[serde(default)]
    bio: Option<String>,
}

impl ProfileRow {
    fn id_uuid(&self) -> Option<Uuid> {
        self.id.as_deref().and_then(|id| Uuid::parse_str(id).ok())
    }

}

impl WebTransport {
    pub fn new(
        instance: impl Into<String>,
        email: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<Self, HarmonyError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("nexus-tui/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(HarmonyError::network)?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Ok(Self {
            instance: instance.into(),
            email: email.into(),
            password: password.into(),
            http,
            info: Mutex::new(None),
            session: Mutex::new(None),
            profiles: Mutex::new(HashMap::new()),
            events_rx: Mutex::new(Some(events_rx)),
            events_tx,
            closed: AtomicBool::new(false),
        })
    }

    // -- bootstrap ---------------------------------------------------------

    fn root(&self) -> Result<Url, HarmonyError> {
        super::instance_root(&self.instance)
    }

    async fn discover(&self) -> Result<InstanceInfo, HarmonyError> {
        if let Some(info) = self.info.lock().expect("info lock").clone() {
            return Ok(info);
        }
        let url = self
            .root()?
            .join("api/federation/instance-info")
            .map_err(|e| HarmonyError::protocol(format!("bad discovery URL: {e}")))?;
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|err| {
                if err.is_connect() || err.is_timeout() {
                    HarmonyError::Unreachable(url.host_str().unwrap_or("instance").to_string())
                } else {
                    HarmonyError::from(err)
                }
            })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(HarmonyError::from_response(status, &body));
        }
        let info: InstanceInfo = response.json().await.map_err(|err| {
            HarmonyError::NotHarmony(format!("{}: {err}", self.instance))
        })?;
        if info.supabase_url.is_empty() || info.supabase_anon_key.is_empty() {
            return Err(HarmonyError::NotHarmony(format!(
                "{} answered without Supabase settings",
                self.instance
            )));
        }
        *self.info.lock().expect("info lock") = Some(info.clone());
        Ok(info)
    }

    fn rest_url(&self, info: &InstanceInfo, path: &str) -> Result<Url, HarmonyError> {
        Url::parse(&format!(
            "{}/rest/v1/{}",
            info.supabase_url.trim_end_matches('/'),
            path
        ))
        .map_err(|e| HarmonyError::protocol(format!("bad REST URL: {e}")))
    }

    async fn rest_get(&self, path: &str) -> Result<Vec<Value>, HarmonyError> {
        let info = self.discover().await?;
        let session = self.session()?;
        let url = self.rest_url(&info, path)?;
        let response = self
            .http
            .get(url)
            .header("apikey", &info.supabase_anon_key)
            .header("Authorization", format!("Bearer {}", session.access_token))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(HarmonyError::from)?;
        self.json_rows(response).await
    }

    async fn json_rows(&self, response: reqwest::Response) -> Result<Vec<Value>, HarmonyError> {
        let status = response.status();
        let text = response.text().await.map_err(HarmonyError::from)?;
        if !status.is_success() {
            return Err(postgrest_error(status.as_u16(), &text));
        }
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(rows)) => Ok(rows),
            Ok(other) => Ok(vec![other]),
            Err(err) => Err(HarmonyError::protocol(format!(
                "unexpected PostgREST answer: {err}"
            ))),
        }
    }

    async fn rest_write(&self, method: reqwest::Method, path: &str, body: &Value) -> Result<(), HarmonyError> {
        let info = self.discover().await?;
        let session = self.session()?;
        let url = self.rest_url(&info, path)?;
        let response = self
            .http
            .request(method, url)
            .header("apikey", &info.supabase_anon_key)
            .header("Authorization", format!("Bearer {}", session.access_token))
            .header("Content-Type", "application/json")
            .header("Prefer", "return=minimal")
            .body(body.to_string())
            .send()
            .await
            .map_err(HarmonyError::from)?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().await.unwrap_or_default();
        Err(postgrest_error(status.as_u16(), &text))
    }

    fn session(&self) -> Result<WebSession, HarmonyError> {
        self.session
            .lock()
            .expect("session lock")
            .clone()
            .ok_or_else(|| HarmonyError::unauthorized("not signed in"))
    }

    async fn authenticate(&self) -> Result<WebSession, HarmonyError> {
        let info = self.discover().await?;
        if self.email.trim().is_empty() || self.password.is_empty() {
            return Err(HarmonyError::unauthorized(
                "the web transport needs an email and a password",
            ));
        }
        let url = Url::parse(&format!(
            "{}/auth/v1/token?grant_type=password",
            info.supabase_url.trim_end_matches('/')
        ))
        .map_err(|e| HarmonyError::protocol(format!("bad auth URL: {e}")))?;

        let response = self
            .http
            .post(url)
            .header("apikey", &info.supabase_anon_key)
            .header("Content-Type", "application/json")
            .json(&json!({ "email": self.email, "password": self.password }))
            .send()
            .await
            .map_err(HarmonyError::from)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let message = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error_description")
                        .or_else(|| value.get("msg"))
                        .or_else(|| value.get("error"))
                        .and_then(|v| v.as_str().map(str::to_string))
                })
                .unwrap_or_else(|| format!("HTTP {status}"));
            return Err(HarmonyError::unauthorized(message));
        }

        let gotrue: GoTrueSession = response.json().await.map_err(HarmonyError::from)?;
        let auth_user_id = gotrue.user.id;

        // The profile row is keyed by `auth_user_id`; the username it carries is
        // the display handle everywhere else in the app.
        let profiles = self
            .rest_get(&format!(
                "profiles?auth_user_id=eq.{auth_user_id}&select=id,username,display_name,avatar_url,is_admin&limit=1"
            ))
            .await
            .unwrap_or_default();
        let profile: ProfileRow = profiles
            .first()
            .and_then(|row| serde_json::from_value(row.clone()).ok())
            .unwrap_or_default();
        let profile_id = profile.id_uuid().unwrap_or(auth_user_id);
        let username = profile
            .username
            .clone()
            .unwrap_or_else(|| format!("user-{}", &auth_user_id.to_string()[..8]));

        let session = WebSession {
            instance: self.instance.clone(),
            supabase_url: Url::parse(&info.supabase_url)
                .map_err(|e| HarmonyError::protocol(format!("bad Supabase URL: {e}")))?,
            anon_key: info.supabase_anon_key.clone(),
            access_token: gotrue.access_token,
            refresh_token: gotrue.refresh_token,
            auth_user_id,
            profile_id,
            username,
        };
        *self.session.lock().expect("session lock") = Some(session.clone());
        self.profiles
            .lock()
            .expect("profiles lock")
            .insert(profile_id, profile);
        Ok(session)
    }

    /// Turn a stored `avatar_url` (an object path or a URL) into something
    /// downloadable, the way the web app resolves it.
    fn resolve_media(&self, info: &InstanceInfo, raw: &str, bucket: &str) -> Option<String> {
        if raw.is_empty() {
            return None;
        }
        if raw.starts_with("http://") || raw.starts_with("https://") {
            return Some(raw.to_string());
        }
        if raw.starts_with('/') {
            return Some(format!(
                "https://{}{raw}",
                info.domain
                    .clone()
                    .unwrap_or_else(|| self.instance.clone())
            ));
        }
        if !looks_like_object_path(raw) {
            return None;
        }
        Some(format!(
            "{}/storage/v1/render/image/public/{bucket}/{}?width=96&height=96&resize=cover&quality=80",
            info.supabase_url.trim_end_matches('/'),
            raw
        ))
    }

    async fn resolve_profiles(&self, ids: &[Uuid]) -> HashMap<Uuid, ProfileRow> {
        let mut found = HashMap::new();
        let mut missing = Vec::new();
        {
            let cache = self.profiles.lock().expect("profiles lock");
            for id in ids {
                match cache.get(id) {
                    Some(row) => {
                        found.insert(*id, row.clone());
                    }
                    None => missing.push(*id),
                }
            }
        }
        if missing.is_empty() {
            return found;
        }
        let list = missing
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let query = format!(
            "profiles?or=(id.in.({list}),auth_user_id.in.({list}))&select=id,auth_user_id,username,display_name,avatar_url,is_admin"
        );
        if let Ok(rows) = self.rest_get(&query).await {
            let mut cache = self.profiles.lock().expect("profiles lock");
            for row in rows {
                if let Ok(profile) = serde_json::from_value::<ProfileRow>(row) {
                    if let Some(id) = profile.id_uuid() {
                        cache.insert(id, profile.clone());
                        found.insert(id, profile.clone());
                        if let Some(auth_id) = profile
                            .auth_user_id
                            .as_deref()
                            .and_then(|value| Uuid::parse_str(value).ok())
                        {
                            cache.entry(auth_id).or_insert(profile);
                        }
                    }
                }
            }
        }
        found
    }

    fn author_from_profile(&self, info: &InstanceInfo, profile: &ProfileRow, fallback_id: Uuid) -> AuthorInfo {
        AuthorInfo {
            id: profile.id_uuid().or(Some(fallback_id)),
            raw_id: profile
                .id
                .clone()
                .unwrap_or_else(|| fallback_id.to_string()),
            username: profile
                .username
                .clone()
                .unwrap_or_else(|| format!("user-{}", &fallback_id.to_string()[..8])),
            display_name: profile.display_name.clone(),
            avatar_url: profile
                .avatar_url
                .as_deref()
                .and_then(|raw| self.resolve_media(info, raw, "avatars")),
            nickname: None,
            bot: false,
        }
    }
}

/// Whether a stored media value looks like a Supabase object path rather than
/// arbitrary text.
fn looks_like_object_path(raw: &str) -> bool {
    let mut parts = raw.splitn(2, '/');
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("");
    !first.is_empty()
        && !rest.is_empty()
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && rest
            .rsplit('.')
            .next()
            .map(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif"
                )
            })
            .unwrap_or(false)
}

/// PostgREST errors are `{message, details, hint, code}` instead of `{error}`.
fn postgrest_error(status: u16, body: &str) -> HarmonyError {
    let parsed: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = parsed
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| parsed.get("error_description").and_then(|v| v.as_str()))
        .or_else(|| parsed.get("error").and_then(|v| v.as_str()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            if body.trim().is_empty() {
                format!("HTTP {status}")
            } else {
                body.chars().take(200).collect()
            }
        });
    let code = parsed
        .get("code")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let message = match code {
        Some(code) => format!("{message} ({code})"),
        None => message,
    };
    match status {
        401 | 403 => HarmonyError::Forbidden { message },
        404 => HarmonyError::NotFound { message },
        409 => HarmonyError::Conflict { message },
        400 | 422 => HarmonyError::BadRequest { message },
        429 => HarmonyError::RateLimited {
            message,
            retry_after: None,
        },
        _ => HarmonyError::Server { status, message },
    }
}

/// Flatten a `messages.content` jsonb value into text plus attachments, the way
/// the app renders it.
pub fn render_content(content: &Value) -> (String, Vec<AttachmentInfo>) {
    let mut text = String::new();
    let mut attachments = Vec::new();

    let mut render_part = |part: &Value, text: &mut String| match part.get("type").and_then(|v| v.as_str()) {
        Some("text") => {
            if let Some(value) = part.get("text").and_then(|v| v.as_str()) {
                text.push_str(value);
            }
        }
        Some("mention") => {
            let name = part
                .get("username")
                .or_else(|| part.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("someone");
            let domain = part.get("domain").and_then(|v| v.as_str());
            match domain {
                Some(domain) => text.push_str(&format!("@{name}@{domain}")),
                None => text.push_str(&format!("@{name}")),
            }
        }
        Some("channel_mention") => {
            let name = part.get("name").and_then(|v| v.as_str()).unwrap_or("channel");
            text.push_str(&format!("#{name}"));
        }
        Some("role_mention") => {
            let name = part
                .get("roleName")
                .and_then(|v| v.as_str())
                .unwrap_or("role");
            text.push_str(&format!("@{name}"));
        }
        Some("emoji") => {
            if let Some(name) = part
                .get("emoji")
                .and_then(|e| e.get("name"))
                .and_then(|v| v.as_str())
            {
                text.push_str(&format!(":{name}:"));
            }
        }
        Some("url") => {
            let url = part.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let label = part.get("label").and_then(|v| v.as_str());
            text.push_str(label.unwrap_or(url));
        }
        Some("file") => {
            let url = part.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let filename = part
                .get("fileName")
                .and_then(|v| v.as_str())
                .unwrap_or("attachment")
                .to_string();
            if !url.is_empty() {
                attachments.push(AttachmentInfo {
                    id: part
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or(url)
                        .to_string(),
                    url: url.to_string(),
                    filename,
                    size: part
                        .get("fileSize")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    content_type: part
                        .get("mime_type")
                        .or_else(|| part.get("mimeType"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                });
            }
        }
        Some(other) => {
            tracing::debug!("harmony: web message part {other} is not rendered");
        }
        None => {
            if let Some(value) = part.as_str() {
                text.push_str(value);
            }
        }
    };

    match content {
        Value::String(value) => text.push_str(value),
        Value::Array(parts) => {
            for part in parts {
                render_part(part, &mut text);
            }
        }
        Value::Object(map) => {
            if let Some(value) = map.get("text").and_then(|v| v.as_str()) {
                text.push_str(value);
            } else if !map.is_empty() {
                render_part(content, &mut text);
            }
        }
        _ => {}
    }

    (text.trim().to_string(), attachments)
}

#[async_trait]
impl Transport for WebTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Web
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::web()
    }

    fn instance(&self) -> &str {
        &self.instance
    }

    async fn sign_in(&self) -> Result<Account, HarmonyError> {
        let session = self.authenticate().await?;
        let info = self.discover().await?;
        let profile = self
            .profiles
            .lock()
            .expect("profiles lock")
            .get(&session.profile_id)
            .cloned()
            .unwrap_or_default();
        // No realtime socket yet: say so once, plainly.
        let _ = self.events_tx.send(TransportEvent::Notice(
            "Web transport: live updates need Harmony Realtime, which this build does not speak yet; \
             use Refresh (F5) to re-read. The bot-gateway transport has live events."
                .to_string(),
        ));
        let _ = self.events_tx.send(TransportEvent::Connected);
        Ok(Account {
            id: session.profile_id,
            username: profile.username.clone().unwrap_or(session.username),
            display_name: profile.display_name,
            avatar_url: profile
                .avatar_url
                .as_deref()
                .and_then(|raw| self.resolve_media(&info, raw, "avatars")),
            bot: false,
        })
    }

    async fn servers(&self) -> Result<Vec<ServerInfo>, HarmonyError> {
        let session = self.session()?;
        let query = format!(
            "user_servers?user_id=eq.{}&status=eq.accepted&select=position,server:servers(id,name,icon,member_count,owner,description)&order=position.asc",
            session.profile_id
        );
        let rows = self.rest_get(&query).await?;
        let mut seen = std::collections::HashSet::new();
        let mut servers = Vec::new();
        for row in rows {
            let Some(server) = row.get("server") else {
                continue;
            };
            if server.is_null() {
                continue;
            }
            let Some(id) = server
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
            else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            let info = self.discover().await?;
            servers.push(ServerInfo {
                id,
                name: server
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("server")
                    .to_string(),
                description: server
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                icon_url: server
                    .get("icon")
                    .and_then(|v| v.as_str())
                    .and_then(|raw| self.resolve_media(&info, raw, "server_icons")),
                owner_id: server
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .and_then(|v| Uuid::parse_str(v).ok()),
                member_count: server.get("member_count").and_then(|v| v.as_u64()),
            });
        }
        Ok(servers)
    }

    async fn channels(&self, server_id: Uuid) -> Result<Vec<ChannelInfo>, HarmonyError> {
        let channels = self
            .rest_get(&format!(
                "channels?server_id=eq.{server_id}&type=neq.2&select=id,name,type,category,\"order\"&order=order.asc"
            ))
            .await?;
        let categories = self
            .rest_get(&format!(
                "channel_categories?server_id=eq.{server_id}&select=id,name,\"order\"&order=order.asc"
            ))
            .await
            .unwrap_or_default();

        let category_names: HashMap<String, String> = categories
            .iter()
            .filter_map(|row| {
                let id = row.get("id")?.as_str()?.to_string();
                let name = row
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Channels")
                    .to_string();
                Some((id, name))
            })
            .collect();

        let mut mapped = Vec::with_capacity(channels.len());
        for row in channels {
            let Some(id) = row
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
            else {
                continue;
            };
            let type_value = row.get("type").cloned().unwrap_or(Value::Null);
            let kind = match type_value {
                Value::String(text) if text.to_ascii_lowercase().contains("voice") => ChannelKind::Voice,
                Value::Number(number) if number.as_i64() == Some(1) => ChannelKind::Voice,
                Value::Number(number) if number.as_i64() == Some(2) => ChannelKind::Category,
                _ => ChannelKind::Text,
            };
            let category = row
                .get("category")
                .and_then(|v| v.as_str())
                .and_then(|id| category_names.get(id).cloned());
            mapped.push(ChannelInfo {
                id,
                server_id,
                name: row
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("channel")
                    .to_string(),
                description: None,
                kind,
                position: row.get("order").and_then(|v| v.as_i64()).unwrap_or(0),
                category,
            });
        }
        Ok(mapped)
    }

    async fn members(&self, server_id: Uuid) -> Result<Vec<MemberInfo>, HarmonyError> {
        let rows = self
            .rest_get(&format!(
                "user_servers?server_id=eq.{server_id}&status=eq.accepted&select=created_at,user:profiles(id,username,display_name,avatar_url,is_admin)&order=created_at.asc&limit=500"
            ))
            .await?;
        let mut members = Vec::new();
        for row in rows {
            let Some(user) = row.get("user").filter(|value| !value.is_null()) else {
                continue;
            };
            let Ok(profile) = serde_json::from_value::<ProfileRow>(user.clone()) else {
                continue;
            };
            let Some(id) = profile.id_uuid() else {
                continue;
            };
            members.push(MemberInfo {
                id,
                username: profile
                    .username
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                display_name: profile.display_name.clone(),
                avatar_url: None,
                nickname: None,
                roles: Vec::new(),
                is_admin: profile.is_admin,
                bot: false,
            });
        }
        Ok(members)
    }

    async fn messages(
        &self,
        channel_id: Uuid,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<MessageInfo>, HarmonyError> {
        let info = self.discover().await?;
        let limit = limit.clamp(1, 200);
        let mut query = format!(
            "messages?channel_id=eq.{channel_id}&thread_id=is.null&is_deleted=eq.false&order=created_at.desc&limit={limit}&select=id,created_at,updated_at,content,user_id,is_system,metadata,reply_to"
        );
        if let Some(before) = before {
            query.push_str(&format!(
                "&created_at=lt.{}",
                super::model::epoch_to_iso8601(before)
            ));
        }
        let rows = self.rest_get(&query).await?;

        let author_ids: Vec<Uuid> = rows
            .iter()
            .filter_map(|row| row.get("user_id").and_then(|v| v.as_str()))
            .filter_map(|value| Uuid::parse_str(value).ok())
            .collect();
        let profiles = self.resolve_profiles(&author_ids).await;

        let mut messages = Vec::with_capacity(rows.len());
        for row in rows {
            let Some(id) = row
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
            else {
                continue;
            };
            let author_id = row
                .get("user_id")
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok());
            let author = author_id.map(|id| match profiles.get(&id) {
                Some(profile) => self.author_from_profile(&info, profile, id),
                None => AuthorInfo {
                    id: Some(id),
                    raw_id: id.to_string(),
                    username: format!("user-{}", &id.to_string()[..8]),
                    display_name: None,
                    avatar_url: None,
                    nickname: None,
                    bot: false,
                },
            });
            let (text, attachments) = render_content(
                row.get("content").unwrap_or(&Value::Null),
            );
            let created = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
            let updated = row.get("updated_at").and_then(|v| v.as_str()).unwrap_or("");
            let created_at = parse_timestamp(created);
            let edited = parse_timestamp(updated);
            messages.push(MessageInfo {
                id,
                channel_id,
                thread_id: None,
                author,
                content: text,
                timestamp: created_at,
                edited: if edited > created_at { Some(edited) } else { None },
                reply_to: row
                    .get("reply_to")
                    .and_then(|v| v.as_str())
                    .and_then(|v| Uuid::parse_str(v).ok()),
                attachments,
                reactions: Vec::<ReactionInfo>::new(),
                system: row.get("is_system").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
        messages.sort_by_key(|message| message.timestamp);
        Ok(messages)
    }

    async fn send_message(
        &self,
        channel_id: Uuid,
        content: &str,
        reply_to: Option<Uuid>,
    ) -> Result<MessageInfo, HarmonyError> {
        let session = self.session()?;
        let mut body = json!({
            "channel_id": channel_id,
            "user_id": session.profile_id,
            // The table enforces an array of parts (`messages_content_is_array`).
            "content": [{ "type": "text", "text": content }],
        });
        if let Some(reply_to) = reply_to {
            body["reply_to"] = json!(reply_to);
        }
        self.rest_write(reqwest::Method::POST, "messages", &body).await?;
        Ok(MessageInfo {
            id: Uuid::new_v4(),
            channel_id,
            thread_id: None,
            author: Some(AuthorInfo {
                id: Some(session.profile_id),
                raw_id: session.profile_id.to_string(),
                username: session.username.clone(),
                display_name: None,
                avatar_url: None,
                nickname: None,
                bot: false,
            }),
            content: content.to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            edited: None,
            reply_to,
            attachments: Vec::new(),
            reactions: Vec::new(),
            system: false,
        })
    }

    async fn edit_message(
        &self,
        message_id: Uuid,
        content: &str,
    ) -> Result<MessageInfo, HarmonyError> {
        let body = json!({ "content": [{ "type": "text", "text": content }] });
        self.rest_write(
            reqwest::Method::PATCH,
            &format!("messages?id=eq.{message_id}"),
            &body,
        )
        .await?;
        Err(HarmonyError::Unsupported(
            "the web transport cannot read the edited message back yet".to_string(),
        ))
    }

    async fn delete_message(&self, message_id: Uuid) -> Result<(), HarmonyError> {
        let body = json!({ "is_deleted": true });
        self.rest_write(
            reqwest::Method::PATCH,
            &format!("messages?id=eq.{message_id}"),
            &body,
        )
        .await
    }

    async fn profile(&self, user_id: Uuid) -> Result<ProfileInfo, HarmonyError> {
        let info = self.discover().await?;
        let mut found = self.resolve_profiles(&[user_id]).await;
        let row = found.remove(&user_id).unwrap_or_default();
        Ok(ProfileInfo {
            id: row.id_uuid().unwrap_or(user_id),
            username: row.username.clone().unwrap_or_else(|| "unknown".to_string()),
            display_name: row.display_name.clone(),
            bio: row.bio.clone(),
            avatar_url: row
                .avatar_url
                .as_deref()
                .and_then(|raw| self.resolve_media(&info, raw, "avatars")),
            bot: false,
        })
    }

    async fn fetch_image(&self, url: &str) -> Result<Option<String>, HarmonyError> {
        use base64::Engine;
        let info = self.discover().await?;
        let response = self
            .http
            .get(url)
            .header("apikey", &info.supabase_anon_key)
            .send()
            .await
            .map_err(HarmonyError::from)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let bytes = response.bytes().await.map_err(HarmonyError::from)?;
        if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
            return Ok(None);
        }
        let mime = super::bot::image_mime(&bytes);
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(Some(format!("data:{mime};base64,{encoded}")))
    }

    fn take_events(&self) -> Option<mpsc::UnboundedReceiver<TransportEvent>> {
        self.events_rx.lock().expect("events lock").take()
    }

    async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        *self.session.lock().expect("session lock") = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_parts_the_app_writes() {
        let content = json!([
            { "type": "mention", "userId": "8a1f3b77-0000-0000-0000-000000000006", "username": "alice", "domain": "harmony.example.com", "isLocal": true },
            { "type": "text", "text": " the deploy finished: " },
            { "type": "url", "url": "https://ci.example.com/runs/412", "preview": true },
            { "type": "emoji", "emoji": { "id": null, "name": "party", "url": "https://x/y.png" } }
        ]);
        let (text, attachments) = render_content(&content);
        assert_eq!(
            text,
            "@alice@harmony.example.com the deploy finished: https://ci.example.com/runs/412:party:"
        );
        assert!(attachments.is_empty());
    }

    #[test]
    fn renders_files_as_attachments() {
        let content = json!([
            { "type": "text", "text": "look" },
            { "type": "file", "url": "https://cdn/x.png", "fileName": "x.png", "fileSize": 12, "fileType": "image", "mime_type": "image/png" }
        ]);
        let (text, attachments) = render_content(&content);
        assert_eq!(text, "look");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "x.png");
        assert_eq!(attachments[0].content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn renders_legacy_string_and_object_content() {
        assert_eq!(render_content(&json!("plain")).0, "plain");
        assert_eq!(render_content(&json!({ "text": "object" })).0, "object");
        assert_eq!(render_content(&Value::Null).0, "");
    }

    #[test]
    fn object_paths_are_distinguished_from_text() {
        assert!(looks_like_object_path("8a1f3b77/avatar-abc.png"));
        assert!(!looks_like_object_path("hello"));
        assert!(!looks_like_object_path("just/a/word"));
        assert!(!looks_like_object_path("/absolute.png"));
    }

    #[test]
    fn postgrest_errors_keep_their_message() {
        let err = postgrest_error(409, r#"{"message":"duplicate key","code":"23505"}"#);
        assert!(matches!(err, HarmonyError::Conflict { .. }));
        assert!(err.to_string().contains("23505"));
    }
}
