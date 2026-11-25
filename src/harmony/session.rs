//! The session: the one place that turns the UI's [`Command`]s into transport
//! calls and transport events into the UI's [`Event`]s.
//!
//! It owns the transport, the member/channel caches and the capability report,
//! and it answers commands the Harmony protocol cannot serve with an
//! [`Event::Unsupported`] that says why.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::mpsc;
use uuid::Uuid;

use crate::domain::{
    Attachment, Capabilities, Channel, ChannelKind, ChannelMessage, Command, ConnectionState,
    DirectMessage, Event, LoginRequest, PaginationCursor, PaginationDirection, Reaction, Server,
    TransportKind, Unsupported, User, UserColor, UserProfile, UserRole, UserStatus,
};

use super::bot::BotTransport;
use super::error::HarmonyError;
use super::transport::{
    Account, ChannelInfo, MemberInfo, MessageInfo, ProfileInfo, ServerInfo, Transport,
    TransportEvent,
};

/// How many messages one history page holds.
const PAGE: u32 = 50;

/// Start a session on the current runtime: read commands from `commands`,
/// report events on `events`. Returns immediately; the work runs in a task.
pub fn spawn(
    commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
    prefill: Option<LoginRequest>,
) {
    tokio::spawn(async move {
        Session::new(commands, events).run(prefill).await;
    });
}

pub struct Session {
    commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<Event>,
    transport: Option<Arc<dyn Transport>>,
    transport_events: Option<mpsc::UnboundedReceiver<TransportEvent>>,
    account: Option<Account>,
    instance: String,
    capabilities: Capabilities,
    caches: Caches,
    /// Messages this session sent, so the gateway's echo is not shown twice.
    sent_messages: HashSet<Uuid>,
    /// Direct conversation of each peer, learned from the DM list or by opening
    /// one on demand.
    dm_conversations: HashMap<Uuid, Uuid>,
}

#[derive(Default)]
struct Caches {
    servers: Vec<Server>,
    channel_server: HashMap<Uuid, Uuid>,
    users: HashMap<Uuid, User>,
}

impl Session {
    pub fn new(
        commands: mpsc::UnboundedReceiver<Command>,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            commands,
            events,
            transport: None,
            transport_events: None,
            account: None,
            instance: String::new(),
            capabilities: Capabilities::bot(),
            caches: Caches::default(),
            sent_messages: HashSet::new(),
            dm_conversations: HashMap::new(),
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn notify(&self, message: impl Into<String>, error: bool) {
        self.emit(Event::Notification(message.into(), error));
    }

    /// Main loop. Stops when the UI drops the command channel.
    pub async fn run(mut self, prefill: Option<LoginRequest>) {
        if let Some(login) = prefill {
            self.login(login).await;
        }

        loop {
            // Take a transport receiver if one appeared (login) or if the
            // previous one ended.
            if self.transport_events.is_none() {
                if let Some(transport) = &self.transport {
                    self.transport_events = transport.take_events();
                }
            }

            // The select borrows two disjoint fields; doing it in its own block
            // means the borrow ends before a handler needs `&mut self`.
            enum Input {
                Command(Option<Command>),
                Transport(Option<TransportEvent>),
            }
            let input = {
                let Session {
                    commands,
                    transport_events,
                    ..
                } = &mut self;
                tokio::select! {
                    command = commands.recv() => Input::Command(command),
                    event = async {
                        match transport_events.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => {
                                futures_util::future::pending::<()>().await;
                                None
                            }
                        }
                    } => Input::Transport(event),
                }
            };

            match input {
                Input::Command(Some(command)) => self.handle_command(command).await,
                Input::Command(None) => break,
                Input::Transport(Some(event)) => self.handle_transport_event(event).await,
                Input::Transport(None) => {
                    // The transport ended (for example a final close code).
                    self.transport_events = None;
                }
            }
        }

        if let Some(transport) = &self.transport {
            transport.shutdown().await;
        }
    }

    // -- sign in / out -----------------------------------------------------

