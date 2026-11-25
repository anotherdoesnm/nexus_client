//! The bot transport: `Authorization: Bot <token>` over the REST API, events
//! from the gateway WebSocket.
//!
//! The bot API cannot list the servers a bot is installed in, so a session is
//! configured with server ids (see [`crate::harmony::transport::parse_server_ids`])
//! and reads each one. Failures on individual servers are reported as notices
//! rather than failing the whole list.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use url::Url;
use uuid::Uuid;

use async_trait::async_trait;

use crate::domain::{Capabilities, ChannelKind, TransportKind};

use super::error::HarmonyError;
use super::gateway::{GatewayClient, GatewayConfig, GatewayEvent, GatewayUpdate};
use super::model::{self, Author, GatewayMessage, Message};
use super::rest::{api_base, BotRest, RetryPolicy};
use super::transport::{
    Account, AttachmentInfo, AuthorInfo, ChannelInfo, MemberInfo, MessageInfo, ProfileInfo,
    ServerInfo, Transport, TransportEvent,
};

pub struct BotTransport {
    instance: String,
    /// Kept for the gateway handshake; never logged (see `BotRest`'s Debug).
    token: String,
    rest: BotRest,
    server_ids: Vec<Uuid>,
    events_rx: Mutex<Option<mpsc::UnboundedReceiver<TransportEvent>>>,
    events_tx: mpsc::UnboundedSender<TransportEvent>,
    shutdown_tx: watch::Sender<bool>,
    gateway_task: Mutex<Option<JoinHandle<()>>>,
    started: AtomicBool,
}

impl BotTransport {
    pub fn new(
        instance: impl Into<String>,
        token: impl Into<String>,
        server_ids: Vec<Uuid>,
    ) -> Result<Self, HarmonyError> {
        Self::with_policy(instance, token, server_ids, RetryPolicy::default())
    }

    pub fn with_policy(
        instance: impl Into<String>,
        token: impl Into<String>,
        server_ids: Vec<Uuid>,
        retry: RetryPolicy,
    ) -> Result<Self, HarmonyError> {
        let instance = instance.into();
        let token = token.into();
        let rest = BotRest::with_policy(api_base(&instance)?, token.clone(), retry)?;
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            instance,
            token,
            rest,
            server_ids,
            events_rx: Mutex::new(Some(events_rx)),
            events_tx,
            shutdown_tx,
            gateway_task: Mutex::new(None),
            started: AtomicBool::new(false),
        })
    }

    /// The configured server ids, for diagnostics.
    pub fn server_ids(&self) -> &[Uuid] {
        &self.server_ids
    }

    /// Spawn the gateway loop. Idempotent; called by `sign_in`.
    fn start_gateway(&self, token: String) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let config = match GatewayConfig::for_instance(&self.instance, token) {
            Ok(config) => config,
            Err(err) => {
                let _ = self
                    .events_tx
                    .send(TransportEvent::Notice(err.user_message()));
                return;
            }
        };

        let client = GatewayClient::new(config);
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel::<GatewayUpdate>();
        let shutdown_rx = self.shutdown_tx.subscribe();
        let events_tx = self.events_tx.clone();

        let gateway = tokio::spawn(async move {
            client.run(updates_tx, shutdown_rx).await;
        });

        tokio::spawn(async move {
            while let Some(update) = updates_rx.recv().await {
                let event = match update {
                    GatewayUpdate::Connected => Some(TransportEvent::Connected),
                    GatewayUpdate::Event(event) => gateway_event_to_transport(event),
                    GatewayUpdate::Closed {
                        close,
                        will_reconnect,
                        ..
                    } => Some(TransportEvent::Disconnected {
                        reason: match close.code {
                            Some(code) => format!("gateway closed ({code}): {}", close.reason),
                            None => format!("gateway closed: {}", close.reason),
                        },
                        will_reconnect,
                    }),
                    GatewayUpdate::Error(message) => Some(TransportEvent::Notice(message)),
                };
                if let Some(event) = event {
                    if events_tx.send(event).is_err() {
                        break;
                    }
                }
            }
        });

        *self.gateway_task.lock().expect("gateway task lock") = Some(gateway);
    }
}

