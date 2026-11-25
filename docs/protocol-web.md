---
title: Harmony web protocol — implementation reference
description: An implementation-grade spec of what Harmony's own Vue app speaks — instance discovery, GoTrue auth, PostgREST tables and RPCs, Realtime Broadcast topics, presence, typing and message inserts — for a Rust client that already speaks the bot protocol.
---

# Harmony web protocol

Harmony's web app has no private API: it is a Supabase project with row level security,
and the browser holds the user's own JWT. That makes the whole data plane reproducible
from a Rust client — discovery gives you the Supabase URL and anon key, GoTrue gives you
a token, PostgREST reads and writes, Realtime pushes Broadcast frames that triggers send.

This page is that surface, with the exact query strings and payloads the app uses. It only
covers what a chat client needs; a feature the app has and this page omits is not part of
the contract.

## Sources and how to read citations

| Shorthand | File |
|---|---|
| `baseline.sql:N` | `nexus/harmony/db_schema/migrations/20260101000000_baseline.sql` |
| `m/<version>_<name>.sql:N` | a later migration under `nexus/harmony/db_schema/migrations/` |
| `m/<version>:N` | short for the one migration whose filename starts with that version |
| `SURFACE.tsv:N` | `nexus/harmony/db_schema/SURFACE.tsv` (generated RPC inventory) |
| `src/…:N` | `nexus/harmony/src/…` (the Vue app) |
| `fb/…:N` | `nexus/harmony/federation-backend/src/…` |
| `timbre/…:N` | `client/src/services/…` (the other client in this workspace) |
| `tui/…:N` | `nexus/nexus_client/src/harmony/…` (the Rust client this page serves) |
| `bot §N` | `nexus/docs/protocol-bot.md` |

Line numbers are from the files as they are in this checkout. When the Vue app and an
older client disagree, this page follows the app; §9 lists the disagreements that matter.

Conventions:

- `UUID` is a canonical lowercase 36-character string. `timestamptz` is ISO 8601 with an
  offset, e.g. `2026-11-25T14:30:00.123456+00:00` (PostgREST emits `+00:00`).
- `X or null` means the key is present and may be `null`; `X?` means the key may be absent.
  Rust: `Option<T>` plus `#[serde(default)]` on every optional field.
- A PostgREST *read* returns a JSON array. `.single()` in the app sends
  `Accept: application/vnd.pgrst.object+json`; zero rows then answers `406` `PGRST116`.
- Every request carries `apikey: <anon key>`. Every authenticated request adds
  `Authorization: Bearer <access token>`. Without a token, RLS sees `anon`.
- PostgREST error bodies are `{"code","details","hint","message"}`; `code` is the
  PostgreSQL SQLSTATE (`42501` = RLS, `23514` = check, `23505` = unique, `P0002` = no data)
  or a `PGRST…` code. Read `message` for the app's own refusals (`SLOWMODE_ACTIVE:12`).

---

## 1. Discovery

### 1.1 Request

```
GET https://<instance-origin>/api/federation/instance-info
Accept: application/json
```

No auth, no cookies, no query. The instance's reverse proxy strips `/api/federation`
before the backend sees it (`self-host/Caddyfile:275-278`), and the backend mounts the
same router at both `/instance-info` and `/api/federation/instance-info`
(`fb/server.ts:141-142`), so either path answers.

Discovery is not rate limited: the route is mounted without `discoveryLimiter`
(`fb/server.ts:141-142`), unlike `/instances/probe`, which applies `discoveryLimiter`
(`fb/server.ts:196`, `fb/routes/instanceProbe.ts:219,288`).

### 1.2 Response

`sendSuccess` spreads the object after `success: true` (`fb/utils/response.ts:8-10`),
and the handler supplies exactly six keys (`fb/routes/instanceInfo.ts:13-21`):

```json
{
  "success": true,
  "name": "Harmony",
  "domain": "har.mony.lol",
  "version": "1.9.8",
  "supabaseUrl": "https://db.har.mony.lol",
  "supabaseAnonKey": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9…",
  "capabilities": ["encrypted-history", "outgoing-webhooks", "…"]
}
```

| Field | Type | Source | Notes |
|---|---|---|---|
| `success` | bool | literal `true` | A `false` here is an error body, not a config |
| `name` | string | `instance_config.instance_name` → `INSTANCE_NAME` → domain (`fb/utils/instanceName.ts:30-32`) | Never empty; cached 30 s |
| `domain` | string | `INSTANCE_DOMAIN` (`fb/config/index.ts`) | Host part of local handles and AP ids |
| `version` | string | `VERSION`, default `1.9.8` (`fb/config/index.ts:25`) | Free-form; use for logging, not gating |
| `supabaseUrl` | string | `PUBLIC_SUPABASE_URL` | `https://db.<domain>`; no trailing slash |
| `supabaseAnonKey` | string | `SUPABASE_ANON_KEY` | The JWT every Supabase request needs as `apikey` |
| `capabilities` | string[] | static `CAPABILITIES` (`fb/config/capabilities.ts:7-71`) | Present since 1.9.0; `[]` on older servers |

### 1.3 Capabilities

One entry per feature whose RPCs/columns/routes exist on that server
(`fb/config/capabilities.ts:1-5`). Gate optional UI on these strings, never on `version`.
The 1.9.8 list, verbatim:

| Capability | What it unlocks for a chat client |
|---|---|
| `encrypted-history` | key relay, `channel_encryption_settings.history_visibility` |
| `outgoing-webhooks` | webhook admin RPCs (not needed to chat) |
| `channel-create-options` | `create_channel` description/slowmode/encryption args |
| `instance-staff-roles` | `profiles.instance_role` |
| `custom-stickers` | `stickers` table, `create_sticker` |
| `server-directory` | `servers.federation_discoverable`, directory routes |
| `instance-invites` | admin invitations |
| `federated-polls` | `message_polls.remote`, poll votes |
| `supporter-messages` | funding RPCs |
| `signup-limits` | `signup_*` config keys |
| `signup-captcha` | `signup_captcha_*`, the hook's `428` |
| `thread-departures` | `leave_thread`, `thread_departures` |
| `invite-widgets` | `GET /widget/invite/:code` |
| `instance-limits` | `profiles.dm_policy`, per-user caps |
| `signup-review` | admin signup review |
| `account-removal` | admin account deletion |
| `channel-host-flags` | `channels.host_flags`, `channel_categories.host_flags` |
| `following-import` | following import RPCs |
| `bot-commands` | `get_channel_bot_commands`, `invoke_bot_command` |
| `voice-announce` | DB-driven voice occupancy, `voice-channels:` topics |
| `member-directory` | `get_server_member_list` and friends |
| `signal-relay` | `send_signal`, `voice_ice_servers` |

A client that only reads and sends channel/DM messages needs **none** of them: the
message, channel, server, member and notification paths are pre-1.9.0. `member-directory`
is the one worth branching on: its absence is detected as `PGRST202`, and the fallback is a
paged `user_servers` read plus `profiles` lookups (§3.3, §4.11).

### 1.4 CORS

`cors({ origin: corsOrigins(CORS_ORIGIN), credentials: true })` (`fb/server.ts:81-84`),
where the allow-list is `CORS_ORIGIN`'s comma-separated entries plus the two native-app
origins `tauri://localhost` and `http://tauri.localhost` (`fb/config/nativeOrigins.ts:12-18`).
`CORS_ORIGIN` is set to `https://$DOMAIN` by the installer
(`self-host/configure.sh:769`).

| Caller | Result |
|---|---|
| Page served by the instance | `Access-Control-Allow-Origin` echoes the origin; works |
| Harmony desktop/Android shell | allowed by the two native origins; works |
| Any other browser origin | **no** `Access-Control-Allow-Origin`; the fetch rejects after the server answered `200` |
| `curl` / Rust `reqwest` | no CORS applies; works |

`/config.json` is same-origin only and no substitute for discovery
(`self-host/web-nginx.conf:53-55` serves it with no CORS header).

### 1.5 Error modes

| Symptom | Meaning |
|---|---|
| DNS/TLS/connect error | Wrong host, or no HTTPS on the origin |
| `404`, or `200` `text/html` (SPA fallback / SSO page) | Not a Harmony instance, or `/api/federation` is not proxied |
| `502`/`503` | Reverse proxy up, federation backend down |
| `200` JSON without `success: true` | Reverse proxy answered with something else |
| `200` with `supabaseUrl` or `supabaseAnonKey` missing/empty | Old instance, or a broken `PUBLIC_SUPABASE_URL` |
| Timeout | Treat as unreachable; the app's own timeout is 10 s (`src/services/runtimeConfig.ts:36`) |

For a bare host the app tries `https://` then `http://` and only reports the last error
(`src/services/instanceConfig.ts:105-160`); a Rust client should do the same once, then
stop.

---

## 2. Auth (GoTrue)

GoTrue v2.197.0 (`self-host/supabase-overrides.yml:240`). Base URL is
`<supabaseUrl>/auth/v1`, i.e. the `db.` host — not the app origin.

### 2.1 Sign-in with email and password

```
POST <supabaseUrl>/auth/v1/token?grant_type=password
apikey: <anon>
Authorization: Bearer <anon>
Content-Type: application/json

{"email":"alice@example.com","password":"correct horse battery staple"}
```

The app calls `supabase.auth.signInWithPassword({ email, password })`
(`src/stores/auth.ts:701`). Response (`GoTrueSession` in `timbre/auth.ts:25-34`,
`tui/web.rs:75-84`):

```json
{
  "access_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9…",
  "token_type": "bearer",
  "expires_in": 3600,
  "expires_at": 1795700000,
  "refresh_token": "v1f3k…",
  "user": {
    "id": "3f2a…-…",
    "aud": "authenticated",
    "role": "authenticated",
    "email": "alice@example.com",
    "email_confirmed_at": "2026-10-01T09:00:00Z",
    "app_metadata": {"provider": "email", "providers": ["email"]},
    "user_metadata": {},
    "identities": [],
    "created_at": "2026-10-01T09:00:00Z",
    "updated_at": "2026-11-25T13:00:00Z",
    "is_anonymous": false
  }
}
```

`expires_in`/`expires_at` are GoTrue's; the app never reads them, it lets auth-js refresh
(§2.4). `token_type` is `"bearer"`.

Failure bodies are one of two shapes; read all keys (`timbre/auth.ts:266-272`,
`tui/web.rs:284-299`):

| Status | Body | Meaning |
|---|---|---|
| `400` | `{"code":400,"error_code":"invalid_credentials","msg":"Invalid login credentials"}` | Wrong email/password |
| `400` | `{"error":"invalid_grant","error_description":"Invalid login credentials"}` | same, older GoTrue spelling |
| `422` | `{"code":422,"error_code":"email_not_confirmed","msg":"Email not confirmed"}` | SMTP instance, unconfirmed account |
| `429` | `{"code":429,"error_code":"over_request_rate_limit","msg":"…"}` | Per-IP limiter (`GOTRUE_RATE_LIMIT_…`) |

After a successful sign-in the app reads the profile and refuses a suspended account
(`src/stores/auth.ts:703-717`):

```
GET <supabaseUrl>/rest/v1/profiles?auth_user_id=eq.<user.id>&select=is_suspended,suspension_reason
```

