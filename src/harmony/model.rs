//! Harmony's wire types, exactly as the API spells them.
//!
//! Everything here is `serde`-shaped to the documented JSON: the REST routes of
//! `bot-gateway/api/v1` (`docs/build/rest.md`) and the gateway's event payloads
//! (`docs/build/gateway.md`). Conversions to the client's own vocabulary live
//! in `super::session`, so a field rename in Harmony only touches this file.

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

/// A profile as the bot routes return it.
#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub is_local: Option<bool>,
}

/// The author of a message. `id` may be a Discord snowflake for relayed users,
/// so it is kept as a string: only UUIDs can be looked up in the member list.
#[derive(Debug, Clone, Deserialize)]
pub struct Author {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub bot: bool,
}

/// `GET /users/@me`: the bot the token belongs to.
#[derive(Debug, Clone, Deserialize)]
pub struct BotIdentity {
    pub id: Uuid,
    pub username: String,
    #[serde(default)]
    pub discriminator: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: bool,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub public: bool,
}

/// A member row. `user.id` is a profile UUID.
#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    pub user: MemberUser,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub roles: Vec<Uuid>,
    #[serde(default)]
    pub joined_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberUser {
    pub id: Uuid,
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub is_local: Option<bool>,
}

// ---------------------------------------------------------------------------
// Message parts
// ---------------------------------------------------------------------------

/// One part of a message's stored content (`content_raw`). Unknown part types
/// are kept so that a newer instance cannot break the client.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum MessagePart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "url")]
    Url {
        url: String,
        #[serde(default)]
        label: Option<String>,
    },
    #[serde(rename = "mention")]
    Mention {
        #[serde(rename = "userId", default)]
        user_id: Option<String>,
        #[serde(default)]
        username: Option<String>,
    },
    #[serde(rename = "role_mention")]
    RoleMention {
        #[serde(rename = "roleId", default)]
        role_id: Option<String>,
        #[serde(rename = "roleName", default)]
        role_name: Option<String>,
    },
    #[serde(rename = "channel_mention")]
    ChannelMention {
        #[serde(rename = "channelId", default)]
        channel_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "emoji")]
    Emoji {
        #[serde(default)]
        emoji: Option<EmojiRef>,
    },
    #[serde(rename = "file")]
    File {
        #[serde(default)]
        url: Option<String>,
        #[serde(rename = "fileType", default)]
        file_type: Option<String>,
        #[serde(rename = "fileName", default)]
        file_name: Option<String>,
        #[serde(rename = "fileSize", default)]
        file_size: Option<u64>,
        #[serde(rename = "mimeType", default)]
        mime_type: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "embed")]
    Embed {
        #[serde(default)]
        url: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EmojiRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub animated: bool,
}

// ---------------------------------------------------------------------------
// Messages and threads
// ---------------------------------------------------------------------------

/// The message object of the REST routes (`docs/build/rest.md#message-object`).
#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub channel_id: Uuid,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub is_system: bool,
    #[serde(default)]
    pub metadata: Value,
}

/// The gateway's message event object: the REST one plus `server_id`,
/// `content_raw` and `mention_everyone`.
#[derive(Debug, Clone, Deserialize)]
pub struct GatewayMessage {
    pub id: Uuid,
    #[serde(default)]
    pub server_id: Option<Uuid>,
    pub channel_id: Uuid,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub content_raw: Vec<MessagePart>,
    #[serde(default)]
    pub is_system: bool,
    #[serde(default)]
    pub reply_to: Option<Uuid>,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub edited_timestamp: Option<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub mention_everyone: bool,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    pub channel_id: Uuid,
    #[serde(default)]
    pub parent_message_id: Option<Uuid>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Servers, channels, roles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub owner_id: Option<Uuid>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub member_count: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    pub id: Uuid,
    /// Discord's numbering: `0` text, `2` voice, `4` category.
    #[serde(rename = "type", default)]
    pub kind: i64,
    #[serde(default)]
    pub guild_id: Option<Uuid>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub category_id: Option<Uuid>,
}

