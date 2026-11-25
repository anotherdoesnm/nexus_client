//! A live probe of the web transport against a real instance. It needs
//! credentials and the network, so it is an example rather than a test:
//!
//! ```bash
//! HARMONY_INSTANCE=har.mony.lol \
//! HARMONY_EMAIL=you@example.com HARMONY_PASSWORD=… \
//!   cargo run --example live_probe
//! ```
//!
//! For an SSO account, set `HARMONY_ACCESS_TOKEN` + `HARMONY_REFRESH_TOKEN`
//! instead. Nothing here prints the credentials.

use std::time::Duration;

use nexus_tui::harmony::transport::{Transport, TransportEvent};
use nexus_tui::harmony::web::WebTransport;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let instance = env("HARMONY_INSTANCE").expect("HARMONY_INSTANCE");
    let transport = match (env("HARMONY_ACCESS_TOKEN"), env("HARMONY_EMAIL"), env("HARMONY_PASSWORD")) {
        (Some(token), _, _) => {
            WebTransport::with_session(instance.clone(), token, env("HARMONY_REFRESH_TOKEN"))
                .expect("session transport")
        }
        (None, Some(email), Some(password)) => {
            WebTransport::new(instance.clone(), email, password).expect("password transport")
        }
        _ => panic!("set HARMONY_ACCESS_TOKEN or HARMONY_EMAIL + HARMONY_PASSWORD"),
    };

    let account = transport.sign_in().await.expect("sign in");
    println!("instance:  {instance}");
    println!("account:   {} ({})", account.username, account.id);
    if account.username.starts_with("user-") {
        eprintln!("warning: the profile did not resolve to a username");
    }

    let servers = transport.servers().await.expect("servers");
    println!("servers:   {}", servers.len());
    if servers.is_empty() {
        eprintln!("warning: the account sees no servers");
    }

    let mut saw_messages = 0;
    for server in servers.iter().take(5) {
        let channels = transport.channels(server.id).await.unwrap_or_default();
        let text: Vec<_> = channels.iter().filter(|channel| channel.kind.is_text()).collect();
        println!(
            "  {} — {} channels ({} text)",
            server.name,
            channels.len(),
            text.len()
        );
        if let Some(channel) = text.first() {
            let messages = transport.messages(channel.id, None, 5).await.unwrap_or_default();
            saw_messages += messages.len();
            println!("    #{} — {} recent messages", channel.name, messages.len());
            for message in messages.iter().rev().take(2) {
                let author = message
                    .author
                    .as_ref()
                    .map(|author| author.username.clone())
                    .unwrap_or_else(|| "?".to_string());
                let text: String = message.content.chars().take(60).collect();
                println!("      {author}: {text}");
            }
        }
    }
    if saw_messages == 0 {
        eprintln!("warning: no messages were readable");
    }

    let conversations = transport.dm_conversations().await.unwrap_or_default();
    println!("dms:       {}", conversations.len());
    for conversation in conversations.iter().take(3) {
        let peer = conversation
            .peer
            .as_ref()
            .map(|peer| peer.username.clone())
            .unwrap_or_else(|| "group".to_string());
        let preview = conversation
            .last_message
            .as_ref()
            .map(|message| message.content.chars().take(40).collect::<String>())
            .unwrap_or_default();
        println!("  with {peer} — {} unread, last: {preview}", conversation.unread);
        let history = transport
            .dm_messages(conversation.id, None, 3)
            .await
            .unwrap_or_default();
        println!("    {} recent messages", history.len());
    }

    // Realtime: watch the first text channel and report what arrives.
    if let Some(server) = servers.first() {
        let channels = transport.channels(server.id).await.unwrap_or_default();
        let text: Vec<_> = channels
            .iter()
            .filter(|channel| channel.kind.is_text())
            .collect();
        let channel = text
            .iter()
            .find(|channel| channel.name == "general")
            .copied()
            .or_else(|| text.first().copied());
        if let Some(channel) = channel {
            transport.watch_channel(channel.id).await;
            // Give the private join a moment before sending, otherwise the
            // broadcast can pass us by.
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut sent_id = None;
            if env("HARMONY_PROBE_SEND").as_deref() == Some("1") {
                match transport
                    .send_message(channel.id, "nexus-tui live probe", None)
                    .await
                {
                    Ok(message) => {
                        println!("sent a probe message to #{} ({})", channel.name, message.id);
                        sent_id = Some(message.id);
                    }
                    Err(err) => eprintln!("could not send the probe: {err}"),
                }
            }
            if let Some(mut events) = transport.take_events() {
                println!("watching #{} for 8s…", channel.name);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
                while tokio::time::Instant::now() < deadline {
                    match tokio::time::timeout(Duration::from_secs(2), events.recv()).await {
                        Ok(Some(TransportEvent::Notice(message))) => println!("  notice: {message}"),
                        Ok(Some(TransportEvent::Connected)) => println!("  connected"),
                        Ok(Some(TransportEvent::ReactionAdd { emoji, .. })) => {
                            println!("  live reaction: {emoji}")
                        }
                        Ok(Some(TransportEvent::MessageCreate(message))) => {
                            let text: String = message.content.chars().take(60).collect();
                            if Some(message.id) == sent_id {
                                println!("  live echo of our own message: {text}");
                            } else {
                                println!("  live message: {text}");
                            }
                        }
                        Ok(Some(TransportEvent::Disconnected { reason, .. })) => {
                            println!("  disconnected: {reason}");
                            break;
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(_) => {}
                    }
                }
            }
        }
    }

    transport.shutdown().await;
    Ok(())
}
