//! End-to-end tests of the web transport against a mock Harmony/Supabase:
//! discovery, the GoTrue password grant, an imported (SSO) session with token
//! refresh, PostgREST reads, the DM RPCs and both send paths. Payloads come
//! from the protocol reference's fixtures.

use axum::{
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::Engine;
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
const ANON: &str = "anon-key";
const REFRESH: &str = "refresh-token-1";

#[derive(Clone)]
struct Mock {
    base: String,
    /// A token the mock accepts (also the one the refresh grant hands out).
    valid: String,
    /// A token the mock rejects with 401, standing in for an expired session.
    expired: String,
}

fn b64(value: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
}

/// A JWT with the claims the transport reads: `sub` and `exp`. The signature is
/// never checked by the client (the server checks it), so its bytes are filler.
fn jwt(sub: &str, exp: i64) -> String {
    let header = b64(&json!({ "alg": "HS256", "typ": "JWT" }));
    let payload = b64(&json!({
        "sub": sub,
        "role": "authenticated",
        "aud": "authenticated",
        "exp": exp,
    }));
    format!("{header}.{payload}.c2lnbmF0dXJl")
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

fn authorised(headers: &HeaderMap, mock: &Mock) -> bool {
    // `mock.expired` is deliberately refused: it is the token the refresh path
    // must replace before any read succeeds.
    let _ = &mock.expired;
    headers
        .get("apikey")
        .and_then(|value| value.to_str().ok())
        .map(|value| value == ANON)
        .unwrap_or(false)
        && headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(|value| value == format!("Bearer {}", mock.valid))
            .unwrap_or(false)
}

fn unauthorised() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "message": "JWT expired", "code": "PGRST301" })),
    )
        .into_response()
}

fn profile() -> Value {
    // The fixture's id doubles as the auth user id, as the app's own rows do
    // (`profiles.id = auth_user_id`).
    fixture("profile-row.json")
}

async fn instance_info(State(mock): State<Mock>) -> Response {
    let mut info = fixture("instance-info.json");
    info["supabaseUrl"] = json!(mock.base);
    info["supabaseAnonKey"] = json!(ANON);
    Json(info).into_response()
}

async fn settings(State(_mock): State<Mock>) -> Response {
    Json(json!({
        "external": { "email": true, "github": true, "google": false },
        "disable_signup": false,
    }))
    .into_response()
}

/// Password grant, refresh grant, and the SSO refusal the hint is about.
async fn token(State(mock): State<Mock>, body: Option<Json<Value>>) -> Response {
    let body = body.map(|Json(value)| value).unwrap_or(Value::Null);
    if let Some(refresh) = body.get("refresh_token").and_then(|value| value.as_str()) {
        if refresh == REFRESH {
            return Json(json!({
                "access_token": mock.valid,
                "refresh_token": REFRESH,
                "user": { "id": AUTH_ID },
            }))
            .into_response();
        }
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant", "error_description": "Invalid Refresh Token" })),
        )
            .into_response();
    }
    if body
        .get("password")
        .and_then(|value| value.as_str())
        .map(|password| password == "sso")
        .unwrap_or(false)
    {
        // What GoTrue says for an account created through GitHub.
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant", "error_description": "Invalid login credentials" })),
        )
            .into_response();
    }
    Json(json!({
        "access_token": mock.valid,
        "refresh_token": REFRESH,
        "user": { "id": AUTH_ID },
    }))
    .into_response()
}

async fn profiles(
    State(mock): State<Mock>,
    original: axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !authorised(&headers, &mock) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("auth_user_id=eq.{AUTH_ID}"))
        || query.contains("id.in.")
        || query.contains(&format!("id=eq.{AUTH_ID}"))
    {
        return Json(json!([profile()])).into_response();
    }
    if query.contains(&format!("id=eq.{PEER_ID}")) {
        return Json(json!([fixture("member-row.json")])).into_response();
    }
    Json(json!([])).into_response()
}

async fn user_servers(
    State(mock): State<Mock>,
    original: axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !authorised(&headers, &mock) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    if query.contains(&format!("user_id=eq.{AUTH_ID}")) {
        return Json(json!([{
            "position": 0,
            "server": fixture("server-row.json"),
        }]))
        .into_response();
    }
    if query.contains(&format!("server_id=eq.{SERVER_ID}")) {
        return Json(json!([{
            "created_at": "2026-10-01T09:00:00+00:00",
            "user": profile(),
        }]))
        .into_response();
    }
    Json(json!([])).into_response()
}

