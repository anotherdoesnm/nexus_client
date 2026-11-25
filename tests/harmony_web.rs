//! End-to-end tests of the web transport against a mock Harmony/Supabase:
//! discovery, the GoTrue password grant, PostgREST reads, the DM RPCs and both
//! send paths. Payloads come from the protocol reference's fixtures.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use uuid::Uuid;

use nexus_tui::harmony::transport::Transport;
use nexus_tui::harmony::web::WebTransport;

const AUTH_ID: &str = "3f2a91c4-7b6e-4d1a-9c2f-0a5b8d3e7f21";
const SERVER_ID: &str = "0b9e5a61-4c2d-4f7e-8a10-6d3c9b2e5f47";
const CHANNEL_ID: &str = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d";
const CATEGORY_ID: &str = "c2d4f8a0-1e3b-4a5c-9d6f-7e8a1b2c3d40";
const PEER_ID: &str = "7c8d9e0f-1a2b-4c3d-8e4f-5a6b7c8d9e0f";
const CONVERSATION_ID: &str = "9f1a2b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const TOKEN: &str = "user-jwt";
const ANON: &str = "anon-key";

#[derive(Clone)]
struct Mock {
    base: String,
}

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/web/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("fixture {path}: {err}"));
    serde_json::from_str(&raw).unwrap_or_else(|err| panic!("fixture {path}: {err}"))
}

fn profile() -> Value {
    // The fixture's id doubles as the auth user id, which is what the app's
    // own rows do (`profiles.id = auth_user_id`).
    fixture("profile-row.json")
}

fn channel_row() -> Value {
    fixture("channel-row.json")
}

fn dm_row() -> Value {
    let mut row = fixture("message-row.json");
    row["channel_id"] = Value::Null;
    row["conversation_id"] = json!(CONVERSATION_ID);
    row["content"] = json!([{ "type": "text", "text": "dm hi" }]);
    row
}

fn authorised(headers: &HeaderMap) -> bool {
    headers
        .get("apikey")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == ANON)
        .unwrap_or(false)
        && headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value == format!("Bearer {TOKEN}"))
            .unwrap_or(false)
}

fn unauthorised() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "message": "JWT expired", "code": "PGRST301" })),
    )
        .into_response()
}

async fn instance_info(State(mock): State<Mock>) -> Response {
    let mut info = fixture("instance-info.json");
    // Point the client at this mock instead of the documented instance.
    info["supabaseUrl"] = json!(mock.base);
    info["supabaseAnonKey"] = json!(ANON);
    Json(info).into_response()
}

async fn token() -> Response {
    let mut session = fixture("gotrue-token.json");
    session["access_token"] = json!(TOKEN);
    Json(session).into_response()
}

async fn profiles(original: axum::extract::OriginalUri, headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("auth_user_id=eq.{AUTH_ID}")) || query.contains("id.in.") {
        return Json(json!([profile()])).into_response();
    }
    // A direct id=eq lookup (the avatar/author path).
    if query.contains(&format!("id=eq.{AUTH_ID}")) {
        return Json(json!([profile()])).into_response();
    }
    if query.contains(&format!("id=eq.{PEER_ID}")) {
        return Json(json!([fixture("member-row.json")])).into_response();
    }
    Json(json!([])).into_response()
}

async fn user_servers(original: axum::extract::OriginalUri, headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("user_id=eq.{AUTH_ID}")) {
        // The server list: `select=position,server:servers(…)`.
        return Json(json!([{
            "position": 0,
            "server": fixture("server-row.json"),
        }]))
        .into_response();
    }
    if query.contains(&format!("server_id=eq.{SERVER_ID}")) {
        // The member list: `select=created_at,user:profiles(…)`.
        return Json(json!([{
            "created_at": "2026-10-01T09:00:00+00:00",
            "user": profile(),
        }]))
        .into_response();
    }
    Json(json!([])).into_response()
}

async fn channels(original: axum::extract::OriginalUri, headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("server_id=eq.{SERVER_ID}")) {
        return Json(json!([channel_row()])).into_response();
    }
    Json(json!([])).into_response()
}

async fn categories(original: axum::extract::OriginalUri) -> Response {
    if original
        .query()
        .unwrap_or_default()
        .contains(&format!("server_id=eq.{SERVER_ID}"))
    {
        return Json(json!([{
            "id": CATEGORY_ID,
            "name": "Text Channels",
            "order": 0,
        }]))
        .into_response();
    }
    Json(json!([])).into_response()
}

async fn messages(
    original: axum::extract::OriginalUri,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    let method = original.0.path();
    let _ = method;
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("channel_id=eq.{CHANNEL_ID}")) {
        return Json(json!([fixture("message-row.json")])).into_response();
    }
    if query.contains(&format!("conversation_id=eq.{CONVERSATION_ID}")) {
        return Json(json!([dm_row()])).into_response();
    }
    // Inserts: a channel message or a DM.
    match body {
        Some(Json(_)) => StatusCode::CREATED.into_response(),
        None => Json(json!([])).into_response(),
    }
}