`is_suspended: true` → sign out and show `suspension_reason`.

### 2.2 Session and JWT claims the server reads

| Claim | Used for |
|---|---|
| `sub` | `auth.uid()`; mapped to the profile by `profiles.auth_user_id` (`baseline.sql:4233-4241`) |
| `role` | `authenticated` — RLS policies and the pre-request hook branch on it (`m/20261005400001_account_security.sql:218-220`) |
| `session_id` | The row in `auth.sessions`; the hook refuses a token whose session is gone (`m/20261005400001_account_security.sql:222-232`) |
| `aal` | `aal1`/`aal2`; `session_meets_aal()` decides whether a 2FA account may use PostgREST (`m/20261005400001_account_security.sql:234-243`) |
| `exp` | Expiry (1 h) |

The app never decodes the token: it uses `session.user.id`, which is the same `sub`
(`src/stores/auth.ts:703-711`). A Rust client needs `sub` only to map auth user → profile
when the profile row has not been fetched yet.

### 2.3 Sign-up

The app creates the auth user, then writes the profile row itself — GoTrue creates **no**
profile (`src/stores/auth.ts:988-996`, `src/views/NewProfile.vue:399-426`).

Step 1 — the auth user:

```
POST <supabaseUrl>/auth/v1/signup
apikey: <anon>; Content-Type: application/json

{"email":"alice@example.com","password":"…","data":{"captcha_token":"…"}}
```

- `data` becomes `user_metadata`; `captcha_token` is the sign-up captcha answer read by
  the `before_user_created_hook` (`src/utils/signupCaptcha.ts:1-6`,
  `m/20261111100001_signup_captcha.sql:29,676`). With no token the hook answers **`428`**;
  the app shows a Turnstile/hCaptcha widget (`src/utils/signupCaptcha.ts:17-31`).
- `DISABLE_SIGNUP` is `true` on a fresh self-host (`self-host/configure.sh:612`);
  sign-up then answers `403`/`422` with a "signups not allowed" message.
- With SMTP configured, `ENABLE_EMAIL_AUTOCONFIRM=false` and the response has **no**
  `access_token`; the account is unusable until the emailed link is followed
  (`self-host/configure.sh:610`, `timbre/onboarding.ts:83-86`).

Step 2 — the profile row. Every policy resolves the session through it, so an auth user
without this row is a valid session that can read nothing:

```
POST <supabaseUrl>/rest/v1/profiles
Prefer: return=representation

{
  "id": "<auth user id>",
  "auth_user_id": "<auth user id>",
  "username": "alice",
  "display_name": "Alice",
  "bio": "…",
  "color": "#5865F2",
  "is_local": true,
  "domain": "har.mony.lol",
  "federated_id": "https://har.mony.lol/users/alice",
  "inbox_url":   "https://har.mony.lol/users/alice/inbox",
  "outbox_url":  "https://har.mony.lol/users/alice/outbox",
  "followers_url":"https://har.mony.lol/users/alice/followers",
  "following_url":"https://har.mony.lol/users/alice/following"
}
```

`src/views/NewProfile.vue:411-425`; insert via `ProfileService.createProfile`
(`src/services/ProfileService.ts:146-151`). Field rules:

| Field | Rule | Evidence |
|---|---|---|
| `id` | Must equal the auth user id — `profiles_insert_own` only checks `auth_user_id`, but the app sets both equal | `src/views/NewProfile.vue:412-413`, `baseline.sql:19247-19248` |
| `auth_user_id` | `= auth.uid()`, else `42501` | `baseline.sql:19247-19248` |
| `username` | local rows: `^[a-z0-9_]{3,24}$`, not in the reserved list, not an instance-domain label, else `23514` | `m/20261031900001_reserved_usernames.sql:129-150` |
| `username` uniqueness | `UNIQUE (username, domain)` | `baseline.sql:245` |
| `domain` | From `instance_config.domain`; a blank/placeholder domain makes `federated_id` permanently wrong | `src/views/NewProfile.vue:401-407` |
| `display_name` | Optional, must not be blank, ≤ 80 chars, ≤ 5 custom-emoji shortcodes | `baseline.sql:198-206`, `src/services/ProfileService.ts:132-140` |

Step 3 — optional: `POST /api/federation/generate-keys` with `{"user_id":"<id>"}` to create
the ActivityPub key pair (`src/views/NewProfile.vue:483-496`). Not needed to chat.

The profile insert may race: `23505` on a re-run means the profile already exists — re-read
by `auth_user_id` instead of asking for another username
(`src/views/NewProfile.vue:433-446`).

### 2.4 Refresh, and token rejection

```
POST <supabaseUrl>/auth/v1/token?grant_type=refresh_token
apikey: <anon>; Authorization: Bearer <anon>; Content-Type: application/json

{"refresh_token":"v1f3k…"}
```

The response is a full token body; the refresh token rotates, so store the new one.
The app's client refreshes automatically (`src/supabase.ts:185-188`); a Rust client should
keep `expires_at` and refresh ~60 s before it. `Authorization: Bearer <anon>` on the
refresh request is what supabase-js sends, and the instance proxy exempts exactly
`/auth/v1/token?grant_type=refresh_token` from the per-IP limiter so a `429` there cannot
sign the user out (`self-host/Caddyfile:380-390`).

Two refusals come from the PostgREST pre-request hook, not from GoTrue
(`src/supabase.ts:158-176`, `m/20261005400001_account_security.sql:210-246`):

| Status | `message` | Cause | Action |
|---|---|---|---|
| `401` | `session_revoked` | `auth.sessions` has no row for the token's `session_id` (signed out elsewhere) | Drop the token, sign in again |
| `403` | `insufficient_aal` | The account has a verified factor; this token is `aal1` | Sign out, sign in, complete the TOTP challenge |

Sign-out: `POST <supabaseUrl>/auth/v1/logout` with the user's bearer. The app only uses
`scope: 'local'` (`src/supabase.ts:198-232`); `global`/`others` go through
`sign_out_my_sessions(p_scope)` because GoTrue does not check assurance itself
(`m/20261007500001_sign_out_scope.sql:1-16`).

### 2.5 Two-factor (only if you must support it)

The app's order matters: `listFactors()` after sign-in, and an AAL1 session is held only
long enough to challenge (`src/stores/auth.ts:720-760`). HTTP equivalents:
`GET /auth/v1/factors`, `POST /auth/v1/factors/<id>/challenge`,
`POST /auth/v1/factors/<id>/verify` with `{"challenge_id","code"}`. The resulting session
has `aal2`. Until then every PostgREST read answers `403 insufficient_aal` — including
the profile lookup.

### 2.6 Anonymous sessions

Not supported by the app: a signed-out client can read a handful of public things
(`instance_config` public keys, `servers` rows, `get_invite_preview`) but has no profile,
so no RLS policy that calls `get_current_profile_id()` returns anything.

---

## 3. PostgREST: table by table

Base: `<supabaseUrl>/rest/v1/<table>?<query>`. The query strings below are the app's,
in PostgREST syntax. `select=*` is used by the app where the row is small; a Rust client
should select the columns it needs — a wide row is a parse risk, not a bandwidth one.

### 3.1 `profiles` — 56 columns

`CREATE TABLE` at `baseline.sql:121-245`; later additions: `deleted_at`
(`m/20261005400001_account_security.sql:70`), `instance_role`
(`m/20261030100001_copy_pins_roles_staff.sql:269`), `dm_policy`
(`m/20261109200001_instance_write_limits.sql:591-596`), `remote_posts_count`,
`remote_followers_count`, `remote_following_count`, `remote_counts_fetched_at`
(`m/20261011500001_remote_engagement_counts.sql:46-52`).

Columns a chat client needs:

| Column | Type | Notes |
|---|---|---|
| `id` | uuid PK | The id every other table's `user_id` points at |
| `auth_user_id` | uuid, unique, FK `auth.users` | Equals `id` for accounts created by the app (`baseline.sql:226-227`) |
| `username` | text | Handle; `web_handle` is `@user` / `@user@domain` (`baseline.sql:175-182`) |
| `display_name` | text? | Fallback for the name; `null` allowed |
| `avatar_url` | text | Object path in the `avatars` bucket, a full URL, or `/default_avatar.webp` |
| `banner_url`, `bio`, `color`, `status` | | `status` is the chosen status, 0–4 |
| `custom_status` | jsonb | `{type, text, emoji, emoji_url, details, state, set_at, expires_at}` (`baseline.sql:4796-4805`) |
| `is_admin`, `is_moderator`, `instance_role` | | Moderation flags |
| `is_local`, `domain`, `federated_id` | | Remote accounts have `is_local=false` |
| `dm_policy` | text | `everyone` / `known` / `following` |

Reads the app makes:

| Purpose | Query |
|---|---|
| Me, after sign-in | `profiles?auth_user_id=eq.<auth id>&select=id,username,display_name,avatar_url,color,is_admin,is_moderator&limit=1` (`timbre/data.ts:284-290`) |
| Me, full row | `profiles?select=*&id=eq.<profile id>` (`src/services/ProfileService.ts:59-62`) |
| Authors of a page | `profiles?or=(id.in.(<ids>),auth_user_id.in.(<ids>))&select=id,auth_user_id,username,display_name,avatar_url,color,is_admin,is_moderator` (`tui/web.rs:389-392`) |
| Members of a server (fallback) | `profiles?select=*&id=in.(<≤100 ids>)` (`src/services/usersService.ts:75-85`) |
| Profile by handle | `profiles?select=*&username=eq.<name>&domain=eq.<domain>&limit=1` |

RLS: **every** profile is readable by anyone, signed in or not — `profiles_select_all`
`USING (true)` (`baseline.sql:19244-19245`). Writes: the owner (`auth_user_id = auth.uid()`)
and the staff paths (`baseline.sql:19247-19255`). A PATCH of `display_name`/`avatar_url`/
`bio`/`color`/`custom_status` is the profile editor; `avatar_url` stores a bare object path,
so upload to `storage/v1/object/avatars/<profile id>/<file>` and then PATCH the path
(`timbre/data.ts:308-359`, `src/utils/avatarUtils.ts:93-108`).

### 3.2 `servers`

`baseline.sql:713-766`; additions: `category`
(`m/20261006400001_server_category.sql:36-52`), `sidebar_banner`
(`m/20261014100002_server_sidebar_banner.sql:11`), `suspended_at`/`suspended_reason`
(`m/20261027250001_instance_staff_safety.sql`), `federation_discoverable`
(`m/20261105100001_server_federation_directory.sql:30`).

| Column | Notes |
|---|---|
| `id`, `name`, `description`, `icon`, `banner`, `owner` | `icon`/`banner` are storage object paths or URLs; `owner` is a **profile** id, `null` after the owner is deleted |
| `public`, `member_count`, `is_featured`, `featured_order`, `category` | Discovery |
| `is_local_server`, `federation_enabled`, `federation_domain`, `federation_discoverable`, `ap_id` | Federation |
| `rules` | jsonb array of strings (legacy welcome rules) |
| `invite_code` | The server's permanent code, or `null` |

Reads:

