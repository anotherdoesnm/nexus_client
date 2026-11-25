//! The seam between the TUI and Harmony: one trait both transports implement,
//! and the neutral shapes they exchange. The bot gateway and the web app's
//! Supabase protocol disagree on almost every field name, so nothing above this
//! module ever sees a Harmony wire type.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::{Capabilities, ChannelKind, TransportKind};

use super::error::HarmonyError;

/// The signed-in account, whichever protocol authenticated it.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bot: bool,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub owner_id: Option<Uuid>,
    pub member_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub kind: ChannelKind,
    pub position: i64,
    pub category: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub nickname: Option<String>,
    pub roles: Vec<Uuid>,
    /// True when one of the member's roles holds the administrator bit.
    pub is_admin: bool,
    pub bot: bool,
}

impl MemberInfo {
    /// The name to show: server nickname, else display name, else username.
    pub fn shown_name(&self) -> String {
        self.nickname
            .clone()
            .or_else(|| self.display_name.clone())
            .unwrap_or_else(|| self.username.clone())
    }
}

#[derive(Debug, Clone)]
pub struct AuthorInfo {
    /// `Some` only when the author is a profile this instance knows; relayed
    /// Discord users carry a snowflake instead.
    pub id: Option<Uuid>,
    pub raw_id: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub nickname: Option<String>,
    pub bot: bool,
}

#[derive(Debug, Clone)]
pub struct AttachmentInfo {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub size: u64,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ReactionInfo {
    pub emoji: String,
    pub by_me: bool,
}

#[derive(Debug, Clone)]
pub struct MessageInfo {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub thread_id: Option<Uuid>,
    pub author: Option<AuthorInfo>,
    pub content: String,
    pub timestamp: i64,
    pub edited: Option<i64>,
    pub reply_to: Option<Uuid>,
    pub attachments: Vec<AttachmentInfo>,
    pub reactions: Vec<ReactionInfo>,
    pub system: bool,
}

#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub id: Uuid,
    pub username: String,
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub bot: bool,
}

/// What a transport pushes at the session.
#[derive(Debug, Clone)]
pub enum TransportEvent {
    /// The socket is up and identified (bot) or the session is signed in (web).
    Connected,
    MessageCreate(MessageInfo),
    MessageUpdate(MessageInfo),
    MessageDelete {
        channel_id: Uuid,
        message_id: Uuid,
    },
    ReactionAdd {
        channel_id: Uuid,
        message_id: Uuid,
        emoji: String,
        by_me: bool,
    },
    ReactionRemove {
        channel_id: Uuid,
        message_id: Uuid,
        emoji: String,
        by_me: bool,
    },
    MemberJoin {
        server_id: Uuid,
        user: MemberInfo,
    },
    MemberLeave {
        server_id: Uuid,
        user_id: Uuid,
    },
    /// The connection dropped; `will_reconnect` says whether frames will resume.
    Disconnected {
        reason: String,
        will_reconnect: bool,
    },
    /// Anything worth a line in the notification bar.
    Notice(String),
}

/// One Harmony instance, spoken to over one protocol.
///
/// Methods take `&self` so the session can hold the transport behind an
/// `Arc` and call it from more than one task; per-transport mutable state
/// (token refresh, cursors) lives in interior mutability.
#[async_trait]
pub trait Transport: Send + Sync {
    fn kind(&self) -> TransportKind;

    fn capabilities(&self) -> Capabilities;

    /// The instance as the user typed it, for display.
    fn instance(&self) -> &str;

    /// Finish authenticating and return the account behind the credentials.
    async fn sign_in(&self) -> Result<Account, HarmonyError>;

    /// Servers this session can see. The bot protocol cannot enumerate them,
    /// so a bot transport answers with the servers it was configured with and
    /// whatever it could read from them.
    async fn servers(&self) -> Result<Vec<ServerInfo>, HarmonyError>;

    async fn channels(&self, server_id: Uuid) -> Result<Vec<ChannelInfo>, HarmonyError>;

    async fn members(&self, server_id: Uuid) -> Result<Vec<MemberInfo>, HarmonyError>;

    /// Newest-first page of messages; `before` is an epoch second.
    async fn messages(
        &self,
        channel_id: Uuid,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<MessageInfo>, HarmonyError>;

    async fn send_message(
        &self,
        channel_id: Uuid,
        content: &str,
        reply_to: Option<Uuid>,
    ) -> Result<MessageInfo, HarmonyError>;

    async fn edit_message(
        &self,
        message_id: Uuid,
        content: &str,
    ) -> Result<MessageInfo, HarmonyError>;

    async fn delete_message(&self, message_id: Uuid) -> Result<(), HarmonyError>;

    async fn profile(&self, user_id: Uuid) -> Result<ProfileInfo, HarmonyError>;

    /// Fetch an avatar or icon and return it as a `data:` URL the image cache
    /// can decode. `Ok(None)` when the resource is gone.
    async fn fetch_image(&self, url: &str) -> Result<Option<String>, HarmonyError>;

    /// Take the event stream. Called once, by the session, when it starts.
    fn take_events(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<TransportEvent>>;

    /// Stop background work (gateway loop, realtime socket, refresh timer).
    async fn shutdown(&self);
}

/// Which servers a bot session should open, given its configuration. Kept here
/// so both the session and the tests use one parser.
pub fn parse_server_ids(raw: &str) -> Vec<Uuid> {
    raw.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| Uuid::parse_str(part.trim()).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_id_lists_tolerate_noise() {
        let raw = "1e7d0000-0000-0000-0000-000000000004, c2d4f8a0-0000-0000-0000-000000000005;\n nope";
        let ids = parse_server_ids(raw);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].to_string(), "1e7d0000-0000-0000-0000-000000000004");
        assert!(parse_server_ids("").is_empty());
    }
}