/// Convert one gateway event; `None` for events the session does not consume.
fn gateway_event_to_transport(event: GatewayEvent) -> Option<TransportEvent> {
    match event {
        GatewayEvent::Ready(ready) => {
            tracing::info!(
                "harmony: gateway ready as {} ({}), heartbeat {:?}",
                ready.bot.username,
                ready.bot.id,
                ready.heartbeat()
            );
            Some(TransportEvent::Connected)
        }
        GatewayEvent::MessageCreate(message) => {
            Some(TransportEvent::MessageCreate(message_from_gateway(message)))
        }
        GatewayEvent::MessageUpdate(message) => {
            Some(TransportEvent::MessageUpdate(message_from_gateway(message)))
        }
        GatewayEvent::MessageDelete(deleted) => Some(TransportEvent::MessageDelete {
            channel_id: deleted.channel_id,
            message_id: deleted.id,
        }),
        GatewayEvent::ReactionAdd(reaction) => Some(TransportEvent::ReactionAdd {
            channel_id: reaction.channel_id,
            message_id: reaction.message_id,
            emoji: reaction.emoji_text(),
            by_me: reaction.bot_id.is_some(),
        }),
        GatewayEvent::ReactionRemove(reaction) => Some(TransportEvent::ReactionRemove {
            channel_id: reaction.channel_id,
            message_id: reaction.message_id,
            emoji: reaction.emoji_text(),
            by_me: reaction.bot_id.is_some(),
        }),
        GatewayEvent::MemberAdd(member) => member.user.map(|user| TransportEvent::MemberJoin {
            server_id: member.server_id,
            user: MemberInfo {
                id: user.id,
                username: user.username.clone().unwrap_or_else(|| {
                    format!("user-{}", &user.id.to_string()[..8])
                }),
                display_name: user.display_name,
                avatar_url: user.avatar_url,
                nickname: None,
                roles: Vec::new(),
                is_admin: false,
                bot: false,
            },
        }),
        GatewayEvent::MemberRemove(member)
        | GatewayEvent::MemberKick(member)
        | GatewayEvent::BanAdd(member) => member.user.map(|user| TransportEvent::MemberLeave {
            server_id: member.server_id,
            user_id: user.id,
        }),
        GatewayEvent::BanRemove(_) => None,
        GatewayEvent::HeartbeatAck => None,
        GatewayEvent::Interaction(payload) => {
            tracing::debug!("harmony: interaction event: {payload}");
            None
        }
        GatewayEvent::Unknown { name, .. } => {
            tracing::debug!("harmony: ignoring gateway event {name}");
            None
        }
    }
}

fn author_from_gateway(author: &Author) -> AuthorInfo {
    AuthorInfo {
        id: Uuid::parse_str(&author.id).ok(),
        raw_id: author.id.clone(),
        username: author.username.clone(),
        display_name: author.display_name.clone(),
        avatar_url: author.avatar.clone(),
        nickname: author.nickname.clone(),
        bot: author.bot,
    }
}

/// Map a gateway message event onto the neutral shape.
pub fn message_from_gateway(message: GatewayMessage) -> MessageInfo {
    let attachments = message
        .content_raw
        .iter()
        .filter_map(|part| match part {
            model::MessagePart::File {
                url,
                file_type,
                file_name,
                file_size,
                mime_type,
                path,
            } => Some(AttachmentInfo {
                id: path.clone().or_else(|| url.clone()).unwrap_or_default(),
                url: url.clone().unwrap_or_default(),
                filename: file_name
                    .clone()
                    .or_else(|| file_type.clone())
                    .unwrap_or_else(|| "attachment".to_string()),
                size: file_size.unwrap_or(0),
                content_type: mime_type.clone(),
            }),
            _ => None,
        })
        .filter(|attachment| !attachment.url.is_empty())
        .collect();

    MessageInfo {
        id: message.id,
        channel_id: message.channel_id,
        conversation_id: None,
        thread_id: message.thread_id,
        author: message.author.as_ref().map(author_from_gateway),
        content: message.content,
        timestamp: model::parse_timestamp(&message.timestamp),
        edited: message
            .edited_timestamp
            .as_deref()
            .map(model::parse_timestamp)
            .filter(|edited| *edited != model::parse_timestamp(&message.timestamp)),
        reply_to: message.reply_to,
        attachments,
        reactions: Vec::new(),
        system: message.is_system,
    }
}

/// Map a REST message object onto the neutral shape. REST objects carry no
/// `content_raw`, so attachments and reactions stay empty until an event
/// arrives for the message.
pub fn message_from_rest(message: Message) -> MessageInfo {
    MessageInfo {
        id: message.id,
        channel_id: message.channel_id,
        conversation_id: None,
        thread_id: message.thread_id,
        author: message.author.as_ref().map(author_from_gateway),
        content: message.content,
        timestamp: model::parse_timestamp(&message.timestamp),
        edited: message
            .edited_timestamp
            .as_deref()
            .map(model::parse_timestamp)
            .filter(|edited| *edited != model::parse_timestamp(&message.timestamp)),
        reply_to: message.reply_to,
        attachments: Vec::new(),
        reactions: Vec::new(),
        system: message.is_system,
    }
}

fn channel_kind(kind: i64) -> ChannelKind {
    match kind {
        2 => ChannelKind::Voice,
        4 => ChannelKind::Category,
        _ => ChannelKind::Text,
    }
}

/// Guess the MIME type of downloaded image bytes from their magic number, so a
/// `data:` URL is well formed without trusting a header.
pub fn image_mime(bytes: &[u8]) -> &'static str {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        _ => "application/octet-stream",
    }
}

#[async_trait]
impl Transport for BotTransport {
    fn kind(&self) -> TransportKind {
        TransportKind::Bot
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::bot()
    }

