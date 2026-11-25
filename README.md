# Nexus TUI client — now speaking Harmony

A terminal client for **Harmony** instances: Discord-style servers and channels, in a
Cyberpunk-flavoured TUI.

This tree is a rewrite of [`y4my4my4m/nexus_client`](https://github.com/y4my4my4m/nexus_client).
The client used to speak the Nexus protocol — its own `nexus-tui-common` bincode frames over
TLS — and now speaks **Harmony's protocols** instead. Nothing of the Nexus wire model remains:
no `nexus-tui-common` dependency, no `bincode`, no `LengthDelimitedCodec`, no
`ClientMessage`/`ServerMessage` on the wire.

```
cargo run -- --instance har.mony.lol --token harmony_bot_… --servers <server-uuid>
cargo run -- --web --instance har.mony.lol --email you@example.com --password …
```

## Two transports, one UI

Harmony exposes two client-facing protocols. Both sit behind one trait
(`src/harmony/transport.rs`), and the UI never sees either payload:

| Transport | What it is | What it gives you |
|---|---|---|
| **bot-gateway** (default) | `Authorization: Bot <token>` over `/bot-gateway/api/v1` and the `/bot-gateway/gateway` WebSocket — Harmony's documented bot protocol (opcodes 0/1/2/11, identify, heartbeat, reconnect). | Live message events, channels, members, roles, sending, editing, deleting, reactions, threads, invite previews. No DMs, no server enumeration. |
| **web-app** | The protocol Harmony's own Vue app speaks: `/api/federation/instance-info` discovery, GoTrue sign-in, PostgREST with the user's JWT, and Supabase Realtime for live messages. | Real accounts, **server enumeration**, message history, live messages for the open channel, and **direct messages** (list, history, send) via `get_user_conversations` / `create_or_get_direct_conversation`. No presence or typing yet. |

What each transport cannot do is reported, not hidden: the session answers with
`Event::Unsupported` and the UI shows the explanation (see "Not in Harmony yet" below).

## Running

Everything is configured by flags or environment; the sign-in screen pre-fills from them.

| Flag | Environment | Meaning |
|---|---|---|
| `--instance`, `-i` | `HARMONY_INSTANCE` | Instance host or URL (`har.mony.lol`, `https://har.mony.lol`, `localhost:3002`). Local hosts get `http`, everything else `https`. |
| `--token`, `-t` | `HARMONY_TOKEN` | Bot token (`harmony_bot_…`), from **User settings → My bots**. |
| `--servers`, `-s` | `HARMONY_SERVERS` | Comma/space-separated server UUIDs. The bot API has **no route that lists a bot's servers**, so a bot session opens exactly these. |
| `--email`, `-e` | `HARMONY_EMAIL` | Web transport: account email. |
| `--password`, `-p` | `HARMONY_PASSWORD` | Web transport: account password. |
| `--web` / `--bot` | `HARMONY_TRANSPORT` | Pick the transport explicitly; otherwise a token means bot and an email+password means web. |

With no credentials the sign-in screen is shown; `Tab` moves between fields, `Enter` submits,
and the transport button switches bot ⇄ web.

## Building

```bash
cargo build                 # no audio: no system packages needed (CI-friendly)
cargo build --features audio # MP3 UI sounds; needs ALSA (libasound2-dev) on Linux
cargo test                  # 47 unit tests + 8 protocol integration tests
cargo clippy --all-targets
```

Audio is opt-in because it needs ALSA; the `audio` feature is the only thing that pulls in
`rodio`.

## How the rewrite is laid out

```
src/domain.rs            the client's vocabulary: User, Server, Channel, ChannelMessage,
                         Command (UI → session), Event (session → UI)
src/harmony/
  mod.rs                 instance/URL normalisation (https, wss, API bases)
  error.rs               HarmonyError, mapped from Harmony's documented status/error pairs
  model.rs               Harmony's JSON exactly as the API spells it (+ tolerant timestamps)
  rest.rs                the bot REST routes, with retry/backoff and 429 handling
  gateway.rs             the bot gateway: identify, heartbeat, event decoding, reconnect policy
  realtime.rs            Supabase Realtime (Phoenix): private joins, heartbeat, broadcasts
  bot.rs                 BotTransport: the bot protocol behind the transport trait
  web.rs                 WebTransport: discovery + GoTrue + PostgREST, behind the same trait
  transport.rs           the trait + the neutral shapes both transports produce
  session.rs             commands → transport calls, transport events → UI events, caches
src/lib.rs               app bootstrap, startup/env parsing, headless UI tests
src/main.rs              `nexus_tui::run()`
docs/protocol-bot.md     the bot protocol, digested from Harmony's docs and gateway source
docs/protocol-web.md     the web app's protocol: tables, RPCs, Realtime topics, gotchas
tests/harmony_bot.rs     end-to-end tests against a mock instance (REST + WebSocket)
tests/harmony_realtime.rs a mock Realtime socket checking the private join and a broadcast
tests/fixtures/bot/      Harmony's own documented payloads, used by those tests
```

The UI modules (`ui/`, `handlers/`, `state/`, `services/`) are the original Nexus screens,
adjusted where the protocol changed: the sign-in screen asks for an instance and a credential
instead of a username and password, and the message/member data comes from Harmony's events
and routes.

## Not in Harmony yet

* **Forums** are on Harmony's [ROADMAP](https://github.com/y4my4my4m/harmony/blob/master/ROADMAP.md).
  The forum screens stay, show nothing, and explain why.
* **Direct messages** are only for the web transport: the bot protocol has no DM route and no
  DM event, so a bot session's DM screen explains that instead of pretending it is empty.
* **Registration** happens on the instance; the "create account" screen sends you to the web app.
* **Presence and typing** on the web transport: the tables, RPCs and Realtime topics are
  documented (`docs/protocol-web.md`) but not wired into the screens yet.

## Credits and licence

Upstream: [`y4my4my4m/nexus_client`](https://github.com/y4my4my4m/nexus_client), MIT, by
y4my4m. Harmony is [`y4my4my4m/harmony`](https://github.com/y4my4my4m/harmony) (AGPL-3.0 with
additional terms); this client only talks to it over its public protocols.
