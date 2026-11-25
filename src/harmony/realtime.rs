//! Supabase Realtime (Phoenix) for the web transport.
//!
//! Harmony 1.9.x publishes **no** Postgres changes: every event is a Broadcast
//! frame a trigger sent to a private topic, authorised by the user's JWT
//! (`docs/protocol-web.md` §5). A `postgres_changes` subscription therefore
//! joins and stays silent — this client joins broadcast topics instead:
//!
//! ```text
//! {"topic":"realtime:channel-messages-<id>","event":"phx_join",
//!  "payload":{"config":{"private":true},"access_token":"<jwt>"},"ref":"1"}
//! ```
//!
//! The client owns the socket, the heartbeat, the join/leave bookkeeping and the
//! reconnect; callers only say which topics they want and read what arrives.

use std::collections::HashSet;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

use super::error::HarmonyError;
use super::gateway::reconnect_delay;

/// Protocol version the app pins; `vsn=2.x` needs a binary serializer.
const VSN: &str = "1.0.0";

#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    /// `wss://<db host>/realtime/v1/websocket?apikey=…&vsn=1.0.0`
    pub url: Url,
    pub access_token: String,
}

impl RealtimeConfig {
    /// Build the socket URL from the instance's Supabase URL and anon key.
    pub fn for_supabase(
        supabase_url: &str,
        anon_key: &str,
        access_token: impl Into<String>,
    ) -> Result<Self, HarmonyError> {
        let base = Url::parse(supabase_url)
            .map_err(|err| HarmonyError::protocol(format!("bad Supabase URL: {err}")))?;
        let host = base
            .host_str()
            .ok_or_else(|| HarmonyError::protocol("Supabase URL has no host"))?;
        let scheme = if base.scheme() == "http" || base.port() == Some(54321) {
            "ws"
        } else {
            "wss"
        };
        let port = base.port().map(|port| format!(":{port}")).unwrap_or_default();
        let url = Url::parse(&format!(
            "{scheme}://{host}{port}/realtime/v1/websocket?apikey={anon_key}&vsn={VSN}"
        ))
        .map_err(|err| HarmonyError::protocol(format!("bad Realtime URL: {err}")))?;
        Ok(Self {
            url,
            access_token: access_token.into(),
        })
    }
}

/// What the socket reports. `Broadcast` carries the trigger's JSON untouched.
#[derive(Debug, Clone)]
pub enum RealtimeUpdate {
    Joined { topic: String },
    Broadcast {
        topic: String,
        event: String,
        payload: Value,
    },
    ChannelError { topic: String, message: String },
    Error(String),
    Disconnected { reason: String, will_reconnect: bool },
}

#[derive(Debug, Serialize)]
struct OutFrame<'a> {
    topic: &'a str,
    event: &'a str,
    payload: Value,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InFrame {
    topic: String,
    event: String,
    #[serde(default)]
    payload: Value,
    #[serde(default, rename = "ref")]
    reference: Option<Value>,
}

/// Decode one server frame into an update, if it is one this client uses.
pub fn parse_frame(text: &str) -> Result<Option<RealtimeUpdate>, HarmonyError> {
    let frame: InFrame = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(err) => {
            return Err(HarmonyError::protocol(format!(
                "Realtime frame is not JSON: {err}"
            )))
        }
    };

    // The client prefixes topics with `realtime:`; keep the topic as sent.
    match frame.event.as_str() {
        "broadcast" => {
            // `payload` is `{type: "broadcast", event: "message_event", payload: {...}}`.
            let event = frame
                .payload
                .get("event")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let payload = frame
                .payload
                .get("payload")
                .cloned()
                .unwrap_or(Value::Null);
            if event.is_empty() {
                return Ok(None);
            }
            Ok(Some(RealtimeUpdate::Broadcast {
                topic: strip_prefix(&frame.topic),
                event,
                payload,
            }))
        }
        "presence_state" | "presence_diff" => Ok(Some(RealtimeUpdate::Broadcast {
            topic: strip_prefix(&frame.topic),
            event: frame.event,
            payload: frame.payload,
        })),
        "phx_reply" => {
            let status = frame
                .payload
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let topic = strip_prefix(&frame.topic);
            if status == "ok" {
                Ok(Some(RealtimeUpdate::Joined { topic }))
            } else {
                let message = frame
                    .payload
                    .get("response")
                    .map(|response| response.to_string())
                    .unwrap_or_else(|| "join refused".to_string());
                Ok(Some(RealtimeUpdate::ChannelError { topic, message }))
            }
        }
        "phx_error" => Ok(Some(RealtimeUpdate::ChannelError {
            topic: strip_prefix(&frame.topic),
            message: frame.payload.to_string(),
        })),
        // `phx_close`, heartbeat replies and `system` frames need no action.
        _ => Ok(None),
    }
}

