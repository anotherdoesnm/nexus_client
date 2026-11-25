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
    let outer_block = Block::default().title(title).borders(Borders::ALL);
    f.render_widget(outer_block, area);

    let mut constraints = vec![Constraint::Length(3), Constraint::Length(3)];
    if app.auth.transport == TransportKind::Web {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Length(3));
    if app.auth.transport == TransportKind::Web {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Min(1));

    let chunks = Layout::default()
        .margin(2)
        .constraints(constraints)
        .split(area);

    let mut row = 0;

    let instance_style = if matches!(app.auth.input_mode, Some(InputMode::LoginUsername)) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(app.auth.current_input.as_str())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Instance (har.mony.lol)"),
            )
            .style(instance_style),
        chunks[row],
    );
    row += 1;

    let mut email_row: Option<usize> = None;
    if app.auth.transport == TransportKind::Web {
        let email_style = if matches!(app.auth.input_mode, Some(InputMode::LoginEmail)) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        f.render_widget(
            Paragraph::new(app.auth.email_input.as_str())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Account email"),
                )
                .style(email_style),
            chunks[row],
        );
        email_row = Some(row);
        row += 1;
    }

    let secret_style = if matches!(app.auth.input_mode, Some(InputMode::LoginPassword)) {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new("*".repeat(app.auth.password_input.len()))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{}  (Ctrl-R reveals)", app.auth.secret_label())),
            )
            .style(secret_style),
        chunks[row],
    );
    let secret_row = row;
    row += 1;

    let button_area = Layout::default()
        .margin(1)
        .constraints([Constraint::Length(3)])
        .split(chunks[row])[0];
    let button_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(26),
            Constraint::Percentage(26),
            Constraint::Percentage(24),
            Constraint::Percentage(24),
        ])
        .split(button_area);

    let submit_style = if matches!(app.auth.input_mode, Some(InputMode::AuthSubmit)) {
        Style::default().bg(Color::Cyan).fg(Color::Black)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(Span::styled("[ SIGN IN ]", submit_style)).alignment(Alignment::Center),
        button_chunks[0],
    );

    let transport_style = if matches!(app.auth.input_mode, Some(InputMode::AuthTransport)) {
        Style::default().bg(Color::Green).fg(Color::Black)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("[ {} ⇄ ]", app.auth.transport_label()),
            transport_style,
        ))
        .alignment(Alignment::Center),
        button_chunks[1],
    );

    let session_style = if matches!(app.auth.input_mode, Some(InputMode::AuthSession)) {
        Style::default().bg(Color::Yellow).fg(Color::Black)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(Span::styled("[ SSO session ]", session_style))
            .alignment(Alignment::Center),
        button_chunks[2],
    );

    let switch_style = if matches!(app.auth.input_mode, Some(InputMode::AuthSwitch)) {
        Style::default().bg(Color::Magenta).fg(Color::Black)
    } else {
        Style::default()
    };
    f.render_widget(
        Paragraph::new(Span::styled("[ Create account ]", switch_style))
            .alignment(Alignment::Center),
        button_chunks[3],
    );

    if app.auth.transport == TransportKind::Web {
        let session_style = if matches!(app.auth.input_mode, Some(InputMode::LoginSession)) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        let masked = if app.auth.session_input.is_empty() {
            String::new()
        } else {
            format!("<{} characters pasted>", app.auth.session_input.len())
        };
        f.render_widget(
            Paragraph::new(masked)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("SSO session: sb-…-auth-token JSON, or an access token"),
                )
                .style(session_style),
            chunks[row],
        );
        row += 1;
    }

    let hint = match app.auth.transport {
        TransportKind::Bot => {
            "Bot token: instance → User settings → My bots. Server ids: --servers <uuid,uuid>."
        }
        TransportKind::Web => "Your own account: email and password, as on the instance's web app.",
    };
    let hint_area = chunks[row.saturating_add(1).min(chunks.len() - 1)];
    let hint_text = match app.startup.instance.as_deref() {
        Some(startup) => format!("{hint}  Starting instance: {startup}"),
        None => hint.to_string(),
    };
    let lines = if let Some(error) = app.auth_error.as_deref() {
        // Keep the failure on screen: the fields are not cleared, so the user
        // can fix a typo and retry.
        vec![
            ratatui::text::Line::from(Span::styled(
                format!("Sign-in failed: {error}"),
                Style::default().fg(Color::Red),
            )),
            ratatui::text::Line::from(""),
            ratatui::text::Line::from(Span::styled(hint_text, Style::default().fg(Color::Gray))),
        ]
    } else {
        vec![ratatui::text::Line::from(Span::styled(
            hint_text,
            Style::default(),
        ))]
    };
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(ratatui::widgets::Wrap { trim: true }),
        hint_area,
    );

    match app.auth.input_mode {
        Some(InputMode::LoginUsername) => f.set_cursor_position((
            chunks[0].x + app.auth.current_input.len() as u16 + 1,
            chunks[0].y + 1,
        )),
        Some(InputMode::LoginPassword) => f.set_cursor_position((
            chunks[secret_row].x + app.auth.password_input.len() as u16 + 1,
            chunks[secret_row].y + 1,
        )),
        Some(InputMode::LoginEmail) => f.set_cursor_position((
            chunks[email_row.unwrap_or(1)].x + app.auth.email_input.len() as u16 + 1,
            chunks[email_row.unwrap_or(1)].y + 1,
        )),
        Some(InputMode::LoginSession) => f.set_cursor_position((
            chunks[secret_row + 2].x + 1,
            chunks[secret_row + 2].y + 1,
        )),
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