    fn instance(&self) -> &str {
        &self.instance
    }

    async fn sign_in(&self) -> Result<Account, HarmonyError> {
        let me = self.rest.me().await?;
        self.start_gateway(self.token.clone());

        Ok(Account {
            id: me.id,
            username: me.username.clone(),
            display_name: Some(me.username),
            avatar_url: me.avatar,
            bot: true,
        })
    }

    async fn servers(&self) -> Result<Vec<ServerInfo>, HarmonyError> {
        let mut servers = Vec::with_capacity(self.server_ids.len());
        for server_id in &self.server_ids {
            match self.rest.server(*server_id).await {
                Ok(server) => servers.push(ServerInfo {
                    id: server.id,
                    name: server.name,
                    description: server.description,
                    icon_url: server.icon,
                    owner_id: server.owner_id,
                    member_count: server.member_count,
                }),
                Err(err) => {
                    let _ = self.events_tx.send(TransportEvent::Notice(format!(
                        "server {server_id}: {}",
                        err.user_message()
                    )));
                }
            }
        }
        Ok(servers)
    }

    async fn channels(&self, server_id: Uuid) -> Result<Vec<ChannelInfo>, HarmonyError> {
        let channels = self.rest.channels(server_id).await?;
        let categories = self.rest.categories(server_id).await.unwrap_or_default();
        let category_names: std::collections::HashMap<Uuid, String> = categories
            .into_iter()
            .map(|category| (category.id, category.name))
            .collect();

        let mut mapped: Vec<ChannelInfo> = channels
            .into_iter()
            .map(|channel| {
                let kind = channel_kind(channel.kind_number());
                let position = channel.order;
                let category = channel
                    .category_id
                    .and_then(|id| category_names.get(&id).cloned());
                ChannelInfo {
                    id: channel.id,
                    server_id,
                    name: channel.name,
                    description: None,
                    kind,
                    position,
                    category,
                }
            })
            .collect();
        mapped.sort_by(|a, b| {
            a.category
                .cmp(&b.category)
                .then(a.position.cmp(&b.position))
                .then(a.name.cmp(&b.name))
        });
        Ok(mapped)
    }

    async fn members(&self, server_id: Uuid) -> Result<Vec<MemberInfo>, HarmonyError> {
        let members = self.rest.members(server_id).await?;
        let admin_roles: std::collections::HashSet<Uuid> = self
            .rest
            .roles(server_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|role| role.is_admin)
            .map(|role| role.id)
            .collect();

        Ok(members
            .into_iter()
            .map(|member| MemberInfo {
                id: member.user.id,
                username: member.user.username,
                display_name: member.user.display_name,
                avatar_url: member.user.avatar,
                nickname: member.nick,
                is_admin: member.roles.iter().any(|role| admin_roles.contains(role)),
                roles: member.roles,
                bot: false,
            })
            .collect())
    }

    async fn messages(
        &self,
        channel_id: Uuid,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<MessageInfo>, HarmonyError> {
        let messages = self.rest.messages(channel_id, before, None, limit).await?;
        Ok(messages.into_iter().map(message_from_rest).collect())
    }

    async fn send_message(
        &self,
        channel_id: Uuid,
        content: &str,
        reply_to: Option<Uuid>,
    ) -> Result<MessageInfo, HarmonyError> {
        let message = self
            .rest
            .send_message(
                channel_id,
                &model::CreateMessage {
                    content: content.to_string(),
                    reply_to,
                    thread_id: None,
                },
            )
            .await?;
        Ok(message_from_rest(message))
    }

    async fn edit_message(
        &self,
        message_id: Uuid,
        content: &str,
    ) -> Result<MessageInfo, HarmonyError> {
        let message = self.rest.edit_message(message_id, content).await?;
        Ok(message_from_rest(message))
    }

    async fn delete_message(&self, message_id: Uuid) -> Result<(), HarmonyError> {
        self.rest.delete_message(message_id).await
    }

    async fn profile(&self, user_id: Uuid) -> Result<ProfileInfo, HarmonyError> {
        let profile = self.rest.user(user_id).await?;
        Ok(ProfileInfo {
            id: profile.id,
            username: profile.username,
            display_name: profile.display_name,
            bio: profile.bio,
            avatar_url: profile.avatar,
            bot: false,
        })
    }

    async fn fetch_image(&self, url: &str) -> Result<Option<String>, HarmonyError> {
        use base64::Engine;
        let bytes = self.rest.download(url).await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let mime = image_mime(&bytes);
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(Some(format!("data:{mime};base64,{encoded}")))
    }

    fn take_events(&self) -> Option<mpsc::UnboundedReceiver<TransportEvent>> {
        self.events_rx.lock().expect("events lock").take()
    }

    async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.gateway_task.lock().expect("gateway task lock").take() {
            task.abort();
        }
    }
}

/// URL of the web app for this instance (used by notices).
pub fn web_url(instance: &str) -> Option<Url> {
    super::instance_root(instance).ok()
}