| Purpose | Query |
|---|---|
| One server | `servers?select=*&id=eq.<id>` |
| Public directory | `servers?public=eq.true&select=id,name,description,icon,member_count&order=member_count.desc&limit=60` (`timbre/data.ts:1665-1669`) |
| Suggested after sign-up | `servers?select=id,name,member_count&public=is.true&is_local_server=is.true&order=member_count.desc&limit=3` (`timbre/onboarding.ts:158-162`) |

RLS: `servers` is world-readable — `Enable read access for all users` `USING (true)`
(`baseline.sql:19343-19345`, converged at `m/20261001200001_converge_rls_policies.sql:668-670`).
**This is not a membership check**: every server row, private or not, is readable. Only the
owner may edit (`baseline.sql:19360-19372`).

### 3.3 `user_servers` — membership and the server list

`baseline.sql:1116-1148`; `nickname`, `folder_id`, `position`, `muted`, `muted_until`,
`temporary`, `status`, plus `welcome_seen_at`/`rules_accepted_at`
(`m/20261006300001_welcome_server_and_rules.sql`).

| Column | Notes |
|---|---|
| `user_id` | profile id |
| `server_id` | server id |
| `status` | `pending` / `accepted` / `banned`; a `NULL` status reads as accepted in places (`src/services/usersService.ts:40-43`) |
| `nickname` | Per-server name |
| `folder_id`, `position` | Rail order within a folder |
| `muted`, `muted_until` | Rail mute |
| `temporary` | Created by a temporary invite |

The rail is one embedded read (`src/stores/useServerChannel.ts:342-366`):

```
user_servers?user_id=eq.<profile id>&select=folder_id,position,muted,muted_until,
  server:server_id(id,name,description,icon,banner,sidebar_banner,owner,
    allow_cross_server_emojis,public,federation_enabled,is_local_server,
    federation_domain,federation_inbox_url,host_polls:federation_metadata->message_polls,
    suspended_at,suspended_reason)&order=position.asc
```

Notes: the embed is named after the **FK column** (`server:server_id`), and
`host_polls:…->message_polls` projects one jsonb leaf. Deserialize it as
`Option<bool>`; the key is absent when the server never federated
(`src/types/chat.ts:34-39`).

RLS: a member sees the rows of servers they belong to (`user_servers_select_co_members`,
`baseline.sql:19552-19560`), so this query returns exactly "my servers". No client can
insert a membership for another user (`baseline.sql:19561-19564`); joining goes through an
RPC (§4.9-4.11). Deleting one's own row is leaving (`baseline.sql:19579-19585`).

### 3.4 `channels`

`baseline.sql:808-846`; `host_flags` (`m/20261109900001_channel_host_flags.sql:47-50`).

| Column | Notes |
|---|---|
| `id`, `server_id`, `category`, `name`, `description` | `category` is a `channel_categories.id` (the column is **not** `category_id`) |
| `type` | `0` text, `1` voice, `2` category row (`baseline.sql:819`; `src/types/chat.ts:79-84`) |
| `order` | Position within the category; a reserved word, quote it in SQL but not in PostgREST |
| `slowmode_seconds` | Per-user send interval |
| `is_remote`, `ap_id`, `federation_status`, `host_flags` | Read-only for clients |

Reads:

```
channels?server_id=eq.<id>&select=*&order=order.asc
```

(`src/stores/useServerChannel.ts:547-552`). Timbre filters `type=neq.2`
(`timbre/data.ts:394-401`) because it folds categories into the channel list; the app
keeps them separate and filters `type == 2` client-side.

RLS: `channels_select_member` → `id IN (SELECT current_user_viewable_channel_ids())`, plus
the owner branch (`m/20261001200002_enforce_channel_view_permission.sql:387-396`). That
function is VIEW_CHANNEL through overrides — a channel a member cannot view is absent, not
403. Writes are the owner's (`baseline.sql:19443-19474`) or, for a member, the
`create_channel`/`update_channel` RPCs.

### 3.5 `channel_categories`

`baseline.sql:785-800`; `host_flags` (`m/20261109900001_channel_host_flags.sql:49-50`).
Columns: `id`, `server_id`, `name`, `order`, `federation_status`, `host_flags`.

```
channel_categories?server_id=eq.<id>&select=*&order=order.asc
```

RLS since 1.9.x hides a category with no channel the caller may view
(`channel_categories_select_member`, `m/20261026200001_private_categories.sql:176-181`).
Consequence for a client: a channel can reference a `category` whose row you cannot read.
Keep an "Uncategorised" bucket keyed by the raw uuid rather than dropping the channel.

### 3.6 `messages`

`baseline.sql:849-918`; `media_paths` (`m/20261007300001_encrypted_message_media.sql:33`).

| Column | Type | Notes |
|---|---|---|
| `id` | uuid PK | Client may choose it (idempotent retry) |
| `created_at`, `updated_at` | timestamptz | `created_at` is overwritten with `now()` on insert; `updated_at > created_at` marks edited/deleted (`m/20261009000001_message_change_feed.sql:1-16`) |
| `user_id` | uuid? | Profile id or `null` (deleted user) |
| `bot_id` | uuid? | Set by the bot API only; mutually exclusive with `user_id` (`baseline.sql:894-896`) |
| `channel_id` | uuid? | Server channel message |
| `conversation_id` | uuid? | DM message |
| `thread_id` | uuid? | Thread reply; still carries `channel_id` |
| `reply_to` | uuid? | Parent message id |
| `content` | jsonb **array** | Non-empty; see §3.6.1 |
| `is_deleted`, `is_system`, `is_pinned`, `pinned_at`, `pinned_by` | | Soft delete; system rows are server-only |
| `encrypted`, `encryption_metadata`, `megolm_session_id`, `megolm_message_index` | | E2EE |
| `reactions` | uuid[] | Deprecated; use the `reactions` table |
| `federation_status`, `metadata` | | `metadata` is `{}` by default |

Constraints that reject a write: `messages_content_is_array` and
`messages_content_not_empty` (`baseline.sql:889-890`), `messages_text_length_check`
(`≤ 50 000` plaintext characters, skipped when `encrypted` or `is_system`;
`baseline.sql:5522-5546`), a per-instance soft limit enforced by `enforce_message_length`
(`max_message_length`, default 2000; `baseline.sql:5446-5485`), and the client-write guard
in §7.

Reads:

| Purpose | Query |
|---|---|
| Newest page (fallback when `get_message_page` is absent) | `messages?channel_id=eq.<id>&select=*&thread_id=is.null&or=(is_deleted.is.null,is_deleted.eq.false)&order=created_at.desc&limit=50` (`src/services/core/CoreMessageService.ts:866-876`) |
| Older page | same `+&created_at=lt.<iso>` |
| Catch-up after a drop | same with `created_at=gt.<iso>` and no `before` |
| DM page | `messages?conversation_id=eq.<id>&select=*&or=(is_deleted.is.null,is_deleted.eq.false)&order=created_at.desc&limit=50` |
| One message | `messages?select=*&id=eq.<id>` (`.single()`) |
| Reply preview | `messages?select=*&id=eq.<id>` (`src/stores/useChat.ts:86-93`) |

RLS: a member of the channel (`current_user_viewable_channel_ids()` **and** the
READ_MESSAGE_HISTORY cutoff) or an active conversation participant
(`m/20261001200002_enforce_channel_view_permission.sql:324-337`,
`m/20261017100001_message_content_permissions.sql:506-518`). Missing rows come back as an
empty array, not an error.

#### 3.6.1 `content` parts

`MessagePart` is a discriminated union (`src/types/chat.ts:289-640`). The app writes and
renders:

| `type` | Fields | Notes |
|---|---|---|
| `text` | `text` | The only part text length counts |
| `url` | `url`, `preview` (bool), `label?` | `preview:true` asks the instance for a link card; a URL inside `text` gets none |
| `mention` | `userId`, `username`, `domain`, `isLocal`, `displayName?` | Drives mention notifications; plain `@name` text notifies nobody |
| `role_mention` | `roleId`, `roleName`, `roleColor` | `roleId:"here"` is @here |
| `channel_mention` | `channelId`, `serverId`, `name`, `messageId?` | Server fills `serverId`/`name` |
| `emoji` | `emoji:{id,name,url}` | Custom emoji; the server refuses another server's emoji without USE_EXTERNAL_EMOJIS |
| `file` | `url`, `fileType`, `fileName?`, `fileSize?`, `mimeType?` (camelCase), `path?`, `altText?`, `width?`, `height?` | `fileType` is a **kind** (`image`/`video`/`audio`/`file`), not a MIME type (`src/types/chat.ts:535-545`; `timbre/data.ts:266-272` sends the same kind) |
| `sticker` | `id`, `url`, `name`, `description`, `width?`, `height?`, `mimeType?` | At most one, beside a `file` part with the same URL |
| `poll` | `pollId`, `question`, `options?`, `allowMultiple?`, `expiresAt?` | Server-side `message_polls` holds the counts |
| `role_menu` | `serverId`, `group`, `title?` | Self-assignable role card |
| `hashtag` | `name`, `id` | |
| `system` | `event_type`, `user`, … | **Refused** from clients (`SYSTEM_PART_FORBIDDEN`, `m/20261006700001_security_app_fixes.sql:95-145`) |

`embed` parts are server-written only for clients.

### 3.7 `server_roles`, `user_roles`

`server_roles` (`baseline.sql:1168-1200`): `id`, `server_id`, `name`, `color`, `position`,
`permissions` (bigint bitmask), `is_default`, `is_admin`, `mentionable`, `hoist`,
`icon_url`, `unicode_emoji`, `ap_id`.

```
server_roles?server_id=eq.<id>&select=*&order=position.desc
```

(`src/services/RoleService.ts:382-385`.) Bit positions are the app's
`PERMISSION_BITS`: `ADMINISTRATOR=0`, `VIEW_CHANNEL=1`, `MANAGE_CHANNELS=2`,
`SEND_MESSAGES=12`, `SEND_MESSAGES_IN_THREADS=13`, `EMBED_LINKS=16`, `ATTACH_FILES=17`,
`ADD_REACTIONS=18`, `USE_EXTERNAL_EMOJIS=19`, `READ_MESSAGE_HISTORY=22`,
`PIN_MESSAGES=23` (`src/services/RoleService.ts:110-150`). A member's effective permissions
for a channel come from `get_user_permissions` (§4.14), not from client arithmetic.

`user_roles` (`baseline.sql:1210-1228`): `(user_id, role_id, server_id)`; `UNIQUE(user_id,
role_id)`.

```
user_roles?server_id=eq.<id>&select=user_id,role_id
```

RLS: role rows and assignments are member-readable (`server_roles` has a member policy;
`user_roles` likewise), writes need MANAGE_ROLES.

### 3.8 `emojis`

`baseline.sql:999-1024`; `deleted_at` and the lifecycle functions
(`m/20261107100001_emoji_lifecycle.sql:56`), `discord_emoji_id`
(`m/20261009400001_bridge_server_emojis.sql:15`), `is_ai_generated`
(`baseline.sql:1018-1019`).

```
emojis?or=(server_id.is.null,server_id.in.(<server ids>))&select=id,name,url,scope,server_id&order=name.asc&limit=800
emojis?server_id=in.(<ids>)&select=*&deleted_at=is.null&order=name
emojis?select=*&scope=eq.instance&domain=is.null&deleted_at=is.null&order=name
emojis?select=*&scope=eq.user&uploader=eq.<profile id>&deleted_at=is.null&order=created_at.desc
```

