//! End-to-end tests of the bot transport against a mock Harmony instance:
//! the REST routes and the gateway, both shaped by the fixtures the protocol
//! reference uses (`../fixtures/bot/*.json`). No real instance is touched.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use nexus_tui::harmony::{
    bot::BotTransport,
    gateway::{GatewayClient, GatewayConfig, GatewayUpdate},
    rest::RetryPolicy,
    transport::{Transport, TransportEvent},
};

const SERVER_ID: &str = "1e7d0000-0000-4000-8000-000000000001";
const TEXT_CHANNEL_ID: &str = "c2d4f8a0-0000-4000-8000-000000000002";
const VOICE_CHANNEL_ID: &str = "7d1e9b30-0000-4000-8000-000000000009";
const TOKEN: &str = "harmony_bot_test";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/bot/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("fixture {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("fixture {path} is not JSON: {err}"))
}

/// A tiny PNG, enough for the image pipeline to see a magic number.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89,
];

#[derive(Clone)]
struct Mock {
    connections: Arc<AtomicUsize>,
    /// Close the first N gateway sockets with this code/reason.
    close_first: Arc<AtomicUsize>,
    close_code: u16,
    close_reason: String,
}

impl Mock {
    fn new() -> Self {
        Self {
            connections: Arc::new(AtomicUsize::new(0)),
            close_first: Arc::new(AtomicUsize::new(0)),
            close_code: 1000,
            close_reason: "Heartbeat timeout".to_string(),
        }
    }
}

fn authorised(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == format!("Bot {TOKEN}"))
        .unwrap_or(false)
}

fn unauthorised() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "Invalid or expired token" })),
    )
        .into_response()
}

async fn me(headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    Json(json!({
        "id": "3f6c1d2e-0000-4000-8000-000000000004",
        "username": "ping-bot",
        "discriminator": "0001",
        "avatar": "http://127.0.0.1/avatar.png",
        "bot": true,
        "verified": true,
        "public": false
    }))
    .into_response()
}

async fn server() -> Response {
    Json(fixture("server.json")).into_response()
}

async fn channels() -> Response {
    let general = fixture("channel.json");
    Json(json!([
        general,
        {
            "id": VOICE_CHANNEL_ID,
            "type": 2,
            "guild_id": SERVER_ID,
            "name": "voice chat",
            "position": 1,
            "order": 1,
            "parent_id": null,
            "category_id": null
        }
    ]))
    .into_response()
}

async fn categories() -> Response {
    Json(json!([
        { "id": "a0a00000-0000-4000-8000-00000000000a", "server_id": SERVER_ID, "name": "Text Channels", "order": 0 }
    ]))
    .into_response()
}

async fn roles() -> Response {
    Json(json!([fixture("role.json")])).into_response()
}

async fn members() -> Response {
    Json(fixture("member-list-page.json")).into_response()
}

async fn messages() -> Response {
    Json(json!([fixture("message-object.json")])).into_response()
}

async fn send_message(Json(body): Json<Value>) -> Response {
    let content = body
        .get("content")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    (
        StatusCode::CREATED,
        Json(json!({
            "id": "d7f1c3a2-0000-4000-8000-00000000000d",
            "channel_id": TEXT_CHANNEL_ID,
            "thread_id": null,
            "author": { "id": "3f6c1d2e-0000-4000-8000-000000000004", "username": "ping-bot", "display_name": "Ping Bot", "bot": true },
            "content": content,
            "reply_to": null,
            "timestamp": "2026-10-13T12:00:05.000000+00:00",
            "edited_timestamp": "2026-10-13T12:00:05.000000+00:00",
            "mentions": [],
            "is_system": false,
            "metadata": {}
        })),
    )
        .into_response()
}

async fn image() -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/png")],
        PNG.to_vec(),
    )
        .into_response()
}

async fn gateway(ws: WebSocketUpgrade, State(mock): State<Mock>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run_socket(socket, mock))
}

