use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
};

use super::help::{HelpIndex, contextual_help, mode_label};
use super::{DownloadsRender, RenderModel};

/// Number of feed rows that fit the primary content pane for a given
/// terminal height: the vertical layout reserves 3 rows for the session
/// header, 5 for the compose buffer, and 6 for the command/status area, and
/// the primary table loses 2 rows to its border plus 1 to its column header.
/// Never fewer than one post so a fetch is never empty.
pub fn feed_limit_for_height(height: u16) -> u16 {
    height.saturating_sub(3 + 5 + 6 + 2 + 1).max(1)
}

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

    // Help wins over the downloads panel: `:help` while the panel is open
    // must still be visible (`render_content` shows help when `model.help`
    // is set); the panel reappears once help is closed with `Esc`.
    match &model.downloads {
        Some(downloads) if model.help.is_none() => {
            render_downloads(frame, areas.as_ref(), downloads)
        }
        _ => render_content(frame, areas.as_ref(), model),
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

    let mut post_rows = model
        .posts
        .iter()
        .map(|post| {
            Row::new(vec![
                Cell::from(post.score.to_string()),
                Cell::from(post.comments.to_string()),
                Cell::from(post.title.as_str()),
            ])
        })
        .collect::<Vec<_>>();
    if model.has_more {
        post_rows.push(Row::new(vec![
            Cell::from("…"),
            Cell::from(""),
            Cell::from("more posts available (> next page)"),
        ]));
    }
    let table = Table::new(
        post_rows,
        [
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec!["score", "comments", "title"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(if model.search.is_empty() {
                "Primary content"
            } else {
                "Search results"
            }),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
    .highlight_symbol("▶ ");
    let mut table_state = TableState::default();
    table_state.select(selected_index(model));
    frame.render_stateful_widget(table, body[0], &mut table_state);

    let detail_text = match &model.detail {
        Some(detail) => {
            let mut lines = vec![Line::from(Span::styled(
                detail.post.title.as_str(),
                Style::default().add_modifier(Modifier::BOLD),
            ))];
            if let Some(body) = &detail.post.body
                && !body.is_empty()
            {
                lines.push(Line::from(""));
                lines.push(Line::from(body.as_str()));
            }
            lines.push(Line::from(format!(
                "Thread comments: {}",
                detail.comments.len()
            )));
            // Blank lines separate comments; the score and author lead each
            // comment so long content can never push them off the pane, and
            // server-side ids stay out of the UI.
            for comment in &detail.comments {
                lines.push(Line::from(""));
                lines.push(Line::from(format!(
                    "[{}] {}:",
                    comment.score, comment.creator_name
                )));
                lines.push(Line::from(comment.content.as_str()));
            }
            lines
        }
        None => vec![Line::from("No detail or thread selected")],
    };
    // Clamp the scroll offset so a short thread (or a very long scroll) can
    // never leave blank space under the pane; wrapped lines are longer than
    // the line count, so reaching the absolute bottom of a deeply wrapped
    // comment may need one more Ctrl-d.
    let pane_lines = body[1].height.saturating_sub(2) as usize;
    let scroll = model
        .detail_scroll
        .min(detail_text.len().saturating_sub(pane_lines)) as u16;
    let detail = Paragraph::new(detail_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Detail / thread"),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Status,
        domain::{Profile, ProfileContext, ProfileId},
        input::Mode,
    };
    use ratatui::backend::TestBackend;
    use url::Url;

    fn model(help: Option<String>, downloads: bool) -> RenderModel {
        let context = ProfileContext {
            profile: Profile {
                id: ProfileId::from("fixture"),
                instance_url: Url::parse("http://127.0.0.1/").unwrap(),
                account_label: Some("fixture".into()),
            },
            session: None,
        };
        RenderModel {
            mode: Mode::Normal,
            posts: Vec::new(),
            selected: None,
            detail: None,
            compose: String::new(),
            search: String::new(),
            has_more: false,
            status: Status::ready(&context),
            downloads: downloads.then(|| DownloadsRender {
                query: String::new(),
                selected: None,
                records: Vec::new(),
            }),
            help,
            detail_scroll: 0,
        }
    }

    fn rendered(model: &RenderModel) -> String {
        rendered_at(model, 80, 24)
    }

    /// Render into a terminal of the given size so layout tests can verify
    /// trailing row metadata at a realistic pane size instead of a narrow
    /// 80-column split, and can see a full detail thread below the fold.
    fn rendered_at(model: &RenderModel, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, model))
            .expect("render into test backend");
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn feed_limit_scales_with_terminal_height() {
        assert_eq!(feed_limit_for_height(24), 7);
        assert_eq!(feed_limit_for_height(40), 23);
        assert_eq!(feed_limit_for_height(20), 3);
        assert_eq!(
            feed_limit_for_height(10),
            1,
            "tiny terminals still fetch one"
        );
    }

    #[test]
    fn feed_rows_show_score_and_comment_count_without_id() {
        let mut model = model(None, false);
        model.posts = vec![crate::api::PostView {
            id: crate::PostId(1),
            title: "Threaded post".into(),
            body: None,
            url: None,
            community_id: crate::CommunityId(1),
            creator_id: crate::UserId(1),
            score: 12,
            comments: 7,
            published: None,
        }];
        let text = rendered_at(&model, 140, 24);
        assert!(
            text.contains("score") && text.contains("comments") && text.contains("title"),
            "the feed table must carry a column header; rendered: {text}"
        );
        assert!(
            text.contains("Threaded post") && text.contains("12") && text.contains("7"),
            "the row must show the title with the score and comment count; rendered: {text}"
        );
        assert!(
            !text.contains("1  Threaded post") && !text.contains("Post 1:"),
            "the feed must not expose post ids; rendered: {text}"
        );
    }

    #[test]
    fn detail_shows_comment_scores_without_ids_and_with_spacing() {
        let mut model = model(None, false);
        model.detail = Some(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: Some("The body".into()),
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 12,
                comments: 2,
                published: None,
            },
            comments: vec![
                crate::api::CommentView {
                    id: crate::CommentId(10),
                    post_id: crate::PostId(1),
                    content: "A comment".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "alice".into(),
                    score: 3,
                },
                crate::api::CommentView {
                    id: crate::CommentId(11),
                    post_id: crate::PostId(1),
                    content: "Another comment".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "bob".into(),
                    score: -1,
                },
            ],
        });
        let text = rendered_at(&model, 140, 48);
        assert!(
            text.contains("[3] alice:") && text.contains("[-1] bob:"),
            "each comment must show its score and author; rendered: {text}"
        );
        assert!(
            text.contains("A comment") && text.contains("Another comment"),
            "comment content must render under its author line; rendered: {text}"
        );
        assert!(
            !text.contains("Comment 10:") && !text.contains("Comment 11:"),
            "comments must not expose their ids; rendered: {text}"
        );
        assert!(
            !text.contains("Post 1:"),
            "the detail header must not expose the post id; rendered: {text}"
        );
        let count = text.matches("Thread comments: 2").count();
        assert!(count >= 1, "detail must still report the thread size");
    }

    #[test]
    fn detail_scroll_shifts_content_above_the_fold() {
        let mut model = model(None, false);
        let comments = (0..8)
            .map(|index| crate::api::CommentView {
                id: crate::CommentId(index + 1),
                post_id: crate::PostId(1),
                content: format!("comment number {index}"),
                creator_id: crate::UserId(2),
                creator_name: "alice".into(),
                score: index,
            })
            .collect();
        model.detail = Some(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: Some("The body".into()),
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 12,
                comments: 8,
                published: None,
            },
            comments,
        });
        let at_top = rendered_at(&model, 140, 24);
        assert!(
            at_top.contains("Threaded post"),
            "the thread title is visible at the top"
        );
        model.detail_scroll = 40;
        let scrolled = rendered_at(&model, 140, 24);
        assert!(
            !scrolled.contains("Threaded post"),
            "scrolling down moves the title above the fold"
        );
        assert!(
            scrolled.contains("comment number 7"),
            "later comments become reachable after scrolling"
        );
    }

    #[test]
    fn help_renders_above_open_downloads_panel() {
        let text = rendered(&model(Some("profile".into()), true));
        assert!(
            text.contains("Help — \"profile\""),
            "open help must be visible while the downloads panel is open"
        );
        assert!(
            !text.contains("Session downloads"),
            "the downloads panel must not cover open help"
        );
    }

    #[test]
    fn downloads_panel_renders_when_help_is_closed() {
        let text = rendered(&model(None, true));
        assert!(
            text.contains("Session downloads"),
            "closing help must restore the downloads panel"
        );
    }
}
