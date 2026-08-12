use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::help::{HelpIndex, contextual_help, mode_label};
use super::{DownloadsRender, RenderModel};

pub fn render(frame: &mut Frame, model: &RenderModel) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(5),
            Constraint::Length(6),
        ])
        .split(frame.area());

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Profile: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(model.status.profile_name.as_str()),
        Span::raw("  |  Instance: "),
        Span::raw(model.status.instance_url.as_str()),
        Span::raw("  |  Network: "),
        Span::styled(network_label(model), network_style(model)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Session"));
    frame.render_widget(header, areas[0]);

    match &model.downloads {
        Some(downloads) => render_downloads(frame, areas.as_ref(), downloads),
        None => render_content(frame, areas.as_ref(), model),
    }

    let compose = Paragraph::new(if model.compose.is_empty() {
        "(empty compose buffer)".to_owned()
    } else {
        model.compose.clone()
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Compose buffer"),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(compose, areas[2]);

    let mut status_lines = vec![Line::from(vec![
        Span::styled(
            format!("Mode: {}", mode_label(model.mode)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::raw(status_message(model)),
    ])];
    if model.status.stale || model.status.retryable {
        status_lines.push(Line::from(
            "[STALE] Data may be out of date; refresh to retry.",
        ));
    }
    if model.status.confirmation_pending {
        status_lines.push(Line::from(
            "[PENDING] Confirmation required before network activity.",
        ));
    }
    if model.status.pending {
        status_lines.push(Line::from("[PENDING] Network activity in progress."));
    }
    if let Some(error) = &model.status.error {
        status_lines.push(Line::from(Span::styled(
            format!("ERROR: {error}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
    }
    let help = contextual_help(model.mode)
        .iter()
        .map(|item| format!("{} {}", item.key, item.action))
        .collect::<Vec<_>>()
        .join("  ");
    status_lines.push(Line::from(format!("Help: {help}")));
    let status = Paragraph::new(status_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Command / status"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(status, areas[3]);
}

fn render_content(frame: &mut Frame, areas: &[ratatui::layout::Rect], model: &RenderModel) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(areas[1]);

    if let Some(query) = &model.help {
        render_help(frame, body.as_ref(), query);
        return;
    }

    let mut post_items = model
        .posts
        .iter()
        .map(|post| ListItem::new(format!("{}  {}", post.id.0, post.title)))
        .collect::<Vec<_>>();
    if model.has_more {
        post_items.push(ListItem::new("… more posts available (load more)"));
    }
    let posts = post_items;
    let mut list_state = ListState::default();
    list_state.select(selected_index(model));
    let primary = List::new(posts)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(if model.search.is_empty() {
                    "Primary content"
                } else {
                    "Search results"
                }),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(primary, body[0], &mut list_state);

    let detail_text = match &model.detail {
        Some(detail) => {
            let mut lines = vec![Line::from(Span::styled(
                format!("Post {}: {}", detail.post.id.0, detail.post.title),
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            if let Some(body) = &detail.post.body {
                lines.push(Line::from(body.as_str()));
            }
            lines.push(Line::from(format!(
                "Thread comments: {}",
                detail.comments.len()
            )));
            lines.extend(detail.comments.iter().map(|comment| {
                Line::from(format!("Comment {}: {}", comment.id.0, comment.content))
            }));
            lines
        }
        None => vec![Line::from("No detail or thread selected")],
    };
    let detail = Paragraph::new(detail_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Detail / thread"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[1]);
}

fn render_help(frame: &mut Frame, body: &[ratatui::layout::Rect], query: &str) {
    let entries = HelpIndex::default().search(query);
    let items = entries
        .iter()
        .map(|entry| {
            ListItem::new(Line::from(vec![
                Span::styled(entry.command, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  —  "),
                Span::raw(entry.description),
            ]))
        })
        .collect::<Vec<_>>();
    let title = if query.is_empty() {
        "Help — all commands".to_owned()
    } else {
        format!("Help — \"{query}\"")
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, body[0], &mut ListState::default());

    let mut groups: Vec<&'static str> = Vec::new();
    for entry in &entries {
        if !groups.contains(&entry.group) {
            groups.push(entry.group);
        }
    }
    let lines = std::iter::once(Line::from("Searchable help"))
        .chain(std::iter::once(Line::from(format!(
            "{} matching command(s)",
            entries.len()
        ))))
        .chain(std::iter::once(Line::from("")))
        .chain(groups.iter().map(|group| Line::from(format!("• {group}"))))
        .collect::<Vec<_>>();
    let detail = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Help groups"))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[1]);
}

fn render_downloads(
    frame: &mut Frame,
    areas: &[ratatui::layout::Rect],
    downloads: &DownloadsRender,
) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(areas[1]);

    let items = downloads
        .records
        .iter()
        .map(|record| {
            let deleted = if record.local_file_deleted {
                " [file deleted]"
            } else {
                ""
            };
            ListItem::new(format!(
                "#{}  {}  [{}]{}",
                record.id.0, record.filename, record.status, deleted
            ))
        })
        .collect::<Vec<_>>();
    let mut list_state = ListState::default();
    list_state.select(
        downloads
            .records
            .iter()
            .position(|record| Some(record.id) == downloads.selected),
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Session downloads{}",
            if downloads.query.is_empty() {
                String::new()
            } else {
                format!(" — \"{}\"", downloads.query)
            }
        )))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, body[0], &mut list_state);

    let detail_lines = match downloads
        .records
        .iter()
        .find(|record| Some(record.id) == downloads.selected)
    {
        Some(record) => vec![
            Line::from(Span::styled(
                format!("#{} — {}", record.id.0, record.filename),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("Status: {}", record.status)),
            Line::from(format!("Source: {}", record.media.url)),
            Line::from(format!(
                "MIME: {}",
                record.mime_type.as_deref().unwrap_or("unknown")
            )),
            Line::from(format!(
                "Profile: {}  |  Instance: {}",
                record.profile.0, record.instance_url
            )),
            Line::from(format!("Requested: {}", record.requested_at)),
            Line::from(format!("Local path: {}", record.local_path.display())),
        ],
        None => vec![Line::from("No download selected")],
    };
    let detail = Paragraph::new(detail_lines)
        .block(Block::default().borders(Borders::ALL).title("Download"))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[1]);
}

fn selected_index(model: &RenderModel) -> Option<usize> {
    model.selected.filter(|index| *index < model.posts.len())
}

fn status_message(model: &RenderModel) -> &str {
    if model.status.message.is_empty() {
        "Ready"
    } else {
        model.status.message.as_str()
    }
}

fn network_label(model: &RenderModel) -> &'static str {
    if model.status.pending {
        "PENDING"
    } else if model.status.error.is_some() {
        "ERROR"
    } else {
        "READY"
    }
}

fn network_style(model: &RenderModel) -> Style {
    match network_label(model) {
        "ERROR" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "PENDING" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    }
}