async fn run_socket(mut socket: WebSocket, mock: Mock) {
    let connection = mock.connections.fetch_add(1, Ordering::SeqCst) + 1;
    let outstanding = mock.close_first.load(Ordering::SeqCst);
    let should_close = outstanding > 0;
    if should_close {
        mock.close_first.fetch_sub(1, Ordering::SeqCst);
    }

    // Identify must be the first frame.
    match socket.recv().await {
        Some(Ok(Message::Text(text))) => {
            let frame: Value = serde_json::from_str(&text).expect("identify is JSON");
            assert_eq!(frame["op"], 2, "first frame must be identify");
            assert_eq!(
                frame["d"]["token"], TOKEN,
                "identify must carry the bot token"
            );
        }
        other => panic!("expected identify, got {other:?}"),
    }

    if should_close {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: mock.close_code,
                reason: mock.close_reason.clone().into(),
            })))
            .await;
        return;
    }

    // READY, then one message event, both straight from the fixtures.
    let ready = fixture("ready.json");
    let _ = socket.send(Message::Text(ready.to_string())).await;

    let create = fixture("message-create-event.json");
    let _ = socket.send(Message::Text(create.to_string())).await;

    // Hold the socket open until the client hangs up.
    while let Some(Ok(message)) = socket.recv().await {
        if matches!(message, Message::Close(_)) {
            return;
        }
    }
    let _ = connection;
}

fn app(mock: Mock) -> Router {
    Router::new()
        .route("/bot-gateway/api/v1/users/@me", get(me))
        .route("/bot-gateway/api/v1/servers/:server", get(server))
        .route("/bot-gateway/api/v1/servers/:server/channels", get(channels))
        .route("/bot-gateway/api/v1/servers/:server/categories", get(categories))
        .route("/bot-gateway/api/v1/servers/:server/roles", get(roles))
        .route("/bot-gateway/api/v1/servers/:server/members", get(members))
        .route("/bot-gateway/api/v1/channels/:channel/messages", get(messages).post(send_message))
        .route("/bot-gateway/gateway", get(gateway))
        .route("/avatar.png", get(image))
        .with_state(mock)
}

/// Start the mock on an ephemeral port; returns the base URL and the mock.
async fn start(mock: Mock) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock server");
    let address = listener.local_addr().expect("local addr");
    let router = app(mock);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{address}")
}

fn bot(instance: &str, token: &str) -> BotTransport {
    BotTransport::with_policy(
        instance.to_string(),
        token.to_string(),
        vec![Uuid::parse_str(SERVER_ID).expect("server id")],
        RetryPolicy::none(),
    )
    .expect("transport")
}

#[tokio::test]
async fn rest_reads_map_onto_the_domain_model() {
    let instance = start(Mock::new()).await;
    let transport = bot(&instance, TOKEN);

    let account = transport.sign_in().await.expect("sign in");
    assert_eq!(account.username, "ping-bot");
    assert!(account.bot);

    let servers = transport.servers().await.expect("servers");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "Lounge");
    assert_eq!(servers[0].member_count, Some(3));

    let channels = transport
        .channels(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("channels");
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0].name, "general");
    assert_eq!(channels[0].position, 0);
    assert_eq!(channels[1].name, "voice chat");
    assert_eq!(channels[1].kind, nexus_tui::domain::ChannelKind::Voice);

    let members = transport
        .members(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("members");
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].username, "alice");
    assert_eq!(members[0].nickname.as_deref(), Some("Al"));
    assert_eq!(members[0].shown_name(), "Al");
    assert!(!members[0].is_admin, "the Moderator role is not an admin role");
    assert_eq!(members[1].username, "bob");

    let messages = transport
        .messages(Uuid::parse_str(TEXT_CHANNEL_ID).unwrap(), None, 50)
        .await
        .expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "Pong!");
    assert_eq!(messages[0].author.as_ref().unwrap().username, "ping-bot");
    assert_eq!(messages[0].timestamp, 1_791_892_802);

    let sent = transport
        .send_message(Uuid::parse_str(TEXT_CHANNEL_ID).unwrap(), "hello", None)
        .await
        .expect("send");
    assert_eq!(sent.content, "hello");
    assert_eq!(sent.channel_id, Uuid::parse_str(TEXT_CHANNEL_ID).unwrap());

    let image = transport
        .fetch_image(&format!("{instance}/avatar.png"))
        .await
        .expect("image")
        .expect("image bytes");
    assert!(image.starts_with("data:image/png;base64,"));
}

#[tokio::test]
async fn a_bad_token_is_reported_not_retried() {
    let instance = start(Mock::new()).await;
    let transport = bot(&instance, "harmony_bot_wrong");
    let error = transport.sign_in().await.expect_err("must fail");
    assert_eq!(error.status(), Some(401));
    assert!(!error.is_retryable());
    assert!(error.user_message().contains("rejected the token"));
}

