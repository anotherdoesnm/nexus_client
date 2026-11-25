//! Sign-in input. The flow is Harmony's: an instance address plus whichever
//! credential the chosen transport takes — a bot token, or an account's email
//! and password.

use crate::app::App;
use crate::domain::{Command, LoginRequest, TransportKind};
use crate::sound::SoundType;
use crossterm::event::{KeyCode, KeyEvent};

fn login_focus(transport: TransportKind) -> Vec<crate::state::InputMode> {
    use crate::state::InputMode;
    // The identity comes before the secret: instance, then (for the web
    // transport) the email, then the password or token.
    let mut fields = vec![InputMode::LoginUsername];
    if transport == TransportKind::Web {
        fields.push(InputMode::LoginEmail);
    }
    fields.push(InputMode::LoginPassword);
    fields.extend([InputMode::AuthTransport, InputMode::AuthSubmit, InputMode::AuthSwitch]);
    fields
}

fn register_focus() -> Vec<crate::state::InputMode> {
    use crate::state::InputMode;
    vec![
        InputMode::RegisterUsername,
        InputMode::RegisterPassword,
        InputMode::AuthSubmit,
        InputMode::AuthSwitch,
    ]
}

/// Handle authentication input (login/register)
pub fn handle_auth_input(key: KeyEvent, app: &mut App) {
    let is_login = app.ui.mode == crate::state::AppMode::Login;
    let focus = if is_login {
        login_focus(app.auth.transport)
    } else {
        register_focus()
    };

    match key.code {
        KeyCode::Char(c) => match app.auth.input_mode {
            Some(crate::state::InputMode::LoginUsername)
            | Some(crate::state::InputMode::RegisterUsername) => app.auth.current_input.push(c),
            Some(crate::state::InputMode::LoginPassword)
            | Some(crate::state::InputMode::RegisterPassword) => app.auth.password_input.push(c),
            Some(crate::state::InputMode::LoginEmail) => app.auth.email_input.push(c),
            _ => {}
        },
        KeyCode::Backspace => match app.auth.input_mode {
            Some(crate::state::InputMode::LoginUsername)
            | Some(crate::state::InputMode::RegisterUsername) => {
                app.auth.current_input.pop();
            }
            Some(crate::state::InputMode::LoginPassword)
            | Some(crate::state::InputMode::RegisterPassword) => {
                app.auth.password_input.pop();
            }
            Some(crate::state::InputMode::LoginEmail) => {
                app.auth.email_input.pop();
            }
            _ => {}
        },
        KeyCode::Tab | KeyCode::BackTab => {
            let current = focus
                .iter()
                .position(|mode| Some(mode) == app.auth.input_mode.as_ref())
                .unwrap_or(0);
            let next = if key.code == KeyCode::Tab {
                (current + 1) % focus.len()
            } else {
                (current + focus.len() - 1) % focus.len()
            };
            app.auth.input_mode = Some(focus[next].clone());
        }
        KeyCode::Enter => match app.auth.input_mode {
            Some(crate::state::InputMode::LoginUsername) => {
                let next = if app.auth.transport == TransportKind::Web {
                    crate::state::InputMode::LoginEmail
                } else {
                    crate::state::InputMode::LoginPassword
                };
                app.auth.input_mode = Some(next);
            }
            Some(crate::state::InputMode::LoginEmail) => {
                app.auth.input_mode = Some(crate::state::InputMode::LoginPassword);
            }
            Some(crate::state::InputMode::LoginPassword) => {
                app.auth.input_mode = Some(crate::state::InputMode::AuthSubmit);
            }
            Some(crate::state::InputMode::RegisterUsername) => {
                app.auth.input_mode = Some(crate::state::InputMode::RegisterPassword);
            }
            Some(crate::state::InputMode::RegisterPassword) => {
                app.auth.input_mode = Some(crate::state::InputMode::AuthSubmit);
            }
            Some(crate::state::InputMode::AuthTransport) => {
                app.auth.toggle_transport();
                app.auth.input_mode = Some(crate::state::InputMode::LoginPassword);
            }
            Some(crate::state::InputMode::AuthSubmit) => {
                if is_login {
                    submit_login(app);
                } else {
                    let username = app.auth.current_input.clone();
                    let password = app.auth.password_input.clone();
                    if username.is_empty() || password.is_empty() {
                        app.set_notification("Fields cannot be empty.", None, false);
                        app.sound_manager.play(SoundType::LoginFailure);
                        return;
                    }
                    app.send_command(Command::Register { username, password });
                }
            }
            Some(crate::state::InputMode::AuthSwitch) => {
                app.auth.clear_inputs();
                if is_login {
                    app.ui.set_mode(crate::state::AppMode::Register);
                    app.auth.input_mode = Some(crate::state::InputMode::RegisterUsername);
                } else {
                    app.ui.set_mode(crate::state::AppMode::Login);
                    app.auth.input_mode = Some(crate::state::InputMode::LoginUsername);
                }
            }
            _ => {}
        },
        KeyCode::Esc => {
            app.ui.quit();
        }
        _ => {}
    }
}

/// Validate the fields and hand the session a login request.
fn submit_login(app: &mut App) {
    let instance = app.auth.current_input.trim().to_string();
    if instance.is_empty() {
        app.set_notification("Enter the instance address first.", None, false);
        app.sound_manager.play(SoundType::LoginFailure);
        return;
    }

    let transport = app.auth.transport;
    let secret = app.auth.password_input.trim().to_string();
    let email = app.auth.email_input.trim().to_string();

    let request = match transport {
        TransportKind::Bot => {
            if secret.is_empty() {
                app.set_notification("Paste a bot token (User settings → My bots).", None, false);
                app.sound_manager.play(SoundType::LoginFailure);
                return;
            }
            LoginRequest {
                instance,
                transport,
                token: Some(secret),
                email: None,
                password: None,
                session: None,
                server_ids: crate::harmony::transport::parse_server_ids(&app.startup.server_ids),
            }
        }
        TransportKind::Web => {
            if email.is_empty() || secret.is_empty() {
                app.set_notification(
                    "The web transport needs the account email and password.",
                    None,
                    false,
                );
                app.sound_manager.play(SoundType::LoginFailure);
                return;
            }
            LoginRequest {
                instance,
                transport,
                token: None,
                email: Some(email),
                password: Some(secret),
                session: None,
                server_ids: Vec::new(),
            }
        }
    };

    app.set_notification(
        match transport {
            TransportKind::Bot => "Connecting to the bot gateway…",
            TransportKind::Web => "Signing in through the web app's protocol…",
        },
        Some(2000),
        true,
    );
    app.send_command(Command::Login(request));
    app.auth.clear_inputs();
}