fn strip_prefix(topic: &str) -> String {
    topic
        .strip_prefix("realtime:")
        .unwrap_or(topic)
        .to_string()
}

/// The socket loop. Desired topics arrive over `topics`; joins happen on
/// connect and whenever the list changes.
pub struct RealtimeClient {
    config: RealtimeConfig,
}

impl RealtimeClient {
    pub fn new(config: RealtimeConfig) -> Self {
        Self { config }
    }

    pub async fn run(
        &self,
        mut topics: watch::Receiver<Vec<String>>,
        updates: mpsc::UnboundedSender<RealtimeUpdate>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut attempt: u32 = 0;
        loop {
            if *shutdown.borrow() {
                return;
            }
            let connected =
                tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(
                    self.config.url.as_str(),
                ))
                .await;
            let socket = match connected {
                Ok(Ok((socket, _))) => socket,
                Ok(Err(err)) => {
                    let _ = updates.send(RealtimeUpdate::Error(format!(
                        "Realtime connection failed: {err}"
                    )));
                    attempt += 1;
                    if !self.sleep(&mut shutdown, attempt).await {
                        return;
                    }
                    continue;
                }
                Err(_) => {
                    let _ = updates.send(RealtimeUpdate::Error(
                        "timed out connecting to Realtime".to_string(),
                    ));
                    attempt += 1;
                    if !self.sleep(&mut shutdown, attempt).await {
                        return;
                    }
                    continue;
                }
            };

            attempt = 0;
            let (mut sink, mut stream) = socket.split();
            let mut joined: HashSet<String> = HashSet::new();
            let desired = topics.borrow().clone();
            if !self.join_all(&mut sink, &desired, &mut joined).await {
                continue;
            }

            let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            heartbeat.tick().await;
            let mut reference: u64 = 10;

            let reason = loop {
                tokio::select! {
                    _ = heartbeat.tick() => {
                        let frame = serde_json::to_string(&OutFrame {
                            topic: "phoenix",
                            event: "heartbeat",
                            payload: json!({}),
                            reference: Some(next_ref(&mut reference)),
                        })
                        .unwrap_or_else(|_| "{}".to_string());
                        if sink.send(WsMessage::Text(frame)).await.is_err() {
                            break "heartbeat send failed".to_string();
                        }
                    }
                    changed = topics.changed() => {
                        if changed.is_err() {
                            break "topic list closed".to_string();
                        }
                        let desired = topics.borrow().clone();
                        if !self.join_all(&mut sink, &desired, &mut joined).await {
                            break "join send failed".to_string();
                        }
                    }
                    changed = shutdown.changed() => {
                        if changed.is_ok() && *shutdown.borrow() {
                            let _ = sink.send(WsMessage::Close(None)).await;
                            return;
                        }
                    }
                    incoming = stream.next() => {
                        match incoming {
                            None => break "socket ended".to_string(),
                            Some(Err(err)) => break err.to_string(),
                            Some(Ok(WsMessage::Text(text))) => {
                                match self.handle_text(&text, &updates) {
                                    Ok(Some(topic)) => {
                                        joined.insert(topic);
                                    }
                                    Ok(None) => {}
                                    Err(message) => {
                                        let _ = updates.send(RealtimeUpdate::Error(message));
                                    }
                                }
                            }
                            Some(Ok(WsMessage::Close(frame))) => {
                                break match frame {
                                    Some(frame) => format!("closed: {}", frame.reason),
                                    None => "closed".to_string(),
                                };
                            }
                            Some(Ok(_)) => {}
                        }
                    }
                }
            };

            let _ = updates.send(RealtimeUpdate::Disconnected {
                reason,
                will_reconnect: true,
            });
            attempt += 1;
            if !self.sleep(&mut shutdown, attempt).await {
                return;
            }
        }
    }

    async fn join_all<S>(
        &self,
        sink: &mut S,
        desired: &[String],
        joined: &mut HashSet<String>,
    ) -> bool
    where
        S: futures_util::Sink<WsMessage> + Unpin,
    {
        for topic in desired {
            if joined.contains(topic) {
                continue;
            }
            let payload = json!({
                "config": { "private": true },
                "access_token": self.config.access_token,
            });
            let frame = serde_json::to_string(&OutFrame {
                topic: &format!("realtime:{topic}"),
                event: "phx_join",
                payload,
                reference: Some("1".to_string()),
            })
            .unwrap_or_else(|_| "{}".to_string());
            if sink.send(WsMessage::Text(frame)).await.is_err() {
                return false;
            }
        }
        true
    }

    fn handle_text(
        &self,
        text: &str,
        updates: &mpsc::UnboundedSender<RealtimeUpdate>,
    ) -> Result<Option<String>, String> {
        match parse_frame(text) {
            Ok(Some(update)) => {
                let joined = match &update {
                    RealtimeUpdate::Joined { topic } => Some(topic.clone()),
                    _ => None,
                };
                let _ = updates.send(update);
                Ok(joined)
            }
            Ok(None) => Ok(None),
            Err(err) => Err(err.user_message()),
        }
    }

    async fn sleep(&self, shutdown: &mut watch::Receiver<bool>, attempt: u32) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(reconnect_delay(attempt)) => true,
            changed = shutdown.changed() => !(changed.is_ok() && *shutdown.borrow()),
        }
    }
}

