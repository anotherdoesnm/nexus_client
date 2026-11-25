//! The web transport's live path: a mock Supabase Realtime socket that checks
//! the join frame the docs require (private topic + user JWT) and then delivers
//! a real `message_event` broadcast, built from the spec's fixture.

use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use url::Url;

use nexus_tui::harmony::realtime::{RealtimeClient, RealtimeConfig, RealtimeUpdate};

const CHANNEL_ID: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
const TOKEN: &str = "user-jwt";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/web/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("fixture {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

async fn gateway(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(run_socket)
}

async fn run_socket(mut socket: WebSocket) {
    // The join must be the first frame, on the channel's topic, private, and
    // carrying the user's token (a public join would be silently dropped).
    let join = match socket.recv().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str::<Value>(&text).expect("join JSON"),
        other => panic!("expected a join frame, got {other:?}"),
    };
    assert_eq!(join["event"], "phx_join");
    assert_eq!(join["topic"], format!("realtime:channel-messages-{CHANNEL_ID}"));
    assert_eq!(join["payload"]["config"]["private"], true);
    assert_eq!(join["payload"]["access_token"], TOKEN);

    let reply = json!({
        "topic": join["topic"],
        "event": "phx_reply",
        "payload": { "status": "ok", "response": {} },
        "ref": join["ref"],
    });
    let _ = socket.send(Message::Text(reply.to_string())).await;

    // Then one broadcast, exactly as a trigger sends it: `payload.event` names
    // the trigger event and `payload.payload` is the trigger's JSON.
    let broadcast = json!({
        "topic": join["topic"],
        "event": "broadcast",
        "payload": {
            "type": "broadcast",
            "event": "message_event",
            "payload": fixture("realtime-message-event.json"),
        },
        "ref": null,
    });
    let _ = socket.send(Message::Text(broadcast.to_string())).await;

    // Hold the socket open until the client goes away.
    while let Some(Ok(message)) = socket.recv().await {
        if matches!(message, Message::Close(_)) {
            return;
        }
    }
}

#[tokio::test]
async fn realtime_joins_privately_and_delivers_message_events() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let router = Router::new().route("/realtime/v1/websocket", get(gateway));
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let config = RealtimeConfig {
        url: Url::parse(&format!(
            "ws://{address}/realtime/v1/websocket?apikey=anon&vsn=1.0.0"
        ))
        .expect("url"),
        access_token: TOKEN.to_string(),
    };
    let client = RealtimeClient::new(config);
    let (topics_tx, topics_rx) = watch::channel(vec![format!("channel-messages-{CHANNEL_ID}")]);
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { client.run(topics_rx, updates_tx, shutdown_rx).await });

    let mut joined = false;
    let mut broadcast = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), updates_rx.recv()).await {
            Ok(Some(RealtimeUpdate::Joined { topic })) => {
                assert_eq!(topic, format!("channel-messages-{CHANNEL_ID}"));
                joined = true;
            }
            Ok(Some(RealtimeUpdate::Broadcast {
                topic,
                event,
                payload,
            })) => {
                broadcast = Some((topic, event, payload));
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    let _ = shutdown_tx.send(true);
    let _ = topics_tx.send(Vec::new());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

    assert!(joined, "the join reply must be reported");
    let (topic, event, payload) = broadcast.expect("a broadcast");
    assert_eq!(topic, format!("channel-messages-{CHANNEL_ID}"));
    assert_eq!(event, "message_event");
    assert_eq!(payload["op"], "INSERT");
    let new = &payload["new"];
    assert_eq!(new["channel_id"], CHANNEL_ID);
    // The payload is the trigger's 17-key whitelist: content is present,
    // attachments/media_paths are not.
    assert!(new["content"].is_array());
    assert!(new.get("media_paths").is_none());
}
