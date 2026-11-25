# Protocol migration: Nexus wire protocol → Harmony protocols

This is the record of what was replaced, how the concepts map, and what a Harmony transport
cannot do. It is meant to be read next to `docs/protocol-bot.md` (the digested bot protocol)
and `src/harmony/`.

## Before and after

| | Before (Nexus) | After (Harmony) |
|---|---|---|
| Wire | `bincode`-serialised `ClientMessage`/`ServerMessage` enums, length-delimited over TLS | HTTP/JSON against `/bot-gateway/api/v1` **or** PostgREST, plus a WebSocket (`/bot-gateway/gateway`) for events |
| Connection | `TcpStream` + `tokio-rustls` + `LengthDelimitedCodec` to `host:port` | URLs; TLS by `reqwest`/`tokio-tungstenite` with the platform roots |
| Auth | `Login { username, password }`, `Register` | Bot transport: a bot token (`Authorization: Bot …`), obtained from the instance's **My bots**. Web transport: GoTrue email/password. No client-side registration (the instance owns accounts). |
| Push | Every server decision arrived as a `ServerMessage` | The gateway's documented events (`READY`, `MESSAGE_CREATE`, `MESSAGE_UPDATE`, `MESSAGE_DELETE`, reactions, member events) |
| Errors | `AuthFailure(String)`, `Notification(String, bool)` | `HarmonyError` with the API's status/`error`/`code`/`missing_permissions` mapped to a kind, plus a sentence for the UI |
| Reconnect | Re-open the socket and lose state | Gateway close-code policy table: retry with exponential backoff except `4001`/`4004`; everything is re-read over REST (there is no RESUME) |

The dependency `nexus-tui-common = "0.1.0"` is gone, together with `bincode`, `tokio-util`'s
codec, and the four `rustls` crates. `src/domain.rs` now owns the client's vocabulary.

## Data model mapping

| Domain type | Source in Harmony | Notes |
|---|---|---|
| `User.id` | profile UUID (`author.id`, `user.id`) | Discord relayed users carry a snowflake instead; they become an id-less author with a placeholder name. |
| `User.username` | `username` (or a server `nickname`) | Display name is kept separately (`display_name`) and preferred by `User::name`. |
| `User.color` | derived from the id (`UserColor::for_id`) | Harmony has no per-user colour; a stable hash keeps people recognisable. |
| `User.role` | member's roles ∩ roles with `is_admin` | Admin renders as `Admin`; everyone else `User`. Harmony role names/colours are not shown yet. |
| `User.profile_pic` | avatar URL → downloaded → `data:` URL | The TUI's image pipeline only decodes base64, so the transport fetches and encodes (4 MiB cap, MIME from magic bytes). |
| `Server` | `GET /servers/{id}` + `GET /servers/{id}/channels` | Member count and owner are carried; the icon is not downloaded yet. |
| `Channel.kind` | `type`: 0 text, 2 voice, 4 category | Categories are used for grouping, not listed as channels. |
| `Channel.category` | `channel_categories` / `category_id` | Rendered as the heading the UI already had. |
| `ChannelMessage` | REST message object or gateway event | `content`, author, time, edit flag. Attachments and reactions only exist in gateway events (`content_raw`), never in REST history. |
| `ChannelMessage.timestamp` | ISO 8601 (event) / SQL timestamp | Parsed tolerantly (`+00`, `+0000`, `+00:00`, `Z`, fractional seconds). |

## Command mapping

| UI command | Bot transport | Web transport |
|---|---|---|
| `Login(LoginRequest)` | `GET /users/@me`, then open the gateway and identify | `/api/federation/instance-info`, GoTrue password grant, then `profiles` by `auth_user_id` |
| `GetServers` / `Refresh` | one `GET /servers/{id}` per configured id (+ channels) | `user_servers` joined with `servers` |
| `GetChannelUserList` / `GetServerMembers` | `GET /servers/{id}/members` (paged by `after`, 1000/page) | `user_servers` joined with `profiles` |
| `GetChannelMessages` | `GET /channels/{id}/messages?limit=&before=` | `messages` by `channel_id`, threaded rows excluded |
| `SendChannelMessage` | `POST /channels/{id}/messages` (`content` as one `text` part) | insert into `messages` with a `text` part (the table enforces an array) |
| `EditChannelMessage` / `DeleteChannelMessage` | `PATCH`/`DELETE /messages/{id}` | `PATCH messages` (content / soft delete) |
| `GetProfile` | `GET /users/{id}` | `profiles` by id |
| `GetUserAvatars` | `GET /users/{id}` + avatar URL, encoded as a data URL | same, through the storage render endpoint |
| `GetForums`, `CreateThread`, forum commands | `Event::Unsupported { Forums }` | same |
| `GetDMUserList`, DM commands | `Event::Unsupported { DirectMessages }` | same (tables exist; screens are not wired) |
| `Register`, `UpdateProfile`, invites, notifications | `Event::Unsupported { … }` with the reason | same |