(`timbre/data.ts:838-845`, `src/stores/useEmojiCache.ts:373-380,469-489`.)
`scope` is `server` / `instance` / `user` (`baseline.sql:1015-1017`). A picked emoji
becomes an `emoji` content part, not `:name:` text (`src/types/chat.ts:502-505`).

### 3.9 `reactions`

`baseline.sql:1051-1090`: `id`, `message_id`, `user_id?`, `bot_id?`, `emoji_id?`,
`custom_emoji_content?`, `channel_id`, `conversation_id`, `metadata`.
`CHECK (emoji_id IS NOT NULL OR custom_emoji_content IS NOT NULL)`, one author, and a
unique index over `(message_id, user_id, emoji_id, custom_emoji_content)`
(`baseline.sql:1068-1070,1081-1087`).

Toggle is a table write, not an RPC (`src/services/core/CoreMessageService.ts:507-590`):

```
GET    reactions?select=id&message_id=eq.<m>&user_id=eq.<me>&emoji_id=eq.<e>       → exists?
DELETE reactions?message_id=eq.<m>&user_id=eq.<me>&emoji_id=eq.<e>                (or custom_emoji_content=eq.<unicode>)
POST   reactions   {"message_id":"<m>","user_id":"<me>","emoji_id":"<e>"}          (or custom_emoji_content:"👍")
```

A unicode reaction is `custom_emoji_content`; a custom one is `emoji_id`. A `23505` after
a lost race is success. Newer instances can refuse with
`Missing permission: ADD_REACTIONS` (`m/20261016100005_permission_fixes.sql:371`).
Reading is normally folded into `get_message_page`; on demand,
`get_message_reactions(message_id)` (§4.21).

### 3.10 `conversations`, `conversation_participants`

`conversations` (`baseline.sql:1231-1259`): `id`, `created_at`, `updated_at`, `name`,
`type` (`direct`/`group`/`channel`), `created_by`, `is_active`, `metadata` jsonb.

`conversation_participants` (`baseline.sql:1261-1288`): `conversation_id`, `user_id`,
`role` (`admin`/`member`), `joined_at`, `left_at`, `last_read_at`,
`last_read_message_id`, `is_muted`, `hidden_at`, `UNIQUE(conversation_id,user_id)`.

The DM list is one RPC, `get_user_conversations()` (`m/20261118100001_dm_preview_skips_deleted.sql:21-110`);
the app falls back to a three-query read (`src/stores/useDM.ts:712-795`):

```
conversation_participants?user_id=eq.<me>&select=conversation_id,hidden_at,conversations!inner(id,created_at,type,name,updated_at,metadata)&left_at=is.null&order=joined_at.desc
conversation_participants?select=conversation_id,user_id&conversation_id=in.(<ids>)&user_id=neq.<me>&left_at=is.null
messages?select=id,conversation_id,content,encrypted,created_at,user_id,is_system,metadata&conversation_id=in.(<ids>)&or=(is_deleted.is.null,is_deleted.eq.false)&order=created_at.desc&limit=min(ids*5,1000)
```

PostgREST spells `IS NULL` as `column=is.null`. RLS: a participant reads their own rows
(`baseline.sql:19594-19601`); messages require an active participant (§3.6).

