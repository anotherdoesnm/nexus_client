---
title: Harmony bot protocol — implementation reference
description: An implementation-grade spec of the Harmony bot gateway and REST API for a Rust TUI client - URLs, auth, all 68 routes, object schemas, gateway frames, events, pagination, rate limits, and what the bot API cannot do.
---

# Harmony bot protocol

A Rust client that speaks the Harmony bot protocol needs two channels to the same
`bot-gateway` process: one WebSocket for events, one HTTPS client for actions. This
page is the whole surface, with exact field names, nullability, and the places where
the prose docs and the shipped code disagree (the code wins).

## Sources and how to read citations

| Shorthand | File |
|---|---|
| `rest.md:N` | `nexus/harmony/docs/build/rest.md` |
| `gateway.md:N` | `nexus/harmony/docs/build/gateway.md` |
| `index.md:N` | `nexus/harmony/docs/build/index.md` |
| `perm.md:N` | `nexus/harmony/docs/build/permissions-rate-limits.md` |
| `cmd.md:N` | `nexus/harmony/docs/build/commands.md` |
| `wh.md:N` / `owh.md:N` | `nexus/harmony/docs/build/webhooks.md` / `outgoing-webhooks.md` |
| `qs.md:N` | `nexus/harmony/docs/build/bot-quickstart.md` |
| `BotRestAPI.ts:N` | `nexus/harmony/bot-gateway/src/api/BotRestAPI.ts` |
| `WSGateway.ts:N` | `nexus/harmony/bot-gateway/src/gateway/WebSocketGateway.ts` |
| `Dispatcher.ts:N` | `nexus/harmony/bot-gateway/src/gateway/EventDispatcher.ts` |
| `auth.ts:N` | `nexus/harmony/bot-gateway/src/auth/BotAuthMiddleware.ts` |
| `botPerms.ts:N` | `nexus/harmony/bot-gateway/src/auth/botPermissions.ts` |
| `config.ts:N` | `nexus/harmony/bot-gateway/src/config/supabase.ts` |
| `index.ts:N` | `nexus/harmony/bot-gateway/src/index.ts` |
| `baseline.sql:N` | `nexus/harmony/db_schema/migrations/20260101000000_baseline.sql` |

A bare `rest.md:N` is documentation; a bare `BotRestAPI.ts:N` is code evidence for
something the docs do not state. Where both exist and disagree, §10 lists the
conflict and the spec follows the code.

Conventions used below:

- `UUID` is a canonical 36-character UUID string; `snowflake` is a 1–20 digit Discord
  ID string (`rest.md:18`). They are **not** interchangeable: relayed Discord authors
  carry snowflakes in `author.id`.
- "omitted" means the key is absent from the JSON object (`undefined` in JS); "null"
  means the key is present with value `null`. Rust: `Option<T>` + `#[serde(default)]`
  handles both, but only "omitted" needs `default` to avoid a deserialization error.
- Every error body is a JSON object with at least `error` (string); some add `code`,
  and a few add `missing_permissions`, `max_position`, `discord_thread_id`,
  `max_bytes`, `retry_after`, `reason` (`rest.md:31`).

---

## 1. Base URLs and how an instance exposes the gateway

The `bot-gateway` service (Express + `ws`) serves the WebSocket, the REST API and the
Bridge v2 API on one port. The instance reverse proxy forwards `/bot-gateway/*` to it
with the prefix **stripped** (`index.md:44`, `self-host/Caddyfile:303-308`).

| What | Behind the proxy | Direct / local dev |
|---|---|---|
| Gateway (WebSocket) | `wss://<instance>/bot-gateway/gateway` | `ws://localhost:3002/gateway` |
| REST API | `https://<instance>/bot-gateway/api/v1` | `http://localhost:3002/api/v1` |
| Bridge v2 | `https://<instance>/bot-gateway/bridge/v2` | `http://localhost:3002/bridge/v2` |
| Health | `https://<instance>/bot-gateway/health` | `http://localhost:3002/health` |
| Channel webhook | `https://<instance>/webhooks/channels/<id>/<token>` | n/a (federation backend) |

`index.md:44-56`, `gateway.md:12-15`, `rest.md:14`.

- Port is `PORT`, default `3002` (`config.ts:53`); the WS path is the literal
  `/gateway` (`index.ts:50-53`). The Express mounts are `/api/v1` and `/bridge/v2`
  (`index.ts:69-72`), plus `/health`, `/status`, `/bridged-users/...`,
  `/attachments/refresh`, `/bridge-setup/:code` at the root (`index.ts:36,186,246,286,353,142`).
- Because the proxy strips the prefix, a client must build **two** bases and must not
  append `/api/v1` to the WS URL: REST root `https://<i>/bot-gateway/api/v1`, WS
  `wss://<i>/bot-gateway/gateway`. Deriving the WS URL from the REST base by string
  surgery is a bug waiting for a custom prefix — take both from config.
- Health is unauthenticated and is the feature probe (`index.md:58`). Body is
  `status`, `uptime` (float seconds), `timestamp` (ISO 8601) (`index.ts:36-42`):

```bash
curl https://<instance>/bot-gateway/health
# { "status": "ok", "uptime": 5321.4, "timestamp": "2026-10-13T09:00:00.000Z" }
```

  Anything other than `"status": "ok"` means the operator has not enabled bots
  (`index.md:58`). Probe once at startup; there is no version field to negotiate.
- There is no origin gate and no TLS requirement of its own: the gateway sets
  `Access-Control-Allow-Origin: *` (`rest.md:20`, `index.ts:31`) and a direct
  `http://localhost:3002` instance works, which is how local development runs.
- The instance proxy caps non-upload request bodies at 50 MB and passes uploads
  through (`self-host/Caddyfile:57-67`); the gateway itself caps JSON at 10 MB
  (`index.ts:33`, `rest.md:16`).

---

## 2. Auth

| Item | Value |
|---|---|
| Token format | `harmony_bot_` + 64 lowercase hex characters (`qs.md:25`) |
| Header | `Authorization: Bot <token>` — the word `Bot`, one space, the token (`rest.md:15`) |
| Verify | SHA-256 of the token, looked up by the `verify_bot_token` RPC (`auth.ts:35-39`) |
| Storage server-side | Only the SHA-256 hash is kept; the token is shown once (`qs.md:25`) |
| Reset | Old token dead at once; open WS closed at the next token check (`qs.md:27`, `perm.md:146`) |

Header parsing is literal: `authorization.split(' ')` must yield exactly two parts and
`parts[0] === 'Bot'` (`auth.ts:27-30`). Consequences for a Rust client:

- `Bot` is case-sensitive and there must be exactly one space. `bot <t>`, `Bot  <t>`
  (two spaces) and `Bot <t> ` (trailing space → 3 parts) all answer `401`
  `Invalid Authorization header format. Expected: Bot TOKEN`.
- The token is not prefix-validated at the edge; a garbage token reaches the RPC and
  answers `401 Invalid or expired token`.
- The header is required on **every** `/api/v1` request including unknown paths: the
  auth middleware is `router.use` before route matching (`BotRestAPI.ts:392-394`,
  `auth.ts:14-63`), so a typo'd path with no header answers `401`
  `Missing Authorization header`, not `404`.

Error mapping — the complete table for auth (`rest.md:40-56`, `auth.ts:20-50`):

| Status | `error` | Cause | Retry? |
|---|---|---|---|
| `401` | `Missing Authorization header` | No header at all | no, fix client |
| `401` | `Invalid Authorization header format. Expected: Bot TOKEN` | Header is not `Bot <token>` | no |
| `401` | `Invalid or expired token` | Unknown, revoked, expired token, or inactive bot | no |
| `503` | `Token verification unavailable` | The lookup RPC failed server-side | yes, with backoff |

The 401/503 split matters: the RPC error path deliberately answers `503` so a broken
database is not reported as a bad credential (`auth.ts:41-46`). A client should treat
`401` as terminal (stop, tell the user the token is wrong) and `503` as transient.
`401` and `503` from authentication are **not** counted against rate limits
(`perm.md:166`).

Gateway auth is the same token in the IDENTIFY frame, with different failure
signalling (close codes `4001`/`4004`, §5.8).

Each `verify_bot_token` call counts a token use (`WSGateway.ts:485-490`); that is why
the revalidator reads `bot_tokens`/`bots` directly instead of calling the RPC, and it
runs every 30 s (`WSGateway.ts:490-533`).

---

## 3. REST API

### 3.0 Request conventions

| | |
|---|---|
| Base | `https://<instance>/bot-gateway/api/v1` |
| Auth | `Authorization: Bot <token>` on every request |
| Request body | JSON, `Content-Type: application/json`, max 10 MB (`index.ts:33`) |
| Response | JSON except `204 No Content` |
| IDs | UUIDs; Discord-derived IDs are 1–20 digit snowflakes |
| Timestamps | ISO 8601 with offset (`2026-10-13T12:00:00.000000+00:00`) |
| CORS | any origin |
| Bodyless methods | `DELETE` accepts an optional JSON body on the reaction routes |

`rest.md:12-20`. A body that is not valid JSON answers `500` `Internal server error`
(`rest.md:54`), not `400` — handle it.

Path parameters are named `{channel.id}` in the docs but are plain UUID path segments
on the wire: `/channels/c2d4f8a0-.../messages`. Route patterns use `:channelId`,
`:messageId`, `:serverId`/`:guildId`, `:userId`, `:roleId`, `:emojiId`, `:stickerId`,
`:soundId`, `:threadId`, `:interactionId`, `:commandId`, `:code`
(`BotRestAPI.ts:396-533`).

Error envelope (`rest.md:31-56`):

```json
{ "error": "Missing permission: send_messages" }
```

| Status | Meaning | Extra fields |
|---|---|---|
| `400` | Missing/invalid field or query parameter | — |
| `401` | Auth (§2) | — |
| `403` | Permission, visibility, AutoMod, ownership | `code`, `missing_permissions` |
| `404` | Not found; unmatched path answers `Not found` | — |
| `409` | Race lost, already paired, Discord ID taken | `code`, `discord_thread_id` |
| `413` | Upload over the ceiling | `max_bytes` |
| `415` | Wrong upload body type | — |
| `422` | Sticker refused | `code`, `reason` |
| `429` | Rate limited | `retry_after` + `Retry-After` header |
| `500` | Database error, constraint violation, unparseable JSON body | — |
| `502` | Discord CDN download/store failed | — |
| `503` | Token verification or an optional lookup failed | — |
| `507` | Bot-file storage quota exhausted | — |

The three `403` channel messages are produced by one helper and are worth matching
exactly (`BotRestAPI.ts:3240-3263`, `perm.md:69-73`):

| Body | When |
|---|---|
| `Missing permission: <flag>` | Installation lacks the flag, bot not installed in the server, or the channel does not exist |
| `Channel not visible to this bot` | Flag held, but the bot cannot see the channel (or the visibility lookup failed) |
| `Missing permission in this channel: <flag>` | Bot sees the channel and @everyone's override there denies the flag's bit |

Read routes collapse all three into `Missing permission: read_messages`
(`rest.md:280`, `BotRestAPI.ts:1456`), so a reader cannot tell "gone" from "hidden".

### 3.1 Route index (68 routes)

The docs' index lists 64 rows (`rest.md:68-133`); the code registers **68**. The
difference: the four Matrix room/member routes exist only in prose
(`rest.md:520-524`) and in `BotRestAPI.ts:410-413`. The tables below enumerate 65
(including those four); the three `/guilds/…` aliases at the end make 68. Counted from
`BotRestAPI.ts:396-533`.

Access levels are the docs' vocabulary: **Token** = any valid token; **Installed** =
active installation in the server; a permission name = that installation flag, plus
the channel checks of `perm.md:54-65` for channel-addressed routes.