async fn channels(
    State(mock): State<Mock>,
    original: axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !authorised(&headers, &mock) {
        return unauthorised();
    }
    if original
        .query()
        .unwrap_or_default()
        .contains(&format!("server_id=eq.{SERVER_ID}"))
    {
        return Json(json!([fixture("channel-row.json")])).into_response();
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
    State(mock): State<Mock>,
    original: OriginalUri,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    if !authorised(&headers, &mock) {
        return unauthorised();
    }
    let query = original.query().unwrap_or_default();
    match body {
        // Inserts return the stored row (`Prefer: return=representation`), so
        // the client gets the real id and content back.
        Some(Json(body)) => {
            let mut row = fixture("message-row.json");
            row["content"] = body
                .get("content")
                .cloned()
                .unwrap_or_else(|| json!([]));
            row["user_id"] = body.get("user_id").cloned().unwrap_or(Value::Null);
            row["channel_id"] = body.get("channel_id").cloned().unwrap_or(Value::Null);
            row["conversation_id"] = body
                .get("conversation_id")
                .cloned()
                .unwrap_or(Value::Null);
            (StatusCode::CREATED, Json(json!([row]))).into_response()
        }
        None if query.contains(&format!("channel_id=eq.{CHANNEL_ID}")) => {
            Json(json!([fixture("message-row.json")])).into_response()
        }
        None if query.contains(&format!("conversation_id=eq.{CONVERSATION_ID}")) => {
            let mut row = fixture("message-row.json");
            row["channel_id"] = Value::Null;
            row["conversation_id"] = json!(CONVERSATION_ID);
            row["content"] = json!([{ "type": "text", "text": "dm hi" }]);
            Json(json!([row])).into_response()
        }
        None => Json(json!([])).into_response(),
    }
}

async fn conversations(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &mock) {
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

async fn open_conversation(State(mock): State<Mock>, headers: HeaderMap) -> Response {
    if !authorised(&headers, &mock) {
        return unauthorised();
    }
    Json(json!(CONVERSATION_ID)).into_response()
}

fn app(mock: Mock) -> Router {
    Router::new()
        .route("/api/federation/instance-info", get(instance_info))
        .route("/auth/v1/settings", get(settings))
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

async fn start() -> (String, String, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("address");
    let base = format!("http://{address}");
    let valid = jwt(AUTH_ID, 2_082_758_400);
    let expired = jwt(AUTH_ID, 1_000_000_000);
    let router = app(Mock {
        base: base.clone(),
        valid: valid.clone(),
        expired: expired.clone(),
    });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (base, valid, expired)
}

fn web(instance: &str) -> WebTransport {
    WebTransport::new(instance.to_string(), "alice@example.com", "secret").expect("transport")
}

#[tokio::test]
async fn discovery_auth_and_the_read_paths_work_against_supabase_shaped_routes() {
    let (base, _, _) = start().await;
    let transport = web(&base);

    let account = transport.sign_in().await.expect("sign in");
    assert_eq!(account.username, "alice");
    assert_eq!(account.id, Uuid::parse_str(AUTH_ID).unwrap());

    let servers = transport.servers().await.expect("servers");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "Terminal People");

    let channels = transport
        .channels(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("channels");
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].name, "general");
    assert_eq!(channels[0].category.as_deref(), Some("Text Channels"));

    let members = transport
        .members(Uuid::parse_str(SERVER_ID).unwrap())
        .await
        .expect("members");
    assert_eq!(members.len(), 1);

    let history = transport
        .messages(Uuid::parse_str(CHANNEL_ID).unwrap(), None, 50)
        .await
        .expect("messages");
    assert_eq!(history.len(), 1);
    assert!(history[0].content.starts_with("morning"));

    let sent = transport
        .send_message(Uuid::parse_str(CHANNEL_ID).unwrap(), "hello", None)
        .await
        .expect("send");
    assert_eq!(sent.content, "hello");
}

#[tokio::test]
async fn an_imported_sso_session_refreshes_an_expired_token() {
    let (base, _valid, expired) = start().await;
    // The seeded access token is expired; the mock answers 401 for it, the
    // refresh grant hands out the valid one, and the retry must succeed.
    let transport =
        WebTransport::with_session(base.clone(), expired, Some(REFRESH.to_string()))
            .expect("transport");
    let account = transport.sign_in().await.expect("sign in with a session");
    assert_eq!(account.username, "alice");

    let servers = transport.servers().await.expect("servers after refresh");
    assert_eq!(servers.len(), 1);
}

#[tokio::test]
async fn a_password_failure_on_an_sso_instance_explains_the_session_route() {
    let (base, _, _) = start().await;
    let transport = WebTransport::new(base, "alice@example.com", "sso").expect("transport");
    let error = transport.sign_in().await.expect_err("no password on an SSO account");
    let message = error.to_string();
    assert!(message.contains("Invalid login credentials"), "{message}");
    assert!(message.contains("github"), "{message}");
    assert!(message.contains("--session"), "{message}");
}

#[tokio::test]
async fn the_dm_rpcs_and_both_send_paths_work() {
    let (base, _, _) = start().await;
    let transport = web(&base);
    transport.sign_in().await.expect("sign in");

    let conversations = transport.dm_conversations().await.expect("dm list");
    assert_eq!(conversations.len(), 1);
    let conversation = &conversations[0];
    assert_eq!(conversation.id, Uuid::parse_str(CONVERSATION_ID).unwrap());
    assert_eq!(conversation.peer.as_ref().expect("peer").username, "bob");
    assert_eq!(conversation.unread, 1);

    let history = transport
        .dm_messages(Uuid::parse_str(CONVERSATION_ID).unwrap(), None, 50)
        .await
        .expect("dm history");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "dm hi");

    let sent = transport
        .send_dm(Uuid::parse_str(CONVERSATION_ID).unwrap(), "see you")
        .await
        .expect("dm send");
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
    let (base, _, _) = start().await;
    let transport = WebTransport::new(base, "", "").expect("transport");
    let error = transport.sign_in().await.expect_err("no credentials");
    assert_eq!(error.status(), Some(401));
    assert!(error.to_string().contains("email"));
}

#[tokio::test]
async fn postgrest_errors_keep_the_sql_code() {
    let (base, _, _) = start().await;
    let transport = web(&base);
    transport.sign_in().await.expect("sign in");
    let broken = WebTransport::new("http://127.0.0.1:1", "a@b.c", "x").expect("transport");
    let error = broken.sign_in().await.expect_err("unreachable");
    assert!(error.is_retryable());
}