Starting a DM is the RPC `create_or_get_direct_conversation(user1_uuid,user2_uuid)`
(`baseline.sql:4567-4624`, converged with the `dm_policy` guard at
`m/20261109200001_instance_write_limits.sql:710-768`). Refusals: `DM_NOT_ALLOWED`
(the peer's `dm_policy`), `DM_UNDELIVERABLE`.

### 3.11 `notifications`

`baseline.sql:1914-1930`: `id`, `user_id` (profile), `type`, `data` jsonb, `is_read`,
`is_clicked`, `created_at`, `updated_at`, `expires_at` (default 30 days), `read_at`.
Types are the union in `src/types/notifications.ts:17-56`; `data` carries ids and previews
(`channel_id`, `message_id`, `server_id`, `conversation_id`, `sender`, `preview`, …,
`src/types/notifications.ts:58-90`).

```
notifications?select=*&user_id=eq.<profile id>&order=created_at.desc&limit=50&offset=0
notifications?select=id,type,is_read,data,created_at,user_id&user_id=eq.<me>&is_read=eq.false&order=created_at.desc&limit=200
PATCH notifications?id=in.(<ids>)  {"is_read":true,"read_at":"<now>"}
```

(`src/stores/useNotification.ts:765-771,858-863,1676-1681`.) `mark_all_notifications_read(p_user_id)`
marks every unread row in one call and sends one `notification:bulk_read` event
(`m/20261003200001_notifications.sql:359-395`).

RLS: own rows only (`m/20261001200001_converge_rls_policies.sql:492-505`). A client may
INSERT only as `service_role`; notifications are server-generated.

### 3.12 `unread_counts` — read through the RPC

`baseline.sql:2092-2108` has `user_id`, `server_id`, `channel_id`, `conversation_id`,
`unread_messages`, `unread_mentions`, `last_read_message_id`, `last_read_at` — but after
1.9.x the count is **derived** from `message_heads` and `unread_counts.read_seq`, and the
table is not for clients (`m/20261005700001_compute_unread_on_read.sql:1-40`). Call
`get_unread_counts()` (§4.4) and `mark_*_as_read` (§4.5-4.7). Never write the table.

### 3.13 `notification_channels` — mutes

`baseline.sql:2022-2055`: `user_id`, `server_id`, `channel_id`, `conversation_id`,
`muted`, `muted_until`, `notification_level`, `suppress_everyone`, `suppress_roles`.
`get_unread_counts` reports `muted` from an active row here
(`m/20261011100001_unread_badge_fixes.sql:65-70`).

```
notification_channels?user_id=eq.<me>&server_id=eq.<s>&channel_id=is.null&conversation_id=is.null&select=muted,notification_level&limit=1
```

(`timbre/data.ts:1580-1585`.)

### 3.14 `channel_encryption_settings`

`m/20261001000001_channel_encryption_settings.sql:53-66`: `channel_id` PK,
`messages_encrypted`, `voice_encrypted`, `history_visibility` (`joined`/`shared`),
`enabled_at`, `enabled_by`.

```
channel_encryption_settings?channel_id=in.(<ids>)&select=channel_id,messages_encrypted,voice_encrypted,enabled_at,enabled_by
```

plus `history_visibility` **only** when the instance advertises `encrypted-history`
(`src/stores/useChannelEncryption.ts:60-72`). Read-only for authenticated users; the
setting changes through `set_channel_encryption`.

### 3.15 `instance_config`

`baseline.sql:251-258` (`config_key` unique, `config_value` jsonb). Only the curated
public keys are readable: `public_instance_config_keys()` (`baseline.sql:12958-12999`),
enforced by `instance_config_select_admin`/public policy
(`m/20261001200001_converge_rls_policies.sql:367-373`).

```
instance_config?select=config_key,config_value
```

(`src/stores/useInstanceSettings.ts:186-189`.) Useful keys: `domain`, `instance_name`,
`max_message_length`, `max_media_attachments_per_post`, `allow_file_uploads`,
`enable_voice_channels`, `features` (jsonb). A key not on the list is invisible — a `200`
with fewer rows, not a `403`.

### 3.16 Pagination and the 1000-row cap

PostgREST caps a response at `PGRST_DB_MAX_ROWS`, **1000** by default
(`self-host/supabase-overrides.yml:90`), with status `200` and no error. Any read that can
exceed it needs ranges. The app's helper reads to the exact count of the first response
(`src/services/pagedRead.ts:1-45`):

```
GET <table>?select=…&order=<unique key>&limit=1000&offset=0
Range: 0-999      (or limit/offset; both work)
```

The `Prefer: count=exact` header returns the total in `Content-Range: 0-999/12345`.
For a Rust client: page with `order` over a unique key plus `limit`/`offset`, stop at the
first short page *and* at the total, and never treat a full 1000-row answer as complete.

---

## 4. RPCs a chat client needs

All are `POST <supabaseUrl>/rest/v1/rpc/<name>` with `Content-Type: application/json` and
a body of named arguments (`{}` when there are none). Signatures and grants below are from
`SURFACE.tsv`; behaviour from the migration named.

| # | Function | Arguments | Returns | Call site |
|---|---|---|---|---|
| 4.1 | `get_message_page` | `p_channel_id uuid`, `p_conversation_id uuid`, `p_limit int=20`, `p_before timestamptz` | jsonb `{messages, reactions, history_since}` | `src/services/core/CoreMessageService.ts:732-737` |
| 4.2 | `get_user_conversations` | — | jsonb array | `src/stores/useDM.ts:1120-1124` |
| 4.3 | `create_or_get_direct_conversation` | `user1_uuid`, `user2_uuid` | uuid | `src/stores/useDM.ts:1960-1965` |
| 4.4 | `get_unread_counts` | — | TABLE(…) | `src/services/readState.ts:22-26` |
| 4.5 | `mark_channel_as_read` | `p_channel_id uuid`, `p_message_id uuid` | void | `src/services/readState.ts:32-37` |
| 4.6 | `mark_conversation_as_read` | `p_conversation_id uuid`, `p_message_id uuid` | void | `src/services/readState.ts:45-50` |
| 4.7 | `mark_server_as_read` | `p_server_id uuid` | void | `src/services/readState.ts:55-58` |
| 4.8 | `get_presence` | `p_profile_ids uuid[]` | TABLE(profile_id,status,online,is_mobile,last_seen_at) | `src/services/userDataService.ts:700-702` |
| 4.9 | `presence_heartbeat` | `p_device_id text`, `p_status smallint`, `p_is_mobile bool` | void | `src/services/userDataService.ts:624-628` |
| 4.10 | `presence_offline` | `p_device_id text` | void | `src/services/userDataService.ts:674` |
| 4.11 | `get_server_member_list` | `p_server_id`, `p_offset int=0`, `p_limit int=100`, `p_query text` | jsonb `{total,offset,groups,members,live,max_age_ms}` | `src/services/memberDirectoryService.ts:92-99` |
| 4.12 | `search_server_members` | `p_server_id`, `p_query`, `p_limit int=20`, `p_without_role uuid` | jsonb array of member | `src/services/memberDirectoryService.ts:129-134` |
| 4.13 | `get_server_member_counts` | `p_server_ids uuid[]` | TABLE(server_id,member_count) | `src/components/InviteModal.vue:374` |
| 4.14 | `get_user_permissions` | `p_user_id`, `p_server_id`, `p_channel_id` | jsonb `{PERMISSION: bool}` | `src/services/RoleService.ts:905-909` |
| 4.15 | `get_server_welcome` | `p_server_id` | jsonb | `src/services/ServerWelcomeService.ts:97` |
| 4.16 | `accept_server_rules` | `p_server_id` | jsonb | rules acceptance flow |
| 4.17 | `get_invite_preview` | `p_code text` | jsonb | `src/services/inviteService.ts:208` |
| 4.18 | `redeem_invite` | `p_code text` | jsonb `{server_id,joined}` | `src/services/inviteService.ts:126` |
| 4.19 | `join_public_server` | `p_server_id uuid` | jsonb `{server_id,joined}` | discovery join |
| 4.20 | `begin_channel_history_view` | `p_channel_id uuid` | timestamptz | `src/services/channelHistoryCutoff.ts:36` |
| 4.21 | `get_message_reactions` | `message_id uuid` | TABLE(count,emoji,reactions,current_user_reacted,message_id_of_reactions) | `src/services/core/CoreMessageService.ts:607-609` |
| 4.22 | `get_batch_message_reactions` | `message_ids uuid[]` | TABLE(message_id,emoji_id,emoji_name,emoji_url,custom_emoji_content,reaction_count,current_user_reacted,users) | `src/services/core/CoreMessageService.ts:658-660` |
| 4.23 | `usable_emojis` | `p_names text[]`, `p_ids uuid[]`, `p_server_id uuid` | TABLE(id,name,url,server_id,scope,domain) | `src/services/emojiShortcodeResolver.ts:211-220` |

### 4.1 `get_message_page` — the page loader

`m/20261017100001_message_content_permissions.sql:756-845`. SECURITY INVOKER, so
`messages`/`reactions` RLS applies. Exactly one of `p_channel_id`/`p_conversation_id`
(`22023` otherwise); `p_limit` outside `1..100` becomes 20.

```json
{
  "messages": [ { …full messages row… } ],
  "reactions": [
    { "message_id":"…","emoji_id":"…","emoji_name":"wave","emoji_url":null,
      "custom_emoji_content":null,"reaction_count":2,"current_user_reacted":true,
      "users":[{"user_id":"…","bot_id":null,"metadata":{}}],
      "first_reaction_at":"2026-11-25T14:00:00+00:00" }
  ],
  "history_since": null
}
```

- `messages` are **oldest-first** (the SQL aggregates `ORDER BY created_at ASC`) even
  though the page is the newest `p_limit` rows before `p_before`. Do not reverse them.
- Thread replies are excluded (`thread_id IS NULL`).
- `history_since` is non-null only when the caller lacks READ_MESSAGE_HISTORY in that
  channel; the first non-null answer requires `begin_channel_history_view` before the rows
  stop being clipped (`src/services/core/CoreMessageService.ts:736-747`).
- `p_limit` is capped at 100; page older history by passing the oldest `created_at` you
  hold as `p_before`.

### 4.2-4.3 DM list and creation

`get_user_conversations()` returns an array of
`{conversation_id, created_at, updated_at, type, name, created_by, is_active, metadata,
hidden_at, user_role, user_joined_at, other_participants[]{user_id,role,joined_at,profile{…}},
last_message{id,user_id,content,encrypted,created_at,metadata}|null, unread_messages,
unread_mentions, is_muted}` (`m/20261118100001_dm_preview_skips_deleted.sql:47-110`).
`last_message` skips deleted rows and thread replies.

`create_or_get_direct_conversation(a,b)` requires the caller to be `a` or `b`, reuses an
existing `type='direct'` conversation whose two participants are active, and otherwise
creates it (`m/20261109200001_instance_write_limits.sql:710-768`).

### 4.4-4.7 Unread state

`get_unread_counts()` returns only contexts with a positive count or mentions
(`m/20261011100001_unread_badge_fixes.sql:47-111`):

| Column | Notes |
|---|---|
| `id` | The `unread_counts` row id, or null when the context never had one |
| `channel_id` / `conversation_id` / `server_id` | Exactly one of the first two |
| `unread_messages`, `unread_mentions` | Counts at read time |
| `last_read_message_id`, `last_read_at` | Position of the "new messages" divider |
| `last_message_at` | Newest message, for sorting |
| `muted` | Active mute; `unread_messages` is then frozen at mute time |

`mark_channel_as_read(channel, message_id?)` is a no-op for a channel the caller cannot
view; it also marks the channel's notifications read (`m/20261011100001_unread_badge_fixes.sql:202-250`).
`mark_conversation_as_read` is the DM twin. `mark_server_as_read` clears every channel of a
server.

### 4.8-4.10 Presence

`get_presence(ids)` returns the caller's own row plus **published** rows for related
profiles (shared server, shared conversation, or followed) — at most 1000 ids
(`m/20261006800001_private_call_channels.sql:703-730`). Absent profiles read as offline.
Rows: `profile_id,status,online,is_mobile,last_seen_at`. `status` is `1..4` on the caller's
own row (`4` = invisible) and the published `0..3` for everyone else, where `0` is what
others see as offline (`m/20261006800001_private_call_channels.sql:581-586,703-730`).

`presence_heartbeat(device_id, status, is_mobile)` upserts a device row and recomputes
liveness; `presence_offline(device_id)` removes it (`m/20261006800001_private_call_channels.sql:607-670`).
The app heartbeats every 60 s while the tab is visible, with `status` from `UserStatus`
(`src/services/userDataService.ts:118,621-632`). Liveness expires after **150 s** without a
heartbeat (`m/20261006800001_private_call_channels.sql:456-560`). `p_device_id` is 1–64
characters; the app generates a per-tab uuid in `sessionStorage`
(`src/utils/clientDeviceId.ts:14-26`).

### 4.11-4.13 Members

`get_server_member_list` (`m/20261113100001_member_directory.sql:591-701`): `p_limit` is
clamped to `1..200`; `members` are in sidebar order with `group` keys and `role_ids`;
`live` is false for a large server whose order is cached. Each member is
`server_member_json`: `id, group, username, web_handle, display_name, avatar_url,
banner_url, bio, color, status, domain, updated_at, created_at, is_local, custom_status,
is_admin, is_moderator, instance_role, online, presence_status, is_mobile, role_ids`
(`m/20261113100001_member_directory.sql:208-248`). A member who blocks the caller is absent from the caller's list
(`m/20261113100001_member_directory.sql:611-620`).

`search_server_members` (`p_limit` clamped `1..50`) returns the same objects without a
group. `get_server_member_counts(ids)` is the badge-safe count for many servers at once
(granted to `anon` too).

### 4.14 Permissions

`get_user_permissions(profile, server, channel?)` returns a map of permission name →
boolean (the `get_user_permissions` row of `SURFACE.tsv`). Compute nothing locally: overrides, category
overrides and the default role all feed this.

### 4.15-4.16 Server rules

`get_server_welcome` returns
`{server_id,name,description,icon,banner,is_local,configured,enabled,message,rules[],
require_acceptance,can_manage,is_member,joined_at,welcome_seen_at,rules_accepted_at,
must_accept,should_show}` (`m/20261006300001_welcome_server_and_rules.sql:200-281`).
When `must_accept` is true, a message insert raises `RULES_NOT_ACCEPTED:<server id>`
(`m/20261006300001_welcome_server_and_rules.sql:528`); call `accept_server_rules` first.
`accept_server_rules` needs the rules-acceptance state to be pending, otherwise `22023`.

### 4.17-4.19 Invites and joining

`get_invite_preview(code)` is granted to `anon`; it returns
`{status:"valid"|"not_found"|"revoked"|"expired"|"exhausted", code, server_id, name,
description, icon, banner, rules, member_count, is_member}` (`m/20261005650001_joins_visibility_boosts.sql:221-280`).
`redeem_invite(code)` inserts an `accepted` `user_servers` row and consumes one use;
refusals are `INVITE_NOT_FOUND`, `INVITE_EXHAUSTED`, `INVITE_REVOKED`, `INVITE_EXPIRED`,
`BANNED_FROM_SERVER` (`m/20261005650001_joins_visibility_boosts.sql:106-166`).
`join_public_server(id)` is the same for a `public` local server, refusing
`SERVER_NOT_PUBLIC` (`:172-220`).

### 4.20 History view

`begin_channel_history_view(channel)` records "this session started viewing now" for a
channel whose READ_MESSAGE_HISTORY is withheld, and returns that timestamp; it returns
`null` for a channel with no cutoff (`m/20261017100001_message_content_permissions.sql:481-499`).
See §9.5.

### 4.21-4.23 Reactions and emoji

`get_message_reactions(message_id)` returns grouped rows when the page loader is not an
option; `get_batch_message_reactions(ids)` returns one row per `(message, emoji)` with the
reacting users. `usable_emojis(names, ids, server_id)` resolves `:shortcode:`s the caller is
allowed to use, `deleted_at IS NULL`, at most 1000 rows
(`m/20261107100001_emoji_lifecycle.sql:1286-1302`).

---

## 5. Realtime

### 5.1 Transport

One WebSocket to the Supabase project: `wss://<db host>/realtime/v1/websocket` with
`?apikey=<anon>&vsn=1.0.0`. The app pins protocol `vsn` to `1.0.0` because 2.x needs a
binary serializer absent from older Realtime images
(`src/supabase.ts:187-194`). Realtime server is `supabase/realtime:v2.153.6`
(`self-host/supabase-overrides.yml:124`).

The socket must authenticate as the user, not the anon key: the access token travels with
every join and in later `access_token` pushes (`src/services/realtimeAccessToken.ts:1-25`;
`src/services/realtimeMemberToken.ts:7-12` names both), and is re-sent after a refresh
(`:44-56`).

### 5.2 There is no CDC: Broadcast only

`supabase_realtime` carries **no** table since 1.9.x — `messages` was dropped
(`m/20261021100001_realtime_publication_drop_messages.sql:1-30`), and nothing used
`postgres_changes` (`docs/contribute/architecture.md:72`). Every event is a Broadcast
frame that a trigger sent with `realtime.send(payload, event, topic, true)`. A client that
subscribes with `postgres_changes` joins fine and receives nothing.

Private topics are authorised by a policy on `realtime.messages` calling
`can_subscribe_to_topic(topic)` (`m/20261025100001_realtime_policy_converge.sql:31-56`).
A topic the function does not recognise is refused with `CHANNEL_ERROR`; there is no
retry that fixes it (`docs/contribute/architecture.md:206`).

### 5.3 Topics, events and payloads

| Topic | Event | Payload | Authorisation |
|---|---|---|---|
| `channel-messages-<channel id>` | `message_event` | `{op,new,old}` | `can_view_channel` (`m/20261001200002:515-523`) |
| `channel-messages-<channel id>` | `reaction_event` | reaction object (§5.3.2) | same |
| `channel-messages-<channel id>` | `poll_event` | `{type, poll_id, message_id, expires_at, total_voters, options[{id,votes}]}` | same |
| `dm-conversation-<conversation id>` | same three | same | active participant (`m/20261001200002:505-513`) |
| `thread-messages-<thread id>` | `message_event` | `{op,new,old}` | view of the thread's channel (`m/20261019100001:157-166`) |
| `server-structure:<server id>` | `server_event` | `{type, new, old, restricted?}` | accepted member |
| `server-presence:<server id>` | `presence_event` | `{type,…}` | accepted member (`m/20261001200002:524-537`) |
| `user:<profile id>` | `user_event` | `{type,…}` | only that profile (`m/20261001200002:538-548`) |
| `typing:channel:<id>` / `typing:thread:<id>` / `typing:conversation:<id>` | `presence_state` / `presence_diff` (realtime-js surfaces `sync`/`join`/`leave`) | presence state | view/participant (`m/20261019100001:218-238`) |
| `dm-calls:<profile id>`, `dm-call:<conversation id>`, `harmony-voice-<room>`, `voice-channels:<server id>`, `voice-channel:<channel id>`, `feed:*` | call/voice/social | not needed to chat | §5.5 |

#### 5.3.1 `message_event`

Sent by `broadcast_message_event()` after every insert/update/delete
(`m/20261110900004_history_import_quiet.sql:223-322`):

```json
{
  "op": "INSERT",
  "new": {
    "id": "…", "created_at": "…", "updated_at": "…",
    "user_id": "…", "bot_id": null,
    "channel_id": "…", "conversation_id": null, "thread_id": null, "reply_to": null,
    "content": [{"type":"text","text":"hello"}],
    "is_deleted": false, "is_system": false, "is_pinned": false,
    "encrypted": false, "encryption_metadata": null, "megolm_session_id": null,
    "metadata": {}
  },
  "old": null
}
```

- `op` is `INSERT`, `UPDATE` or `DELETE` (`TG_OP`). On `DELETE`, `new` is `null` and `old`
  carries the row.
- The `new` object is a **whitelist**, not `to_jsonb(row)`: six columns are missing —
  `federation_status`, `media_paths`, `megolm_message_index`, `reactions`, `pinned_at`,
  `pinned_by`.
- A message created before the current transaction in a channel that withholds
  READ_MESSAGE_HISTORY from everyone arrives as
  `{"id","created_at","updated_at","channel_id","thread_id","is_deleted","restricted":true}`
  — ids only; re-read the row (`m/20261110900004:250-270`,
  `src/stores/shared/realtimeMessageEvent.ts:52-72`).
- A thread reply also goes to `thread-messages-<thread id>`; the channel topic still
  receives it, so filter on `thread_id` (`src/stores/useChat.ts:1191-1194`).
- The DB does not send to the sender only; dedupe by `id` against the insert response.

#### 5.3.2 `reaction_event`

`broadcast_message_reaction_event()` (`m/20261117900002_matrix_authors.sql:889-980`):

```json
{"type":"reaction:insert","op":"INSERT","reaction_id":"…","message_id":"…",
 "emoji_id":"…","emoji_name":"wave","emoji_url":null,"custom_emoji_content":null,
 "user_id":"…","bot_id":null,"metadata":{},
 "username":"alice","display_name":"Alice","avatar_url":"…"}
```

`type` is `reaction:insert` or `reaction:delete`. `emoji_name`/`emoji_url` are resolved for
a custom emoji so a chip can render without the emoji cache.

#### 5.3.3 `server_event` on `server-structure:<server id>`

One topic per open server; the app subscribes when a server is opened
(`src/stores/useServerChannel.ts:1873-1965`). `type` values the client handles:
`channel:insert|update|delete`, `category:insert|update|delete`,
`thread:insert|update|delete`, `settings:insert|update`,
`role:insert|update|delete`, `user_role:insert|delete`,
`permission_override:insert|update|delete`,
`category_permission_override:insert|update|delete`, `bot:insert|update|delete|presence`,
`membership:event`. A restricted channel/category goes out as ids with
`restricted:true` (`m/20261001200002:560-600`).

#### 5.3.4 `presence_event` on `server-presence:<server id>`

| `type` | Extra fields | Meaning |
|---|---|---|
| `presence:update` | `user_id`, `online`, `status`, `is_mobile` | Published presence changed (`m/20261006800001:581-586`) |
| `member:join` | `user_id`, `server_id` | `user_servers` row inserted (`baseline.sql:9966-9987`) |
| `member:leave` | `user_id`, `server_id` | row deleted |
| `profile:updated` | `user_id`, `display_name`, `avatar_url`, `banner_url`, `color`, `bio`, `username`, `custom_status`, `federation_metadata` | profile changed (`baseline.sql:10019-10045`) |
| `emoji:*` | emoji row | server emoji changed |

#### 5.3.5 `user_event` on `user:<profile id>`

One topic per signed-in account (`src/services/UserEventChannel.ts:105-160`), carrying
every user-scoped event. Types the app dispatches (union at
`src/services/UserEventChannel.ts:22-58`):

| `type` | Payload |
|---|---|
| `notification:new` | `{notification:{id,type,data,is_read,is_clicked,created_at,updated_at,user_id,expires_at}}` (`m/20261003200001:322-351`) |
| `notification:update` | `{id,is_read}` |
| `notification:bulk_read` | `{count}` |
| `notification:deleted` | `{ids: [uuid] or null}` |
| `unread:change` | `{action,count:{id,user_id,server_id,channel_id,conversation_id,unread_messages,unread_mentions,last_read_message_id,last_read_at}}` (`m/20261005700001:225-247`) |
| `conversation:new`, `conversation:updated`, `conversation:encryption`, `conversation:message_deleted` | conversation id / message id |
| `server:joined`, `server:left`, `server:updated` | server id |
| `preferences:updated`, `mute:insert`, `mute:delete`, `block:insert`, `block:delete` | |
| `profile:updated` | a DM partner's profile change |
| `presence:update` | a DM partner's presence (`m/20261006800001:581-586`) |
| `_reconnected` | client-synthesised after a resubscribe; re-read state |

### 5.4 Subscribing and unsubscribing

The app has one connection and a registry keyed by topic
(`src/services/RealtimeConnectionManager.ts:299-470`). The wire operations:

| Operation | Phoenix frame |
|---|---|
| Join | `{"topic":"realtime:<topic>","event":"phx_join","payload":{"config":{"private":true},"access_token":"<jwt>"},"ref":"1"}` — `realtime-js` prefixes the topic with `realtime:` and puts the token in the join payload (`src/services/realtimeMemberToken.ts:7-12`) |
| Leave | `{"topic":"realtime:<topic>","event":"phx_leave","payload":{},"ref":"2"}` |
| Broadcast delivery | event `"broadcast"`; the app unwraps `payload.payload`, which holds the trigger's JSON |
| Presence | event `"presence_state"` / `"presence_diff"`; track with `"presence"` + `{"event":"track","payload":{…}}` |

`private:true` is mandatory: opened as a public channel, the join succeeds and the server
sends nothing (`timbre/realtime.ts:10-20` states it for the other client in this workspace, and
`src/stores/useChat.ts:1368-1377` is the app doing it; the policy at `m/20261025100001:43-49`).

Lifecycle the app implements and a Rust client should copy:

1. Join the topics for the open view: messages topic for the open channel/conversation,
   `server-structure` and `server-presence` for the open server, `user:<me>` once.
2. On `SUBSCRIBED`, reconcile: re-read rows newer than the newest held message
   (`src/stores/useChat.ts:1383-1405`), because broadcasts during the gap are lost.
3. On `CHANNEL_ERROR` with a refusal, do not retry on the same token; retry after a token
   refresh. `TIMED_OUT`/`CLOSED` are retried with backoff.
4. Leave a topic when the view changes; the app tears down the previous channel before
   joining the next (`src/stores/useChat.ts:1140-1147`).
5. Re-send `access_token` after every refresh, or private joins fail once the old token
   expires.

### 5.5 Topics a chat client can ignore

`dm-calls:*`, `harmony-voice-*`, `voice-channel(s):*`, `feed:*`, `easter-egg:*`. They are
authorised but irrelevant without voice/social features. `send_signal` is the RPC for call
signalling and refuses topics it does not know (`m/20261124100001_call_signal_relay.sql:212-330`).

---

## 6. Presence and typing

### 6.1 Presence is database state, not Realtime presence

Unlike typing, presence does not use Realtime Presence. The database keeps
`presence_devices` and `user_presence`, recomputes on heartbeat, and broadcasts
`presence:update` (`m/20261006800001_private_call_channels.sql:434-600`). A client:

1. On sign-in and every 60 s: `presence_heartbeat(device_id, status, is_mobile=false)`.
2. On status change: `presence_heartbeat` with the new `status` for 1 online / 2 away /
   3 dnd / 4 invisible, and `presence_offline` for 0 offline
   (`src/services/userDataService.ts:615-630`).
3. On sign-out/unload: `presence_offline(device_id)`; the app also sends it with
   `fetch(..., {keepalive:true})` on pagehide (`src/services/userDataService.ts:641-660`).
4. To show peers: `get_presence([ids])` for the visible member page, then apply
   `presence:update` from `server-presence` / `user:` topics.

If the heartbeat stops, peers see the account go offline after 150 s.

### 6.2 Typing is Realtime Presence

Topic `typing:<kind>:<id>`, opened **private**, tracked with
(`src/services/TypingIndicatorService.ts:231-237`):

```json
{"user_id":"<profile id>","typing":true,"typing_at":"2026-11-25T14:30:00.000Z",
 "display_name":"Alice","username":"alice"}
```

Clearing sends `{"user_id":"…","typing":false}`
(`src/services/TypingIndicatorService.ts:259-262`). Receivers read `channel.presenceState()`,
keep entries with `typing === true`, drop their own id, and expire anything older than
**3000 ms** locally (`:418-445`). The app re-evaluates every second because Realtime only
emits `sync` on change (`:33-45,437-448`).

The server allows `track` only on `typing:%` and `dm-call:%`
(`m/20261124100001_call_signal_relay.sql:232-246`), and only for a context the caller can
view (`m/20261019100001_thread_message_broadcasts.sql:218-238`). Clients cannot
*broadcast* on typing topics.

---

## 7. Sending a message

### 7.1 Channel message

```
POST <supabaseUrl>/rest/v1/messages
Prefer: return=representation
Content-Type: application/json

{
  "channel_id": "0b9e5a61-…",
  "user_id": "3f2a…",
  "content": [{"type":"text","text":"hello"}],
  "reply_to": null,
  "metadata": {"created_via":"harmony_client","client_nonce":"0f3c…"}
}
```

The app's insert (`src/services/core/CoreMessageService.ts:138-157`) also sends
`encrypted`, `encryption_metadata`, and (for attachments) `media_paths`. It uses
`.insert(row).select('*')` — `return=representation` — because AutoMod can drop the row and
the client must see **zero rows** (`:154-179`). With `Prefer: return=minimal` the same drop
is a silent `201`.

Rules the database enforces on that insert:

| Rule | Failure |
|---|---|
| `content` must be a non-empty JSON array | `23514 messages_content_is_array` / `messages_content_not_empty` (`baseline.sql:889-890`) |
| text parts ≤ instance `max_message_length` (default 2000), ≤ 50 000 absolute | `check_violation`, message names the limit (`baseline.sql:5473-5484,5539-5546`) |
| `is_system` must not be true; `bot_id`, pin fields, non-`pending` `federation_status` are refused | `42501` with `SYSTEM_MESSAGE_FORBIDDEN` / `pin fields are set by the server` (`m/20261005600001_message_path_security.sql:100-113`) |
| reserved `metadata` keys (`type`, `federated`, `ap_id`, `embeds`, `bot`, `automod`, …) | `42501 metadata key reserved for the server` (`m/20261005600001:82-89,113-117`) |
| `created_at` is overwritten with `now()` | silent (`m/20261005600001:118-120`) |
| a `system` content part | `42501 SYSTEM_PART_FORBIDDEN` (`m/20261006700001_security_app_fixes.sql:124-140`) |
| RLS: caller must be the author and hold `SEND_MESSAGES` in a viewable channel | `42501` (`m/20261001200002:339-361`) |
| slowmode | `SLOWMODE_ACTIVE:<seconds>` (`baseline.sql:10671`) |
| server rules not accepted | `RULES_NOT_ACCEPTED:<server id>` (`m/20261006300001:528`) |
| AutoMod / timeout / antispam | `AUTOMOD_BLOCKED`, `MEMBER_TIMED_OUT`, `ANTISPAM_*`, or **0 rows** (`src/services/AutoModService.ts:361-366`) |
| channel requires encryption | `CHANNEL_ENCRYPTED` (`src/services/core/channelMessageEncryption.ts:99-120`) |
| duplicate id | `23505` — the earlier attempt landed; treat it as success. The app never reuses an id, so it has no handler (`src/services/core/CoreMessageService.ts:157-173`) |

Attachment permission refusals are `Missing permission: ATTACH_FILES` / `EMBED_LINKS` /
`USE_EXTERNAL_EMOJIS` (`m/20261016100005_permission_fixes.sql:441`,
`m/20261017100001:1-20`).

### 7.2 DM message

Same table, `conversation_id` instead of `channel_id` (`src/services/core/CoreMessageService.ts:226-258`).
Extra refusals: `RECIPIENT_DELETED` (every other participant's account is gone,
`m/20261005600001_message_path_security.sql:201`), `DM_NOT_ALLOWED`,
`ENCRYPTION_LOCKED` / `ENCRYPTION_UNAVAILABLE` for an encrypted conversation. The
conversation must have an active participant row for the caller (`m/20261001200002:363-372`).

### 7.3 Attachment parts

Upload to `POST <supabaseUrl>/storage/v1/object/message_media/c/<channel id>/<auth user id>/<file>`
(or `d/<conversation id>/…` for a DM) — the third segment is the **auth user id**, which the
INSERT policy checks (`m/20261006900001_private_user_media.sql:8-20`,
`src/services/privateMedia.ts:17-20,93-95`; the app passes `session.user.id`,
`src/components/MessageInput.vue:590,763`). Then insert a `file` part with `path`, `url`,
`fileName`, `fileSize`, `fileType` (a kind, not a MIME type) and optionally `mimeType`
(`src/services/privateMedia.ts:696-722`). The app does not write a MIME type on a part; Timbre's `mime_type` is ignored by every
Harmony renderer. The bucket is private: readers sign `path` with
`POST /storage/v1/object/sign/message_media/<path>` `{"expiresIn":…}` and storage RLS checks
room membership, so the signer must be a member of `c/<channel>`'s server or a participant
of `d/<conversation>` (`m/20261006900001_private_user_media.sql:14-30,63-95`). The app stores
a `url` signed for `COMPAT_URL_TTL_SECONDS` = 7 days so older clients can render it
(`src/services/privateMedia.ts:20,39,573-582`); a 365-day URL as in Timbre
(`timbre/data.ts:502`) is not what the app writes.

### 7.4 Edit and delete

```
PATCH messages?id=eq.<id>   {"content":[…],"encrypted":false,"encryption_metadata":null,"updated_at":"<now>"}
PATCH messages?id=eq.<id>   {"content":[{"type":"text","text":"[deleted]"}],"is_deleted":true}
```

(`src/services/core/CoreMessageService.ts:430-478`.) Only content/`encrypted`/
`encryption_metadata`/`media_paths` may change; author, placement, `is_system`, pin and
federation fields are fixed, a deleted row cannot be restored, and a non-author may only
set `is_deleted:true` (`m/20261005600001_message_path_security.sql:122-166`). The app sends
`updated_at` but it does not decide anything: the trigger rewrites it on a content change and
the guard restores the old value otherwise (`src/services/core/CoreMessageService.ts:431-438`,
`m/20261009000001:33-57`, `m/20261005600001_message_path_security.sql:160-164`).

A PATCH that matches no visible row returns `200` with an empty array; the app treats that
as "not found or not allowed" (`src/services/core/CoreMessageService.ts:447-456`).

### 7.5 Optimistic send and ordering

The app sends one message per channel at a time (`sendKeyForChannel`/`takeSendTurn`),
tags the row with `metadata.client_nonce`, and resolves duplicates by nonce or id when the
broadcast wins the race (`src/stores/useChat.ts:863-881`). `client_nonce` is not a
server-side key; it is only echoed in `metadata`. A Rust client that retries should pick
the message `id` itself (uuid v4) so a retry after a timeout cannot double-post.

---

## 8. Differences from the bot protocol

The bot protocol (`bot §1-§12`) is a server-scoped, credential-per-bot API over
`bot-gateway`. The web protocol is a user-scoped Supabase session. In the Rust client both
implement `Transport` (`tui/transport.rs:166-220`); this table is what the web transport
can fill that the bot one cannot, and the reverse.

### 8.1 Web can, bot cannot

| Capability | Web evidence | Bot |
|---|---|---|
| List the account's servers and channels | `user_servers` embed (§3.3) | No route lists the servers a bot is installed in (`bot §8.1a`) |
| Direct messages | `get_user_conversations`, `dm-conversation-<id>` (§3.10, §5.3) | DMs produce no events and no routes (`bot §8.1b`) |
| Human presence | `presence_heartbeat` / `get_presence` / `presence:update` (§6.1) | No human presence (`bot §8.1c`) |
| Typing indicators | Realtime Presence on `typing:*` (§6.2) | `POST …/typing` "has no effect" (`bot §8.1d`) |
| Notifications and unread badges | `notifications`, `get_unread_counts`, `user:` events (§3.11, §4.4) | None |
| Profile edit, avatar upload | `PATCH profiles`, `storage/v1/object/avatars` (§3.1) | `GET /users/{id}` only |
| Join/leave servers, invites, discovery | `redeem_invite`, `join_public_server`, `get_invite_preview` (§4.17-4.19) | No join route |
| Read rules-acceptance state | `get_server_welcome` (§4.15) | Not exposed |
| Vote in polls | `vote_message_poll`, `poll_event` (§5.3) | No poll routes |
| Voice channels and calls | `voice-channels:` topics, `send_signal` (§5.5) | None |
| Server structure stream (roles, categories, overrides) | `server-structure` `server_event` (§5.3.3) | Only member add/remove events |
| Message E2EE fields | `encrypted`, `encryption_metadata`, `megolm_*` (§3.6) | Message object has no `encrypted` flag (`bot §4`) |

### 8.2 Bot can, web cannot

| Capability | Bot evidence | Web |
|---|---|---|
| Act without a human account | bot token per bot, `bot_id` authorship (`bot §2`) | Needs an email/password account |
| Read/write servers the bot is installed in without joining | `bot_server_permissions` (`bot §3`) | Needs an `accepted` `user_servers` row |
| Slash commands and interaction callbacks | `INTERACTION_CREATE`, `/interactions/*` (`bot §5`) | Can only invoke commands |
| One socket for every server | gateway carries all installations (`bot §5`) | One topic per channel + per server |
| Metadata merge, silent content patch, attachment refresh | `PATCH /messages/:id/metadata`, `/content-silent`, `/attachments/refresh` (`bot §3`) | No equivalent; a PATCH of `metadata` replaces keys it names, and `updated_at` moves |
| Bridge APIs (relay authors, op 6/7) | `bot §5` | None |
| Explicit member add/kick/ban events | `SERVER_MEMBER_*` (`bot §5`) | `membership:event` on `server-structure` carries the row, not the moderator |
| Bot presence (for bots) | `bot_presence` (`bot §8.1c`) | `get_presence` never returns bots |
| Server-scoped rate-limit buckets | `bot §7` | Per-user; instance-wide limiter only at the edge |

### 8.3 Same feature, different shape

| Feature | Bot | Web |
|---|---|---|
| Message content | string in, `content` string out + `content_raw` (`bot §4`) | jsonb array in and out (§3.6.1) |
| Author | `author{id,username,display_name,avatar,bot}` | `user_id`/`bot_id` → a `profiles` row |
| Channel kind | `type` string | `type` smallint 0/1/2 |
| Newest page | `before` cursor by message id | `p_before` timestamptz / `created_at=lt.` |
| Live events | `{op,t,d}` frames | `{op,new,old}` inside `message_event` |
| Editing | `PATCH /messages/:id` with `content` string | `PATCH messages` with a parts array |
| Reaction identity | emoji string in the path | `emoji_id` uuid or `custom_emoji_content` |
| Errors | `{error, code?}` HTTP bodies (`bot §3`) | PostgREST `{code,message,details,hint}` (§3) |

### 8.4 What neither can do

- Enumerate every server on the instance usefully: `servers` is world-readable but has no
  member-only filter and no pagination by relevance beyond `member_count`.
- Search messages the caller cannot view: `message_search_index` RLS mirrors
  `messages` (`m/20261001200002:373-383`).
- Read a channel's history before joining when READ_MESSAGE_HISTORY is withheld (§9.5).
- Send into an encrypted channel without implementing Megolm (§9.6).

---

## 9. Gotchas

### 9.1 Realtime is Broadcast-only, and private

`supabase_realtime` carries no table (`m/20261021100001_realtime_publication_drop_messages.sql:1-30`).
A `postgres_changes` subscription joins and stays silent — the single most likely way to
build a client that "works" but never updates. Topics must be joined with
`config.private: true` and the user's JWT (`timbre/realtime.ts:10-20`, and the app's own joins at
`src/stores/useChat.ts:1368-1377`; policy at `m/20261025100001_realtime_policy_converge.sql:43-49`).
Without a token, or with the anon key, private joins are refused
(`src/services/realtimeAccessToken.ts:1-25`).