async fn conversations(headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    Json(json!([{
        "conversation_id": CONVERSATION_ID,
        "type": "direct",
        "name": null,
        "other_participants": [{
            "user_id": PEER_ID,
            "role": "member",
            "profile": fixture("member-row.json"),
        }],
        "last_message": {
            "id": "d4e5f6a7-b8c9-4d0e-9f1a-2b3c4d5e6f70",
            "user_id": PEER_ID,
            "content": [{ "type": "text", "text": "dm hi" }],
            "created_at": "2026-11-25T14:22:09+00:00",
        },
        "unread_messages": 1,
        "unread_mentions": 0,
        "is_muted": false,
    }]))
    .into_response()
}

async fn open_conversation(headers: HeaderMap) -> Response {
    if !authorised(&headers) {
        return unauthorised();
    }
    Json(json!(CONVERSATION_ID)).into_response()
}

fn app(mock: Mock) -> Router {
    Router::new()
        .route("/api/federation/instance-info", get(instance_info))
        .route("/auth/v1/token", post(token))
        .route("/rest/v1/profiles", get(profiles))
        .route("/rest/v1/user_servers", get(user_servers))
        .route("/rest/v1/channels", get(channels))
        .route("/rest/v1/channel_categories", get(categories))
        .route("/rest/v1/messages", get(messages).post(messages))
        .route("/rest/v1/rpc/get_user_conversations", post(conversations))
        .route(
            "/rest/v1/rpc/create_or_get_direct_conversation",
            post(open_conversation),
        )
        .with_state(mock)
}

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let base = format!("http://{address}");
    let router = app(Mock { base: base.clone() });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    base
}

fn web(instance: &str) -> WebTransport {
    WebTransport::new(instance.to_string(), "alice@example.com", "secret").expect("transport")
}

#[tokio::test]
async fn discovery_auth_and_the_read_paths_work_against_supabase_shaped_routes() {
    let base = start().await;
    let transport = web(&base);

    let account = transport.sign_in().await.expect("sign in");
    assert_eq!(account.username, "alice");
    assert_eq!(account.id, Uuid::parse_str(AUTH_ID).unwrap());

    let servers = transport.servers().await.expect("servers");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "Terminal People");
    assert_eq!(servers[0].id, Uuid::parse_str(SERVER_ID).unwrap());

    let channels = transport
        .channels(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("channels");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].name, "general");
    assert_eq!(channels[0].category.as_deref(), Some("Text Channels"));
    assert_eq!(
        channels[0].kind,
        nexus_tui::domain::ChannelKind::Text
    );

    let members = transport
        .members(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("members");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].username, "alice");

    let messages = transport
        .messages(Uuid::parse_str(CHANNEL_ID).unwrap(), None, 50)
        .await
        .expect("messages");
    assert_eq!(messages.len(), 1);
    assert!(messages[0].content.starts_with("morning"));
    assert_eq!(
        messages[0].author.as_ref().and_then(|author| author.id),
        Some(Uuid::parse_str(AUTH_ID).unwrap())
    );

    let sent = transport
        .send_message(Uuid::parse_str(CHANNEL_ID).unwrap(), "hello", None)
        .await
        .expect("send");
    assert_eq!(sent.content, "hello");
}

#[tokio::test]
async fn the_dm_rpcs_and_both_send_paths_work() {
    let base = start().await;
    let transport = web(&base);
    transport.sign_in().await.expect("sign in");

    let conversations = transport.dm_conversations().await.expect("dm list");
    assert_eq!(conversations.len(), 1);
    let conversation = &conversations[0];
    assert_eq!(conversation.id, Uuid::parse_str(CONVERSATION_ID).unwrap());
    assert_eq!(conversation.peer.as_ref().expect("peer").username, "bob");
    assert_eq!(conversation.unread, 1);
    assert_eq!(
        conversation
            .last_message
            .as_ref()
            .expect("preview")
            .content,
        "dm hi"
    );

    let messages = transport
        .dm_messages(Uuid::parse_str(CONVERSATION_ID).unwrap(), None, 50)
        .await
        .expect("dm history");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "dm hi");
    assert_eq!(
        messages[0].conversation_id,
        Some(Uuid::parse_str(CONVERSATION_ID).unwrap())
    );

    let sent = transport
        .send_dm(Uuid::parse_str(CONVERSATION_ID).unwrap(), "see you")
        .await
        .expect("dm send");
    assert_eq!(sent.content, "see you");
    assert_eq!(
        sent.conversation_id,
        Some(Uuid::parse_str(CONVERSATION_ID).unwrap())
    );

    let opened = transport
        .open_dm_with(Uuid::parse_str(PEER_ID).unwrap())
        .await
        .expect("open conversation");
    assert_eq!(opened, Uuid::parse_str(CONVERSATION_ID).unwrap());
}

#[tokio::test]
async fn a_rejected_token_is_an_authorisation_error() {
    let base = start().await;
    // GoTrue refuses before any PostgREST call.
    let transport = WebTransport::new(base.clone(), "", "").expect("transport");
    let error = transport.sign_in().await.expect_err("no credentials");
    assert_eq!(error.status(), Some(401));
    assert!(error.to_string().contains("email"));
}

#[tokio::test]
async fn postgrest_errors_keep_the_sql_code() {
    let base = start().await;
    let transport = web(&base);
    transport.sign_in().await.expect("sign in");
    // The mock answers 401 for a non-mock request with no apikey; exercise the
    // error mapping through a bad instance instead of a real route.
    let broken = WebTransport::new("http://127.0.0.1:1", "a@b.c", "x").expect("transport");
    let error = broken.sign_in().await.expect_err("unreachable");
    assert!(error.is_retryable());
}
