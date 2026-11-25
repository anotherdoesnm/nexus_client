//! The Nexus TUI client, rewritten to speak Harmony's protocol.
//!
//! The binary in `main.rs` only starts [`run`]; everything the client does lives
//! in these modules. The protocol layer is [`harmony`]: Harmony's bot REST API
//! plus gateway WebSocket, and the web app's Supabase protocol, behind one
//! transport trait. The UI never sees a Harmony payload — [`domain`] is the
//! vocabulary it renders and [`harmony::session`] translates.

pub mod app;
pub mod desktop_notifications;
pub mod domain;
pub mod global_prefs;
pub mod handlers;
pub mod harmony;
pub mod model;
pub mod services;
pub mod sound;
pub mod state;
pub mod ui;

use std::error::Error;
use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, Event as CEvent},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use app::App;
use domain::{Command, Event, LoginRequest, TransportKind};
use sound::SoundManager;

/// Application events: terminal input, session events and the UI tick.
#[allow(clippy::large_enum_variant)]
enum AppEvent {
    Terminal(CEvent),
    Session(Event),
    Tick,
}

/// What the command line and the environment say about signing in.
#[derive(Debug, Default, Clone)]
pub struct Startup {
    pub instance: Option<String>,
    pub transport: Option<TransportKind>,
    pub token: Option<String>,
    pub email: Option<String>,
    pub password: Option<String>,
    /// An imported web session (`--session <file|json>` or
    /// `--access-token` + `--refresh-token`), for SSO accounts.
    pub session_access: Option<String>,
    pub session_refresh: Option<String>,
    pub server_ids: String,
}

impl Startup {
    /// Read `HARMONY_*` and the process arguments. Arguments win over the
    /// environment, so a launcher can override a profile’s defaults.
    pub fn from_env_and_args() -> Self {
        let mut startup = Self {
            instance: env_non_empty("HARMONY_INSTANCE"),
            transport: env_non_empty("HARMONY_TRANSPORT").map(|value| parse_transport(&value)),
            token: env_non_empty("HARMONY_TOKEN"),
            email: env_non_empty("HARMONY_EMAIL"),
            password: env_non_empty("HARMONY_PASSWORD"),
            session_access: env_non_empty("HARMONY_ACCESS_TOKEN"),
            session_refresh: env_non_empty("HARMONY_REFRESH_TOKEN"),
            server_ids: std::env::var("HARMONY_SERVERS").unwrap_or_default(),
        };

        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--instance" | "-i" | "--token" | "-t" | "--email" | "-e" | "--password" | "-p"
                | "--servers" | "-s" | "--access-token" | "--refresh-token" | "--session" => {
                    if let Some(value) = args.get(index + 1).cloned() {
                        match arg {
                            "--instance" | "-i" => startup.instance = Some(value),
                            "--token" | "-t" => startup.token = Some(value),
                            "--email" | "-e" => startup.email = Some(value),
                            "--password" | "-p" => startup.password = Some(value),
                            "--access-token" => startup.session_access = Some(value),
                            "--refresh-token" => startup.session_refresh = Some(value),
                            "--session" => {
                                let (access, refresh) = read_session(&value);
                                startup.session_access = access.or(startup.session_access);
                                startup.session_refresh = refresh.or(startup.session_refresh);
                            }
                            _ => startup.server_ids = value,
                        }
                        index += 2;
                        continue;
                    }
                }
                "--web" => startup.transport = Some(TransportKind::Web),
                "--bot" => startup.transport = Some(TransportKind::Bot),
                other if !other.starts_with('-') && startup.instance.is_none() => {
                    startup.instance = Some(other.to_string());
                }
                _ => {}
            }
            index += 1;
        }
        startup
    }

    /// A login request when the startup has enough to try one.
    pub fn login_request(&self) -> Option<LoginRequest> {
        let instance = self.instance.clone()?;
        let transport = self.transport.unwrap_or_else(|| {
            if self.session_access.is_some()
                || (self.email.is_some() && self.password.is_some() && self.token.is_none())
            {
                TransportKind::Web
            } else {
                TransportKind::Bot
            }
        });
        match transport {
            TransportKind::Bot => {
                let token = self.token.clone()?;
                Some(LoginRequest {
                    instance,
                    transport,
                    token: Some(token),
                    email: None,
                    password: None,
                    session: None,
                    server_ids: harmony::transport::parse_server_ids(&self.server_ids),
                })
            }
            TransportKind::Web => {
                if let Some(access_token) = self.session_access.clone() {
                    return Some(LoginRequest {
                        instance,
                        transport,
                        token: None,
                        email: None,
                        password: None,
                        session: Some(domain::SessionTokens {
                            access_token,
                            refresh_token: self.session_refresh.clone(),
                        }),
                        server_ids: Vec::new(),
                    });
                }
                let email = self.email.clone()?;
                let password = self.password.clone()?;
                Some(LoginRequest {
                    instance,
                    transport,
                    token: None,
                    email: Some(email),
                    password: Some(password),
                    session: None,
                    server_ids: Vec::new(),
                })
            }
        }
    }
}