### 9.2 The message event is not the row

`new` omits six columns — `federation_status`, `media_paths`, `megolm_message_index`,
`reactions`, `pinned_at`, `pinned_by` (`m/20261110900004_history_import_quiet.sql:274-300`, and
the fixture diff in §10). A renderer that needs an attachment
path must re-read the row. `restricted:true` events carry seven keys and no content
(`:250-260`).

### 9.3 RLS returns empty, not 403

Every read policy filters rows; a `200 []` means "nothing visible", not "wrong query"
(`m/20261001200002:324-337` for messages, `baseline.sql:19427-19441` for channels). The two
exceptions that return
an error are the pre-request hook's `401 session_revoked` and `403 insufficient_aal`
(`src/supabase.ts:158-176`). Diagnose an empty sidebar as "no membership", not as a bug.

### 9.4 `get_current_profile_id()` is not `auth.uid()`

Policies compare `profiles.auth_user_id = auth.uid()` and then use `profiles.id` everywhere
else (`baseline.sql:4233-4241`). The app creates profiles with `id = auth_user_id`, but
fixtures and imported accounts may not (`docs/contribute/architecture.md:186`). A client
that sends `auth.uid()` as `user_id` on insert gets `42501`
(`m/20261001200002:339-341`). Always use the profile `id`.

