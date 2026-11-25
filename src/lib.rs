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
            server_ids: std::env::var("HARMONY_SERVERS").unwrap_or_default(),
        };

        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--instance" | "-i" | "--token" | "-t" | "--email" | "-e" | "--password" | "-p"
                | "--servers" | "-s" => {
                    if let Some(value) = args.get(index + 1).cloned() {
                        match arg {
                            "--instance" | "-i" => startup.instance = Some(value),
                            "--token" | "-t" => startup.token = Some(value),
                            "--email" | "-e" => startup.email = Some(value),
                            "--password" | "-p" => startup.password = Some(value),
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
            if self.email.is_some() && self.password.is_some() && self.token.is_none() {
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
                    server_ids: harmony::transport::parse_server_ids(&self.server_ids),
                })
            }
            TransportKind::Web => {
                let email = self.email.clone()?;
                let password = self.password.clone()?;
                Some(LoginRequest {
                    instance,
                    transport,
                    token: None,
                    email: Some(email),
                    password: Some(password),
                    server_ids: Vec::new(),
                })
            }
        }
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

/// Run the client until the user quits.
pub async fn run() -> Result<(), Box<dyn Error>> {
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