#[tokio::test]
async fn gateway_delivers_ready_and_message_events() {
    let mock = Mock::new();
    let instance = start(mock).await;
    let url = url::Url::parse(&format!(
        "{}/bot-gateway/gateway",
        instance.replacen("http://", "ws://", 1)
    ))
    .expect("ws url");

    let client = GatewayClient::new(GatewayConfig {
        url,
        token: TOKEN.to_string(),
    });
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { client.run(updates_tx, shutdown_rx).await });

    let mut saw_connected = false;
    let mut saw_ready = false;
    let mut saw_message = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let update = tokio::time::timeout(Duration::from_secs(2), updates_rx.recv())
            .await
            .expect("gateway update")
            .expect("channel open");
        match update {
            GatewayUpdate::Connected => saw_connected = true,
            GatewayUpdate::Event(nexus_tui::harmony::gateway::GatewayEvent::Ready(ready)) => {
                assert_eq!(ready.bot.username, "ping-bot");
                assert_eq!(ready.heartbeat(), Duration::from_secs(30));
                saw_ready = true;
            }
            GatewayUpdate::Event(nexus_tui::harmony::gateway::GatewayEvent::MessageCreate(
                message,
            )) => {
                saw_message = Some(message);
                break;
            }
            _ => {}
        }
    }
    let _ = shutdown_tx.send(true);
    let _ = handle.await;

    assert!(saw_connected);
    assert!(saw_ready);
    let message = saw_message.expect("a MESSAGE_CREATE");
    assert_eq!(message.content, "!ping");
    assert_eq!(message.author.as_ref().unwrap().nickname.as_deref(), Some("Al"));
}

#[tokio::test]
async fn a_revocable_close_code_stops_the_gateway() {
    let mock = Mock {
        close_first: Arc::new(AtomicUsize::new(1)),
        close_code: 4004,
        close_reason: "Invalid or expired token".to_string(),
        ..Mock::new()
    };
    let connections = mock.connections.clone();
    let instance = start(mock).await;
    let url = url::Url::parse(&format!(
        "{}/bot-gateway/gateway",
        instance.replacen("http://", "ws://", 1)
    ))
    .expect("ws url");

    let client = GatewayClient::new(GatewayConfig {
        url,
        token: TOKEN.to_string(),
    });
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { client.run(updates_tx, shutdown_rx).await });

    let mut stopped = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), updates_rx.recv()).await {
            Ok(Some(GatewayUpdate::Closed {
                will_reconnect,
                close,
                ..
            })) => {
                assert_eq!(close.code, Some(4004));
                assert!(!will_reconnect);
                stopped = true;
                break;
            }
            Ok(None) => break,
            Ok(Some(_)) => continue,
            Err(_) => break,
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;
    assert!(stopped, "a 4004 close must be reported as final");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "a revoked token must not be retried"
    );
}

#[tokio::test]
async fn a_transient_close_reconnects() {
    let mock = Mock {
        close_first: Arc::new(AtomicUsize::new(1)),
        close_code: 1000,
        close_reason: "Heartbeat timeout".to_string(),
        ..Mock::new()
    };
    let connections = mock.connections.clone();
    let instance = start(mock).await;
    let url = url::Url::parse(&format!(
        "{}/bot-gateway/gateway",
        instance.replacen("http://", "ws://", 1)
    ))
    .expect("ws url");

    let client = GatewayClient::new(GatewayConfig {
        url,
        token: TOKEN.to_string(),
    });
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(async move { client.run(updates_tx, shutdown_rx).await });

    let mut connected = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while tokio::time::Instant::now() < deadline && connected < 2 {
        match tokio::time::timeout(Duration::from_secs(3), updates_rx.recv()).await {
            Ok(Some(GatewayUpdate::Connected)) => connected += 1,
            Ok(None) => break,
            Ok(Some(_)) => continue,
            Err(_) => break,
        }
    }
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(3), handle).await;

    assert_eq!(connected, 2, "the second connection must be identified");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn transport_events_reach_the_session_shape() {
    let instance = start(Mock::new()).await;
    let transport = bot(&instance, TOKEN);
    let mut events = transport.take_events().expect("events");
    transport.sign_in().await.expect("sign in");

    let mut saw_connected = false;
    let mut saw_message = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
            Ok(Some(TransportEvent::Connected)) => saw_connected = true,
            Ok(Some(TransportEvent::MessageCreate(message))) => {
                saw_message = Some(message);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert!(saw_connected);
    assert_eq!(saw_message.expect("message").content, "!ping");
}