## Capabilities the UI reads

`Capabilities` (in `src/domain.rs`) is what a transport claims; the constants are written to
match what is implemented, not what the protocol could eventually do. Bot sessions say
`server_listing: false`, and when `--servers` left the list empty the app says so; the old
client never had to, because the Nexus server listed them.

| Field | Bot | Web |
|---|---|---|
| `server_listing` | no | yes |
| `direct_messages` | no | no (data reachable, screens pending) |
| `forums` | no | no |
| `message_edit`, `attachments` | yes | yes (edit via PATCH; attachments are read-only) |
| `threads`, `reactions` | no (no thread/reaction UI yet) | no |
| `presence`, `typing`, `voice`, `encrypted_channels` | no | no (Realtime and E2EE not implemented) |

## Gotchas a Harmony client has to live with

Verified against `docs/build/*.md` and `bot-gateway/src`, and covered by
`tests/harmony_bot.rs` where it is testable:

1. **No server enumeration.** There is no route that lists a bot's installations, so a bot
   session must be told its server ids. This is the one place the Nexus model was nicer.
2. **REST history is text-only.** Message objects from `/channels/{id}/messages` carry no
   `content_raw`, so attachments, custom emoji and embeds appear only when the gateway is
   connected and the message arrives as an event.
3. **`READY` is not guaranteed to be the first frame.** Events may arrive before it. The
   gateway loop decodes every frame in the same loop and only switches to the `READY`
   heartbeat interval when it sees `READY`.
4. **No RESUME.** A reconnect is a new session; missed messages are re-read over REST. The
   close-code table decides whether a reconnect is allowed at all (`4001`, `4004` = no).
5. **Heartbeat timing is server-driven.** `READY.heartbeat_interval` sets the interval; the
   gateway disconnects a connection that skips two intervals.
6. **Frame rate limit.** 120 non-heartbeat frames per 60 s; heartbeats do not count.
7. **Encrypted channels produce no message events.** The only signal is
   `GET /channels/{id}/encryption`; a `503` there must be treated as unsafe, never plaintext.
8. **History can contain soft-deleted, encrypted and threaded rows** with nothing in the
   object to mark the first two. The client does not pretend otherwise.
9. **`limit=0` returns one message**; the client clamps `1..=100`.
10. **Permissions are per-installation, not roles.** Every refusal is a `403` with one of
    three documented `error` strings, which `HarmonyError` surfaces verbatim; a bot cannot
    see every channel of a server even though `GET /servers/{id}/channels` returns them.

## How this was verified

```
cargo test              # 39 unit + 7 integration
cargo clippy --all-targets
```

The unit tests cover the pure pieces (URL normalisation, timestamp shapes, error mapping,
close-code/backoff policy, frame decoding, unknown events, message-part rendering, PostgREST
error shapes, domain mapping). The integration tests run the real `BotTransport` and
`GatewayClient` against a mock instance built from Harmony's own documented payloads
(`tests/fixtures/bot/`): REST reads and mapping, a bad token, `READY` + `MESSAGE_CREATE`
delivery, a `4004` close (no retry), a heartbeat-timeout close (reconnect), and the transport
event stream the session consumes. Headless `ratatui` tests render the sign-in screen for both
transports and drive `AuthSuccess`/`Ready`/`Unsupported` through the app.

The original tree did not compile against current dependency versions (`cargo check` failed on
the TLS/bincode block and on `rodio`); with the protocol layer replaced it builds, and
`--no-default-features` builds without ALSA.

## Live probe (2026-11-25)

The URLs and error shapes the client builds were checked against the flagship instance:

| Request | Answer |
|---|---|
| `GET https://har.mony.lol/api/federation/instance-info` | `200`, `{"success":true,"name":"Harmony","domain":"har.mony.lol","version":"1.9.8","supabaseUrl":"https://db.mony.lol",…}` — the fields the web transport's discovery reads |
| `GET https://har.mony.lol/bot-gateway/health` | `200`, `{"status":"ok",…}` — the gateway is where `gateway_url()` points |
| `GET https://har.mony.lol/bot-gateway/api/v1/users/@me` (no header) | `401` `{"error":"Missing Authorization header"}` |
| same with `Authorization: Bot <invalid>` | `401` `{"error":"Invalid or expired token"}` — mapped to `HarmonyError::Unauthorized`, which is final (no retry) |
