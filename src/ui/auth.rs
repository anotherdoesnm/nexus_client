//! Sign-in screens. The client speaks Harmony's protocols, so what is asked for
//! is an instance and a credential: a bot token for the bot gateway, or an
//! account email and password for the web app's protocol.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Span,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use crate::app::{App, InputMode};
use crate::domain::TransportKind;

pub fn draw_login(f: &mut Frame, app: &mut App, area: Rect) {
    let title = format!(
        "Sign in to a Harmony instance — {}",
        app.auth.transport_label()
    );
    f.render_widget(
        Block::default().title(title).borders(Borders::ALL),
        area,
    );

    let web = app.auth.transport == TransportKind::Web;
    let mut constraints = vec![Constraint::Length(3)]; // instance
    if web {
        constraints.push(Constraint::Length(3)); // email
    }
    constraints.push(Constraint::Length(3)); // password / token
    constraints.push(Constraint::Length(3)); // buttons
    if web {
        constraints.push(Constraint::Length(3)); // imported session
    }
    constraints.push(Constraint::Min(1)); // hint / failure

    let chunks = Layout::default()
        .margin(2)
        .constraints(constraints)
        .split(area);

    let mut index = 0;
    let focused = |mode: InputMode| {
        if app.auth.input_mode == Some(mode) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        }
    };

    f.render_widget(
        Paragraph::new(app.auth.current_input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Instance (har.mony.lol)"),
            )
            .style(focused(InputMode::LoginUsername)),
        chunks[index],
    );
    let instance_row = index;
    index += 1;

    let mut email_row = None;
    if web {
        f.render_widget(
            Paragraph::new(app.auth.email_input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Account email"))
                .style(focused(InputMode::LoginEmail)),
            chunks[index],
        );
        email_row = Some(index);
        index += 1;
    }

    let secret_text = if app.auth.reveal_secret {
        app.auth.password_input.clone()
    } else {
        "*".repeat(app.auth.password_input.len())
    };
    f.render_widget(
        Paragraph::new(secret_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{}  (Ctrl-R reveals)", app.auth.secret_label())),
            )
            .style(focused(InputMode::LoginPassword)),
        chunks[index],
    );
    let secret_row = index;
    index += 1;

    // Buttons: sign in, transport, imported session, account creation.
    let buttons = Layout::horizontal([
        Constraint::Percentage(26),
        Constraint::Percentage(26),
        Constraint::Percentage(24),
        Constraint::Percentage(24),
    ])
    .split(chunks[index]);
    let style_for = |mode: InputMode| {
        if app.auth.input_mode == Some(mode) {
            Style::default().bg(Color::Cyan).fg(Color::Black)
        } else {
            Style::default()
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled("[ SIGN IN ]", style_for(InputMode::AuthSubmit)))
            .alignment(Alignment::Center),
        buttons[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("[ {} ⇄ ]", app.auth.transport_label()),
            style_for(InputMode::AuthTransport),
        ))
        .alignment(Alignment::Center),
        buttons[1],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "[ SSO session ]",
            style_for(InputMode::AuthSession),
        ))
        .alignment(Alignment::Center),
        buttons[2],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "[ Create account ]",
            style_for(InputMode::AuthSwitch),
        ))
        .alignment(Alignment::Center),
        buttons[3],
    );
    index += 1;

    let mut session_row = None;
    if web {
        let masked = if app.auth.session_input.is_empty() {
            String::new()
        } else {
            format!("<{} characters>", app.auth.session_input.len())
        };
        f.render_widget(
            Paragraph::new(masked)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("SSO session: sb-…-auth-token JSON, or an access token"),
                )
                .style(focused(InputMode::LoginSession)),
            chunks[index],
        );
        session_row = Some(index);
        index += 1;
    }

    let hint = match app.auth.transport {
        TransportKind::Bot => {
            "Bot token: instance → User settings → My bots. Server ids: --servers <uuid,uuid>."
        }
        TransportKind::Web => "Your own account: email and password, as on the instance's web app.",
    };
    let hint_text = match app.startup.instance.as_deref() {
        Some(startup) => format!("{hint}  Starting instance: {startup}"),
        None => hint.to_string(),
    };
    let lines = if let Some(error) = app.auth_error.as_deref() {
        vec![
            ratatui::text::Line::from(Span::styled(
                format!("Sign-in failed: {error}"),
                Style::default().fg(Color::Red),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(Span::styled(
                hint_text,
                Style::default().fg(Color::Gray),
            )),
        ]
    } else {
        vec![ratatui::text::Line::from(Span::styled(hint_text, Style::default()))]
    };
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        chunks[index],
    );

    match app.auth.input_mode {
        Some(InputMode::LoginUsername) => f.set_cursor_position((
            chunks[instance_row].x + app.auth.current_input.len() as u16 + 1,
            chunks[instance_row].y + 1,
        )),
        Some(InputMode::LoginEmail) => {
            let row = email_row.unwrap_or(secret_row);
            f.set_cursor_position((
                chunks[row].x + app.auth.email_input.len() as u16 + 1,
                chunks[row].y + 1,
            ))
        }
        Some(InputMode::LoginPassword) => f.set_cursor_position((
            chunks[secret_row].x + app.auth.password_input.len() as u16 + 1,
            chunks[secret_row].y + 1,
        )),
        Some(InputMode::LoginSession) => {
            let row = session_row.unwrap_or(secret_row);
            f.set_cursor_position((chunks[row].x + 1, chunks[row].y + 1))
        }
        _ => {}
    }
}

pub fn draw_register(f: &mut Frame, app: &mut App, area: Rect) {
    let outer_block = Block::default()
        .title("Create an account")
        .borders(Borders::ALL);
    f.render_widget(outer_block, area);

    let chunks = Layout::default()
        .margin(2)
        .constraints([Constraint::Min(3)])
        .split(area);

    let text = vec![
        ratatui::text::Line::from(Span::styled(
            "Accounts are created by the Harmony instance, not by this client.",
            Style::default().fg(Color::Yellow),
        )),
        ratatui::text::Line::from(""),
        ratatui::text::Line::from(format!(
            "Open https://{} in a browser, sign up there, then either",
            app.auth
                .current_input
                .trim()
                .trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string()
                .as_str()
                .to_owned()
                .as_str()
        )),
        ratatui::text::Line::from(
            "create a bot token (User settings → My bots) to sign in here,",
        ),
        ratatui::text::Line::from(
            "or use the web-app transport with that account's email and password.",
        ),
        ratatui::text::Line::from(""),
        ratatui::text::Line::from(Span::styled(
            "[ Esc ] back to sign-in",
            Style::default().fg(Color::Cyan),
        )),
    ];
    f.render_widget(
        Paragraph::new(text).alignment(Alignment::Left),
        chunks[0],
    );
}
