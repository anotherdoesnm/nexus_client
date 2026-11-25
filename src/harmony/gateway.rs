//! The Harmony bot gateway: one WebSocket, opcodes 0/1/2/11, identify once,
//! heartbeat on the interval `READY` carries, and a reconnect loop with the
//! documented close-code policy (`docs/build/gateway.md`).
//!
//! There is no RESUME and no sequence number: after a close the connection is
//! rebuilt and identified again, and anything missed is re-read over REST.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

use super::error::HarmonyError;
use super::model::{
    GatewayFrame, GatewayMessage, MemberEvent, MessageDeleted, ReactionEvent, ReadyPayload,
};

/// Opcodes, named as the docs name them.
pub mod op {
    pub const DISPATCH: i64 = 0;
    pub const HEARTBEAT: i64 = 1;
    pub const IDENTIFY: i64 = 2;
    pub const HEARTBEAT_ACK: i64 = 11;
}

/// A decoded gateway event.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum GatewayEvent {
    Ready(ReadyPayload),
    MessageCreate(GatewayMessage),
    MessageUpdate(GatewayMessage),
    MessageDelete(MessageDeleted),
    ReactionAdd(ReactionEvent),
    ReactionRemove(ReactionEvent),
    MemberAdd(MemberEvent),
    MemberRemove(MemberEvent),
    MemberKick(MemberEvent),
    BanAdd(MemberEvent),
    BanRemove(MemberEvent),
    Interaction(Value),
    HeartbeatAck,
    /// An event this build does not model; forwarded so a caller can log it.
    Unknown { name: String, data: Value },
}

#[derive(Debug, Clone)]
pub struct CloseInfo {
    pub code: Option<u16>,
    pub reason: String,
}

impl CloseInfo {
    pub fn is_clean_shutdown(&self) -> bool {
        matches!(self.code, Some(1000)) && self.reason.eq_ignore_ascii_case("server shutting down")
    }
}

/// Whether the bot should open a new connection after this close.
///
/// Per the close-code table: `4001` (missing token) and `4004` (invalid token,
/// revoked/expired, bot missing) are final; everything else is retried with a
/// backoff. A `4004` whose reason says the lookup was unavailable is a
/// transient condition and is retried.
pub fn should_reconnect(close: &CloseInfo) -> bool {
    match close.code {
        Some(4001) => false,
        Some(4004) => close.reason.to_ascii_lowercase().contains("unavailable"),
        Some(4029) => true,
        Some(4003) | Some(4008) | Some(1008) | Some(1000) | None => true,
        Some(_) => true,
    }
}

/// Backoff for the `attempt`-th reconnect (1-based): 1s, 2s, 4s … capped at 60s.
pub fn reconnect_delay(attempt: u32) -> Duration {
    let secs = 2u64.saturating_pow(attempt.saturating_sub(1).min(6));
    Duration::from_secs(secs.min(60))
}

/// Decode one text frame into an event. Unknown types and unparsable payloads
/// become `Unknown` rather than closing the connection: a newer instance must
/// not be able to knock the client offline with a field change.
pub fn parse_frame(text: &str) -> Result<Option<GatewayEvent>, HarmonyError> {
    let frame: GatewayFrame = serde_json::from_str(text)
        .map_err(|e| HarmonyError::protocol(format!("gateway frame is not JSON: {e}")))?;
    match frame.op {
        op::HEARTBEAT_ACK => Ok(Some(GatewayEvent::HeartbeatAck)),
        op::DISPATCH => {
            let Some(name) = frame.t else {
                return Ok(None);
            };
            let data = frame.d.unwrap_or(Value::Null);
            let event = match name.as_str() {
                "READY" => serde_json::from_value::<ReadyPayload>(data)
                    .map(GatewayEvent::Ready)
                    .map_err(|e| HarmonyError::protocol(format!("READY payload: {e}")))?,
                "MESSAGE_CREATE" => decode(data, GatewayEvent::MessageCreate, &name)?,
                "MESSAGE_UPDATE" => decode(data, GatewayEvent::MessageUpdate, &name)?,
                "MESSAGE_DELETE" => decode(data, GatewayEvent::MessageDelete, &name)?,
                "MESSAGE_REACTION_ADD" => decode(data, GatewayEvent::ReactionAdd, &name)?,
                "MESSAGE_REACTION_REMOVE" => decode(data, GatewayEvent::ReactionRemove, &name)?,
                "SERVER_MEMBER_ADD" => decode(data, GatewayEvent::MemberAdd, &name)?,
                "SERVER_MEMBER_REMOVE" => decode(data, GatewayEvent::MemberRemove, &name)?,
                "SERVER_MEMBER_KICK" => decode(data, GatewayEvent::MemberKick, &name)?,
                "SERVER_BAN_ADD" => decode(data, GatewayEvent::BanAdd, &name)?,
                "SERVER_BAN_REMOVE" => decode(data, GatewayEvent::BanRemove, &name)?,
                "INTERACTION_CREATE" => GatewayEvent::Interaction(data),
                other => GatewayEvent::Unknown {
                    name: other.to_string(),
                    data,
                },
            };
            Ok(Some(event))
        }
        _ => Ok(None),
    }
}