The "Success" column names a schema defined in §4; field lists are not repeated per
route.

#### Messages

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `POST /channels/:channelId/messages` | `send_messages` | — | `content`, `embeds`, `reply_to`, `thread_id`, `metadata` | `201` Message, or `200` Message when this bot already stored that `metadata.discord_message_id` | `400` empty / `thread_id is not a thread of this channel` / `reply_to names a message in another channel` (`REPLY_OUT_OF_SCOPE`) / text over limit; `403` `Thread is locked`, AutoMod (`AUTOMOD_BLOCKED`), channel trio; `409` `That Discord message id is taken in this channel` (`DISCORD_MESSAGE_ID_TAKEN`) |
| `GET /channels/:channelId/messages` | `read_messages` | `limit`, `before`, `after` | — | `200` Message[] | `403` `Missing permission: read_messages` |
| `GET /messages/:messageId` | `read_messages` | — | — | `200` Message | `404` `Message not found` / `Channel not found`; `403` read |
| `PATCH /messages/:messageId` | `send_messages`, own message | — | `content` | `200` Message | `400` `Cannot send an empty message`; `403` `Cannot edit messages from other bots or users`, AutoMod |
| `DELETE /messages/:messageId` | own: channel visible. Others: `manage_messages` | — | — | `204` | `404` `Message not found`; `403` channel trio |
| `PATCH /messages/:messageId/content-silent` | `send_messages`, own message | — | `content` (Part[], required) | `200` `{ "ok": true }` | `400` `content must be a MessagePart array`; `403` `Cannot update messages from other bots or users`; `409` `Message content changed; re-fetch and retry` |
| `PATCH /messages/:messageId/metadata` | `send_messages` | — | `metadata` (object, required) | `200` `{ "ok": true, "metadata": {…} }` | `400` `metadata must be a JSON object`; `403` `Bots can only update metadata on their own messages`; `403` `The author opted out of this bridge` (`BRIDGE_AUTHOR_OPTED_OUT`); `404` `Message not found` |
| `GET /channels/:channelId/messages/lookup` | `read_messages` | `discord_message_id` | — | `200` Message | `400` (missing param); `404` `Message not found` |
| `POST /channels/:channelId/typing` | `send_messages` | — | — | `204` | `403` channel trio |

`rest.md:193-232,266-364`. Notes:

- **Create**: `content` may be a string (becomes one `text` part) or an array of parts
  (§4.4). `embeds` are appended as `embed` parts and dropped unless valid. `reply_to`
  may name a vanished message — the send succeeds without the reply (`rest.md:201`).
  `metadata` loses reserved keys; `bot: true` and `created_via: "bot_api"` are set by
  the server (`rest.md:216`). An empty result after checks answers
  `400 Cannot send an empty message`.
- **Idempotency**: re-sending a message whose `metadata.discord_message_id` this bot
  already used in that channel returns `200` **with the existing message** rather than
  creating a second row (`BotRestAPI.ts:928-950`, `rest.md:220`). A different bot's row
  holding that ID is a final `409` — do not retry.
- **`content-silent`** exists so bridges can refresh expired signed URLs without an
  edit marker or `MESSAGE_UPDATE` (`rest.md:319-327`). It is compare-and-set on the
  stored content; a lost race is `409` and the client must re-fetch.
- **`metadata` merge** returns the merged object; the top-level keys replace. It
  changes no timestamp and sends no event (`rest.md:345`). On another author's message
  only a bridge bot may write, and only the mapping keys (`rest.md:343`).
- **`typing`** has no effect of any kind — no row, no event (`rest.md:364`,
  `BotRestAPI.ts:2127-2138`). It exists only to answer "may I send here".

#### Reactions

`{emoji}` is a custom emoji's UUID, the URL-encoded text of any other emoji
(`%F0%9F%91%8D` for 👍), or a bridged identifier such as `discord:name:id`
(`rest.md:368`).

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `PUT /messages/:messageId/reactions/:emoji` | `add_reactions` | — | `metadata` (object, optional) | `204` | `400` `Maximum number of reactions reached (20)` (`code` `30010`); `403` channel trio; `404` `Message not found` |
| `DELETE /messages/:messageId/reactions/:emoji` | `add_reactions` | — | `discord_user_id` (string, optional) | `204`, also when nothing matched | `403` channel trio; `404` `Message not found` |

`rest.md:366-387`, `BotRestAPI.ts:2001-2110`. Notes:

- Every `PUT` adds one reaction **by the bot**; a bridge adds one per remote user with
  `metadata.discord_user` naming that user (`rest.md:376`).
- `metadata.remote_emoji_url` is kept only for an `https` URL on `cdn.discordapp.com`
  or `media.discordapp.net`; any other value is dropped (`rest.md:374`,
  `BotRestAPI.ts:338-348`).
- The `DELETE` body's `discord_user_id` limits the removal to reactions whose
  `metadata.discord_user.id` matches (`rest.md:387`).
- There is no route that lists a message's reactions (§4.6).

#### Threads

A thread's messages carry the **parent channel's** `channel_id` and the thread's
`thread_id`; writing in a thread follows the parent channel's `send_messages` access
(`rest.md:391`).

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `POST /channels/:channelId/threads` | `send_messages` | — | `name`*, `message_id`, `discord_thread_id`, `discord_user`, `created_at`, `starter_discord_user_id` | `201` Thread (also when an existing thread is returned) | `400` `name is required` / `message_id must be a message id` / `discord_thread_id must be a Discord snowflake` (and, documented only in code, `created_at must be an ISO 8601 time`, `starter_discord_user_id must be a Discord snowflake`); `403` `The channel is not bridged by this bot`; `404` `Message not found in this channel` |
| `GET /channels/:channelId/threads/lookup` | `read_messages`, Discord bridge | `discord_thread_id` | — | `200` Thread | `400` (not a snowflake); `403` `Channel not visible to this bot`; `404` `Thread not found` |
| `GET /threads/:threadId` | `read_messages` | — | — | `200` Thread | `404` `Thread not found` (also for an unreadable channel) |
| `PUT /threads/:threadId/discord` | `read_messages`, Discord bridge | — | `discord_thread_id`* | `200` Thread | `400`; `403` `The channel is not bridged by this bot`; `404` `Thread not found`; `409` `Thread is already paired` + `discord_thread_id` of the standing pair |

`rest.md:389-460`, `BotRestAPI.ts:668-768`. Notes:

- A message holds one thread: `message_id` that already has one returns that thread and
  posts no notice. With `discord_thread_id` the call is idempotent for the bot's bridge
  (`rest.md:423`).
- `name` is trimmed and cut to 100 characters. A standalone thread hangs off a
  server-generated notice with `metadata.type` `thread_created` and
  `metadata.standalone: true` (`rest.md:412,421`).
- A locked thread refuses new bot messages with `403` `Thread is locked` on the
  **message** routes, not here (`rest.md:402`).
- There is **no** route that lists a channel's threads (§8.2 #6).

#### Attachments (uploads)

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `POST /channels/:channelId/attachments` | `send_messages` + `attach_files` | `filename` | raw file bytes, its own `Content-Type` | `201` file part (§4.4) | `408` (30 min overrun); `400` `The request body is empty`; `413` `File too large` + `max_bytes`; `415` `Send the file itself as the request body, with its own Content-Type`; `429` `Too many uploads at once` + `retry_after`; `507` `This server's storage for bot files is full` |
| `POST /channels/:channelId/bridge-files` | Matrix bridge, read/send | — | raw bytes + `X-File-Name` (URI-encoded) + `X-File-Type`; `Content-Type: application/octet-stream` | `201` file part (`fileType` `image` or `file` only) | as above, plus `415` for active content names |
| `GET /channels/:channelId/messages/:messageId/bridge-files/:index` | Matrix bridge, current visibility | — | — | `200` bytes with `Content-Type: application/octet-stream`, `Content-Length`, `X-File-Name`, `X-File-Type`, `Content-Disposition: attachment`, `Cache-Control: no-store`, `X-Content-Type-Options: nosniff` | `400`, `403`, `404`, `409`, `413`, `415`, `422`, `503` (`rest.md:539`) |

`rest.md:236-264,533-539`. Notes for a TUI that only posts images:

- The upload returns a **`file` part**, not an attachment object. Post it back inside
  `content` of a message on the same channel (`rest.md:240`). Its `url` is a signed
  link valid 7 days; `path` is the durable identifier (`rest.md:254`).
- `path` is only preserved for a file **this bot** uploaded **to that channel**;
  every other `path` is stripped on create and edit (`rest.md:174`,
  `messageContent.ts:230-237`).
- Stored type and extension follow the file's leading bytes, not the declared type
  (`rest.md:238`). Photos and videos are metadata-stripped server-side with no opt-out
  (`rest.md:176-178`).
- Concurrency: 3 uploads of one bot run at once, 10 more queue unread; >13 open
  answers `429` (`rest.md:254,261`). File quota is 5 GB per server by default,
  shared with mirrored Discord files and Matrix avatars.
- `index` on the download route is the **raw content-array index**, not an ordinal
  among files (`rest.md:537`).

