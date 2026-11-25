//! The client's own vocabulary: the data the UI renders and the commands it
//! issues. Nothing here knows how bytes reach an instance — that lives in
//! [`crate::harmony`], which maps Harmony's JSON onto these types.
//!
//! This module replaces the old `nexus-tui-common` wire model. The names that
//! survived (`Server`, `Channel`, `ChannelMessage`, …) describe UI concepts,
//! and the command/event pairs are the seam between the UI thread and the
//! transport task (see `crate::harmony::session`).

use ratatui::style::Color;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Users
// ---------------------------------------------------------------------------

/// A user colour. Kept as a string so preferences and themes can round-trip it;
/// Harmony has no per-user colour, so the transport derives one from the id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserColor(pub String);

impl UserColor {
    pub fn new(color: impl Into<String>) -> Self {
        Self(color.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A stable colour for an account, so the same person keeps the same colour
    /// across sessions and instances.
    pub fn for_id(id: Uuid) -> Self {
        const PALETTE: [&str; 8] = [
            "Cyan", "Magenta", "Yellow", "Green", "LightBlue", "LightMagenta", "LightYellow",
            "LightGreen",
        ];
        let bytes = id.as_bytes();
        let idx = (bytes[0] as usize ^ bytes[15] as usize) % PALETTE.len();
        Self(PALETTE[idx].to_string())
    }
}

impl From<String> for UserColor {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for UserColor {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<Color> for UserColor {
    fn from(color: Color) -> Self {
        match color {
            Color::Rgb(r, g, b) => Self(format!("#{:02X}{:02X}{:02X}", r, g, b)),
            Color::LightBlue => Self("LightBlue".to_string()),
            Color::LightCyan => Self("LightCyan".to_string()),
            Color::LightGreen => Self("LightGreen".to_string()),
            Color::LightMagenta => Self("LightMagenta".to_string()),
            Color::LightYellow => Self("LightYellow".to_string()),
            Color::LightRed => Self("LightRed".to_string()),
            Color::Red => Self("Red".to_string()),
            Color::Green => Self("Green".to_string()),
            Color::Blue => Self("Blue".to_string()),
            Color::Yellow => Self("Yellow".to_string()),
            Color::Cyan => Self("Cyan".to_string()),
            Color::Magenta => Self("Magenta".to_string()),
            Color::White => Self("White".to_string()),
            Color::Black => Self("Black".to_string()),
            Color::Gray => Self("Gray".to_string()),
            Color::DarkGray => Self("DarkGray".to_string()),
            _ => Self("Cyan".to_string()),
        }
    }
}

impl From<UserColor> for Color {
    fn from(user_color: UserColor) -> Color {
        if user_color.0.starts_with('#') && user_color.0.len() == 7 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&user_color.0[1..3], 16),
                u8::from_str_radix(&user_color.0[3..5], 16),
                u8::from_str_radix(&user_color.0[5..7], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
        }

        match user_color.0.as_str() {
            "LightBlue" => Color::LightBlue,
            "LightCyan" => Color::LightCyan,
            "LightGreen" => Color::LightGreen,
            "LightMagenta" => Color::LightMagenta,
            "LightYellow" => Color::LightYellow,
            "LightRed" => Color::LightRed,
            "Red" => Color::Red,
            "Green" => Color::Green,
            "Blue" => Color::Blue,
            "Yellow" => Color::Yellow,
            "Cyan" => Color::Cyan,
            "Magenta" => Color::Magenta,
            "White" => Color::White,
            "Black" => Color::Black,
            "Gray" => Color::Gray,
            "DarkGray" => Color::DarkGray,
            _ => Color::Cyan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UserRole {
    User,
    Moderator,
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserStatus {
    Connected,
    Away,
    Busy,
    Offline,
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    /// Display name when the instance has one; falls back to `username`.
    pub display_name: Option<String>,
    pub color: UserColor,
    pub role: UserRole,
    /// Base64 data URL, so the TUI's image pipeline can decode it without
    /// network access at render time.
    pub profile_pic: Option<String>,
    pub cover_banner: Option<String>,
    pub status: UserStatus,
    pub bot: bool,
}

impl User {
    /// The name to draw in a message line or a member list.
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }

    pub fn placeholder(id: Uuid) -> Self {
        Self {
            id,
            username: format!("user-{}", &id.to_string()[..8]),
            display_name: None,
            color: UserColor::for_id(id),
            role: UserRole::User,
            profile_pic: None,
            cover_banner: None,
            status: UserStatus::Offline,
            bot: false,
        }
    }
}

/// A user without images, for lists.
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub id: Uuid,
    pub username: String,
    pub color: UserColor,
    pub role: UserRole,
    pub status: UserStatus,
    pub bot: bool,
}

impl From<&User> for UserInfo {
    fn from(user: &User) -> Self {
        Self {
            id: user.id,
            username: user.username.clone(),
            color: user.color.clone(),
            role: user.role,
            status: user.status,
            bot: user.bot,
        }
    }
}

/// A full profile, as the profile screen shows and edits it.
#[derive(Debug, Clone)]
pub struct UserProfile {
    pub id: Uuid,
    pub username: String,
    pub color: UserColor,
    pub role: UserRole,
    pub bio: Option<String>,
    pub url1: Option<String>,
    pub url2: Option<String>,
    pub url3: Option<String>,
    pub location: Option<String>,
    pub profile_pic: Option<String>,
    pub cover_banner: Option<String>,
}

// ---------------------------------------------------------------------------
// Servers and channels
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Text,
    Voice,
    Category,
}

impl ChannelKind {
    pub fn is_text(self) -> bool {
        matches!(self, ChannelKind::Text)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChannelPermissions {
    pub can_read: Vec<Uuid>,
    pub can_write: Vec<Uuid>,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: ChannelKind,
    /// Position inside its category, as the instance orders it.
    pub position: i64,
    /// Parent category name, when the instance groups channels.
    pub category: Option<String>,
    pub permissions: ChannelPermissions,
    pub userlist: Vec<Uuid>,
    pub messages: Vec<ChannelMessage>,
}

#[derive(Debug, Clone)]
pub struct Server {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub public: bool,
    pub invite_code: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    pub owner: Option<Uuid>,
    pub mods: Vec<Uuid>,
    pub userlist: Vec<Uuid>,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Default)]
pub struct Attachment {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub size: u64,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
}

#[derive(Debug, Clone)]
pub struct ChannelMessage {
    pub id: Uuid,
    pub channel_id: Uuid,
    pub thread_id: Option<Uuid>,
    /// The author's id. The UI looks the author up in the member list.
    pub sent_by: Uuid,
    pub timestamp: i64,
    pub content: String,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub reply_to: Option<Uuid>,
    pub edited: bool,
}

#[derive(Debug, Clone)]
pub struct DirectMessage {
    pub id: Uuid,
    pub from: Uuid,
    pub to: Uuid,
    pub timestamp: i64,
    pub content: String,
}

// ---------------------------------------------------------------------------
// Forums (kept for the UI; Harmony has no forums yet — see ROADMAP.md)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Forum {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone)]
pub struct ForumLightweight {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub threads: Vec<ThreadLightweight>,
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: Uuid,
    pub title: String,
    pub author: User,
    pub posts: Vec<Post>,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct ThreadLightweight {
    pub id: Uuid,
    pub title: String,
    pub author: UserInfo,
    pub posts: Vec<PostLightweight>,
    pub timestamp: i64,
}

#[derive(Debug, Clone)]
pub struct Post {
    pub id: Uuid,
    pub author: User,
    pub content: String,
    pub timestamp: i64,
    pub reply_to: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct PostLightweight {
    pub id: Uuid,
    pub author: UserInfo,
    pub content: String,
    pub timestamp: i64,
    pub reply_to: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Notifications and invites
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationType {
    ThreadReply,
    DM,
    Announcement,
    Mention,
    Other(String),
}

#[derive(Debug, Clone)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub notif_type: NotificationType,
    pub related_id: Uuid,
    pub created_at: i64,
    pub read: bool,
    pub extra: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerInviteStatus {
    Pending,
    Accepted,
    Declined,
    Expired,
}

#[derive(Debug, Clone)]
pub struct ServerInvite {
    pub id: Uuid,
    pub from_user: User,
    pub to_user_id: Uuid,
    pub server: Server,
    pub timestamp: i64,
    pub status: ServerInviteStatus,
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationCursor {
    /// Epoch seconds (the transport converts Harmony's ISO timestamps).
    Timestamp(i64),
    Offset(usize),
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationDirection {
    Forward,
    Backward,
}

// ---------------------------------------------------------------------------
// Transport vocabulary
// ---------------------------------------------------------------------------

/// Which Harmony protocol a session speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// `bot-gateway`: `Authorization: Bot <token>` over REST plus the gateway
    /// WebSocket. Documented under `/build/rest` and `/build/gateway`.
    Bot,
    /// The web app's own protocol: instance discovery, GoTrue, PostgREST and
    /// Realtime with a user's JWT.
    Web,
}

impl TransportKind {
    pub fn label(self) -> &'static str {
        match self {
            TransportKind::Bot => "bot-gateway",
            TransportKind::Web => "web-app",
        }
    }
}

/// What the chosen transport can actually do on this instance. The UI reads
/// this to explain missing screens instead of pretending they are empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Can the session enumerate the servers it belongs to? The bot REST API
    /// has no such route, so bot sessions rely on configured server ids.
    pub server_listing: bool,
    pub direct_messages: bool,
    pub forums: bool,
    pub threads: bool,
    pub reactions: bool,
    pub presence: bool,
    pub typing: bool,
    pub voice: bool,
    pub encrypted_channels: bool,
    pub message_edit: bool,
    pub attachments: bool,
}

impl Capabilities {
    /// What the bot-gateway transport actually implements today: servers,
    /// channels, message history and sends, edits and deletes, and gateway
    /// events. No threads UI, no reaction UI, no presence (the protocol has
    /// none), no DMs, no forums.
    pub const fn bot() -> Self {
        Self {
            server_listing: false,
            direct_messages: false,
            forums: false,
            threads: false,
            reactions: false,
            presence: false,
            typing: false,
            voice: false,
            encrypted_channels: false,
            message_edit: true,
            attachments: true,
        }
    }

    /// What the web transport actually implements today: discovery, GoTrue
    /// sign-in, PostgREST reads/writes, message parts with attachments. Live
    /// updates (Realtime), DMs, reactions and E2EE are not wired up yet.
    pub const fn web() -> Self {
        Self {
            server_listing: true,
            direct_messages: true,
            forums: false,
            threads: false,
            reactions: false,
            presence: false,
            typing: false,
            voice: false,
            encrypted_channels: false,
            message_edit: true,
            attachments: true,
        }
    }
}

/// Why a command could not be served. The session turns this into an [`Event`]
/// the UI can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    Forums,
    DirectMessages,
    Register,
    ProfileUpdate,
    ServerInvites,
    Notifications,
    Threads,
    Other(&'static str),
}

impl Unsupported {
    pub fn explain(&self) -> String {
        match self {
            Unsupported::Forums => {
                "Harmony has no forums yet (it is on Harmony's ROADMAP); use servers and channels."
                    .to_string()
            }
            Unsupported::DirectMessages => {
                "Harmony's bot protocol cannot read direct messages; sign in with the web-app transport for DMs."
                    .to_string()
            }
            Unsupported::Register => {
                "Accounts are created by the instance, not by this client; sign in with a bot token or an account."
                    .to_string()
            }
            Unsupported::ProfileUpdate => {
                "This client cannot edit profiles through Harmony's bot protocol yet.".to_string()
            }
            Unsupported::ServerInvites => {
                "Server invites are not part of Harmony's bot protocol; join with an invite link on the web app."
                    .to_string()
            }
            Unsupported::Notifications => {
                "Harmony does not deliver this client's notifications through the bot protocol.".to_string()
            }
            Unsupported::Threads => {
                "Threads are not wired up in this build yet.".to_string()
            }
            Unsupported::Other(what) => format!("{what} is not supported by this Harmony transport."),
        }
    }
}

/// A sign-in request, however the transport authenticates.
#[derive(Debug, Clone)]
pub struct LoginRequest {
    /// Instance host or URL, e.g. `har.mony.lol` or `https://har.mony.lol`.
    pub instance: String,
    pub transport: TransportKind,
    /// Bot token (`harmony_bot_…`) for [`TransportKind::Bot`].
    pub token: Option<String>,
    /// Account credentials for [`TransportKind::Web`].
    pub email: Option<String>,
    pub password: Option<String>,
    /// Bot sessions start from these server ids: the bot API cannot list them.
    pub server_ids: Vec<Uuid>,
}

/// Commands the UI issues. Everything that a transport can serve maps to one or
/// more Harmony requests; everything else answers with [`Event::Unsupported`].
#[derive(Debug, Clone)]
pub enum Command {
    // Session
    Login(LoginRequest),
    Logout,
    /// Re-read the servers and channels this session can see.
    Refresh,

    // Servers and channels
    GetServers,
    GetChannelUserList { channel_id: Uuid },
    GetServerMembers { server_id: Uuid },

    // Messages
    GetChannelMessages { channel_id: Uuid, before: Option<i64> },
    GetChannelMessagesPaginated {
        channel_id: Uuid,
        cursor: PaginationCursor,
        limit: Option<usize>,
        direction: PaginationDirection,
    },
    SendChannelMessage { channel_id: Uuid, content: String },
    EditChannelMessage { channel_id: Uuid, message_id: Uuid, content: String },
    DeleteChannelMessage { channel_id: Uuid, message_id: Uuid },

    // People
    GetProfile { user_id: Uuid },
    GetUserList,
    GetUserAvatars { user_ids: Vec<Uuid> },

    // Notifications (web transport only)
    GetNotifications { before: Option<i64> },
    MarkNotificationRead { notification_id: Uuid },

    // Cache
    InvalidateImageCache { keys: Vec<String> },
    GetCacheStats,

    // --- Not served by either Harmony transport today; kept so the UI compiles
    // --- and so the session can answer with a precise explanation.
    Register { username: String, password: String },
    UpdatePassword(String),
    UpdateColor(UserColor),
    UpdateProfile {
        bio: Option<String>,
        url1: Option<String>,
        url2: Option<String>,
        url3: Option<String>,
        location: Option<String>,
        profile_pic: Option<String>,
        cover_banner: Option<String>,
    },
    GetForums,
    CreateForum { name: String, description: String },
    DeleteForum { forum_id: Uuid },
    CreateThread { forum_id: Uuid, title: String, content: String },
    CreatePost { thread_id: Uuid, content: String },
    CreatePostReply { thread_id: Uuid, content: String, reply_to: Uuid },
    DeletePost(Uuid),
    DeleteThread(Uuid),
    GetDMUserList,
    GetDirectMessages { user_id: Uuid, before: Option<i64> },
    GetDirectMessagesPaginated {
        user_id: Uuid,
        cursor: PaginationCursor,
        limit: Option<usize>,
        direction: PaginationDirection,
    },
    SendDirectMessage { to: Uuid, content: String },
    SendServerInvite { to_user_id: Uuid, server_id: Uuid },
    RespondToServerInvite { invite_id: Uuid, accept: bool },
    AcceptServerInviteFromUser { from_user_id: Uuid },
    DeclineServerInviteFromUser { from_user_id: Uuid },
}

/// Where the session is in its connect/heartbeat lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Online,
    Reconnecting,
}

impl ConnectionState {
    pub fn label(self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "offline",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Online => "online",
            ConnectionState::Reconnecting => "reconnecting",
        }
    }
}

/// Events the session pushes to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    // Session
    AuthSuccess(User),
    AuthFailure(String),
    Ready {
        account: User,
        instance: String,
        transport: TransportKind,
        capabilities: Capabilities,
        /// Servers the session knows about but could not read (bot transport).
        unavailable_servers: Vec<Uuid>,
    },
    Connection(ConnectionState),
    /// A command the transport cannot serve, with a human explanation.
    Unsupported { what: Unsupported, detail: Option<String> },

    // People
    Profile(UserProfile),
    UserUpdated(User),
    UserJoined(User),
    UserLeft(Uuid),
    UserList(Vec<User>),
    ChannelUserList { channel_id: Uuid, users: Vec<User> },
    UserAvatars { avatars: Vec<(Uuid, Option<String>)> },

    // Servers and channels
    Servers(Vec<Server>),

    // Messages
    NewChannelMessage(ChannelMessage),
    ChannelMessages {
        channel_id: Uuid,
        messages: Vec<ChannelMessage>,
        history_complete: bool,
    },
    ChannelMessagesPaginated {
        channel_id: Uuid,
        messages: Vec<ChannelMessage>,
        has_more: bool,
        next_cursor: Option<PaginationCursor>,
        prev_cursor: Option<PaginationCursor>,
        total_count: Option<usize>,
    },
    ChannelMessageUpdated(ChannelMessage),
    ChannelMessageDeleted { channel_id: Uuid, message_id: Uuid },

    // Direct messages (web transport only)
    DMUserList(Vec<User>),
    DirectMessage(DirectMessage),
    DirectMessages {
        user_id: Uuid,
        messages: Vec<DirectMessage>,
        history_complete: bool,
    },
    DirectMessagesPaginated {
        user_id: Uuid,
        messages: Vec<DirectMessage>,
        has_more: bool,
        next_cursor: Option<PaginationCursor>,
        prev_cursor: Option<PaginationCursor>,
        total_count: Option<usize>,
    },

    // Forums (kept for the UI; never emitted by a Harmony transport)
    Forums(Vec<Forum>),
    ForumsLightweight(Vec<ForumLightweight>),
    ForumReplyNotification {
        thread_id: Uuid,
        from_username: String,
        message: String,
        from_user_profile_pic: Option<String>,
    },

    // Notifications
    Notification(String, bool),
    Notifications {
        notifications: Vec<Notification>,
        history_complete: bool,
    },
    MentionNotification {
        from: User,
        content: String,
    },
    ServerInviteReceived(ServerInvite),
    ServerInviteResponse {
        invite_id: Uuid,
        accepted: bool,
        user: User,
    },

    // Diagnostics
    CacheStats {
        total_entries: usize,
        total_size_mb: f64,
        hit_ratio: f64,
        expired_entries: usize,
    },
    ImageCacheInvalidated { keys: Vec<String> },
    PerformanceMetrics {
        query_time_ms: u64,
        cache_hit_rate: f64,
        message_count: usize,
    },
}

impl From<String> for Event {
    fn from(message: String) -> Self {
        Event::Notification(message, false)
    }
}