### 9.5 READ_MESSAGE_HISTORY silently truncates history

In a local server channel where the member's role denies bit 22, the newest page is cut to
messages created after `begin_channel_history_view`; older rows are absent from
`get_message_page`, `messages`, pins and search, and the app must call
`begin_channel_history_view` before its first page
(`m/20261017100001:1-20,481-518`). The RPC returns `history_since` so a client can tell
"channel is empty" from "history withheld" (`m/20261017100001:849`). Own messages stay
readable.

### 9.6 Encrypted channels fail closed

`channel_encryption_settings.messages_encrypted` (or the server default) makes a plaintext
insert fail with a message containing `CHANNEL_ENCRYPTED`
(`src/services/core/channelMessageEncryption.ts:99-120`). There is no server-side
plaintext fallback: the client must implement Megolm (`src/services/encryption/`) or refuse
to send. Reading yields ciphertext with `encrypted:true` and
`encryption_metadata{algorithm:"m.megolm.v1.aes-sha2",…}`; a client without the keys should
render a placeholder, as the app does for replies
(`src/stores/useChat.ts:104-110`). `history_visibility` gates key relay per channel
(`m/20261001000001_channel_encryption_settings.sql:53-78`).

### 9.7 The 1000-row cap is silent

`PGRST_DB_MAX_ROWS=1000` (`self-host/supabase-overrides.yml:90`) truncates a read with
`200` and no error (`src/services/pagedRead.ts:1-10`). Member lists, emoji and the server
rail are the places this bites.