/// Read `--session`: a file path or inline JSON, and find the token pair at
/// whatever depth the web app's storage object nests it.
fn read_session(value: &str) -> (Option<String>, Option<String>) {
    let raw = if std::path::Path::new(value).exists() {
        std::fs::read_to_string(value).unwrap_or_default()
    } else {
        value.to_string()
    };
    match harmony::web::parse_session_json(&raw) {
        Some((access, refresh)) => (Some(access), refresh),
        None => (None, None),
    }
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_transport(value: &str) -> TransportKind {
    match value.trim().to_ascii_lowercase().as_str() {
        "web" | "supabase" | "app" => TransportKind::Web,
        _ => TransportKind::Bot,
    }
}

/// `--check <instance>`: probe the instance's discovery document, its bot
/// gateway health endpoint, and print the URLs the client would use. Useful to
/// verify a downloaded binary's TLS stack end to end without credentials.
fn check_target_in(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--check" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--check=") {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

async fn check_instance(instance: &str) -> Result<(), Box<dyn Error>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("nexus-tui/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let root = harmony::instance_root(instance)?;
    println!("instance   {root}");

    match client.get(harmony::instance_info_url(instance)?).send().await {
        Ok(response) if response.status().is_success() => {
            let status = response.status();
            let info: serde_json::Value = response.json().await?;
            println!(
                "discovery  {} {} at {} ({status})",
                info.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                info.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
                info.get("domain").and_then(|v| v.as_str()).unwrap_or("?")
            );
        }
        Ok(response) => println!("discovery  HTTP {}", response.status()),
        Err(err) => println!("discovery  unreachable: {err}"),
    }

    let health = root.join("bot-gateway/health")?;
    match client.get(health).send().await {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            println!(
                "gateway    {} ({status}) {}",
                harmony::gateway_url(instance)?,
                body.trim()
            );
        }
        Err(err) => println!("gateway    unreachable: {err}"),
    }
    Ok(())
}

/// Print the release identity and exit, so a packaged binary can be checked
/// without a terminal (and without credentials).
fn handle_meta_flags() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!(
            "nexus-tui-client {} ({}-{})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::ARCH,
            std::env::consts::OS
        );
        return true;
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "nexus-tui-client {} — a terminal client for Harmony instances\n\n\
             Usage: nexus-tui-client [options]\n\n\
             Options:\n  \
             -i, --instance <host>   Harmony instance (host or URL)\n  \
             -t, --token <token>     bot token (bot-gateway transport)\n  \
             -e, --email <email>     account email (web-app transport)\n  \
             -p, --password <pass>   account password (web-app transport)\n  \
             -s, --servers <ids>     comma-separated server UUIDs to open (bot transport)\n      \
             --session <file|json>   web session imported from the app (SSO accounts)\n      \
             --access-token <jwt>    the same, as a bare access token\n      \
             --refresh-token <jwt>   refresh token to pair with --access-token\n      \
             --web | --bot       choose the transport explicitly\n  \
             --check <host>          probe an instance and exit\n  \
             -V, --version           print the version and exit\n  \
             -h, --help              print this help and exit\n\n\
             Environment: HARMONY_INSTANCE, HARMONY_TOKEN, HARMONY_EMAIL, HARMONY_PASSWORD,\n\
             HARMONY_SERVERS, HARMONY_TRANSPORT, HARMONY_ACCESS_TOKEN, HARMONY_REFRESH_TOKEN",
            env!("CARGO_PKG_VERSION")
        );
        return true;
    }
    false
}