#### Channels (read, edit, encryption, bridge state)

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /servers/:serverId/channels` | Installed | — | — | `200` Channel[] | `403` `Bot not in server` |
| `POST /servers/:serverId/channels` | `manage_channels` | — | `name`*, `type`, `category_id`, `description`, `order` | `201` Channel | `400` `name is required` / name emptied by the naming rule; `403` `Missing permission: manage_channels` |
| `PATCH /channels/:channelId` | `manage_channels` | — | `order`, `category_id` (nullable) | `200` Channel | `400` `No valid fields to update`; `403`; `404` `Channel not found` |
| `GET /channels/:channelId/encryption` | `read_messages` | — | — | `200` `{ "messages_encrypted": bool }` | `403` read; `503` `Channel encryption status unavailable` |
| `GET /channels/:channelId/permission-overrides` | Installed | — | — | `200` Override[] | `403` `Bot not in server`; `404` `Channel not found` |
| `PUT /channels/:channelId/permission-overrides` | `manage_channels` | — | `target_type`*, `role_id`, `user_id`, `allow_permissions`, `deny_permissions` | `201` Override (created) / `200` Override (updated) / `204` (both masks `0` → deleted) | `400` `target_type must be role or user` / `role_id is required for role overrides` / `user_id is required for user overrides` / `Invalid permission bitmask`; `403` `Missing permission: manage_channels`, `Cannot grant permissions the bot does not hold in this channel` + `missing_permissions`; `404` `Channel not found` |
| `DELETE /channels/:channelId/permission-overrides/role/:roleId` | `manage_channels` | — | — | `204` | `403` as above (lifting a deny needs the bit); `404` `Channel not found` |
| `GET /channels/:channelId/bridge-platform` | channel read | — | — | `200` `{ "platform": "matrix" / "discord" / null }` | `403`; `503` (failed/malformed lookup) |
| `GET /channels/:channelId/bridge-consent/:userId` | channel read, registered bridge | — | — | `200` `{ "relay_allowed": bool }` | `400` (malformed user UUID); `403`; `503` |
| `PUT /channels/:channelId/bridge-room` | Matrix binding + read/send | — | `room_id`*, `initial_sender_mode` (`ghost` / `relay`) | `200` `{ "room_id", "sender_mode" }` | `400`, `403`, `503` |
| `GET /channels/:channelId/bridge-room` | Matrix binding + read/send | — | — | `200` `{ "room_id", "sender_mode" }` | `400`, `403`, `503` |
| `GET /channels/:channelId/bridge-members` | Matrix binding + read/send | — | — | `200` array of `{ id, username, display_name, avatar_url, domain, is_local, nickname }` | `400`, `403`, `503` |
| `GET /channels/:channelId/bridge-members/:userId/avatar` | Matrix binding + read/send | — | — | `200` `{ "data": base64, "content_type" }` | `400`, `403`, `404`, `503` |
| `POST /channels/:channelId/bridge-avatar` | Matrix binding + read/send | — | `data` (base64)*, `content_type`* | `200` `{ "avatar_url": "https://…" }` | `400`; `403`; `429` + `retry_after`; `507`; `503` |

`rest.md:502-549,553-593,686-716`. Notes:

- **Channel type numbering is Discord's**, not the database's: DB `0=text,1=voice,2=category`
  (`baseline.sql:820`) is returned as `0`, `2`, `4` (`BotRestAPI.ts:266,3358`). The
  create body takes `type: 1` for voice and **anything else** for text
  (`rest.md:575`, `BotRestAPI.ts:2314`) — there is no way to create a category as a
  channel; use `/categories`.
- Channel names are normalized by the database on create (lowercase, spaces→`-`,
  punctuation dropped) and the response carries the stored name (`rest.md:580`).
- `position` and `order` are always the same value; `parent_id` and `category_id` are
  always the same value (`rest.md:563-566`).
- `GET /channels/:channelId/encryption` is the only way to learn whether a channel is
  end-to-end encrypted, and a `503` must be treated as **unsafe to relay**, not as
  plaintext (`rest.md:508`).
- `bridge-consent` returns `{relay_allowed:false}` for an opt-out; treat anything but
  an explicit `true` as refusal, and re-check right before delivery
  (`rest.md:514`).
- The four `bridge-room` / `bridge-members` routes are Matrix-bridge plumbing; a TUI
  ignores them. They are in the code (`BotRestAPI.ts:410-413`) but not in the docs'
  route index.

#### Servers, members, categories

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /servers/:serverId` | Installed | — | — | `200` Server | `403` `Bot not in server` |
| `GET /servers/:serverId/members` | Installed | `limit`, `after` | — | `200` Member[] | `400` `after must be a user id`; `403` `Bot not in guild` |
| `GET /servers/:serverId/categories` | Installed | — | — | `200` Category[] | `403` `Bot not in server` |
| `POST /servers/:serverId/categories` | `manage_channels` | — | `name`*, `order` | `201` Category | `400` `name is required`; `403` `Missing permission: manage_channels` |
| `PATCH /servers/:serverId/categories/:categoryId` | `manage_channels` | — | `name`, `order` (both optional) | `200` Category | `400` `No valid fields to update`; `403`; an unknown category answers `500` — the update's `.single()` read-back finds no row, so the handler's `404 Category not found` is unreachable (`BotRestAPI.ts:2365-2374`) |

`rest.md:464-500,595-616`. Notes:

- `GET /servers/:serverId` fields: `id`, `name`, `icon` (absolute URL or `null`),
  `owner_id` (UUID or `null` when the owner deleted), `description` (string or `null`),
  `member_count` (`rest.md:468-475`, `BotRestAPI.ts:3344-3353`).
- Members page by **user ID** (`after` = last `user.id`), ordered by user ID, max 1000
  per page (`rest.md:483-486`). Memberships are not filtered by status, so pending and
  banned rows appear (`rest.md:486`; the query has no status filter,
  `BotRestAPI.ts:2199-2214`).
- **Doc/code conflict**: `rest.md:555` says the channel list includes channels the bot
  cannot see; the code filters them out (`BotRestAPI.ts:2247-2248`). Follow the code —
  see §10.
- **Undocumented**: categories are filtered for bots without `manage_channels` to
  those holding a channel the bot can see (`BotRestAPI.ts:2444-2453`).

#### Roles

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /servers/:serverId/roles` | Installed | — | — | `200` Role[] | `403` `Bot not in server` |
| `POST /servers/:serverId/roles` | `manage_roles` | — | `name`*, `color`, `position`, `permissions`, `mentionable`, `hoist` | `201` Role (6 fields) | `400` `name is required` / `position must be an integer`; `403` `Missing permission: manage_roles` / `Role position must be below the bot's highest manageable position` + `max_position` / `Cannot grant permissions the bot does not hold in this server` + `missing_permissions` |
| `PATCH /servers/:serverId/roles/:roleId` | `manage_roles` | — | same fields, all optional | `200` Role (6 fields) | create errors, plus `403` `Cannot modify default or admin roles via bot API` / `Role is at or above the bot's highest manageable position`; `404` `Role not found` |
| `DELETE /servers/:serverId/roles/:roleId` | `manage_roles` | — | — | `204` | `403` as on edit; `404` `Role not found` |
| `PUT /servers/:serverId/members/:userId/roles/:roleId` | `manage_roles` | — | — | `204` (idempotent) | `400` `userId and roleId must be UUIDs`; `403` `Missing permission: manage_roles` / `Cannot modify default or admin roles via bot API` / `Role is at or above the bot's highest manageable position` / `Cannot grant permissions the bot does not hold through this role` + `missing_permissions`; `404` `Role not found` / `Member not found` |
| `DELETE /servers/:serverId/members/:userId/roles/:roleId` | `manage_roles` | — | — | `204` (idempotent) | as `PUT` minus the grant-bit check |

`rest.md:620-684,100-105`. Notes:

- `permissions` is a 64-bit mask; **send it as a decimal string** — a JSON number above
  2^53 loses precision (`rest.md:620`, `perm.md:116`). It comes back as a string
  (`BotRestAPI.ts:2473`).
- Bit 0 (`ADMINISTRATOR`) is cleared on every write (`rest.md:620`,
  `BotRestAPI.ts:2539-2541`).
- Role writes are bounded by two independent limits: the bits the bot could grant, and
  the **position cap** derived from the member who installed it (`perm.md:96-102`).
  A refused position carries `max_position` = cap − 1 (`BotRestAPI.ts:2532-2538`).
- `GET .../roles` returns `id, name, color, position, permissions, is_default,
  is_admin, mentionable, hoist` ordered by position descending; create/edit return the
  6-field subset `id, name, color, position, permissions, mentionable, hoist`
  (`rest.md:628,643`, `BotRestAPI.ts:2468,2607`).

#### Emojis, stickers, sounds

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /emojis` | Token | `url`, `id` | — | `200` Emoji[] (every column) or `200` Emoji when `id` given | `400` `id must be an emoji UUID`; `404` `Emoji not found` |
| `POST /emojis` | Token | — | `name`*, `url`*, `domain`* (and `server_id` must be absent/null) | `201` Emoji | `400` `Missing required fields: name, url` / domain required; `403` `Bots can only create federated emojis (server_id must be null)` |
| `GET /servers/:serverId/emojis` | Installed | — | — | `200` Emoji[] with `discord_emoji_id` | `400`; `403` `Bot is not in this server` |
| `GET /servers/:serverId/emojis/discord` | server's Discord bridge bot | — | — | `200` `{ "limit", "emojis": [...] }` | `403` `Bot is not the Discord bridge of this server`; `500` `LIST_TOO_LARGE` past 10 000 rows |
| `POST /servers/:serverId/emojis/discord` | same | — | `discord_emoji_id`*, `name`*, `animated`, `update` | `200` (`existing`/`linked`/`updated`) or `201` (`created`): `{status,id,name,url,discord_emoji_id,removed}` | `400`; `403` (+ `EMOJI_REMOVED`); `409` `EMOJI_LIMIT` / `EMOJI_NAME_TAKEN`; `502` |
| `DELETE /servers/:serverId/emojis/:emojiId` | same, imported only | — | — | `204` (idempotent) | `400`; `403` (+ `EMOJI_NOT_LINKED`); `404` |
| `GET /servers/:serverId/stickers/discord` | same | — | — | `200` `{ "limit", "stickers": [...] }` | `403`; `500` `LIST_TOO_LARGE` |
| `POST /servers/:serverId/stickers/discord` | same | — | `discord_sticker_id`*, `name`*, `description`*, `format_type`*, `update` | `200`/`201` `{status,id,name,description,url,discord_sticker_id,removed}` | `400`; `403` (+ `STICKER_REMOVED`); `409` `STICKER_LIMIT`/`STICKER_NAME_TAKEN`; `422` `STICKER_FILE` + `reason`; `502` |
| `DELETE /servers/:serverId/stickers/:stickerId` | same, imported only | — | — | `204` | `400`; `403` (+ `STICKER_NOT_LINKED`); `404` |
| `GET /servers/:serverId/sounds` | same | — | — | `200` Sound[] | `403` |
| `POST /servers/:serverId/sounds/discord` | same | — | `discord_sound_id`*, `name`*, `emoji`, `emoji_id`, `volume`*, `update` | `200`/`201` `{status,id,name,emoji,emoji_id,volume,discord_sound_id}` | `400`; `403` (+ `SOUND_REMOVED`); `409` `SOUND_LIMIT`; `502` |
| `DELETE /servers/:serverId/sounds/:soundId` | same, imported only | — | — | `204` | `400`; `403` (+ `SOUND_NOT_LINKED`); `404` |

`rest.md:718-850`. A TUI needs only `GET /emojis` (resolve a custom emoji by ID) and
`GET /servers/:serverId/emojis` (render server emoji). `volume` may arrive as a string
(`"0.50"`), not a number (`rest.md:821`) — deserialize as `serde_json::Value` or a
`#[serde(deserialize_with)]` that accepts both.

#### Users, invites

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /users/@me` | Token | — | — | `200` Bot | `500` |
| `GET /users/:userId` | Token | — | — | `200` User | `404` `User not found` (bot IDs are not profiles) |
| `GET /invites/:code/preview` | Token | — | — | `200` InvitePreview | `400` `Invite code is required`; `404` `Invite not found or expired` / `Invite expired` |

`rest.md:852-889`. `@me` is registered before `/users/:userId` precisely so the literal
wins (`BotRestAPI.ts:513-517`).

#### Slash commands

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `GET /commands` | Token | — | — | `200` Command[] | — |
| `PUT /commands` | Token | — | Command[] | `200` Command[] (the stored set) | `400` (validation) |
| `POST /commands` | Token | — | Command | `201` (new) / `200` (replaced same name) | `400` |
| `DELETE /commands/:commandId` | Token | — | — | `204` | `404` `Unknown command` |

`cmd.md:36-43`. `PUT` replaces the whole set: a command whose name is kept keeps its
ID, names absent from the array are deleted, so a bot can send its full list at every
start (`cmd.md:43`). Max 100 commands, 25 options each (`cmd.md:67,71`).

#### Interactions

| Route | Access | Query | Body | Success | Errors |
|---|---|---|---|---|---|
| `POST /interactions/:interactionId/callback` | the bot's own interaction, within 5 s, once | — | `type` (`4` reply / `5` defer), `data` (`content`, `embeds`, `flags`) | `204` | `400` `type must be 4 (reply) or 5 (defer)` / `data must be an object` / `flags must be 0 or 64` / `Cannot send an empty message` / `Interaction already acknowledged` / `Interaction expired`; `403` channel errors; `404` `Unknown interaction` |
| `PATCH /interactions/:interactionId/response` | the bot's interaction, within 15 min | — | `content`, `embeds` | `200` Message, or `{id, ephemeral: true, content, content_raw}` | `400` `Cannot send an empty message` / `Interaction not acknowledged` / `Interaction expired`; `403`; `404`; `409` `Response in progress` |
| `POST /interactions/:interactionId/followups` | same | — | `content`, `embeds`, `flags` | `200` as above | `400` `Follow-up limit reached` (11th); others as `PATCH` |

`cmd.md:146-176`, `BotRestAPI.ts:1830-1940`. `flags: 64` makes the reply visible only
to the invoking member; it is not stored as a message (`cmd.md:159`). Public replies
are judged by AutoMod as the member's own message (`cmd.md:163`).

#### Deprecated aliases

`GET /guilds/:serverId`, `GET /guilds/:serverId/members`,
`GET /guilds/:serverId/channels` behave exactly as the `/servers/…` routes and each
counts in its own rate-limit bucket (`rest.md:891-893`, `BotRestAPI.ts:479-481`).

### 3.2 Endpoints outside `/api/v1` (not for bots)

Served by the same process, authenticated by a **user session** (`Authorization:
Bearer …`), not a bot token (`rest.md:895-907`):

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/health` | none | `{status,uptime,timestamp}` |
| `GET` | `/status` | user JWT | `connected_bots`, `total_connections`, `bots[]` |
| `GET` | `/bridged-users/:channelId` | user JWT, channel visible | `{channel_id, has_bridge, users}` |
| `GET` | `/bridged-users/server/:serverId` | user JWT, member | `{server_id, has_bridge, users}` |
| `POST` | `/attachments/refresh` | user JWT, channel visible | body `{messageId}`; may emit `REFRESH_ATTACHMENTS` |
| `GET` | `/bridge-setup/:pairingCode` | none | legacy pairing lookup |