### 9.8 Usernames: the app's own rule, not the old one

`profiles_username_check` was dropped (`m/20261111300001_remote_username_dots.sql:7-18`);
local rows are guarded by `guard_local_username`: `^[a-z0-9_]{3,24}$`, a reserved list and
the instance's domain labels (`m/20261031900001_reserved_usernames.sql:129-160`). Timbre
accepts `[a-z0-9_.-]{2,32}` (`timbre/onboarding.ts:57-59`) and its profile insert can fail
with `23514`; other clients must not copy that regex.

### 9.9 `channel_categories` can be hidden but still referenced

A category with no viewable channel is invisible (`m/20261026200001_private_categories.sql:176-181`),
while `channels.category` still names it. Group by the uuid, not by the joined name.

### 9.10 `servers` is world-readable

`Enable read access for all users USING (true)` (`baseline.sql:19343-19345`). Do not treat
"the server row is readable" as "the user is a member"; membership is `user_servers`.

### 9.11 Presence needs an explicit device row and expiry

No heartbeat for 150 s ⇒ offline (`m/20261006800001:456-560`). Two tabs are two device rows;
`presence_offline` removes one and must not be sent with someone else's id (it always uses
`get_current_profile_id()`). `get_presence` only answers for related profiles, so a client
cannot poll a stranger's status (`m/20261006800001:703-730`).

### 9.12 Typing has no server-side TTL

Realtime Presence keeps an entry until the socket leaves; the app expires entries older
than 3 s **client-side** (`src/services/TypingIndicatorService.ts:33-45,418-445`). A Rust
client must do the same or a frozen tab types forever.

### 9.13 Sign-up is two writes and may be closed

`DISABLE_SIGNUP=true` by default (`self-host/configure.sh:612`), SMTP makes autoconfirm
false (`:610`), and the captcha hook answers `428` (`src/utils/signupCaptcha.ts:17-31`).
An auth user without a profile row is a valid, useless session: every policy resolves to
`NULL` (`src/services/AuthContextService.ts:74-82`).

### 9.14 Timbre and harmony disagree — follow harmony

| Topic | Timbre | Harmony |
|---|---|---|
| Realtime | broadcast + private, correct (`timbre/realtime.ts`) | same (`src/stores/useChat.ts:1368-1377`) |
| Mentions in `metadata` | comments say `mentions` live in metadata (`timbre/realtime.ts:28-29`) | no `metadata.mentions` anywhere in the schema; mentions are `content` parts and the DB derives notifications (`m/20261003400001_enforce_mention_permissions.sql:1-20`) |
| Channel list | filters `type=neq.2` and folds categories (`timbre/data.ts:394-401`) | keeps categories separate, `type==2` filtered client-side (`src/stores/useServerChannel.ts:547-552`) |
| Profile insert | writes `domain: instance.origin` (`timbre/onboarding.ts:144-151`) | writes `instance_config.domain` plus the five actor URLs (`src/views/NewProfile.vue:411-425`) |
| Username regex | `[a-z0-9_.-]{2,32}` | `^[a-z0-9_]{3,24}$` (`m/20261031900001:129-132`) |
| Message text limit | sends without reading `max_message_length` | reads `instance_config` (`src/services/core/messageLimits.ts:32-55`) |
| Attachment part MIME field | writes `mime_type` (`timbre/data.ts:524-536`) | the part field is `mimeType` and the app writes none (`src/types/chat.ts:535-545`, `src/services/privateMedia.ts:696-722`) |
| Attachment URL lifetime | signs for 365 days (`timbre/data.ts:502`) | `COMPAT_URL_TTL_SECONDS` = 7 days (`src/services/privateMedia.ts:39,573-582`) |

### 9.15 Version-dependent behaviour

| Thing | Version | Effect |
|---|---|---|
| `capabilities` present | 1.9.0+ | `[]`/absent on older servers |
| `get_message_page`, `get_unread_counts`, `get_server_member_list` | absent on old self-hosts | `PGRST202`; fall back to table reads (`src/services/memberDirectoryService.ts:69-80`) |
| Realtime `vsn` | pin `1.0.0` | 2.x serializer absent from older images (`src/supabase.ts:187-194`) |
| `channel_encryption_settings.history_visibility` | 1.9.0 (`encrypted-history`) | selecting it on an older server is `42703`; the app omits it (`src/stores/useChannelEncryption.ts:66-72`) |
| `profiles.instance_role` | 1.9.x (`instance-staff-roles`) | `42703` if selected unconditionally |
| `channels.host_flags` | 1.9.x (`channel-host-flags`) | `42703` if selected unconditionally |

### 9.16 Query syntax traps

- `order=order.asc` is fine, but `"order"` is a reserved word: quote it inside `select`
  aliases, not in the query parameter (`src/stores/useServerChannel.ts:546`).
- `is null` is `column=is.null`, not `column=null` (`src/stores/useDM.ts:727`).
- An embedded resource is named after the FK column when it is not the table name:
  `server:server_id(...)`, `conversations!inner(...)` (`src/stores/useServerChannel.ts:346`,
  `src/stores/useDM.ts:717`).
- `in.(a,b)` values must be URL-encoded; an empty list is a syntax error — skip the filter.
- A jsonb leaf projection needs the `->` arrow inside the embed:
  `host_polls:federation_metadata->message_polls` (`src/stores/useServerChannel.ts:360`).

### 9.17 The Rust client today

`tui/web.rs:1-9` says it plainly: REST only, no Realtime, no refresh. It signs in, reads
the server/channel/member/message tables, and sends plaintext messages. Everything in §5,
§6 and §9.1-9.7 is missing; the trait's `capabilities()` is where the TUI should learn that
(`tui/transport.rs:169`). Its avatar URL builder assumes a public `avatars` bucket
(`tui/web.rs:342-369`), which matches the app (`src/utils/avatarUtils.ts:59-72`) but not a
private-media instance.

---

## 10. Fixtures

`nexus/fixtures/web/*.json` — one file per payload. JSON is bare (no wrapper) except
`instance-info.json` and `realtime-message-event.json`, which are wire objects.

| File | Payload | Origin |
|---|---|---|
| `instance-info.json` | discovery response | Copied field-for-field from `fb/routes/instanceInfo.ts:14-21` and `fb/config/capabilities.ts:7-71`; UUIDs/keys are placeholders |
| `gotrue-token.json` | `POST /auth/v1/token?grant_type=password` | **Reconstructed** from GoTrue v2.197.0's documented session body; the app only reads `access_token`, `refresh_token`, `user.id` (`tui/web.rs:75-84`) |
| `profile-row.json` | `select=*` on `profiles` | Column list from `baseline.sql:121-215` plus the seven later columns; all 56 are present, values are examples |
| `server-row.json` | `select=*` on `servers` | Column list from `baseline.sql:713-766` plus `category`, `sidebar_banner`, suspension columns |
| `channel-row.json` | `select=*` on `channels` | `baseline.sql:808-846` plus `host_flags` |
| `message-row.json` | `select=*` on `messages` | `baseline.sql:849-918` plus `media_paths`; content parts per §3.6.1 |
| `member-row.json` | one element of `get_server_member_list.members` | **Copied shape** from `server_member_json` (`m/20261113100001_member_directory.sql:221-248`) |
| `realtime-message-event.json` | the `message_event` Broadcast payload | **Copied shape** from `broadcast_message_event` (`m/20261110900004_history_import_quiet.sql:274-315`) |

Where a fixture is reconstructed rather than verbatim:

- `gotrue-token.json`: no token body exists in the repository to copy. The key set is
  GoTrue's, and the fields the app depends on are exactly `access_token`, `refresh_token`,
  `user.id`; the rest is illustrative.
- Every `id`/`token`/URL value is a placeholder; none is a real account.
- `message-row.json` and `realtime-message-event.json` are the same message, so the six
  columns the Broadcast `new` omits (`federation_status`, `media_paths`,
  `megolm_message_index`, `reactions`, `pinned_at`, `pinned_by`) are visible as a diff. The
  row is what `select=*` returns; the event is what the socket delivers.
- `member-row.json` is one member from inside the `members` array, not a page.

---

## 11. Minimal client checklist

1. `GET /api/federation/instance-info`; keep `supabaseUrl`, `supabaseAnonKey`,
   `capabilities`, `domain`. Never assume `/config.json` is readable.
2. `POST /auth/v1/token?grant_type=password`; store `access_token`, `refresh_token`,
   `expires_at`; refresh 60 s early; on `401 session_revoked` / `403 insufficient_aal`
   drop the session and sign in again.
3. `GET profiles?auth_user_id=eq.<sub>` → profile `id`; use that everywhere, never `sub`.
   A missing row means "sign-up half-finished", not "signed out".
4. Rail: `user_servers?user_id=eq.<profile id>&select=…server:server_id(…)&order=position.asc`.
5. Server view: `channels?server_id=eq.<id>&order=order.asc`,
   `channel_categories?server_id=eq.<id>&order=order.asc`; keep an "Uncategorised" bucket
   for categories RLS hides.
6. Page: `POST /rpc/get_message_page` with exactly one of `p_channel_id`/`p_conversation_id`,
   `p_limit ≤ 100`; keep the returned order (oldest first); page with `p_before` = oldest
   `created_at`.
7. Live: one WebSocket, private `phx_join` with the JWT; topics per §5.3; on `SUBSCRIBED`
   re-read rows newer than the newest held; dedupe by `id`; ignore `restricted:true`
   events until re-read.
8. Send: `POST messages` with `Prefer: return=representation`, `channel_id`/`conversation_id`,
   `user_id` = profile id, non-empty `content` array; uuid v4 id of your own for retries;
   render a 0-row answer as "blocked", and decode `SLOWMODE_ACTIVE:` /
   `RULES_NOT_ACCEPTED:` / `AUTOMOD_BLOCKED` / `CHANNEL_ENCRYPTED` from `message`.
9. Presence: heartbeat every 60 s with a stable per-tab `device_id`; `presence_offline` on
   exit; expect peers offline after 150 s.
10. Typing: track/expire on `typing:<kind>:<id>` with a 3 s local TTL.
11. Unread: `get_unread_counts()` at start and on `unread:change`; `mark_*_as_read` when a
    view opens; notifications via `notification:new` on `user:<profile id>`.
12. Never `postgres_changes`, never write `unread_counts`, never send `auth.uid()` as
    `user_id`, never select a column the capability list does not cover.