    async fn login(&mut self, login: LoginRequest) {
        self.emit(Event::Connection(ConnectionState::Connecting));
        let instance = login.instance.clone();
        self.instance = instance.clone();

        let transport: Result<Arc<dyn Transport>, HarmonyError> = match login.transport {
            TransportKind::Bot => {
                let token = login.token.clone().unwrap_or_default();
                if token.trim().is_empty() {
                    self.emit(Event::AuthFailure(
                        "A bot token is required for the bot-gateway transport".to_string(),
                    ));
                    self.emit(Event::Connection(ConnectionState::Disconnected));
                    return;
                }
                BotTransport::new(instance, token, login.server_ids)
                    .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
            }
            TransportKind::Web => {
                let email = login.email.clone().unwrap_or_default();
                let password = login.password.clone().unwrap_or_default();
                super::web::WebTransport::new(instance, email, password)
                    .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
            }
        };

        let transport = match transport {
            Ok(transport) => transport,
            Err(err) => {
                self.emit(Event::AuthFailure(err.user_message()));
                self.emit(Event::Connection(ConnectionState::Disconnected));
                return;
            }
        };

        match transport.sign_in().await {
            Ok(account) => {
                self.capabilities = transport.capabilities();
                self.account = Some(account.clone());
                self.emit(Event::AuthSuccess(self.user_from_account(&account)));
                self.transport_events = transport.take_events();
                self.transport = Some(transport);

                let unavailable = match self.load_servers().await {
                    Ok(unavailable) => unavailable,
                    Err(err) => {
                        self.notify(err.user_message(), true);
                        Vec::new()
                    }
                };
                self.emit(Event::Ready {
                    account: self.user_from_account(&account),
                    instance: self.instance.clone(),
                    transport: self.capabilities_transport_kind(),
                    capabilities: self.capabilities,
                    unavailable_servers: unavailable,
                });
            }
            Err(err) => {
                self.emit(Event::AuthFailure(err.user_message()));
                self.emit(Event::Connection(ConnectionState::Disconnected));
            }
        }
    }

    fn capabilities_transport_kind(&self) -> TransportKind {
        self.transport
            .as_ref()
            .map(|transport| transport.kind())
            .unwrap_or(TransportKind::Bot)
    }

    async fn logout(&mut self) {
        if let Some(transport) = self.transport.take() {
            transport.shutdown().await;
        }
        self.transport_events = None;
        self.account = None;
        self.caches = Caches::default();
        self.dm_conversations.clear();
        self.sent_messages.clear();
        self.emit(Event::Connection(ConnectionState::Disconnected));
    }

    // -- commands ----------------------------------------------------------