fn decode<T, F>(data: Value, wrap: F, name: &str) -> Result<GatewayEvent, HarmonyError>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> GatewayEvent,
{
    match serde_json::from_value::<T>(data.clone()) {
        Ok(parsed) => Ok(wrap(parsed)),
        Err(err) => {
            tracing::warn!("harmony: could not decode {name}: {err}");
            Ok(GatewayEvent::Unknown {
                name: name.to_string(),
                data,
            })
        }
    }
}

/// Where to connect and with which token.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub url: Url,
    pub token: String,
}

impl GatewayConfig {
    pub fn for_instance(instance: &str, token: impl Into<String>) -> Result<Self, HarmonyError> {
        let url = super::gateway_url(instance)?;
        Ok(Self {
            url,
            token: token.into(),
        })
    }
}

/// What the connection loop reports back to the session.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum GatewayUpdate {
    Connected,
    Event(GatewayEvent),
    /// The socket is gone; `will_reconnect` says whether another attempt follows.
    Closed {
        close: CloseInfo,
        will_reconnect: bool,
        attempt: u32,
    },
    Error(String),
}

/// The looping gateway client. It owns reconnects; the caller only reads
/// [`GatewayUpdate`]s. Stop it by flipping `shutdown` to `true`.
pub struct GatewayClient {
    config: GatewayConfig,
    /// Test hook: connects to `ws://…` fixtures with no TLS expectations.
    connect_timeout: Duration,
}