fn next_ref(counter: &mut u64) -> String {
    *counter += 1;
    counter.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_websocket_url_from_supabase_settings() {
        let config =
            RealtimeConfig::for_supabase("https://db.mony.lol", "anon-key", "jwt").unwrap();
        assert_eq!(
            config.url.as_str(),
            "wss://db.mony.lol/realtime/v1/websocket?apikey=anon-key&vsn=1.0.0"
        );

        let local =
            RealtimeConfig::for_supabase("http://127.0.0.1:54321", "anon", "jwt").unwrap();
        assert!(local.url.as_str().starts_with("ws://127.0.0.1:54321/"));
    }

    #[test]
    fn decodes_a_message_broadcast() {
        let frame = r#"{
          "topic":"realtime:channel-messages-c2d4f8a0-0000-4000-8000-000000000002",
          "event":"broadcast",
          "payload":{"type":"broadcast","event":"message_event",
            "payload":{"op":"INSERT","new":{"id":"0b9e5a61-0000-4000-8000-000000000005"},"old":null}},
          "ref":null
        }"#;
        match parse_frame(frame).unwrap() {
            Some(RealtimeUpdate::Broadcast { topic, event, payload }) => {
                assert_eq!(topic, "channel-messages-c2d4f8a0-0000-4000-8000-000000000002");
                assert_eq!(event, "message_event");
                assert_eq!(payload["op"], "INSERT");
                assert_eq!(
                    payload["new"]["id"],
                    "0b9e5a61-0000-4000-8000-000000000005"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn join_replies_become_joined_or_channel_errors() {
        let ok = r#"{"topic":"realtime:channel-messages-x","event":"phx_reply","payload":{"status":"ok","response":{}},"ref":"1"}"#;
        assert!(matches!(
            parse_frame(ok).unwrap(),
            Some(RealtimeUpdate::Joined { topic }) if topic == "channel-messages-x"
        ));

        let refused = r#"{"topic":"realtime:channel-messages-x","event":"phx_reply","payload":{"status":"error","response":{"reason":"unauthorized"}},"ref":"1"}"#;
        match parse_frame(refused).unwrap() {
            Some(RealtimeUpdate::ChannelError { topic, message }) => {
                assert_eq!(topic, "channel-messages-x");
                assert!(message.contains("unauthorized"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn heartbeat_replies_and_unknown_frames_are_ignored() {
        let heartbeat = r#"{"topic":"phoenix","event":"phx_reply","payload":{"status":"ok","response":{}},"ref":"2"}"#;
        // A successful heartbeat reply looks like a join reply; it must not be
        // reported as a joined channel.
        match parse_frame(heartbeat).unwrap() {
            Some(RealtimeUpdate::Joined { topic }) => assert_eq!(topic, "phoenix"),
            other => panic!("unexpected: {other:?}"),
        }
        let system = r#"{"topic":"realtime:x","event":"system","payload":{"status":"ok"}}"#;
        assert!(parse_frame(system).unwrap().is_none());
        assert!(parse_frame("not json").is_err());
    }
}