    async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Login(login) => self.login(login).await,
            Command::Logout => self.logout().await,
            Command::Refresh | Command::GetServers => self.refresh_servers().await,
            Command::GetChannelMessages { channel_id, before } => {
                self.load_messages(channel_id, before).await;
            }
            Command::GetChannelMessagesPaginated {
                channel_id,
                cursor,
                limit,
                direction,
            } => {
                let limit = limit.unwrap_or(PAGE as usize) as u32;
                let before = match (direction, cursor) {
                    (PaginationDirection::Backward, PaginationCursor::Timestamp(ts)) => Some(ts),
                    _ => None,
                };
                self.load_messages_with_limit(channel_id, before, limit).await;
            }
            Command::GetChannelUserList { channel_id } => {
                self.load_channel_members(channel_id).await;
            }
            Command::GetServerMembers { server_id } => {
                self.load_server_members(server_id).await;
            }
            Command::SendChannelMessage { channel_id, content } => {
                self.send_message(channel_id, content).await;
            }
            Command::EditChannelMessage {
                channel_id: _,
                message_id,
                content,
            } => {
                let Some(transport) = self.transport.clone() else {
                    return self.not_signed_in();
                };
                match transport.edit_message(message_id, &content).await {
                    Ok(message) => {
                        let mapped = self.message_to_domain(message);
                        self.emit(Event::ChannelMessageUpdated(mapped));
                    }
                    Err(err) => self.notify(err.user_message(), true),
                }
            }
            Command::DeleteChannelMessage {
                channel_id,
                message_id,
            } => {
                let Some(transport) = self.transport.clone() else {
                    return self.not_signed_in();
                };
                match transport.delete_message(message_id).await {
                    Ok(()) => self.emit(Event::ChannelMessageDeleted {
                        channel_id,
                        message_id,
                    }),
                    Err(err) => self.notify(err.user_message(), true),
                }
            }
            Command::GetProfile { user_id } => {
                let Some(transport) = self.transport.clone() else {
                    return self.not_signed_in();
                };
                match transport.profile(user_id).await {
                    Ok(profile) => self.emit(Event::Profile(self.profile_to_domain(profile))),
                    Err(err) => self.notify(err.user_message(), true),
                }
            }
            Command::GetUserAvatars { user_ids } => {
                self.load_avatars(user_ids).await;
            }
            Command::GetUserList => {
                // The connected-user list is a Nexus concept; version it as the
                // members of the current server instead.
                let server_id = self
                    .caches
                    .channel_server
                    .values()
                    .next()
                    .copied()
                    .or_else(|| self.caches.servers.first().map(|server| server.id));
                if let Some(server_id) = server_id {
                    self.load_server_members(server_id).await;
                } else {
                    self.emit(Event::UserList(Vec::new()));
                }
            }
            Command::GetCacheStats => self.emit(Event::CacheStats {
                total_entries: self.caches.users.len() + self.caches.servers.len(),
                total_size_mb: 0.0,
                hit_ratio: 0.0,
                expired_entries: 0,
            }),
            Command::InvalidateImageCache { keys } => {
                self.emit(Event::ImageCacheInvalidated { keys });
            }
            Command::GetForums => {
                self.emit(Event::Unsupported {
                    what: Unsupported::Forums,
                    detail: Some(
                        "Harmony's roadmap has forums; this client shows servers and channels until then."
                            .to_string(),
                    ),
                });
                self.emit(Event::Forums(Vec::new()));
            }
            Command::Register { .. } => self.emit(Event::Unsupported {
                what: Unsupported::Register,
                detail: None,
            }),
            Command::UpdateProfile { .. }
            | Command::UpdateColor(_)
            | Command::UpdatePassword(_) => self.emit(Event::Unsupported {
                what: Unsupported::ProfileUpdate,
                detail: None,
            }),
            Command::SendServerInvite { .. }
            | Command::RespondToServerInvite { .. }
            | Command::AcceptServerInviteFromUser { .. }
            | Command::DeclineServerInviteFromUser { .. } => self.emit(Event::Unsupported {
                what: Unsupported::ServerInvites,
                detail: None,
            }),
            Command::GetNotifications { .. } | Command::MarkNotificationRead { .. } => {
                self.emit(Event::Unsupported {
                    what: Unsupported::Notifications,
                    detail: None,
                });
                self.emit(Event::Notifications {
                    notifications: Vec::new(),
                    history_complete: true,
                });
            }
            // Forums, threads and DMs are Nexus features with no Harmony
            // equivalent yet; say so once instead of failing silently.
            Command::CreateForum { .. }
            | Command::DeleteForum { .. }
            | Command::CreateThread { .. }
            | Command::CreatePost { .. }
            | Command::CreatePostReply { .. }
            | Command::DeletePost(_)
            | Command::DeleteThread(_) => self.emit(Event::Unsupported {
                what: Unsupported::Forums,
                detail: None,
            }),
            Command::GetDMUserList => self.load_dm_list().await,
            Command::GetDirectMessages { user_id, before } => {
                self.load_dm_messages(user_id, before).await;
            }
            Command::SendDirectMessage { to, content } => self.send_dm(to, content).await,
            Command::GetDirectMessagesPaginated {
                user_id,
                cursor,
                limit,
                direction,
            } => {
                let limit = limit.unwrap_or(PAGE as usize) as u32;
                let before = match (direction, cursor) {
                    (PaginationDirection::Backward, PaginationCursor::Timestamp(ts)) => Some(ts),
                    _ => None,
                };
                self.load_dm_messages_with_limit(user_id, before, limit).await;
            }
        }
    }

    fn not_signed_in(&self) {
        self.notify("Not signed in to a Harmony instance yet", true);
    }

    // -- loading -----------------------------------------------------------

    /// Read every configured server. Returns the ids that could not be read.
    async fn load_servers(&mut self) -> Result<Vec<Uuid>, HarmonyError> {
        let Some(transport) = self.transport.clone() else {
            return Ok(Vec::new());
        };
        let infos = transport.servers().await?;
        let mut servers = Vec::with_capacity(infos.len());
        let mut unavailable = Vec::new();
        for info in infos {
            let channels = match transport.channels(info.id).await {
                Ok(channels) => channels,
                Err(err) => {
                    tracing::warn!("harmony: channels of {}: {}", info.id, err);
                    unavailable.push(info.id);
                    Vec::new()
                }
            };
            servers.push(self.server_to_domain(info, channels));
        }
        self.caches.servers = servers.clone();
        self.emit(Event::Servers(servers));
        Ok(unavailable)
    }

    async fn refresh_servers(&mut self) {
        if self.transport.is_none() {
            return self.not_signed_in();
        }
        match self.load_servers().await {
            Ok(_) => self.notify("Refreshed", false),
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    async fn load_messages(&mut self, channel_id: Uuid, before: Option<i64>) {
        self.load_messages_with_limit(channel_id, before, PAGE).await;
    }

    async fn load_messages_with_limit(&mut self, channel_id: Uuid, before: Option<i64>, limit: u32) {
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        transport.watch_channel(channel_id).await;
        match transport.messages(channel_id, before, limit).await {
            Ok(messages) => {
                for message in &messages {
                    self.remember_author(message);
                }
                let mapped: Vec<ChannelMessage> =
                    messages.into_iter().map(|m| self.message_to_domain(m)).collect();
                let history_complete = mapped.len() < limit as usize;
                self.emit(Event::ChannelMessages {
                    channel_id,
                    messages: mapped,
                    history_complete,
                });
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    async fn load_channel_members(&mut self, channel_id: Uuid) {
        let server_id = self
            .caches
            .channel_server
            .get(&channel_id)
            .copied()
            .or_else(|| {
                self.caches
                    .servers
                    .iter()
                    .find(|server| server.channels.iter().any(|c| c.id == channel_id))
                    .map(|server| server.id)
            });
        match server_id {
            Some(server_id) => self.load_server_members(server_id).await,
            None => self.notify("Unknown channel: no server holds it", true),
        }
    }

    async fn load_server_members(&mut self, server_id: Uuid) {
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        match transport.members(server_id).await {
            Ok(members) => {
                let users: Vec<User> = members.iter().map(|m| self.member_to_domain(m)).collect();
                for user in &users {
                    self.caches.users.insert(user.id, user.clone());
                }
                let channel_id = self
                    .caches
                    .servers
                    .iter()
                    .find(|server| server.id == server_id)
                    .and_then(|server| server.channels.iter().find(|c| c.kind.is_text()))
                    .map(|channel| channel.id)
                    .unwrap_or(Uuid::nil());
                self.emit(Event::ChannelUserList { channel_id, users });
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    async fn load_avatars(&mut self, user_ids: Vec<Uuid>) {
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        let mut avatars = Vec::with_capacity(user_ids.len());
        for user_id in user_ids.into_iter().take(20) {
            let avatar = match transport.profile(user_id).await {
                Ok(profile) => match profile.avatar_url {
                    Some(url) => match transport.fetch_image(&url).await {
                        Ok(image) => image,
                        Err(err) => {
                            tracing::debug!("harmony: avatar {url}: {}", err);
                            None
                        }
                    },
                    None => None,
                },
                Err(err) => {
                    tracing::debug!("harmony: profile {user_id}: {}", err);
                    None
                }
            };
            avatars.push((user_id, avatar));
        }
        self.emit(Event::UserAvatars { avatars });
    }

    async fn send_message(&mut self, channel_id: Uuid, content: String) {
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        match transport.send_message(channel_id, &content, None).await {
            Ok(message) => {
                self.remember_author(&message);
                self.sent_messages.insert(message.id);
                if self.sent_messages.len() > 500 {
                    self.sent_messages.clear();
                }
                let mapped = self.message_to_domain(message);
                self.emit(Event::NewChannelMessage(mapped));
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    // -- direct messages ---------------------------------------------------

    async fn load_dm_list(&mut self) {
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        match transport.dm_conversations().await {
            Ok(conversations) => {
                let mut users = Vec::new();
                for conversation in conversations {
                    if let Some(peer) = &conversation.peer {
                        self.dm_conversations.insert(peer.id, conversation.id);
                        let user = self.member_to_domain(peer);
                        self.caches.users.insert(user.id, user.clone());
                        users.push(user);
                    }
                }
                self.emit(Event::DMUserList(users));
            }
            Err(HarmonyError::Unsupported(_)) => {
                self.emit(Event::Unsupported {
                    what: Unsupported::DirectMessages,
                    detail: None,
                });
                self.emit(Event::DMUserList(Vec::new()));
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    async fn load_dm_messages(&mut self, user_id: Uuid, before: Option<i64>) {
        self.load_dm_messages_with_limit(user_id, before, PAGE).await;
    }

    async fn load_dm_messages_with_limit(&mut self, user_id: Uuid, before: Option<i64>, limit: u32) {
        let Some(conversation_id) = self.conversation_for(user_id).await else {
            return;
        };
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        match transport.dm_messages(conversation_id, before, limit).await {
            Ok(messages) => {
                for message in &messages {
                    self.remember_author(message);
                }
                let mapped: Vec<DirectMessage> = messages
                    .into_iter()
                    .map(|message| self.dm_to_domain(message, user_id))
                    .collect();
                let history_complete = mapped.len() < limit as usize;
                self.emit(Event::DirectMessages {
                    user_id,
                    messages: mapped,
                    history_complete,
                });
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    async fn send_dm(&mut self, to: Uuid, content: String) {
        let Some(conversation_id) = self.conversation_for(to).await else {
            return;
        };
        let Some(transport) = self.transport.clone() else {
            return self.not_signed_in();
        };
        match transport.send_dm(conversation_id, &content).await {
            Ok(message) => {
                let mapped = self.dm_to_domain(message, to);
                self.emit(Event::DirectMessage(mapped));
            }
            Err(err) => self.notify(err.user_message(), true),
        }
    }

    /// The conversation with a peer, opening it when the list has not seen it.
    async fn conversation_for(&mut self, user_id: Uuid) -> Option<Uuid> {
        if let Some(conversation_id) = self.dm_conversations.get(&user_id) {
            return Some(*conversation_id);
        }
        let transport = self.transport.clone()?;
        match transport.open_dm_with(user_id).await {
            Ok(conversation_id) => {
                self.dm_conversations.insert(user_id, conversation_id);
                Some(conversation_id)
            }
            Err(err) => {
                self.notify(err.user_message(), true);
                None
            }
        }
    }

    fn dm_to_domain(&self, message: MessageInfo, peer: Uuid) -> DirectMessage {
        DirectMessage {
            id: message.id,
            from: message
                .author
                .as_ref()
                .and_then(|author| author.id)
                .unwrap_or(peer),
            to: peer,
            timestamp: message.timestamp,
            content: message.content,
        }
    }

    // -- transport events --------------------------------------------------

    async fn handle_transport_event(&mut self, event: TransportEvent) {
        match event {
            TransportEvent::Connected => {
                self.emit(Event::Connection(ConnectionState::Online));
            }
            TransportEvent::MessageCreate(message) => {
                if self.sent_messages.remove(&message.id) {
                    // Our own message, already shown from the REST response.
                    return;
                }
                self.remember_author(&message);
                let channel_id = message.channel_id;
                let mapped = self.message_to_domain(message);
                self.emit(Event::NewChannelMessage(mapped));
                let _ = channel_id;
            }
            TransportEvent::MessageUpdate(message) => {
                self.remember_author(&message);
                let mapped = self.message_to_domain(message);
                self.emit(Event::ChannelMessageUpdated(mapped));
            }
            TransportEvent::MessageDelete {
                channel_id,
                message_id,
            } => self.emit(Event::ChannelMessageDeleted {
                channel_id,
                message_id,
            }),
            TransportEvent::ReactionAdd {
                channel_id,
                message_id,
                emoji,
                by_me,
            } => {
                self.emit(Event::Notification(
                    format!(
                        "{} reacted with {emoji}",
                        if by_me { "You" } else { "Someone" }
                    ),
                    false,
                ));
                let _ = (channel_id, message_id);
            }
            TransportEvent::ReactionRemove { emoji, .. } => {
                tracing::debug!("harmony: reaction {emoji} removed");
            }
            TransportEvent::MemberJoin { server_id, user } => {
                let domain = self.member_to_domain(&user);
                self.caches.users.insert(domain.id, domain.clone());
                let _ = server_id;
                self.emit(Event::UserJoined(domain));
            }
            TransportEvent::MemberLeave { server_id, user_id } => {
                let _ = server_id;
                self.emit(Event::UserLeft(user_id));
            }
            TransportEvent::Disconnected {
                reason,
                will_reconnect,
            } => {
                let state = if will_reconnect {
                    ConnectionState::Reconnecting
                } else {
                    ConnectionState::Disconnected
                };
                self.emit(Event::Connection(state));
                self.notify(reason, !will_reconnect);
            }
            TransportEvent::Notice(message) => self.notify(message, false),
        }
    }

    // -- mapping -----------------------------------------------------------

    fn user_from_account(&self, account: &Account) -> User {
        User {
            id: account.id,
            username: account.username.clone(),
            display_name: account.display_name.clone(),
            color: UserColor::for_id(account.id),
            role: UserRole::User,
            profile_pic: None,
            cover_banner: None,
            status: UserStatus::Connected,
            bot: account.bot,
        }
    }

    fn member_to_domain(&self, member: &MemberInfo) -> User {
        User {
            id: member.id,
            username: member.username.clone(),
            display_name: member.nickname.clone().or_else(|| member.display_name.clone()),
            color: UserColor::for_id(member.id),
            role: if member.is_admin {
                UserRole::Admin
            } else {
                UserRole::User
            },
            profile_pic: None,
            cover_banner: None,
            status: UserStatus::Offline,
            bot: member.bot,
        }
    }

    fn server_to_domain(&mut self, info: ServerInfo, channels: Vec<ChannelInfo>) -> Server {
        let mapped: Vec<Channel> = channels
            .into_iter()
            .filter(|channel| channel.kind != ChannelKind::Category)
            .map(|channel| {
                self.caches.channel_server.insert(channel.id, info.id);
                Channel {
                    id: channel.id,
                    server_id: info.id,
                    name: channel.name,
                    description: channel.description.unwrap_or_default(),
                    kind: channel.kind,
                    position: channel.position,
                    category: channel.category,
                    permissions: Default::default(),
                    userlist: Vec::new(),
                    messages: Vec::new(),
                }
            })
            .collect();

        Server {
            id: info.id,
            name: info.name,
            description: info.description.unwrap_or_default(),
            public: true,
            invite_code: None,
            icon: None,
            banner: None,
            owner: info.owner_id,
            mods: Vec::new(),
            userlist: Vec::new(),
            channels: mapped,
        }
    }

    fn message_to_domain(&mut self, message: MessageInfo) -> ChannelMessage {
        let author_id = message
            .author
            .as_ref()
            .and_then(|author| author.id)
            .unwrap_or(Uuid::nil());

        let author = match message.author.as_ref() {
            Some(author) => match author.id {
                Some(id) => self.caches.users.get(&id).cloned().unwrap_or_else(|| {
                    let user = self.user_from_author(author, id);
                    self.caches.users.insert(id, user.clone());
                    user
                }),
                None => self.user_from_author(author, Uuid::nil()),
            },
            None => User::placeholder(Uuid::nil()),
        };
        // Make sure the author is in the user cache under its id.
        if author_id != Uuid::nil() {
            self.caches.users.entry(author_id).or_insert_with(|| author.clone());
        }

        ChannelMessage {
            id: message.id,
            channel_id: message.channel_id,
            thread_id: message.thread_id,
            sent_by: author_id,
            timestamp: message.timestamp,
            content: message.content,
            attachments: message
                .attachments
                .into_iter()
                .map(|attachment| Attachment {
                    id: attachment.id,
                    url: attachment.url,
                    filename: attachment.filename,
                    size: attachment.size,
                    content_type: attachment.content_type,
                })
                .collect(),
            reactions: message
                .reactions
                .into_iter()
                .map(|reaction| Reaction {
                    emoji: reaction.emoji,
                    count: 1,
                    me: reaction.by_me,
                })
                .collect(),
            reply_to: message.reply_to,
            edited: message.edited.is_some(),
        }
    }

    fn user_from_author(&self, author: &super::transport::AuthorInfo, id: Uuid) -> User {
        User {
            id,
            username: author
                .nickname
                .clone()
                .unwrap_or_else(|| author.username.clone()),
            display_name: author.display_name.clone(),
            color: UserColor::for_id(id),
            role: UserRole::User,
            profile_pic: None,
            cover_banner: None,
            status: UserStatus::Offline,
            bot: author.bot,
        }
    }

    fn profile_to_domain(&self, profile: ProfileInfo) -> UserProfile {
        UserProfile {
            id: profile.id,
            username: profile.username,
            color: UserColor::for_id(profile.id),
            role: UserRole::User,
            bio: profile.bio,
            url1: None,
            url2: None,
            url3: None,
            location: None,
            profile_pic: None,
            cover_banner: None,
        }
    }

    fn remember_author(&mut self, message: &MessageInfo) {
        if let Some(author) = &message.author {
            if let Some(id) = author.id {
                if !self.caches.users.contains_key(&id) {
                    let user = self.user_from_author(author, id);
                    self.caches.users.insert(id, user);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::transport::DmConversation;

    fn session() -> (Session, mpsc::UnboundedSender<Command>) {
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        let (events_tx, _events_rx) = mpsc::unbounded_channel();
        (Session::new(commands_rx, events_tx), commands_tx)
    }

    struct DmStub {
        peer: Uuid,
        conversation: Uuid,
        me: Uuid,
    }

    #[async_trait::async_trait]
    impl Transport for DmStub {
        fn kind(&self) -> TransportKind {
            TransportKind::Web
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::web()
        }
        fn instance(&self) -> &str {
            "stub.mony.lol"
        }
        async fn sign_in(&self) -> Result<Account, HarmonyError> {
            Ok(Account {
                id: self.me,
                username: "me".to_string(),
                display_name: None,
                avatar_url: None,
                bot: false,
            })
        }
        async fn servers(&self) -> Result<Vec<ServerInfo>, HarmonyError> {
            Ok(Vec::new())
        }
        async fn channels(&self, _server_id: Uuid) -> Result<Vec<ChannelInfo>, HarmonyError> {
            Ok(Vec::new())
        }
        async fn members(&self, _server_id: Uuid) -> Result<Vec<MemberInfo>, HarmonyError> {
            Ok(Vec::new())
        }
        async fn messages(
            &self,
            _channel_id: Uuid,
            _before: Option<i64>,
            _limit: u32,
        ) -> Result<Vec<MessageInfo>, HarmonyError> {
            Ok(Vec::new())
        }
        async fn send_message(
            &self,
            _channel_id: Uuid,
            _content: &str,
            _reply_to: Option<Uuid>,
        ) -> Result<MessageInfo, HarmonyError> {
            Err(HarmonyError::Unsupported("channels".to_string()))
        }
        async fn edit_message(
            &self,
            _message_id: Uuid,
            _content: &str,
        ) -> Result<MessageInfo, HarmonyError> {
            Err(HarmonyError::Unsupported("edit".to_string()))
        }
        async fn delete_message(&self, _message_id: Uuid) -> Result<(), HarmonyError> {
            Err(HarmonyError::Unsupported("delete".to_string()))
        }
        async fn profile(&self, _user_id: Uuid) -> Result<ProfileInfo, HarmonyError> {
            Err(HarmonyError::Unsupported("profile".to_string()))
        }
        async fn fetch_image(&self, _url: &str) -> Result<Option<String>, HarmonyError> {
            Ok(None)
        }
        fn take_events(&self) -> Option<mpsc::UnboundedReceiver<TransportEvent>> {
            None
        }
        async fn shutdown(&self) {}

        async fn dm_conversations(&self) -> Result<Vec<DmConversation>, HarmonyError> {
            Ok(vec![DmConversation {
                id: self.conversation,
                peer: Some(MemberInfo {
                    id: self.peer,
                    username: "bob".to_string(),
                    display_name: Some("Bob".to_string()),
                    avatar_url: None,
                    nickname: None,
                    roles: Vec::new(),
                    is_admin: false,
                    bot: false,
                }),
                name: None,
                is_group: false,
                unread: 2,
                last_message: None,
            }])
        }
        async fn dm_messages(
            &self,
            conversation_id: Uuid,
            _before: Option<i64>,
            _limit: u32,
        ) -> Result<Vec<MessageInfo>, HarmonyError> {
            Ok(vec![MessageInfo {
                id: Uuid::new_v4(),
                channel_id: Uuid::nil(),
                conversation_id: Some(conversation_id),
                thread_id: None,
                author: Some(super::super::transport::AuthorInfo {
                    id: Some(self.peer),
                    raw_id: self.peer.to_string(),
                    username: "bob".to_string(),
                    display_name: None,
                    avatar_url: None,
                    nickname: None,
                    bot: false,
                }),
                content: "hi from bob".to_string(),
                timestamp: 1_791_892_800,
                edited: None,
                reply_to: None,
                attachments: Vec::new(),
                reactions: Vec::new(),
                system: false,
            }])
        }
        async fn send_dm(
            &self,
            conversation_id: Uuid,
            content: &str,
        ) -> Result<MessageInfo, HarmonyError> {
            Ok(MessageInfo {
                id: Uuid::new_v4(),
                channel_id: Uuid::nil(),
                conversation_id: Some(conversation_id),
                thread_id: None,
                author: Some(super::super::transport::AuthorInfo {
                    id: Some(self.me),
                    raw_id: self.me.to_string(),
                    username: "me".to_string(),
                    display_name: None,
                    avatar_url: None,
                    nickname: None,
                    bot: false,
                }),
                content: content.to_string(),
                timestamp: 1_791_892_900,
                edited: None,
                reply_to: None,
                attachments: Vec::new(),
                reactions: Vec::new(),
                system: false,
            })
        }
        async fn open_dm_with(&self, _user_id: Uuid) -> Result<Uuid, HarmonyError> {
            Ok(self.conversation)
        }
    }

    #[tokio::test]
    async fn dm_commands_resolve_the_peer_conversation() {
        let peer = Uuid::new_v4();
        let conversation = Uuid::new_v4();
        let me = Uuid::new_v4();
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let (events, mut events_rx) = mpsc::unbounded_channel();
        let mut session = Session::new(commands_rx, events);
        session.transport = Some(Arc::new(DmStub {
            peer,
            conversation,
            me,
        }));

        // The DM list maps the peer to its conversation.
        session.handle_command(Command::GetDMUserList).await;
        match events_rx.try_recv() {
            Ok(Event::DMUserList(users)) => {
                assert_eq!(users.len(), 1);
                assert_eq!(users[0].id, peer);
                assert_eq!(users[0].username, "bob");
            }
            other => panic!("expected DMUserList, got {other:?}"),
        }
        assert_eq!(session.dm_conversations.get(&peer), Some(&conversation));

        // Opening the peer's history asks the conversation the mapping holds.
        session
            .handle_command(Command::GetDirectMessages {
                user_id: peer,
                before: None,
            })
            .await;
        match events_rx.try_recv() {
            Ok(Event::DirectMessages {
                user_id,
                messages,
                history_complete,
            }) => {
                assert_eq!(user_id, peer);
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0].content, "hi from bob");
                assert_eq!(messages[0].from, peer);
                assert_eq!(messages[0].to, peer);
                assert!(history_complete);
            }
            other => panic!("expected DirectMessages, got {other:?}"),
        }

        // Sending goes to the same conversation and reports the DM back.
        session
            .handle_command(Command::SendDirectMessage {
                to: peer,
                content: "yo".to_string(),
            })
            .await;
        match events_rx.try_recv() {
            Ok(Event::DirectMessage(message)) => {
                assert_eq!(message.content, "yo");
                assert_eq!(message.to, peer);
                assert_eq!(message.from, me);
            }
            other => panic!("expected DirectMessage, got {other:?}"),
        }
        let _ = commands; // the sender is only needed to keep the channel alive
    }

    #[test]
    fn unsupported_features_explain_themselves() {
        let reason = Unsupported::Forums.explain();
        assert!(reason.contains("ROADMAP"));
        assert!(Unsupported::DirectMessages.explain().contains("bot protocol"));
    }

    #[test]
    fn channel_mapping_drops_categories() {
        let (mut session, _tx) = session();
        let info = ServerInfo {
            id: Uuid::new_v4(),
            name: "Book Club".to_string(),
            description: None,
            icon_url: None,
            owner_id: None,
            member_count: Some(3),
        };
        let text = ChannelInfo {
            id: Uuid::new_v4(),
            server_id: info.id,
            name: "general".to_string(),
            description: None,
            kind: ChannelKind::Text,
            position: 0,
            category: None,
        };
        let category = ChannelInfo {
            kind: ChannelKind::Category,
            ..text.clone()
        };
        let server = session.server_to_domain(info.clone(), vec![category, text.clone()]);
        assert_eq!(server.channels.len(), 1);
        assert_eq!(server.channels[0].kind, ChannelKind::Text);
        assert_eq!(
            session.caches.channel_server.get(&text.id),
            Some(&info.id)
        );
    }

    #[test]
    fn author_unknown_members_become_placeholders() {
        let (mut session, _tx) = session();
        let author_id = Uuid::new_v4();
        let message = MessageInfo {
            id: Uuid::new_v4(),
            channel_id: Uuid::new_v4(),
            conversation_id: None,
            thread_id: None,
            author: Some(super::super::transport::AuthorInfo {
                id: Some(author_id),
                raw_id: author_id.to_string(),
                username: "alice".to_string(),
                display_name: Some("Alice".to_string()),
                avatar_url: None,
                nickname: None,
                bot: false,
            }),
            content: "hi".to_string(),
            timestamp: 1_791_633_600,
            edited: None,
            reply_to: None,
            attachments: Vec::new(),
            reactions: Vec::new(),
            system: false,
        };
        let mapped = session.message_to_domain(message);
        assert_eq!(mapped.sent_by, author_id);
        assert!(session.caches.users.contains_key(&author_id));
    }
}