`/status` is the only place a bot's online state is exposed, and a bot token cannot
call it (`index.ts:185-214`).

---

## 4. Object schemas

Field lists are exhaustive for what the gateway and the REST routes actually emit.
"omitted" = key absent; "nullable" = key present, value may be `null`. Rust guidance
assumes `serde` with `#[serde(default)]` on every optional field.

### 4.1 Message event object (`MESSAGE_CREATE`, `MESSAGE_UPDATE`)

`gateway.md:152-171`, built by `Dispatcher.ts:1082-1098`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `id` | UUID | no | |
| `server_id` | UUID | no | Present on events, **absent from REST message objects** |
| `channel_id` | UUID | no | A thread message carries the *parent* channel |
| `thread_id` | UUID | yes | Thread the message was posted in |
| `author` | object (§4.3) | yes | `null` when the account no longer exists |
| `content` | string | no | `text` parts joined with one space, **trimmed** (`Dispatcher.ts:1147`) |
| `content_raw` | Part[] (§4.4) | no | The stored parts; the only place links, files, emoji and mentions live |
| `is_system` | boolean | no | Server notice |
| `reply_to` | UUID | yes | |
| `timestamp` | ISO 8601 | no | Creation time |
| `edited_timestamp` | ISO 8601 | no | Last content edit; equals `timestamp` until edited |
| `mentions` | string[] | no | `user_id` of each `mention` part — **empty for app-written parts**, which carry `userId` (`gateway.md:167`, `Dispatcher.ts:1153-1160`) |
| `mention_everyone` | boolean | no | @everyone/@here mentioned *and* the author may (`Dispatcher.ts:1096`) |
| `metadata` | object | yes | Free-form; default `{}`, `null` only if the row's column is null (`baseline.sql:887`) |

In `content_raw`, a `file` part whose `path` names a stored attachment carries a `url`
signed for this event (7 days), and a `.jpg/.jpeg/.png` image also carries
`render_url` (≤1600×1600 rendition, same lifetime, absent if it cannot be signed)
(`gateway.md:171`, `utils/messageMedia.ts`).

### 4.2 Message object (REST)

`rest.md:143-157`, built by `BotRestAPI.ts:3319-3342`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `id` | UUID | no | |
| `channel_id` | UUID | no | |
| `thread_id` | UUID | yes | |
| `author` | object (§4.3) | yes | `null` on `GET /messages/:id` for member-authored rows (§8, gotcha 4) |
| `content` | string | no | `text` parts joined with one space, **not trimmed** (`BotRestAPI.ts:3406-3417`) |
| `is_system` | boolean | no | |
| `reply_to` | UUID | yes | |
| `timestamp` | ISO 8601 | no | |
| `edited_timestamp` | ISO 8601 | no | |
| `mentions` | string[] | no | Same `user_id` caveat |
| `metadata` | object | yes | Same keys as the event; see §4.18 |

Deliberate absences (`rest.md:157`, `BotRestAPI.ts:3319-3342`): **no `content_raw`**,
**no `mention_everyone`**, **no `server_id`**, no reaction or attachment summary. The
only REST payload that carries `content_raw` is an ephemeral interaction reply
(`BotRestAPI.ts:1745`).

### 4.3 Author

Gateway (`gateway.md:203-212`, `Dispatcher.ts:1053-1080`):

| Variant | `id` | `bot` | Extra keys |
|---|---|---|---|
| Member | profile UUID | `false` | `nickname`: string or `null` |
| Bot | bot UUID | `true` | `nickname: null` |
| Channel webhook | hidden bot UUID | `true` | `webhook: true`; `username`/`display_name` = posted name; `avatar` = URL or `null` |
| Relayed Discord user | Discord snowflake | `false` | `discord_user: true`, `nickname: null` |
| Relayed Matrix user | MXID | `false` | `bridge_source: "matrix"`, `bridge_author: true`, `nickname: null` (`bridge/bridgeAuthors.ts:36-51`) |

Common keys, in emission order: `id`, `username`, `display_name`, `avatar`,
`nickname`, `bot`.

- `avatar` is an absolute URL, **omitted** when a member or bot has none
  (`gateway.md:212`, `utils/avatarUrl.ts:14-36`), but **`null`** for a webhook author
  (`Dispatcher.ts:1063`). Both must deserialize.
- `username` and `display_name` are nullable columns (`baseline.sql:125-126`) even
  though the docs type them as `string`. Deserialize them as `Option<String>` (or
  `#[serde(default)]`) so one unfinished profile does not fail a whole page.
- `nickname` is resolved for members only, cached 60 s (`Dispatcher.ts:92-94`).
- `id` is a `String`, never a `Uuid`: snowflakes and MXIDs are not UUIDs.

REST author is the same object minus `nickname` for the member/bot/webhook cases — the
REST formatter emits exactly `id`, `username`, `display_name`, `avatar`, `bot`
(`BotRestAPI.ts:3326-3332`) — and the **relayed** variant is emitted verbatim from
`bridgedMessageAuthor`, i.e. with `nickname: null`, `bot: false`, plus
`discord_user`/`bridge_source` (`BotRestAPI.ts:3326`, `bridge/bridgeAuthors.ts:36-51`).
This contradicts `rest.md:157` ("the author … is the bridge bot") — see §10.

### 4.4 Message parts

`rest.md:161-174`. Bots may send these; the server drops `system` parts a bot sends
and removes every `path` it did not store itself.

| `type` | Fields | Notes |
|---|---|---|
| `text` | `text` | Plain text; markdown rendered by the app. Never read for mentions |
| `url` | `url`, `preview` (bool), `label`? | A link. `preview:false` suppresses the card; a link inside a `text` part gets no card |
| `mention` | `userId`, `username`, `domain`, `isLocal` | Member mention. Note **camelCase** |
| `role_mention` | `roleId`, `roleName`, `roleColor` | `roleId: "here"` is @here |
| `channel_mention` | `channelId`, `name`, `messageId`? | Server fills in `serverId` and the stored name |
| `emoji` | `emoji: { id, name, url }` | Custom emoji |
| `file` | `url`, `fileType` (`image` / `video` / `audio` / `file`), `fileName`?, `fileSize`?, `altText`?, `path`?, `mimeType`? | Attachments are parts, never a separate object |
| `embed` | `url`, `provider` (`harmony-post` / `harmony-invite` / `fediverse-post` / `youtube` / `spotify` / `generic`), `previewId`, `collapsed`? | Kept only when valid; reduced to these fields (`BotRestAPI.ts:20-26`) |
| `system` | server-generated | Dropped from bot input; appears in stored content of notices |

Mention handling on write (`rest.md:184-191`, `messageContent.ts:141-227`): a
`role_mention`/`channel_mention` naming a role/channel of the channel's server keeps
its part with the stored name; anything else becomes the text `@name`/`#name`.
@everyone and @here stay mentions only with the `mention_everyone` flag; otherwise they
become text. A string `content` is scanned for typed lowercase `@everyone`/`@here`
outside code and URLs (`messageContent.ts:53-81`).

### 4.5 Attachments — there is no attachment object

Verified: no route, event or schema defines one. A file appears as a `file` part
(§4.4), an upload answers a `file` part (`rest.md:240-252`,
`BotRestAPI.ts:2794-2804`), and the gateway adds `url`/`render_url` inside the part
(`gateway.md:171`). A Rust struct named `Attachment` would have nothing to deserialize.

### 4.6 Reactions — there is no reaction object

Verified: reactions are only ever delivered as `MESSAGE_REACTION_ADD` /
`MESSAGE_REACTION_REMOVE` payloads (§5.11). Neither message object carries reactions,
so a UI cannot render existing reaction chips from history or from an event — it must
track them from add/remove events (and loses them on restart). There is no
`GET /messages/{id}/reactions`.

### 4.7 Server

`rest.md:468-475`, `BotRestAPI.ts:3344-3353`.

| Field | Type | Null? |
|---|---|---|
| `id` | UUID | no |
| `name` | string | no |
| `icon` | string (absolute URL) | yes — `null` when the server has none |
| `owner_id` | UUID | yes — `null` when the owner's account was deleted |
| `description` | string | yes |
| `member_count` | integer | no |

Icon URL shape: `…/storage/v1/render/image/public/server_icons/<path>?width=128&height=128&resize=contain&quality=80`
(`utils/avatarUrl.ts:60-62`).

### 4.8 Member, user, bot

Member (`rest.md:488-498`, `BotRestAPI.ts:3368-3382`):

| Field | Type | Null? | Notes |
|---|---|---|---|
| `user.id` | UUID | no | Profile ID |
| `user.username` | string | maybe | Nullable column (`baseline.sql:125`) |
| `user.display_name` | string | maybe | Nullable column (`baseline.sql:126`) |
| `user.domain` | string | yes | The member's instance |
| `user.is_local` | boolean | no | `false` for remote members |
| `user.avatar` | string | **omitted** | Absolute URL; the profile default is the bundled `/default_avatar.webp`, which resolves against the instance host |
| `nick` | string | yes | Nickname in this server |
| `roles` | UUID[] | no | @everyone **not** included |
| `joined_at` | timestamp | no | Membership creation |

For a member whose account is gone, the *member event* payload has `username`,
`display_name`, `avatar_url` all `null` (`gateway.md:297`) — that is the event object
(§5.11), not this one.

User (`rest.md:870`, `BotRestAPI.ts:3384-3392`): `id`, `username`, `display_name`,
`avatar` (omitted when unset), `bio`. Bot IDs are not profiles and answer `404`.

Bot (`GET /users/@me`, `rest.md:856-866`, `BotRestAPI.ts:3394-3404,3161`):

| Field | Type | Notes |
|---|---|---|
| `id` | UUID | |
| `username` | string | |
| `discriminator` | string | `"0000"` in the shipped example |
| `avatar` | string | omitted when unset |
| `bot` | boolean | always `true` |
| `verified` | boolean | |
| `public` | boolean | any server owner may add a public bot |
| `capabilities` | string[] | **not in `rest.md`**; mirrors the READY list (`capabilities.ts:5-11`) |

### 4.9 Channel

`rest.md:557-566`, `BotRestAPI.ts:3355-3366`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `id` | UUID | no | |
| `type` | integer | no | Discord numbering: `0` text, `2` voice, `4` category |
| `guild_id` | UUID | no | Server ID (Discord name kept) |
| `name` | string | no | |
| `position` | integer | no | Same value as `order` |
| `order` | integer | no | |
| `parent_id` | UUID | yes | Same value as `category_id` |
| `category_id` | UUID | yes | |

