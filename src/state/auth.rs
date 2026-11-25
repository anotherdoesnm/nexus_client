use crate::domain::{TransportKind, User};

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    /// The instance address (host or URL).
    LoginUsername,
    /// The bot token, or the account password on the web transport.
    LoginPassword,
    /// The account email, web transport only.
    LoginEmail,
    AuthSubmit,
    AuthSwitch,
    /// Switch between the bot-gateway and web-app protocols.
    AuthTransport,
    /// The "paste a session" button.
    AuthSession,
    /// The session field itself (JSON from the web app, or a bare token).
    LoginSession,
    /// The register screen's fields (accounts are made on the instance, so this
    /// screen explains rather than submits).
    RegisterUsername,
    RegisterPassword,
    NewThreadTitle,
    NewThreadContent,
    NewPostContent,
    NewForumName,
    NewForumDescription,
    UpdatePassword,
}

/// State management for signing in to a Harmony instance.
pub struct AuthState {
    pub current_user: Option<User>,
    /// Instance address; reused by the register screen.
    pub current_input: String,
    /// Secret: a bot token or an account password.
    pub password_input: String,
    /// Account email (web transport).
    pub email_input: String,
    /// A session imported from the web app, for SSO accounts.
    pub session_input: String,
    /// Ctrl-R: show the secret instead of masking it.
    pub reveal_secret: bool,
    pub transport: TransportKind,
    pub input_mode: Option<InputMode>,
}

impl Default for AuthState {
    fn default() -> Self {
        Self {
            current_user: None,
            current_input: String::new(),
            password_input: String::new(),
            email_input: String::new(),
            session_input: String::new(),
            reveal_secret: false,
            transport: TransportKind::Bot,
            input_mode: Some(InputMode::LoginUsername),
        }
    }
}

impl AuthState {
    pub fn is_logged_in(&self) -> bool {
        self.current_user.is_some()
    }

    pub fn login(&mut self, user: User) {
        self.current_user = Some(user);
        self.clear_inputs();
    }

    pub fn logout(&mut self) {
        self.current_user = None;
        self.clear_inputs();
        self.input_mode = Some(InputMode::LoginUsername);
    }

    pub fn clear_inputs(&mut self) {
        self.current_input.clear();
        self.password_input.clear();
        self.email_input.clear();
        self.session_input.clear();
    }

    pub fn set_input_mode(&mut self, mode: InputMode) {
        self.input_mode = Some(mode);
        self.clear_inputs();
    }

    /// The token field's label follows the transport.
    pub fn secret_label(&self) -> &'static str {
        match self.transport {
            TransportKind::Bot => "Bot token (harmony_bot_…)",
            TransportKind::Web => "Password",
        }
    }

    pub fn transport_label(&self) -> &'static str {
        match self.transport {
            TransportKind::Bot => "bot-gateway",
            TransportKind::Web => "web-app",
        }
    }

    pub fn toggle_transport(&mut self) {
        self.transport = match self.transport {
            TransportKind::Bot => TransportKind::Web,
            TransportKind::Web => TransportKind::Bot,
        };
    }
}