impl GatewayClient {
    pub fn new(config: GatewayConfig) -> Self {
        Self {
            config,
            connect_timeout: Duration::from_secs(15),
        }
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn url(&self) -> &Url {
        &self.config.url
    }

    /// Run until `shutdown` is set or a close code forbids reconnecting.
    pub async fn run(
        &self,
        updates: mpsc::UnboundedSender<GatewayUpdate>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut attempt: u32 = 0;
        loop {
            if *shutdown.borrow() {
                return;
            }

            let connection = tokio::time::timeout(
                self.connect_timeout,
                tokio_tungstenite::connect_async(self.config.url.as_str()),
            )
            .await;

            let socket = match connection {
                Ok(Ok((socket, _response))) => socket,
                Ok(Err(err)) => {
                    let error = HarmonyError::from(err);
                    let _ = updates.send(GatewayUpdate::Error(error.user_message()));
                    attempt += 1;
                    if !self.sleep_before_retry(&mut shutdown, attempt).await {
                        return;
                    }
                    continue;
                }
                Err(_) => {
                    let _ = updates.send(GatewayUpdate::Error(format!(
                        "timed out connecting to {}",
                        self.config.url
                    )));
                    attempt += 1;
                    if !self.sleep_before_retry(&mut shutdown, attempt).await {
                        return;
                    }
                    continue;
                }
            };

            // Identify immediately: a connection that has not identified within
            // 30 seconds is closed by the gateway.
            let (mut sink, mut stream) = socket.split();
            if let Err(err) = sink
                .send(WsMessage::Text(
                    json!({ "op": op::IDENTIFY, "d": { "token": self.config.token } }).to_string(),
                ))
                .await
            {
                let _ = updates.send(GatewayUpdate::Error(
                    HarmonyError::from(err).user_message(),
                ));
                attempt += 1;
                if !self.sleep_before_retry(&mut shutdown, attempt).await {
                    return;
                }
                continue;
            }

            attempt = 0;
            let _ = updates.send(GatewayUpdate::Connected);

            let close = self
                .pump(&mut sink, &mut stream, &updates, &mut shutdown)
                .await;

            let reconnect = should_reconnect(&close);
            let _ = updates.send(GatewayUpdate::Closed {
                close: close.clone(),
                will_reconnect: reconnect,
                attempt,
            });

            if !reconnect {
                return;
            }
            attempt += 1;
            if !self.sleep_before_retry(&mut shutdown, attempt).await {
                return;
            }
        }
    }

    /// Read frames until the socket closes or `shutdown` flips.
    async fn pump<S, R>(
        &self,
        sink: &mut S,
        stream: &mut R,
        updates: &mpsc::UnboundedSender<GatewayUpdate>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> CloseInfo
    where
        S: futures_util::Sink<WsMessage> + Unpin,
        R: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately; skip it so identify is not followed
        // by an instant heartbeat.
        interval.tick().await;

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let frame = json!({ "op": op::HEARTBEAT }).to_string();
                    if let Err(err) = sink.send(WsMessage::Text(frame)).await {
                        let _ = err;
                        return CloseInfo { code: None, reason: "could not send heartbeat".to_string() };
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        let _ = sink.send(WsMessage::Close(None)).await;
                        return CloseInfo { code: Some(1000), reason: "client shutdown".to_string() };
                    }
                }
                incoming = stream.next() => {
                    match incoming {
                        None => {
                            return CloseInfo { code: None, reason: "socket ended".to_string() };
                        }
                        Some(Err(err)) => {
                            let close = match &err {
                                tokio_tungstenite::tungstenite::Error::Protocol(
                                    tokio_tungstenite::tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                                ) => CloseInfo { code: None, reason: "connection reset".to_string() },
                                _ => CloseInfo { code: None, reason: HarmonyError::from(err).to_string() },
                            };
                            return close;
                        }
                        Some(Ok(WsMessage::Close(frame))) => {
                            let (code, reason) = match frame {
                                Some(frame) => (Some(u16::from(frame.code)), frame.reason.to_string()),
                                None => (None, String::new()),
                            };
                            return CloseInfo { code, reason };
                        }
                        Some(Ok(WsMessage::Text(text))) => {
                            match parse_frame(&text) {
                                Ok(Some(GatewayEvent::Ready(ready))) => {
                                    // Send heartbeats on the interval READY carries.
                                    interval = tokio::time::interval(ready.heartbeat());
                                    interval.set_missed_tick_behavior(
                                        tokio::time::MissedTickBehavior::Delay,
                                    );
                                    interval.tick().await;
                                    let _ = updates.send(GatewayUpdate::Event(GatewayEvent::Ready(ready)));
                                }
                                Ok(Some(event)) => {
                                    let _ = updates.send(GatewayUpdate::Event(event));
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    let _ = updates.send(GatewayUpdate::Error(err.user_message()));
                                }
                            }
                        }
                        Some(Ok(WsMessage::Ping(payload))) => {
                            let _ = sink.send(WsMessage::Pong(payload)).await;
                        }
                        Some(Ok(_)) => {}
                    }
                }
            }
        }
    }

    /// Sleep before a retry. Returns `false` when shutdown was requested while
    /// waiting, so the caller stops instead of reconnecting.
    async fn sleep_before_retry(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        attempt: u32,
    ) -> bool {
        let delay = reconnect_delay(attempt);
        tokio::select! {
            _ = tokio::time::sleep(delay) => true,
            changed = shutdown.changed() => {
                !(changed.is_ok() && *shutdown.borrow())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_codes_follow_the_docs() {
        let close = |code, reason: &str| CloseInfo {
            code: Some(code),
            reason: reason.to_string(),
        };
        assert!(!should_reconnect(&close(4001, "Missing token")));
        assert!(!should_reconnect(&close(4004, "Invalid or expired token")));
        assert!(!should_reconnect(&close(4004, "Token revoked")));
        assert!(should_reconnect(&close(
            4004,
            "Token verification unavailable"
        )));
        assert!(should_reconnect(&close(4003, "Not authenticated")));
        assert!(should_reconnect(&close(4008, "rate limited")));
        assert!(should_reconnect(&close(1000, "Heartbeat timeout")));
        assert!(should_reconnect(&close(4029, "Too many connections")));
        assert!(should_reconnect(&CloseInfo {
            code: None,
            reason: "socket ended".to_string()
        }));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(reconnect_delay(1), Duration::from_secs(1));
        assert_eq!(reconnect_delay(2), Duration::from_secs(2));
        assert_eq!(reconnect_delay(3), Duration::from_secs(4));
        assert_eq!(reconnect_delay(7), Duration::from_secs(60));
        assert_eq!(reconnect_delay(40), Duration::from_secs(60));
    }

    #[test]
    fn parses_ready_and_message_events() {
        let ready = r#"{"op":0,"t":"READY","d":{"bot":{"id":"3f6c1d2e-0000-0000-0000-000000000001","username":"ping-bot"},"session_id":"b1a7e0c4-0000-0000-0000-000000000002","heartbeat_interval":30000,"capabilities":["bridge_emoji_sync"]}}"#;
        match parse_frame(ready).unwrap() {
            Some(GatewayEvent::Ready(payload)) => {
                assert_eq!(payload.bot.username, "ping-bot");
                assert_eq!(payload.heartbeat(), Duration::from_secs(30));
                assert_eq!(payload.capabilities, vec!["bridge_emoji_sync"]);
            }
            other => panic!("unexpected: {other:?}"),
        }

        let create = r#"{"op":0,"t":"MESSAGE_CREATE","d":{"id":"0b9e5a61-0000-0000-0000-000000000003","server_id":"1e7d0000-0000-0000-0000-000000000004","channel_id":"c2d4f8a0-0000-0000-0000-000000000005","thread_id":null,"author":{"id":"8a1f3b77-0000-0000-0000-000000000006","username":"alice","display_name":"Alice","nickname":"Al","bot":false},"content":"!ping","content_raw":[{"type":"text","text":"!ping"}],"is_system":false,"reply_to":null,"timestamp":"2026-10-13T12:00:00.000000+00:00","edited_timestamp":"2026-10-13T12:00:00.000000+00:00","mentions":[],"mention_everyone":false,"metadata":{}}}"#;
        match parse_frame(create).unwrap() {
            Some(GatewayEvent::MessageCreate(message)) => {
                assert_eq!(message.content, "!ping");
                assert_eq!(message.author.unwrap().nickname.as_deref(), Some("Al"));
                assert_eq!(message.content_raw.len(), 1);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn unknown_events_and_opcodes_do_not_fail() {
        assert!(matches!(
            parse_frame(r#"{"op":11}"#).unwrap(),
            Some(GatewayEvent::HeartbeatAck)
        ));
        assert!(parse_frame(r#"{"op":7,"d":{}}"#).unwrap().is_none());
        match parse_frame(r#"{"op":0,"t":"SOMETHING_NEW","d":{"a":1}}"#).unwrap() {
            Some(GatewayEvent::Unknown { name, .. }) => assert_eq!(name, "SOMETHING_NEW"),
            other => panic!("unexpected: {other:?}"),
        }
        // A known name with a payload this build cannot decode is not fatal.
        match parse_frame(r#"{"op":0,"t":"MESSAGE_CREATE","d":{"broken":true}}"#).unwrap() {
            Some(GatewayEvent::Unknown { name, .. }) => assert_eq!(name, "MESSAGE_CREATE"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn malformed_frames_are_reported() {
        assert!(parse_frame("not json").is_err());
    }
}