impl Channel {
    pub fn kind_number(&self) -> i64 {
        self.kind
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub order: i64,
    #[serde(default)]
    pub federation_status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub permissions: Option<Value>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub mentionable: bool,
    #[serde(default)]
    pub hoist: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Emoji {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub server_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvitePreview {
    pub code: String,
    #[serde(default)]
    pub invite_url: Option<String>,
    pub server_name: String,
    #[serde(default)]
    pub server_description: Option<String>,
    #[serde(default)]
    pub server_icon_url: Option<String>,
    #[serde(default)]
    pub member_count: u64,
}

/// The body of `POST /channels/{id}/messages`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CreateMessage {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Gateway frames and events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayFrame {
    pub op: i64,
    #[serde(default)]
    pub t: Option<String>,
    #[serde(default)]
    pub d: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadyPayload {
    pub bot: ReadyBot,
    pub session_id: String,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_interval: u64,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl ReadyPayload {
    pub fn heartbeat(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.heartbeat_interval.max(1000))
    }
}

fn default_heartbeat() -> u64 {
    30_000
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReadyBot {
    pub id: Uuid,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageDeleted {
    pub id: Uuid,
    #[serde(default)]
    pub server_id: Option<Uuid>,
    pub channel_id: Uuid,
    #[serde(default)]
    pub thread_id: Option<Uuid>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReactionEvent {
    #[serde(default)]
    pub reaction_id: Option<Uuid>,
    pub message_id: Uuid,
    #[serde(default)]
    pub server_id: Option<Uuid>,
    pub channel_id: Uuid,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub bot_id: Option<Uuid>,
    #[serde(default)]
    pub emoji: Option<EmojiRef>,
    #[serde(default)]
    pub metadata: Value,
}

impl ReactionEvent {
    /// `:name:` for a Unicode or custom emoji, or a fallback marker.
    pub fn emoji_text(&self) -> String {
        match &self.emoji {
            Some(emoji) => match (&emoji.name, &emoji.id) {
                (Some(name), Some(_)) => format!(":{name}:"),
                (Some(name), None) => name.clone(),
                (None, _) => "?".to_string(),
            },
            None => "?".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberEvent {
    pub server_id: Uuid,
    #[serde(default)]
    pub user: Option<MemberEventUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberEventUser {
    pub id: Uuid,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Harmony writes `2026-10-13T12:00:00.000000+00:00` in events and
/// `2026-10-13 12:00:00+00` in some rows. Parse both, plus plain RFC 3339,
/// into epoch seconds; unparseable input becomes `0` rather than failing a
/// whole list.
pub fn parse_timestamp(raw: &str) -> i64 {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return 0;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return dt.timestamp();
    }

    // SQL-style offsets turn up as `+00`, `+0000` or `+00:00` depending on the
    // column and the driver; normalise to the `+00:00` chrono parses.
    let normalised = normalise_offset(trimmed);
    for format in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
    ] {
        if let Ok(dt) = chrono::DateTime::parse_from_str(&normalised, format) {
            return dt.timestamp();
        }
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(&normalised, format) {
            return dt.and_utc().timestamp();
        }
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(&normalised, "%Y-%m-%d") {
        return date.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc().timestamp()).unwrap_or(0);
    }
    0
}

/// `2026-10-13 12:00:00+00` → `…+00:00`, `…+0000` → `…+00:00`.
fn normalise_offset(value: &str) -> String {
    let bytes = value.as_bytes();
    let len = bytes.len();
    // +HHMM
    if len >= 5 {
        let sign = bytes[len - 5];
        if (sign == b'+' || sign == b'-')
            && bytes[len - 4..].iter().all(u8::is_ascii_digit)
        {
            let mut out = String::with_capacity(len + 1);
            out.push_str(&value[..len - 5]);
            out.push(sign as char);
            out.push_str(&value[len - 4..len - 2]);
            out.push(':');
            out.push_str(&value[len - 2..]);
            return out;
        }
    }
    // +HH
    if len >= 3 {
        let sign = bytes[len - 3];
        if (sign == b'+' || sign == b'-') && bytes[len - 2..].iter().all(u8::is_ascii_digit) {
            let mut out = String::with_capacity(len + 3);
            out.push_str(&value[..len - 3]);
            out.push(sign as char);
            out.push_str(&value[len - 2..]);
            out.push_str(":00");
            return out;
        }
    }
    value.to_string()
}

/// The gateway's `before`/`after` query values are ISO 8601 strings, so an
/// epoch second the client holds has to go back to that form.
pub fn epoch_to_iso8601(epoch_secs: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(epoch_secs, 0)
        .unwrap_or_else(|| chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_timestamp_shapes_harmony_writes() {
        assert_eq!(parse_timestamp("2026-10-13T12:00:00.000000+00:00"), 1_791_892_800);
        assert_eq!(parse_timestamp("2026-10-13T12:00:00Z"), 1_791_892_800);
        assert_eq!(parse_timestamp("2026-10-13 12:00:00+00"), 1_791_892_800);
        assert_eq!(parse_timestamp("2026-10-13 12:00:00"), 1_791_892_800);
        assert_eq!(parse_timestamp(""), 0);
        assert_eq!(parse_timestamp("not a date"), 0);
    }

    #[test]
    fn epoch_round_trips_through_iso8601() {
        let iso = epoch_to_iso8601(1_791_892_800);
        assert_eq!(parse_timestamp(&iso), 1_791_892_800);
    }

    #[test]
    fn message_parts_keep_unknown_kinds() {
        let parts: Vec<MessagePart> = serde_json::from_str(
            r#"[{"type":"text","text":"hi"},{"type":"future_thing","x":1}]"#,
        )
        .expect("parts parse");
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], MessagePart::Text { .. }));
        assert!(matches!(parts[1], MessagePart::Other));
    }

    #[test]
    fn reaction_emoji_text_covers_unicode_and_custom() {
        let unicode: ReactionEvent = serde_json::from_str(
            r#"{"message_id":"0b9e5a61-0000-0000-0000-000000000001","channel_id":"c2d4f8a0-0000-0000-0000-000000000002","emoji":{"id":null,"name":"👍","url":null,"animated":false}}"#,
        )
        .unwrap();
        assert_eq!(unicode.emoji_text(), "👍");

        let custom: ReactionEvent = serde_json::from_str(
            r#"{"message_id":"0b9e5a61-0000-0000-0000-000000000001","channel_id":"c2d4f8a0-0000-0000-0000-000000000002","emoji":{"id":"5e2c0b1d-0000-0000-0000-000000000003","name":"party","url":"https://x/y.png","animated":false}}"#,
        )
        .unwrap();
        assert_eq!(custom.emoji_text(), ":party:");
    }
}