/// Run the client until the user quits.
pub async fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(instance) = check_target_in(&args) {
        return check_instance(&instance).await;
    }
    if handle_meta_flags() {
        return Ok(());
    }
    global_prefs::init_global_prefs();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (commands_tx, commands_rx) = mpsc::unbounded_channel::<Command>();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();

    let startup = Startup::from_env_and_args();
    let prefill = startup.login_request();

    let sound_manager = SoundManager::new();
    let mut app = App::new(commands_tx, &sound_manager);
    app.startup = startup;
    app.apply_startup();

    // The session owns whichever Harmony transport signs in.
    harmony::session::spawn(commands_rx, events_tx.clone(), prefill);

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AppEvent>();

    // Terminal events and the UI tick.
    let terminal_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        loop {
            interval.tick().await;
            if event::poll(Duration::from_millis(0)).unwrap_or(false) {
                if let Ok(event) = event::read() {
                    if terminal_tx.send(AppEvent::Terminal(event)).is_err() {
                        break;
                    }
                }
            }
            if terminal_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    // Session events.
    tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            if event_tx.send(AppEvent::Session(event)).is_err() {
                break;
            }
        }
    });

    while !app.ui.should_quit {
        terminal.draw(|f| ui::ui(f, &mut app))?;

        if let Some(event) = event_rx.recv().await {
            match event {
                AppEvent::Terminal(CEvent::Key(key)) => {
                    handlers::handle_key_event(key, &mut app);
                }
                AppEvent::Terminal(_) => {}
                AppEvent::Session(event) => app.handle_event(event),
                AppEvent::Tick => app.on_tick(),
            }
        }
    }

    // Best-effort shutdown of the transport.
    let _ = app.to_session.send(Command::Logout);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headless_app() -> (App<'static>, mpsc::UnboundedReceiver<Command>) {
        // The stub SoundManager (audio feature off) makes this safe in CI.
        let sound: &'static SoundManager = Box::leak(Box::new(SoundManager::new()));
        let (commands_tx, commands_rx) = mpsc::unbounded_channel();
        (App::new(commands_tx, sound), commands_rx)
    }

    fn render(app: &mut App<'_>) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(110, 40)).expect("terminal");
        terminal
            .draw(|frame| ui::ui(frame, app))
            .expect("the login screen renders");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_login_screen_asks_for_a_harmony_instance() {
        let (mut app, _commands) = headless_app();
        let screen = render(&mut app);
        assert!(screen.contains("Sign in to a Harmony instance"), "{screen}");
        assert!(screen.contains("Instance (har.mony.lol)"), "{screen}");
        assert!(screen.contains("Bot token"), "{screen}");
        assert!(screen.contains("bot-gateway"), "{screen}");
    }

    #[test]
    fn switching_to_the_web_transport_asks_for_an_email() {
        let (mut app, _commands) = headless_app();
        app.auth.toggle_transport();
        let screen = render(&mut app);
        assert!(screen.contains("web-app"), "{screen}");
        assert!(screen.contains("Account email"), "{screen}");
        assert!(screen.contains("Password"), "{screen}");
    }

    #[test]
    fn a_ready_event_records_the_instance_and_capabilities() {
        let (mut app, _commands) = headless_app();
        let user = domain::User {
            id: uuid::Uuid::nil(),
            username: "ping-bot".to_string(),
            display_name: None,
            color: domain::UserColor::for_id(uuid::Uuid::nil()),
            role: domain::UserRole::User,
            profile_pic: None,
            cover_banner: None,
            status: domain::UserStatus::Connected,
            bot: true,
        };
        app.handle_event(Event::AuthSuccess(user.clone()));
        assert_eq!(app.ui.mode, state::AppMode::MainMenu);
        app.handle_event(Event::Ready {
            account: user,
            instance: "har.mony.lol".to_string(),
            transport: TransportKind::Bot,
            capabilities: domain::Capabilities::bot(),
            unavailable_servers: Vec::new(),
        });
        assert_eq!(app.instance.as_deref(), Some("har.mony.lol"));
        assert_eq!(app.transport, Some(TransportKind::Bot));
        assert!(app
            .unsupported
            .as_deref()
            .unwrap_or_default()
            .contains("--servers"));
    }

    #[test]
    fn a_failed_sign_in_keeps_the_typed_fields_and_shows_the_reason() {
        let (mut app, _commands) = headless_app();
        app.auth.current_input = "har.mony.lol".to_string();
        app.auth.email_input = "alice@example.com".to_string();
        app.auth.password_input = "secret".to_string();
        app.handle_event(Event::AuthFailure(
            "Invalid login credentials — this instance offers github sign-in".to_string(),
        ));
        assert_eq!(app.auth.current_input, "har.mony.lol");
        assert_eq!(app.auth.email_input, "alice@example.com");
        assert_eq!(app.auth.password_input, "secret");
        let error = app.auth_error.as_deref().unwrap_or_default();
        assert!(error.contains("Invalid login credentials"));
        // A connection notice must not cover the reason up.
        app.handle_event(Event::Connection(domain::ConnectionState::Disconnected));
        let shown = app
            .notifications
            .current_notification
            .as_ref()
            .map(|(text, _, _)| text.clone())
            .unwrap_or_default();
        assert!(
            shown.contains("Invalid login credentials"),
            "the reason must stay on screen, got: {shown}"
        );
    }

    #[test]
    fn unsupported_features_leave_a_sticky_explanation() {
        let (mut app, _commands) = headless_app();
        app.handle_event(Event::Unsupported {
            what: domain::Unsupported::Forums,
            detail: None,
        });
        let notice = app.unsupported.as_deref().unwrap_or_default();
        assert!(notice.contains("ROADMAP"), "{notice}");
    }

    #[test]
    fn outgoing_commands_reach_the_session_channel() {
        let (mut app, mut commands) = headless_app();
        app.send_command(Command::GetServers);
        match commands.try_recv() {
            Ok(Command::GetServers) => {}
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn check_flag_finds_its_instance() {
        let args = |list: &[&str]| list.iter().map(|value| value.to_string()).collect::<Vec<_>>();
        assert_eq!(
            check_target_in(&args(&["--check", "har.mony.lol"])),
            Some("har.mony.lol".to_string())
        );
        assert_eq!(
            check_target_in(&args(&["--instance", "x", "--check=localhost:3002"])),
            Some("localhost:3002".to_string())
        );
        assert_eq!(check_target_in(&args(&["--check"])), None);
        assert_eq!(check_target_in(&args(&["--version"])), None);
    }

    #[test]
    fn transport_names_parse() {
        assert_eq!(parse_transport("web"), TransportKind::Web);
        assert_eq!(parse_transport("SUPABASE"), TransportKind::Web);
        assert_eq!(parse_transport("bot"), TransportKind::Bot);
        assert_eq!(parse_transport("anything else"), TransportKind::Bot);
    }

    #[test]
    fn empty_credentials_do_not_make_a_login_request() {
        let startup = Startup {
            instance: Some("har.mony.lol".to_string()),
            transport: Some(TransportKind::Bot),
            ..Startup::default()
        };
        assert!(startup.login_request().is_none());
    }

    #[test]
    fn bot_login_needs_only_a_token() {
        let startup = Startup {
            instance: Some("har.mony.lol".to_string()),
            token: Some("harmony_bot_abc".to_string()),
            server_ids: "1e7d0000-0000-0000-0000-000000000004".to_string(),
            ..Startup::default()
        };
        let request = startup.login_request().expect("request");
        assert_eq!(request.transport, TransportKind::Bot);
        assert_eq!(request.server_ids.len(), 1);
        assert!(request.email.is_none());
    }

    #[test]
    fn web_login_needs_an_email_and_password() {
        let startup = Startup {
            instance: Some("har.mony.lol".to_string()),
            email: Some("a@b.c".to_string()),
            password: Some("secret".to_string()),
            ..Startup::default()
        };
        let request = startup.login_request().expect("request");
        assert_eq!(request.transport, TransportKind::Web);
    }
}