### 4.10 Category

`rest.md:599`, `baseline.sql:785-798` (the row is returned as stored).

`id` (UUID), `created_at` (ISO 8601), `updated_at` (ISO 8601, nullable),
`server_id` (UUID), `name` (string, ≤100), `order` (integer, default 0),
`federation_status` (string, default `"pending"`).

### 4.11 Role

`rest.md:628`, `BotRestAPI.ts:2468`.

| Field | Type | Notes |
|---|---|---|
| `id` | UUID | |
| `name` | string | ≤100 |
| `color` | string or `null` | `#RGB`, `#RRGGBB` or `#RRGGBBAA`; the app's validator is `src/utils/serverTemplate.ts:205-206` |
| `position` | integer | |
| `permissions` | string | decimal bitmask; parse as `u64`, never `f64` |
| `is_default` | boolean | `true` marks @everyone — list route only |
| `is_admin` | boolean | list route only |
| `mentionable` | boolean | default `true` |
| `hoist` | boolean | default `false` |

Create/edit answer without `is_default`/`is_admin` (`rest.md:643`).

### 4.12 Emoji

`POST /emojis` answers exactly (`create_federated_emoji`, `baseline.sql:12416-12422`):
`id` (UUID), `created_at` (ISO 8601), `name` (string), `url` (string),
`server_id` (`null` for a federated emoji), `uploader` (UUID of the bot's owner),
`domain` (string), `scope` (`"instance"`).

`GET /emojis` returns **every column** of the row (`BotRestAPI.ts:3495,3505`) —
`id`, `created_at`, `updated_at`, `name`, `url`, `server_id`, `uploader`,
`usage_count`, `last_used`, `file_size`, `domain`, `scope`, `is_ai_generated`,
plus `deleted_at` / `discord_emoji_id` where present (`baseline.sql:999-1023`).
Deserialize with `#[serde(default)]` on the extras or allow unknown fields.

`GET /servers/:serverId/emojis` trims to `id`, `name`, `url`, `server_id`,
`discord_emoji_id` (`BotRestAPI.ts:3532`).

### 4.13 Invite preview

`rest.md:880-887`, `BotRestAPI.ts:3468-3475`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `code` | string | no | |
| `invite_url` | string | no | `/invite/<code>` on the gateway's public URL, or the bare path when unset |
| `server_name` | string | no | Falls back to `"Harmony Server"` |
| `server_description` | string | yes | |
| `server_icon_url` | string | yes | |
| `member_count` | integer | no | Accepted memberships only |

### 4.14 Permission override

`rest.md:694`, `BotRestAPI.ts:2939`.

`id` (UUID), `channel_id` (UUID), `target_type` (`"role"` | `"user"`),
`role_id` (UUID, `null` for a user target), `user_id` (UUID, `null` for a role target),
`allow_permissions` (decimal string), `deny_permissions` (decimal string). Rows are
returned as stored, so the mask columns come back as strings.

### 4.15 Thread

`rest.md:395-404`, `BotRestAPI.ts:656-665`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `id` | UUID | no | |
| `channel_id` | UUID | no | Parent channel |
| `parent_message_id` | UUID | no | For a standalone thread, its "started a thread" notice |
| `name` | string | no | `"Thread"` when withheld from a bridge (privacy) |
| `archived` | boolean | no | |
| `locked` | boolean | no | Locked threads refuse bot messages |
| `created_at` | ISO 8601 | no | |
| `discord_thread_id` | string (snowflake) | yes | The pair made by **this bot's** bridge |

### 4.16 Interaction

`gateway.md:325-341`, `cmd.md:117-142`.

| Field | Type | Null? | Notes |
|---|---|---|---|
| `id` | UUID | no | Used in the three response routes |
| `type` | integer | no | `2` (application command) |
| `server_id` | UUID | no | |
| `channel_id` | UUID | no | A thread's parent channel when run in a thread |
| `thread_id` | UUID | yes | |
| `user` | object | no | `id`, `username`, `display_name`, `avatar` (omitted when unset), `bot: false` |
| `member` | object | no | `nickname` (string or `null`), `roles` (UUID[]), `permissions` (permission **names** in the channel) |
| `data.id` | UUID | yes | `null` when the command was deleted after being run |
| `data.name` | string | no | |
| `data.type` | integer | no | `1` (slash command) |
| `data.options` | array | yes | `{name,type,value}`, or one type-1/type-2 entry with nested `options`; absent options are omitted |
| `data.resolved` | object | yes | `users`/`members` keyed by profile UUID, `channels` keyed by channel UUID, `roles` keyed by role UUID |
| `created_at` | ISO 8601 | no | |
| `respond_by` | ISO 8601 | no | `created_at` + 5 s |
| `expires_at` | ISO 8601 | no | `created_at` + 15 min |

### 4.17 Command and option

`cmd.md:62-84`. Command: `id` (UUID, server-set), `name` (1–32, `[letters digits - _]`,
lowercase where the script has case, unique per bot), `description` (1–100), `options`
(≤25), `required_permissions` (string[], Harmony permission names; empty/absent = any
member who can send), `created_at`, `updated_at`.

Option: `type` (1 sub-command, 2 sub-command group, 3 string, 4 integer, 5 boolean,
6 user, 7 channel, 8 role, 10 number), `name`, `description`, `required` (default
false), `choices` (≤25 `{name,value}`, types 3/4/10), `min_value`, `max_value`
(4/10), `min_length`, `max_length` (3, 0–6000), `options` (1/2 only).
Not supported: 9, 11, autocomplete, context menus, per-server commands (`cmd.md:100`).

### 4.18 Message metadata keys

Reserved keys a bot cannot set (dropped on write): `type`, `federated`, `ap_id`,
`from_domain`, `original_url`, `published`, `conversation`, `in_reply_to_ap`,
`pending_thread_ap_id`, `federated_at`, `federated_to`, `automod`, `bot`,
`created_via`, `embeds`, `suppress_embeds`, `webhook`, `interaction` (`rest.md:216`).

Keys the server sets: `bot: true` and `created_via: "bot_api"` on bot messages;
`created_via: "webhook"` and `webhook: {id, name, avatar_url}` on webhook messages
(`wh.md:121`); `interaction: {id, name, user_id, followup?}` on public command
answers; `embeds` once link previews are fetched; `discord_user`, `bridge_source`,
`matrix_user` on relayed rows; `history_import: true` and `bridge_erased: true` as
internal markers that suppress events (`Dispatcher.ts:887,925,948`); system-notice
payload in `metadata.type` + actor/subject IDs (§5.11).

---

## 5. Gateway (WebSocket)

### 5.1 Connect and connection limits

| Item | Value |
|---|---|
| URL | `wss://<instance>/bot-gateway/gateway` (`ws://localhost:3002/gateway` direct) |
| Identified connections per bot | 5, or `WS_MAX_CONNECTIONS_PER_BOT` (`config.ts:72`) |
| Identify deadline | 30 s from socket open, or `WS_IDENTIFY_TIMEOUT_MS` (clamped 1–60 s) |
| Heartbeat interval | 30 000 ms, or `WS_HEARTBEAT_INTERVAL` |
| Session revalidation | every 30 s, or `WS_REVALIDATE_INTERVAL_MS` (1–60 s) |
| Frames per connection | 120 per rolling 60 s, op 1 excluded |

`gateway.md:12-17,44,70,79,87`, `config.ts:69-83`. Every open connection of a bot
receives **every** event, so a bot connected twice handles each event twice
(`gateway.md:17`) — a TUI that reconnects without closing the old socket will duplicate
timelines.

The connection cap is enforced **per gateway process** (`config.ts:71` comment), so a
multi-process deployment can admit more than 5 sockets and the `4029` limit is not a
global invariant.

### 5.2 Frame format

Every frame in both directions is a JSON object in a WebSocket **text** frame
(`gateway.md:21-27`):

| Field | Type | When |
|---|---|---|
| `op` | integer | always |
| `t` | string | op 0 only (event name) |
| `d` | object | event data, or the request body of an op the bot sends |

Rust: `#[serde(tag = "op")]` does not work well because `t` only exists on op 0; use a
struct with `op: u8`, `t: Option<String>`, `d: Option<serde_json::Value>` and dispatch
on `op` first, then `t`.

### 5.3 Opcodes

| Op | Name | Dir | Payload | Notes |
|---|---|---|---|---|
| 0 | Dispatch | recv | `{op:0, t, d}` | Every event |
| 1 | Heartbeat | send | `{op:1}` | `d` not read |
| 2 | Identify | send | `{op:2, d:{token}}` | Once per connection |
| 6 | Register bridge data | send | `d.channels[]`, `d.members[]` | Bridge bots only (§5.12) |
| 7 | Bridge presence update | send | `d.updates[]` | Bridge bots only (§5.12) |
| 11 | Heartbeat ACK | recv | `{op:11}` | Answer to every op 1 |

`gateway.md:31-38`. Unknown opcodes are ignored, and ops 1/6/7 are ignored on an
unidentified connection (`gateway.md:40`, `WSGateway.ts:279-311`). There is **no**
HELLO, RESUME, sequence number or intent (`index.md:40`).

### 5.4 IDENTIFY

```json
{ "op": 2, "d": { "token": "harmony_bot_…" } }
```

`gateway.md:47`. Send it immediately on open; a second IDENTIFY on the same socket
replaces the identity (`WSGateway.ts:389-394`). A missing `d.token` closes with
`4001`; a non-string token is a handled-frame failure → `1008`.

### 5.5 READY

```json
{
  "op": 0,
  "t": "READY",
  "d": {
    "bot": { "id": "3f6c1d2e-…", "username": "ping-bot" },
    "session_id": "b1a7e0c4-…",
    "heartbeat_interval": 30000,
    "capabilities": ["bridge_sound_sync", "bridge_emoji_sync", "bridge_sticker_sync", "bridge_history_import", "bridge_privacy"]
  }
}
```

`gateway.md:52-71`, `WSGateway.ts:419-431`, capability list `capabilities.ts:5-11`.

| Field | Type | Notes |
|---|---|---|
| `bot.id` | UUID | The ID messages and reactions the bot writes carry |
| `bot.username` | string | |
| `session_id` | UUID | New per connection; no resume |
| `heartbeat_interval` | integer | Milliseconds |
| `capabilities` | string[] | Absent on gateways before 1.9.7; treat absent as "new routes 404" |

**READY is not the first frame.** The connection starts receiving events before the
gateway writes READY, so an event can arrive ahead of it (`gateway.md:73`). A client
that blocks its read loop waiting for READY will drop events. Handle READY inside the
normal dispatch loop and only start heartbeating / enabling UI actions after it.

### 5.6 Heartbeat

- Send `{ "op": 1 }` every `heartbeat_interval` ms **after READY**; each is answered
  with `{ "op": 11 }` (`gateway.md:79`).
- The gateway closes with `1000 Heartbeat timeout` any connection whose last heartbeat
  (or its identify, before it has sent one) is older than **2×** the interval, checked
  once per interval. With defaults that is 60–90 s after the last heartbeat
  (`gateway.md:81`, `WSGateway.ts:460-473`). The close frame is sent and the socket
  destroyed without waiting for the answer (`WSGateway.ts:191-194`).
- Heartbeats also keep the bot shown **Online**; the app shows a bot offline once its
  last heartbeat is >90 s old (`gateway.md:83`).
- ACKs are not tracked server-side beyond updating the timestamp; a client does not
  need to await op 11, but should treat missing ACKs as a dead link.

Rust: spawn the heartbeat with `tokio::time::interval` and send through the same
`SplitSink` (or an `mpsc` channel) — `tokio-tungstenite` requires the write half to be
owned by one task.

### 5.7 Frame rate limit

120 frames in any rolling 60 s window per connection, **op 1 excluded**; identify and
bridge frames count. The frame over the limit closes with `4008 rate limited` and
frames already buffered behind it are dropped (`gateway.md:87`,
`WSGateway.ts:176-214,263-277`). The counter is a sliding window, not a fixed bucket.
Reconnect with backoff.

### 5.8 Close codes and reconnect policy

`gateway.md:89-107`, `WSGateway.ts:182-184,255-259,343-378,460-473,490-533`.

| Code | Reason | Cause | Reconnect? |
|---|---|---|---|
| `1000` | `Heartbeat timeout` | No heartbeat for >2× interval | Yes |
| `1000` | `Server shutting down` | Process stopping | Yes, with backoff |
| `1008` | `Invalid payload` | Frame not valid JSON or not handleable | Yes, after fixing the frame |
| `4001` | `Missing token` | Identify without `d.token` | **No** |
| `4003` | `Not authenticated` | No successful identify within the deadline | Yes, identify immediately on open |
| `4004` | `Invalid or expired token` | Unknown/revoked/expired token | **No** |
| `4004` | `Bot not found or inactive` | Valid token, deleted/deactivated bot | **No** |
| `4004` | `Token verification unavailable` | Lookup failed server-side | **Yes, with backoff** |
| `4004` | `Token revoked` / `Token expired` / `Bot inactive` | Session invalidated mid-flight (checked every 30 s) | **No** |
| `4008` | `rate limited` | >120 non-op-1 frames in 60 s | Yes, with backoff |
| `4029` | `Too many connections` | Bot already holds the maximum | Yes, after closing one |

Policy: the code alone is not enough — `4004` is both terminal and retryable, so match
on the **reason string** `"Token verification unavailable"` for the retry case (this is
exactly what `qs.md:137-140` does). Recommended backoff: 1, 2, 4 … capped at 60 s,
reset to 0 on READY.

There is **no resume**: after any close, open a new connection and identify again;
`READY` carries a new `session_id`. Events that occur while disconnected are never
queued or replayed (`gateway.md:107`). Recovery is REST history reads (§6).

### 5.9 Event delivery rules

`gateway.md:111-125`.

| Rule | Detail |
|---|---|
| Server channels only | Events come from channels of servers; **direct messages produce no events** |
| Readers | Active installation in the channel's server with `read_messages`, **and** the channel is visible: either the installation's channel list names it, or (without a list) @everyone can view it |
| Own events | A bot receives events for its own messages and reactions — compare `author.id`/`bot_id` with `bot.id` from READY |
| Every connection | Each open connection of the bot gets each event |
| Encrypted channels | End-to-end encrypted messages produce **no** `MESSAGE_CREATE`/`MESSAGE_UPDATE` |
| No thread events | Thread messages arrive as `MESSAGE_CREATE` with `thread_id`; there is no separate thread event |
| Interactions | Only to the owning bot, every connection, **without** the read_messages check |
| No connection | An outgoing webhook receives the same events by HTTPS POST (`owh.md:19-39`) |

Polling intervals (the gateway reads the database, it does not subscribe):

| Feed | Interval | Batch |
|---|---|---|
| New messages | 1 s | 50 per read |
| Edits + deletes | 2 s | — |
| Reactions (adds; removals via a 200-entry window) | 2 s | 100 new per read |
| Member + moderation events | 2 s | — |
| Installations / permissions recheck | 10 s | — |
| @everyone channel visibility cache | 10 s | — |
| Author name/avatar cache | 10 min | — |
| Nickname cache | 60 s | — |
| `INTERACTION_CREATE` | 250 ms | — |

`gateway.md:118-120`, `Dispatcher.ts:235-251,83-94`, `InteractionPoller.ts:6`.

Consequences a UI must design around:

- An event can arrive up to ~1–2 s after the REST write that caused it. A message the
  bot posts itself may appear in the timeline twice (once from the REST response, once
  from `MESSAGE_CREATE`). De-duplicate by message `id`.
- An event can arrive **before** `READY`.
- A permission change takes up to ~10 s to affect delivery (`perm.md:141-146`).
- A nickname/avatar change can take 60 s / 10 min to show in events (`gateway.md:120`).
- A reaction removal is only detected if the reaction is among the 200 most recently
  seen instance-wide; older removals emit nothing (`gateway.md:284`,
  `Dispatcher.ts:491`).
- Reaction adds triggered before the bot connected are not replayed; the tracker is
  seeded with the last 24 h of rows at startup and those are treated as already known
  (`Dispatcher.ts:432-451`).

### 5.10 Event catalogue (14 events)

`gateway.md:129-144`. Receivers are as stated; every event is `{op:0,t,d}`.

| # | `t` | Sent when | Payload |
|---|---|---|---|
| 1 | `READY` | Identify succeeds | §5.5 |
| 2 | `MESSAGE_CREATE` | A message is posted in a server channel or thread | §4.1 |
| 3 | `MESSAGE_UPDATE` | A message's stored content changes | §4.1 |
| 4 | `MESSAGE_DELETE` | A message is deleted | §5.11 |
| 5 | `MESSAGE_REACTION_ADD` | A reaction is added | §5.11 |
| 6 | `MESSAGE_REACTION_REMOVE` | A reaction is removed | §5.11 |
| 7 | `SERVER_MEMBER_ADD` | Someone joins | §5.11 |
| 8 | `SERVER_MEMBER_REMOVE` | A member leaves, is kicked or banned | §5.11 |
| 9 | `SERVER_MEMBER_KICK` | A member is kicked | §5.11 |
| 10 | `SERVER_BAN_ADD` | A member is banned | §5.11 |
| 11 | `SERVER_BAN_REMOVE` | A ban is lifted | §5.11 |
| 12 | `REFRESH_ATTACHMENTS` | A member asks for expired Discord links to be renewed | §5.11 |
| 13 | `BRIDGE_CONFIG_UPDATE` | A Discord bridge's config changes | §5.11 |
| 14 | `INTERACTION_CREATE` | A member runs one of the bot's slash commands | §4.16 |

There is no presence, typing, channel/role/emoji CRUD, pin, thread-lifecycle or
read-state event. See §8.

### 5.11 Event payloads

**MESSAGE_UPDATE** — same object as `MESSAGE_CREATE` (§4.1) built from the edited row,
with `edited_timestamp` = edit time (`gateway.md:230`, `Dispatcher.ts:917-937`). No
event for changes that leave content alone (metadata, pins, link previews) and none for
a silent content patch.

**MESSAGE_DELETE** (`gateway.md:236-245`, `Dispatcher.ts:968-978`):

```json
{
  "op": 0,
  "t": "MESSAGE_DELETE",
  "d": {
    "id": "0b9e5a61-…",
    "server_id": "1e7d…",
    "channel_id": "c2d4f8a0-…",
    "thread_id": null,
    "metadata": { "bot": true, "created_via": "bot_api" }
  }
}
```

`metadata` is the message's last metadata (object or `null`); for a bridge the message
is withheld from, only the recorded mapping. Sent once per message. A row removed by
other means (e.g. deleting its channel) emits nothing. Only `id`, `channel_id` and
`thread_id` are safe to rely on.

**MESSAGE_REACTION_ADD** (`gateway.md:250-278`, `Dispatcher.ts:567-576`):

| Field | Type | Notes |
|---|---|---|
| `reaction_id` | UUID | Match removals on this |
| `message_id` | UUID | |
| `server_id` | UUID | **Missing from the docs' JSON example**, present in the table and code |
| `channel_id` | UUID | |
| `user_id` | UUID | `null` when a bot reacted |
| `bot_id` | UUID | `null` when a member reacted |
| `emoji.id` | UUID | `null` for a Unicode or bridged emoji |
| `emoji.name` | string | Shortcode, the Unicode character, or `discord:name:id` |
| `emoji.url` | string | Absolute URL for a custom emoji, else `null` |
| `emoji.animated` | boolean | `true` when the URL is a `.gif` or asks for `animated=true` |
| `metadata` | object | `{}` when unset |

**MESSAGE_REACTION_REMOVE** — same shape and **the same values as the add**, as the
gateway saw it when added; match on `reaction_id` (`gateway.md:282`).

**Member events** — `SERVER_MEMBER_ADD`, `SERVER_MEMBER_REMOVE`,
`SERVER_MEMBER_KICK`, `SERVER_BAN_ADD`, `SERVER_BAN_REMOVE` share one payload
(`gateway.md:288-297`, `Dispatcher.ts:307-315`):

```json
{
  "op": 0,
  "t": "SERVER_MEMBER_ADD",
  "d": {
    "server_id": "55555555-0000-0000-0000-000000000005",
    "user": { "id": "22222222-…", "username": "bob", "display_name": "Bob", "avatar_url": null }
  }
}
```

A kick or ban also ends the membership, so it is **followed by**
`SERVER_MEMBER_REMOVE`. The moderator and reason are not included. For a deleted
account `username`, `display_name` and `avatar_url` are all `null`.

**REFRESH_ATTACHMENTS** (`gateway.md:301-313`, `index.ts:409-413`) — camelCase field
names, unlike every other event:

```json
{
  "op": 0,
  "t": "REFRESH_ATTACHMENTS",
  "d": { "messageId": "0b9e5a61-…", "channelId": "c2d4f8a0-…", "content": [ … ] }
}
```

Sent to the bot that wrote the message only when bridge attachment mode is `refresh`,
the message has a `file`/`url` part on `discordapp.com`/`discordapp.net`, and the bot
is connected; requests for one message within 15 s are deduplicated. The bot re-signs
and writes back with a silent content patch.

**BRIDGE_CONFIG_UPDATE** (`gateway.md:315-319`) — sent to a Discord bridge's bot once
per new connection (at the first config check after identify) and whenever the bridge's
configuration changes. `d` is the object `GET /bridge/v2/config` returns, except
`base_url` is `null` when no public URL is configured. Config is polled every 5 s
(`BRIDGE_CONFIG_POLL_MS`, 1–60 s).

**INTERACTION_CREATE** — §4.16. Answer within 5 s via the callback routes; an
interaction that cannot reach an open connection fails at once, and one older than its
5 s window when read is dropped (`gateway.md:123`).

### 5.12 Bridge opcodes (op 6, op 7)

Only a bot of type Bridge needs these; a normal TUI bot ignores them entirely.

- **op 6** registers the remote platform's members for the channels the bridge relays
  (`gateway.md:347-395`). `d.channels` is required: `[{ harmonyChannelId, members? }]`.
  An entry whose channel ID is not a UUID, is in a server with no active installation,
  or names a channel the bot cannot see, is dropped. A channel entry with its own
  `members` array (empty included) uses that list; the top-level `members` applies to
  the entries without one, and only when they are all in one server. Each registration
  **replaces** the previous one — publish the whole snapshot, not one channel per frame.
  The data lives in the gateway's memory while any connection of the bot is open.
  Member fields: `id`, `username`, `displayName`, `avatarUrl`, optional `bannerUrl`,
  `accentColor`, `harmonyRoleIds`, `roles`, `joinedAt`, `createdAt`, `presenceStatus`,
  `customStatus`, `source`.
- **op 7** changes presence of already-registered members
  (`gateway.md:397-419`): `d.updates[] = { id, presenceStatus?, customStatus? }`.
  `id` must be 1–20 digits; `presenceStatus` is `online|away|busy|offline` (absent or
  unknown leaves the stored value); `customStatus: null` clears it, `{text, emoji}`
  sets it (text cut to 128, emoji to 64). IDs not in the last op 6 registration are
  ignored.

---

## 6. Pagination

Only two routes take a `limit` (`rest.md:62`). Everything else returns the whole list.

### 6.1 `GET /channels/{channel.id}/messages`

`rest.md:266-282`, `BotRestAPI.ts:1447-1496`.

| Query | Default | Max | Semantics |
|---|---|---|---|
| `limit` | 50 | 100 | A larger value returns 100; a value that is not a whole number uses the default |
| `before` | — | — | ISO 8601 timestamp; only messages created **before** it |
| `after` | — | — | ISO 8601 timestamp; only messages created **after** it |

- **Ordering is always newest-first**, with or without `after` (`BotRestAPI.ts:1472`).
  There is no oldest-first mode and no cursor object.
- `after` + `limit` returns the **newest** `limit` messages after that time, not the
  oldest. To read a gap forward, you cannot: page backwards with `before` set to the
  oldest `timestamp` you hold until you reach messages you already have
  (`rest.md:278`).
- Canonical catch-up loop after a reconnect: call with no `before`, take the newest
  page, keep pages while the oldest `timestamp` on the page is newer than your newest
  stored message.
- The result mixes in **thread messages** (whose `channel_id` is the parent channel and
  `thread_id` is set), **soft-deleted** messages and **end-to-end encrypted** messages,
  and the message object has no field marking the last two (`rest.md:276`).
- For a bridge bot, rows the native author opted out of are filtered **after** the page
  is read, so a page can hold fewer than `limit` results, and the route refills from
  older rows up to a bounded number of extra reads (`rest.md:282`,
  `BotRestAPI.ts:1480-1491`).
- **Undocumented**: the query parser clamps `limit` to `1..max`
  (`BotRestAPI.ts:372-375`), so `limit=0` returns **1** message, not 0.
- **Undocumented**: a bot without the `READ_MESSAGE_HISTORY` bit (and without the
  channel in its installation channel list) can only read messages created **after it
  was installed**; older rows are silently absent here and `404` on
  `GET /messages/:id` (`BotRestAPI.ts:1458-1461,3275-3282`, `botPerms.ts:177-180`).
  The installation always counts `read_messages` as `VIEW_CHANNEL` +
  `READ_MESSAGE_HISTORY` (`perm.md:18`), so this bites only when @everyone's override
  denies history in that channel.

### 6.2 `GET /servers/{server.id}/members`

`rest.md:477-500`, `BotRestAPI.ts:2178-2225`.

| Query | Default | Max | Semantics |
|---|---|---|---|
| `limit` | 100 | 1000 | A larger value returns 1000; a non-integer uses the default |
| `after` | — | — | **User ID** (UUID). Only members whose `user.id` sorts after it |

- Ordered by `user.id` ascending — a keyset cursor, not an offset. Page until a page
  holds fewer than `limit` members (`rest.md:486`).
- A malformed `after` answers `400 after must be a user id`; the cursor is compared to
  `user_id` in SQL, so it must be a UUID, not a username.
- No status filter: pending and banned memberships appear (`rest.md:486`).
- Roles are resolved per page with batched queries; a role lookup failure answers
  `500 Role lookup failed` (`BotRestAPI.ts:2219`).

### 6.3 Unpaginated lists

`GET /servers/:id/channels`, `/categories`, `/roles`, `/emojis`, `/emojis/discord`,
`/stickers/discord`, `/sounds`, `GET /channels/:id/permission-overrides`,
`GET /emojis`, `GET /commands` return every row with no cursor (`rest.md:62`).

Practical bounds: a server holds at most **100 channels** (`docs/run/setup.md:53`); a
Discord-linked emoji or sticker list is never partial — past 10 000 rows the answer is
`500 { "code": "LIST_TOO_LARGE" }` (`rest.md:753`). There are no `has_more`, `total` or
`next_cursor` fields anywhere; the only count in the API is
`InvitePreview.member_count`.

---

## 7. Rate limits

### 7.1 REST buckets

`perm.md:148-170`, `auth.ts:65-105`.

| Rule | Detail |
|---|---|
| Unit | One bucket = one route **on one channel/server**; every message/emoji/invite ID on a route shares it |
| All methods share | `GET` and `POST /channels/:id/messages` are one bucket |
| Spelling | Case, a trailing slash and the query string do not matter; a non-UUID channel/server param becomes one shared `:invalid` bucket |
| Unmatched paths | All share one `unmatched` bucket per bot |
| Default | 100 requests per 60 s window per bucket, window starts at the first request |
| Tunables | `RATE_LIMIT_MAX_REQUESTS`, `RATE_LIMIT_WINDOW_MS` (`config.ts:85-86`) |
| Overall ceiling | 600 requests per 60 s per bot, 3 000 for a linked Discord bridge; instance staff set it under Anti-spam |
| Ceiling counting | Bucket-refused requests count toward it; the ceiling never reads below the route limit per minute; exempt owners have none |
| Not counted | Requests refused with `401` or `503` during authentication |
| Unknown paths | Also authed and rate-limited (`BotRestAPI.ts:534-535`) |

Refusal (`perm.md:156-166`):

```http
HTTP/1.1 429 Too Many Requests
Content-Type: application/json
Retry-After: 17

{ "error": "Rate limit exceeded", "retry_after": 17 }
```

`retry_after` (seconds) and `Retry-After` are identical; **no other rate-limit headers
are sent** — there is no `X-RateLimit-Remaining`/`Reset`/`Bucket` on the bot API, so
proactive pacing needs your own token bucket mirroring the rules above.

If the rate-limit RPC itself fails, the gateway counts locally per process and keeps
refusing at the same numbers, with a 3 000/min overall fallback (`auth.ts:131-164,221-245`).

### 7.2 Interaction bucket

The three interaction routes share **one** bucket per bot: **50 requests per 5 s**,
counted apart from the overall ceiling (`perm.md:170`, `auth.ts:69-73,185-206`). Each
interaction already permits one callback, its edits and 10 follow-ups. Members running
commands are limited to 5 per 10 s per account (`perm.md:170`, `cmd.md:111`).

### 7.3 Gateway

120 frames per rolling 60 s per connection, op 1 excluded, close `4008` (§5.7).

### 7.4 Other limits worth encoding

| Limit | Value | Source |
|---|---|---|
| Message text (`text` parts only) | instance limit, 2000 chars by default | `rest.md:180` |
| Different emoji on one message | 20; the 21st answers `400` with `code` `30010` | `rest.md:380` |
| JSON request body | 10 MB | `index.ts:33` |
| Upload body deadline | 30 min from first byte, then `408` | `rest.md:254`, `index.ts:48` |
| Uploads in flight per bot | 3 concurrent, 10 queued, >13 → `429` | `rest.md:254` |
| Bot file storage per server | 5 GB default, shared with mirrored files/avatars | `rest.md:254` |
| Discord emoji import | 512 KiB | `perm.md:195` |
| Commands per bot | 100; options per command 25; choices 25 | `cmd.md:71,81` |
| Webhooks per channel / server | 10 / 50 | `wh.md:115` |
| Incoming webhook body | 64 KB (1 MB on `/github`) | `wh.md:38,97` |
| Incoming webhook rate | 5/2 s and 30/min per URL; 10/2 s and 60/min per channel | `wh.md:146-150` |
| Outgoing webhook rate | instance events per minute (120 default) per webhook | `owh.md:98` |
| Bridge v2 public routes | IP-based, `RateLimit`/`RateLimit-Policy` headers, `429` `{error, code:"rate_limited"}` | `perm.md:176-186` |

The Bridge v2 public route limits are the only place Harmony sends IETF
`RateLimit`/`RateLimit-Policy` headers (`perm.md:186`); a bot client never sees them.

---

## 8. Gaps and gotchas

### 8.1 The seven verified questions

**(a) Can a bot enumerate the servers it is installed in? — No.**

- Docs, explicitly: "The API has no route that lists the servers a bot is installed
  in." (`rest.md:135`).
- Code: the 68 registered routes (`BotRestAPI.ts:396-533`) contain no collection of
  servers. Every server-scoped route takes a `:serverId` the caller must already know
  (`BotRestAPI.ts:449-471`), and installation lookups are always
  `bot_server_permissions` filtered by `bot_id` **and** `server_id`
  (`BotRestAPI.ts:3301-3311`, `botPerms.ts:266`).
- `GET /users/@me` returns no guild list; `GET /status` would list *connected bots* but
  needs a user session (`index.ts:185-214`).

Consequence: the server IDs must come from the user (config file) or be harvested from
events (`MESSAGE_CREATE.server_id`, `SERVER_MEMBER_*`, `INTERACTION_CREATE.server_id`).
A TUI cannot show a server list on first run with only a token. Store discovered
`server_id`s and let the user pin the ones to join.

**(b) Direct messages — No.**

- Gateway: "Server channels only. Events come from channels of servers. Direct messages
  produce no events." (`gateway.md:113`).
- Commands: "Direct messages and copies of servers hosted on another instance have no
  bot commands." (`cmd.md:106`).
- Outgoing webhooks: "Never sent: direct messages…" (`owh.md:39`).
- Code: DMs are `messages.conversation_id` with `channel_id` NULL
  (`baseline.sql:858-860`); every bot route resolves a channel to a server via
  `lookupChannelServerId` and fails without one
  (`BotRestAPI.ts:3240-3249`), and the dispatcher's `resolveServerId(null)` returns
  `null`, which drops the row (`Dispatcher.ts:594-596,898-902`). A DM's conversation ID
  passed to any `/channels/:id/...` route therefore answers
  `403 Missing permission: …`.

**(c) Presence for humans — No.**

- No presence event exists (`gateway.md:129-144`), and no route returns a member's
  status. The member object has no `status`/`presence` field
  (`rest.md:488-498`, `BotRestAPI.ts:3368-3382`) even though the query selects
  `profiles.status` (`BotRestAPI.ts:2200`) and the formatter drops it.
- `bot_presence` is written for **bots** only (`WSGateway.ts:398-408,447-457`), and is
  readable only through the user-authenticated `GET /status`
  (`index.ts:186-214`).
- op 7 presence is bridge-only, for **remote platform** members a bridge registered
  (`gateway.md:397-419`) — not Harmony humans.

A TUI can show "member exists / nickname / roles", never "online".

**(d) Typing indicators — No.**

- `POST /channels/:id/typing` "has no effect" and only answers as a send would
  (`rest.md:362-364`); the handler performs a permission check and returns `204`
  without touching the database (`BotRestAPI.ts:2127-2138`).
- There is no `TYPING_START` event in the catalogue (`gateway.md:129-144`), and no
  typing table or poller in the dispatcher (`Dispatcher.ts:232-252`).

So a bot cannot show "Alice is typing…" and cannot tell a human it is typing.

**(e) Message edit history — No.**

- `PATCH /messages/:id` overwrites the stored content and bumps `updated_at`
  (`rest.md:297-303`); `MESSAGE_UPDATE` carries only the new state
  (`gateway.md:228-232`).
- No route or event returns prior revisions (`BotRestAPI.ts:396-533`), and the
  `messages` table has **no** history column (`baseline.sql:849-889`; the
  `edit_history` column at `baseline.sql:372` belongs to `posts`, not messages).
- Silent content patches are invisible by design (`rest.md:319-325`): content can
  change with no event and no `edited_timestamp` bump, so even a client that caches
  every event can hold stale text.

**(f) End-to-end encrypted channels — read state only, never content.**

- `GET /channels/:id/encryption` → `{ "messages_encrypted": true|false }`, the
  authoritative effective state including inherited server settings; `503` if the
  lookup fails or is not a boolean, and a relay must treat that as **unsafe**, not as
  plaintext (`rest.md:504-508`, `BotRestAPI.ts:1299-1340`).
- No `MESSAGE_CREATE`/`MESSAGE_UPDATE` for encrypted messages (`gateway.md:117`,
  `Dispatcher.ts:890-893,921`).
- However `GET /channels/:id/messages` **includes** encrypted messages and the message
  object "has no field that marks" them (`rest.md:276`) — see gotcha 4 below.
- Interaction options never arrive from an encrypted channel because commands are not
  offered there (`cmd.md:107`).

**(g) Forums — No, and there are no other channel types either.**

- "A server holds text channels and voice channels. There are no other channel types
  (no announcement, forum or stage channels, and no NSFW flag). A server holds at most
  100 channels." (`nexus/harmony/docs/run/setup.md:53`).
- Code: the channel `type` domain is `0=text, 1=voice, 2=category`
  (`baseline.sql:816-820`), exposed as `0`, `2`, `4` (`BotRestAPI.ts:266`), and
  `createChannel` collapses every input to text unless `type === 1`
  (`BotRestAPI.ts:2314`). No `forum`, `thread-only` or tag concept exists in the bot
  gateway (`grep -ri forum bot-gateway/src` → no hits).
- Threads are a separate object reachable only by ID (§8.2).

### 8.2 Other gaps a UI client hits immediately

| # | Gap | Evidence |
|---|---|---|
| 1 | **REST history has no `content_raw`.** Attachments, custom emoji, links with previews, embeds and mention targets are unrecoverable from `GET /channels/:id/messages`; only `content` text survives. A restarted TUI cannot render a channel's files. | `rest.md:157`, `BotRestAPI.ts:3319-3342` |
| 2 | **REST messages have no `server_id`.** Only events carry it. A message fetched by ID cannot tell you which server it belongs to. | `BotRestAPI.ts:3319-3342`, `rest.md:143-157` |
| 3 | **`GET /messages/:id` returns `author: null` for member-authored rows**, while the same row in `GET /channels/:id/messages` has a full author (that list joins `user`, the single-message route joins only `bot`). | `rest.md:288`, `BotRestAPI.ts:1498-1514,1464-1470`, `formatMessage` at `3319-3332` |
| 4 | **Encrypted and soft-deleted rows are indistinguishable in history.** No `encrypted` or `is_deleted` field; only thread rows are marked, by `thread_id`. | `rest.md:276` |
| 5 | **No reactions in history and no reaction read route.** A UI must aggregate `MESSAGE_REACTION_*` from connect time; counts and old reactors are lost on restart. | §4.6, `rest.md:366-387` |
| 6 | **No way to list a channel's threads.** Only `GET /threads/:id` (by ID) and `GET /channels/:id/threads/lookup?discord_thread_id=` (bridge). A TUI cannot build a thread browser. | `BotRestAPI.ts:437-445` |
| 7 | **No channel/category delete, no channel rename.** `PATCH /channels/:id` accepts only `order` and `category_id`; there is no `DELETE /channels/:id` and no `DELETE /servers/:id/categories/:id`. | `rest.md:584-593`, `BotRestAPI.ts:396-533` |
| 8 | **No pins, no search, no unread state.** No pin route or event, no search route, no read-receipt route. | `BotRestAPI.ts:396-533` |
| 9 | **No file URLs older than 7 days.** `url` on a `file` part is a 7-day signed link (`rest.md:254`); a normal bot has no re-sign route (the bridge-files download route requires a Matrix room binding). | `rest.md:254,537`, `BotRestAPI.ts:1108-1122` |
| 10 | **Member events carry only `id`, `username`, `display_name`, `avatar_url`** — no nickname, roles or permissions, so a profile card needs a `GET /servers/:id/members` page. | `gateway.md:288-297` |
| 11 | **`mentions` is empty for app-written parts.** The array reads `user_id`; the app writes `userId`. Parse `content_raw` mention parts instead. | `gateway.md:167`, `Dispatcher.ts:1153-1160` |
| 12 | **The same message has two different `content` strings**: the gateway trims, the REST formatter does not. Message text ending in a space differs between the event and a later fetch. | `Dispatcher.ts:1147` vs `BotRestAPI.ts:3406-3417` |
| 13 | **No ability to know which servers to reconnect to after restart** — same as (a); the server set is client state. | §8.1(a) |
| 14 | **`GET /servers/:id/channels` hides channels the bot cannot see**, so a sidebar cannot grey out unreadable channels; they are simply absent. | `BotRestAPI.ts:2247-2248` (contradicts `rest.md:555`) |
| 15 | **No way to fetch a user's mutual servers or a bot's owner**, no `GET /servers/:id/bans`, no audit-log read even though bot writes are audited. | `BotRestAPI.ts:396-533` |
| 16 | **Rate-limit headers are absent**, so `429` is the only feedback; a burst UI can only react, not pace. | `perm.md:166` |

---

## 9. Fixtures

`nexus/fixtures/bot/*.json` — one file per distinct payload, valid JSON, wrappers kept.

| File | Payload | Origin |
|---|---|---|
| `ready.json` | `{op,t,d}` READY | `gateway.md:52-63`, capabilities from `capabilities.ts:5-11` |
| `message-create-event.json` | `{op,t,d}` MESSAGE_CREATE | `gateway.md:173-201` |
| `message-update-event.json` | `{op,t,d}` MESSAGE_UPDATE | Shape of `Dispatcher.ts:1082-1098` with an edit timestamp (no doc example) |
| `message-delete-event.json` | `{op,t,d}` MESSAGE_DELETE | `gateway.md:236-245`, `Dispatcher.ts:968-974` |
| `reaction-events.json` | object keyed `MESSAGE_REACTION_ADD` / `MESSAGE_REACTION_REMOVE`, each `{op,t,d}` | `gateway.md:250-278`; `server_id` added per the field table and `Dispatcher.ts:570` |
| `message-object.json` | REST Message, bare | `rest.md:143-157`, `BotRestAPI.ts:3319-3342` |
| `server.json` | Server, bare | `rest.md:468-475`, `BotRestAPI.ts:3344-3353` |
| `member-list-page.json` | Member[] page, bare | `rest.md:488-498`, `BotRestAPI.ts:3368-3382`, `api/__tests__/BotRestAPI.test.ts:735-750` |
| `channel.json` | Channel, bare | `rest.md:557-566` |
| `category.json` | Category row, bare | `rest.md:599`, `baseline.sql:785-798` |
| `role.json` | Role row as `GET /roles` returns it | `rest.md:628`, `BotRestAPI.ts:2468` |
| `emoji.json` | `POST /emojis` response | `rest.md:741`, `create_federated_emoji` in `baseline.sql:12416-12422` |
| `invite-preview.json` | InvitePreview, bare | `rest.md:880-887`, `BotRestAPI.ts:3468-3475` |
| `user.json` | User, bare | `rest.md:870`, `BotRestAPI.ts:3384-3392` |
| `bot-me.json` | `GET /users/@me` | `qs.md:56-66` plus `capabilities` from `BotRestAPI.ts:3161` |
| `error-examples.json` | array of `{status, body}` | `rest.md:33-56`, `auth.ts:20-50`, `BotRestAPI.ts:67-70` |

Fixture conventions (documented so nobody mistakes them for new fields):

- Doc examples elide UUIDs with `…`; the fixtures expand them to canonical UUIDs so
  they deserialize as `Uuid`. Prefixes are preserved (`0b9e5a61-…` →
  `0b9e5a61-0000-4000-8000-000000000005`).
- `reaction-events.json` and `error-examples.json` group several payloads, because the
  docs present them as one table; each inner value keeps its wire shape.
- `message-object.json` is the bot's reply (`d7f1c3a2-…`) to alice's message
  (`0b9e5a61-…`), so `reply_to` points at a different message.
- `emoji.json` is the create response, not the `GET /emojis` row (that one has every
  column; see §4.12).
- `bot-me.json` includes `capabilities`, which `qs.md:56-66` omits and
  `BotRestAPI.ts:3161` always adds.

---

## 10. Where docs and code disagree

Follow the code. Each row names the doc claim and the code fact.

| # | Docs say | Code does | Effect on a client |
|---|---|---|---|
| 1 | `GET /servers/:id/channels` "Returns every channel of the server, ordered by `order`, including channels the bot cannot see" (`rest.md:555`) | Filters to channels the bot can see (`BotRestAPI.ts:2247-2248`) | Sidebar shows only readable channels; no greyed-out entries |
| 2 | REST message `author` "is the account that wrote the row: for a relayed Discord message it is the bridge bot" (`rest.md:157`) | Returns the relayed author from metadata — Discord snowflake `id`, `bot:false`, `discord_user:true` (`BotRestAPI.ts:3326`, `bridge/bridgeAuthors.ts:36-51`) | `author.id` can be a snowflake; render the relayed name, and don't assume UUID |
| 3 | Reaction example JSON has no `server_id` (`gateway.md:264-278`) | Always sends `server_id`; the field table and code both have it (`gateway.md:254`, `Dispatcher.ts:570`) | Deserialize `server_id` as required on reaction events |
| 4 | `GET /users/@me` fields list stops at `public` (`rest.md:856-866`); the quickstart example omits it too (`qs.md:56-66`) | Adds `capabilities` (`BotRestAPI.ts:3161`) | Extra field; ignore or use it for feature gating |
| 5 | Nothing about a history cutoff | A bot without `READ_MESSAGE_HISTORY` reads only messages newer than its install; older rows vanish from the list and `404` by ID (`BotRestAPI.ts:1458-1461,3275-3282`) | Empty-looking history in channels where @everyone denies history |
| 6 | `limit` "at most 100" / "at most 1000"; nothing about 0 | Clamped to `1..max`, so `limit=0` → 1 (`BotRestAPI.ts:372-375`) | Don't send 0 expecting an empty page |
| 7 | Categories list described as the whole set (`rest.md:597-599`) | Filtered for bots without `manage_channels` (`BotRestAPI.ts:2444-2453`) | Fewer categories than the server has |
| 8 | Error table lists no `MEMBER_TIMED_OUT` (`rest.md:37-56`) | A timed-out actor's write answers `403` with `code: "MEMBER_TIMED_OUT"` (`BotRestAPI.ts:78-81`) | Handle unknown `code` values generically |
| 9 | `POST /channels/:id/threads` errors list three `400`s (`rest.md:429`) | Also validates `created_at` (`400 created_at must be an ISO 8601 time`) and `starter_discord_user_id` (`BotRestAPI.ts:691-697`) | Two undocumented `400` messages |
| 10 | Connection limit stated as a per-bot global (`gateway.md:17`) | Enforced per gateway process (`config.ts:71` comment, `WSGateway.ts:373-378`) | Multi-process instances admit more sockets |

---

## 11. Minimal client checklist

1. Config: `rest_base`, `ws_url`, `token`. Health probe `/health` before anything.
2. `GET /users/@me` → bot id, username, `capabilities`.
3. WS connect → send op 2 immediately → dispatch loop that handles `READY` **inside**
   the loop → start op 1 every `heartbeat_interval` → dedupe by `(t, d.id)`.
4. Maintain server IDs from config plus every `server_id` seen in events; never expect
   a list route.
5. On `MESSAGE_CREATE`: drop `author.id == bot.id` if the UI echoes optimistically;
   render `content_raw` (not `content`), treat `mentions` as unreliable.
6. On reconnect: `GET /channels/:id/messages` newest-first, page backwards with
   `before` until you overlap; accept that files/reactions/previews are lost for the
   gap.
7. Never `PATCH /messages/:id` for anything but your own messages; use
   `/content-silent` only for URL renewal.
8. Treat `401` as terminal, `503` as retryable, `429` as `retry_after` seconds, `409`
   as "drop and re-fetch".
