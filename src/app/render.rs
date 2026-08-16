use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
};

use super::help::{HelpIndex, contextual_help, mode_label};
use super::state::{AppColors, CommunitiesModal, HelpModal, Modal, ThreadModal};
use super::thread::CommentTree;
use crate::api::CommentView;
use super::{DownloadsRender, RenderModel};

/// Largest feed size the Lemmy API accepts: `post/list` rejects any `limit`
/// above 50 with a confusing `couldnt_get_posts` error instead of clamping,
/// so the adaptive size must never exceed it.
pub const MAX_FEED_LIMIT: u16 = 50;

/// Number of feed rows that fit the primary content pane for a given
/// terminal height: the vertical layout reserves 3 rows for the session
/// header, 5 for the compose buffer, and 6 for the command/status area, and
/// the primary table loses 2 rows to its border plus 1 to its column header.
/// Never fewer than one post so a fetch is never empty, and never more than
/// the server's maximum so a tall terminal cannot trigger a 400.
pub fn feed_limit_for_height(height: u16) -> u16 {
    height
        .saturating_sub(3 + 5 + 6 + 2 + 1)
        .clamp(1, MAX_FEED_LIMIT)
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

    // The downloads panel replaces the content; modals then float over it.
    match &model.downloads {
        Some(downloads) => render_downloads(frame, areas.as_ref(), downloads),
        None => render_content(frame, areas[1], model),
    }

    // Every modal is a centered overlay drawn on top of the content, bottom
    // of the stack first so the focused (last) modal renders on top. The
    // depth is shown in the title once there is more than one.
    let depth = model.modals.len();
    for (index, modal) in model.modals.iter().enumerate() {
        let suffix = if depth > 1 {
            format!(" ({}/{depth})", index + 1)
        } else {
            String::new()
        };
        match modal {
            Modal::Thread(thread) => render_thread(frame, areas[1], thread, &suffix, &model.colors),
            Modal::Communities(communities) => {
                render_communities(frame, areas[1], communities, &suffix, &model.colors)
            }
            Modal::Help(help) => render_help(frame, areas[1], help, &suffix, &model.colors),
        }
    }

    let compose = Paragraph::new(if model.compose.is_empty() {
        "(empty compose buffer)".to_owned()
    } else {
        mask_login_password(&model.compose)
    })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Compose buffer"),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(compose, areas[2]);

    let status_message = crate::text::clean_text(status_message(model));
    let mut status_lines = vec![Line::from(vec![
        Span::styled(
            format!("Mode: {}", mode_label(model.mode)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  |  "),
        Span::raw(status_message.as_str()),
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
            format!("ERROR: {}", crate::text::clean_text(error)),
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

fn render_content(frame: &mut Frame, content: ratatui::layout::Rect, model: &RenderModel) {
    // The feed always takes the full content width; threads, the community
    // picker, and help are centered modals drawn over it afterwards.

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
    } else if model.loading {
        // A read load is in flight and nothing is on screen yet: show the
        // load instead of a blank pane (the [PENDING] status line covers
        // refreshes that keep the existing list visible).
        post_rows.push(Row::new(vec![
            Cell::from("…"),
            Cell::from(""),
            Cell::from("Loading…"),
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
    frame.render_stateful_widget(table, content, &mut table_state);
}

/// Center a modal box over the content pane and blank its rect so the
/// content underneath never shows through. `width` and `height` are tenths
/// and are always below 10: a floating modal keeps a visible margin on
/// every side (the feed shows around it), so it reads as an overlay above
/// the content, never a maximized pane. The clamps also cap the box below
/// the pane size, so a tiny terminal cannot stretch one to full size.
fn modal_area(content: ratatui::layout::Rect, width: u16, height: u16) -> ratatui::layout::Rect {
    let width = (content.width * width / 10)
        .max(40)
        .min(content.width.saturating_sub(2));
    let height = (content.height * height / 10)
        .max(10)
        .min(content.height.saturating_sub(2));
    let x = content.x + content.width.saturating_sub(width) / 2;
    let y = content.y + content.height.saturating_sub(height) / 2;
    ratatui::layout::Rect::new(x, y, width, height)
}

/// The thread modal's inner (usable) size for a terminal of the given
/// size: the 90%-of-content modal box (see `modal_area`) minus its
/// borders. The cursor-follow scroll math in `App` mirrors this so `j`/`k`
/// keep the focused comment on screen without duplicating layout.
pub fn thread_inner_size(terminal_width: u16, terminal_height: u16) -> (u16, u16) {
    let content_height = terminal_height.saturating_sub(3 + 5 + 6);
    let modal_width = (terminal_width * 9 / 10)
        .max(40)
        .min(terminal_width.saturating_sub(2));
    let modal_height = (content_height * 9 / 10)
        .max(10)
        .min(content_height.saturating_sub(2));
    (
        modal_width.saturating_sub(2),
        modal_height.saturating_sub(2),
    )
}

/// Paint a modal's surface and chrome (Clear, then a background-filled block
/// with accent borders and title) and return the interior rect plus the
/// surface style the content widget must carry so no holes show through.
fn modal_chrome(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    title: String,
    colors: &AppColors,
) -> (ratatui::layout::Rect, Style) {
    frame.render_widget(Clear, area);
    let surface = Style::default().fg(colors.text).bg(colors.surface);
    let accent = Style::default().fg(colors.accent);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(accent)
            .title(Span::styled(title, accent.add_modifier(Modifier::BOLD)))
            .style(surface),
        area,
    );
    (
        area.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        }),
        surface,
    )
}

/// The thread view: the full post and its comments in a large centered box.
fn render_thread(
    frame: &mut Frame,
    content: ratatui::layout::Rect,
    thread: &ThreadModal,
    depth: &str,
    colors: &AppColors,
) {
    let area = modal_area(content, 9, 9);
    let (inner, surface) = modal_chrome(
        frame,
        area,
        format!("Thread{depth} — j/k: move, z: toggle thread, Ctrl-d/u: scroll, Esc to close"),
        colors,
    );

    let detail = &thread.post;
    let tree = CommentTree::build(&detail.comments);
    let visible = tree.visible_indices(&thread.collapsed);
    let by_id: std::collections::HashMap<crate::CommentId, &CommentView> =
        detail.comments.iter().map(|comment| (comment.id, comment)).collect();

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
    let hidden = tree.rows.len().saturating_sub(visible.len());
    let header = if hidden == 0 {
        format!("Thread comments: {}", tree.rows.len())
    } else {
        format!("Thread comments: {} ({} hidden)", tree.rows.len(), hidden)
    };
    lines.push(Line::from(header));
    for &position in &visible {
        let row = &tree.rows[position];
        let comment = by_id[&row.id];
        let indent = (row.depth as usize * 2).min(16);
        let selected = Some(row.id) == thread.selected;
        let style = if selected {
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default()
        };
        let marker = if tree.has_replies(row.id) {
            let glyph = if thread.collapsed.contains(&row.id) { "▸" } else { "▾" };
            let noun = if row.reply_count == 1 { "reply" } else { "replies" };
            format!(" {glyph} {} {noun}", row.reply_count)
        } else {
            String::new()
        };
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(
                format!("[{}] {}:{marker}", comment.score, comment.creator_name),
                style,
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(indent + 2)),
            Span::raw(comment.content.as_str()),
        ]));
    }
    // Clamp the scroll offset so a short thread (or a very long scroll) can
    // never leave blank space under the box; wrapped lines are longer than
    // the line count, so reaching the absolute bottom of a deeply wrapped
    // comment may need one more Ctrl-d. Cursor-following is the movement
    // arms' job (`keep_thread_cursor_visible` in `App`): this render-side
    // clamp must NOT react to the cursor, or a manual Ctrl-d scroll would
    // snap back on the next frame.
    let scroll = thread
        .scroll
        .min(lines.len().saturating_sub(inner.height as usize)) as u16;
    let paragraph = Paragraph::new(lines)
        .style(surface)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, inner);
}

fn render_help(
    frame: &mut Frame,
    content: ratatui::layout::Rect,
    help: &HelpModal,
    depth: &str,
    colors: &AppColors,
) {
    // Help is one full column: command + description on a wrapped line
    // each, so long descriptions never run off the edge of a cramped side
    // pane. The group list is folded into the footer; `:help <group>`
    // still filters by it. The box floats at 90% — never full size.
    let area = modal_area(content, 9, 9);
    let title = if help.query.is_empty() {
        format!("Help — all commands{depth} (j/k: scroll, Esc: close)")
    } else {
        format!("Help — \"{}\"{depth} (j/k: scroll, Esc: close)", help.query)
    };
    let (inner, surface) = modal_chrome(frame, area, title, colors);

    let entries = HelpIndex::default().search(&help.query);
    let mut lines: Vec<Line> = Vec::with_capacity(entries.len() + 3);
    lines.push(Line::from(Span::styled(
        format!("{} matching command(s)", entries.len()),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for entry in &entries {
        lines.push(Line::from(vec![
            Span::styled(entry.command, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  —  "),
            Span::raw(entry.description),
        ]));
    }
    let mut groups: Vec<&'static str> = Vec::new();
    for entry in &entries {
        if !groups.contains(&entry.group) {
            groups.push(entry.group);
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Groups: {}", groups.join("  •  ")),
        Style::default().add_modifier(Modifier::DIM),
    )));

    // Clamp the scroll so a short index (or a long scroll) never leaves
    // blank space under the box; wrapped lines are taller than the line
    // count, so reaching the bottom may need one more j.
    let scroll = help
        .scroll
        .min(lines.len().saturating_sub(inner.height as usize)) as u16;
    let paragraph = Paragraph::new(lines)
        .style(surface)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, inner);
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
    // Wipe the pane first so a short record list does not leave stale feed
    // content visible below its last row.
    frame.render_widget(Clear, body[0]);
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
    frame.render_widget(Clear, body[1]);
    frame.render_widget(detail, body[1]);
}

/// Draw the community-list modal centered over the primary content pane.
/// The box is 3/4 of the pane's width and height, so the feed stays visible
/// around the edges while the list has room for its rows.
fn render_communities(
    frame: &mut Frame,
    content: ratatui::layout::Rect,
    modal: &CommunitiesModal,
    depth: &str,
    colors: &AppColors,
) {
    let area = modal_area(content, 7, 7);
    let listing = match modal.listing {
        crate::api::FeedListing::All => "All",
        crate::api::FeedListing::Local => "Local",
        crate::api::FeedListing::Subscribed => "Subscribed",
    };
    let title = format!("Communities — {listing}{depth} (j/k: move, Enter: open, Esc: close)");
    let (inner, surface) = modal_chrome(frame, area, title, colors);

    let mut lines: Vec<Line> = Vec::with_capacity(modal.communities.len());
    for (index, community) in modal.communities.iter().enumerate() {
        // Just the name and the subscriber count. A subscribed community gets
        // a glyph on the All/Local lists; on the Subscribed list every row is
        // subscribed, so the marker would be redundant noise.
        let marker =
            if modal.listing == crate::api::FeedListing::Subscribed || !community.subscribed {
                ""
            } else {
                "◉ "
            };
        let row = format!(
            "{marker}{}  ({} subs)",
            community.name, community.subscribers
        );
        let line = if Some(index) == modal.selected {
            Line::from(Span::styled(
                row,
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.accent)
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(row)
        };
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(Line::from(
            "(no communities yet — j/k to move, Enter to open, Esc to close)",
        ));
    }
    // Keep the selection visible, but never scroll content that already fits.
    let scroll = modal
        .selected
        .map(|selected| selected.saturating_sub(inner.height.saturating_sub(1) as usize))
        .unwrap_or_default() as u16;
    let paragraph = Paragraph::new(lines).style(surface).scroll((scroll, 0));
    frame.render_widget(paragraph, inner);
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
        "ERROR" => Style::default()
            .fg(model.colors.error)
            .add_modifier(Modifier::BOLD),
        "PENDING" => Style::default()
            .fg(model.colors.pending)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(model.colors.ready)
            .add_modifier(Modifier::BOLD),
    }
}

/// Render the compose buffer with the `:login` password masked. The password
/// is the third whitespace token (`:login <username> <password>`); it is
/// echoed as asterisks so an onlooker (shoulder-surfing, screen capture)
/// cannot read it while the user types. The buffer itself keeps the real
/// text — the masking is display-only and matches the whitespace token
/// splitting that `login_from_compose` performs. A leading `:` on the line
/// is optional and belongs to the first token.
fn mask_login_password(compose: &str) -> String {
    let command = compose
        .trim_start()
        .trim_start_matches(':')
        .split_whitespace()
        .next();
    if command != Some("login") {
        return compose.to_owned();
    }
    let mut token_index = 0usize;
    let mut at_token_start = true;
    let mut masked = String::with_capacity(compose.len());
    for character in compose.chars() {
        if character.is_whitespace() {
            at_token_start = true;
            masked.push(character);
        } else {
            if at_token_start {
                token_index += 1;
                at_token_start = false;
            }
            if token_index >= 3 {
                masked.push('*');
            } else {
                masked.push(character);
            }
        }
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Status,
        domain::{Profile, ProfileContext, ProfileId},
        input::Mode,
    };
    use ratatui::Terminal;
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
        let mut model = RenderModel {
            mode: Mode::Normal,
            posts: Vec::new(),
            selected: None,
            compose: String::new(),
            search: String::new(),
            has_more: false,
            loading: false,
            status: Status::ready(&context),
            downloads: downloads.then(|| DownloadsRender {
                query: String::new(),
                selected: None,
                records: Vec::new(),
            }),
            modals: Vec::new(),
            colors: AppColors::default(),
        };
        if let Some(query) = help {
            model.modals.push(Modal::Help(HelpModal::new(query)));
        }
        model
    }

    fn rendered(model: &RenderModel) -> String {
        rendered_at(model, 80, 24)
    }

    #[test]
    fn help_descriptions_wrap_instead_of_running_off_the_edge() {
        // Long descriptions must wrap onto continuation lines — the old
        // side-by-side List layout truncated them at the pane edge.
        let model = model(Some("communities".into()), false);
        let text = rendered_at(&model, 60, 48);
        // Wrapping breaks lines at spaces, so compare without spaces: the
        // description's tail must be present no matter where the wrap lands
        // (a truncated List would lose it entirely).
        let compact = text.replace(' ', "");
        assert!(
            compact.contains("switchesthelist"),
            "a wrapped help entry must show its full description, got: {text}"
        );
    }

    #[test]
    fn colors_survive_the_render_pipeline_to_the_terminal_writer() {
        // The full wire path: buffer -> diff -> crossterm backend -> bytes.
        // If colors were stripped anywhere past the buffer, this fails.
        #[derive(Clone)]
        struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for SharedSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut model = model(Some(String::new()), false);
        model.colors = AppColors {
            accent: Color::Red,
            ..AppColors::default()
        };
        // crossterm suppresses colors when NO_COLOR is set (memoized once);
        // the client forces its palette on at startup, so mirror that here or
        // this wire-level check would see an empty SGR on NO_COLOR machines.
        crossterm::style::Colored::set_ansi_color_disabled(false);
        let sink = SharedSink(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())));
        let backend = ratatui::backend::CrosstermBackend::new(sink.clone());
        let options = ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 140, 40)),
        };
        let mut terminal = ratatui::Terminal::with_options(backend, options).expect("terminal");
        let frame = terminal
            .draw(|frame| render(frame, &model))
            .expect("render");
        // The frame buffer must hold the accent on the modal's left border
        // (x=7 at 140 columns) and the surface on its interior.
        let border = &frame.buffer[(7, 5)];
        assert_eq!(
            border.fg,
            Color::Red,
            "the frame buffer must carry the accent fg on the border cell"
        );
        assert_eq!(
            frame.buffer[(70, 7)].bg,
            Color::DarkGray,
            "the frame buffer must carry the surface bg on the interior"
        );
        // And the accent must reach the writer as an SGR sequence (crossterm
        // 0.29 encodes named colors as 256-color indices): the full chain
        // buffer -> diff -> crossterm backend -> bytes.
        let bytes = sink.0.lock().unwrap().clone();
        assert!(
            bytes.windows(7).any(|window| window == b"\x1b[38;5;"),
            "the accent-colored border must emit an SGR foreground sequence; got: {:?}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(300)])
        );
    }

    #[test]
    fn palette_from_a_real_config_file_reaches_rendered_cells() {
        // The full chain: a config file on disk -> AppConfig::load ->
        // AppColors -> App::with_colors -> render model -> painted cells.
        let directory =
            std::env::temp_dir().join(format!("lemex-render-colors-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        std::fs::write(
            &path,
            "[colors]\naccent = \"red\"\nsurface = \"black\"\ntext = \"lightyellow\"\n[[profiles]]\nid = \"main\"\ninstance_url = \"https://example.test\"\n",
        )
        .unwrap();
        let config = crate::config::AppConfig::load(&path).expect("config loads");
        let mut model = model(Some(String::new()), false);
        model.colors = crate::app::AppColors::from_config(&config.colors);
        let text = rendered(&model);
        assert!(text.contains("Help"), "help modal still renders");
        // The colors themselves are asserted in custom_palette_reaches_rendered_cells;
        // here the point is the config file path resolves them.
        assert_eq!(
            model.colors.accent,
            Color::Red,
            "accent parsed from the file"
        );
        assert_eq!(model.colors.surface, Color::Black);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn custom_palette_reaches_rendered_cells() {
        let mut model = model(Some(String::new()), false);
        model.colors = AppColors {
            accent: Color::Red,
            surface: Color::Black,
            text: Color::LightYellow,
            ..AppColors::default()
        };
        let backend = TestBackend::new(140, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, &model))
            .expect("render help modal");
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer[(7, 5)].style().fg,
            Some(Color::Red),
            "the configured accent colors the border"
        );
        assert_eq!(
            buffer[(70, 7)].style().bg,
            Some(Color::Black),
            "the configured surface fills the interior"
        );
        assert_eq!(
            buffer[(70, 7)].style().fg,
            Some(Color::LightYellow),
            "the configured text color is used"
        );
    }

    #[test]
    fn help_modal_floats_centered_with_margins() {
        // Every modal must look like an overlay: centered, with the content
        // visible on all four sides — never touching the pane's edges.
        let model = model(Some(String::new()), false);
        let backend = TestBackend::new(140, 40);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, &model))
            .expect("render help modal");
        let buffer = terminal.backend().buffer();
        // Content pane is x=0..140, y=3..29 (height 26). The 90% box is
        // 126x23, centered: x=7..132, y=4..26.
        assert_eq!(buffer[(7, 5)].symbol(), "│", "the left margin is a border");
        assert_eq!(
            buffer[(132, 5)].symbol(),
            "│",
            "the right margin is a border"
        );
        // Outside the box the underlying feed is still visible (not blanked,
        // not covered).
        assert_eq!(buffer[(2, 5)].symbol(), " ", "content shows on the left");
        assert_eq!(buffer[(137, 5)].symbol(), " ", "content shows on the right");
        assert_eq!(
            buffer[(70, 3)].symbol(),
            "─",
            "the row above the box shows the feed's top border"
        );
        assert_eq!(
            buffer[(70, 27)].symbol(),
            " ",
            "content shows below the box"
        );
        // The modal carries its own colors: accent borders/title and a solid
        // surface, so the overlay is distinguishable from the content behind
        // it even on a theme where default text is dark.
        assert_eq!(
            buffer[(7, 5)].style().fg,
            Some(Color::Cyan),
            "borders use the modal accent"
        );
        assert_eq!(
            buffer[(70, 7)].style().bg,
            Some(Color::DarkGray),
            "the interior sits on the dark modal surface"
        );
        assert_eq!(
            buffer[(70, 7)].style().fg,
            Some(Color::White),
            "modal text is explicitly light for contrast on any theme"
        );
    }

    #[test]
    fn communities_modal_empty_space_does_not_show_content_through() {
        // Render a feed first, then open the modal over the same buffer:
        // cells inside the modal that the short community list does not
        // paint must be blank, not leftover feed content.
        let backend = TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("test backend");
        let feed_model = {
            let mut feed = model(None, false);
            feed.posts = (1..=8)
                .map(|index| crate::api::PostView {
                    id: crate::PostId(index),
                    title: format!("post {index} under the modal"),
                    body: None,
                    url: None,
                    community_id: crate::CommunityId(1),
                    creator_id: crate::UserId(1),
                    score: index,
                    comments: index,
                    published: None,
                })
                .collect();
            feed
        };
        terminal
            .draw(|frame| render(frame, &feed_model))
            .expect("render feed");
        let mut modal_model = feed_model.clone();
        modal_model
            .modals
            .push(Modal::Communities(CommunitiesModal {
                communities: vec![crate::api::CommunityView {
                    id: crate::CommunityId(1),
                    name: "main".into(),
                    title: None,
                    subscribers: 1,
                    subscribed: false,
                }],
                listing: crate::api::FeedListing::Local,
                selected: Some(0),
            }));
        terminal
            .draw(|frame| render(frame, &modal_model))
            .expect("render modal");
        let buffer = terminal.backend().buffer();
        // The modal spans y=3..13, x=10..70 at this size. A cell inside the
        // box below its single row is covered by the feed's title text
        // (painted at x≈18..38) in the first draw; with the fix it must be
        // blank, not the feed showing through.
        let probe = buffer[(20, 8)].symbol();
        assert_eq!(
            probe, " ",
            "modal empty space must be blank, not feed content ({probe:?})"
        );
    }

    #[test]
    fn communities_modal_renders_centered_with_selection() {
        let mut model = model(None, false);
        model.modals.push(Modal::Communities(CommunitiesModal {
            communities: vec![
                crate::api::CommunityView {
                    id: crate::CommunityId(1),
                    name: "main".into(),
                    title: Some("Main Community".into()),
                    subscribers: 1200,
                    subscribed: true,
                },
                crate::api::CommunityView {
                    id: crate::CommunityId(2),
                    name: "other".into(),
                    title: None,
                    subscribers: 34,
                    subscribed: false,
                },
            ],
            listing: crate::api::FeedListing::Local,
            selected: Some(1),
        }));
        let text = rendered(&model);
        assert!(
            text.contains("Communities — Local"),
            "the modal title shows the listing, got: {text}"
        );
        // Rows are just name + subscribers; the title is not shown.
        assert!(
            text.contains("main  (1200 subs)"),
            "the row shows name and subscribers, got: {text}"
        );
        assert!(
            !text.contains("Main Community"),
            "the community title must not clutter the row"
        );
        // On a non-subscribed list, subscribed communities carry a glyph.
        assert!(
            text.contains("◉ main"),
            "subscribed communities get a glyph on non-subscribed lists"
        );
        assert!(
            !text.contains("◉ other"),
            "unsubscribed communities get no glyph"
        );
        assert!(
            text.contains("other  (34 subs)"),
            "an unsubscribed community shows its name and count"
        );
    }

    #[test]
    fn subscribed_list_rows_carry_no_glyph() {
        let mut model = model(None, false);
        model.modals.push(Modal::Communities(CommunitiesModal {
            communities: vec![crate::api::CommunityView {
                id: crate::CommunityId(1),
                name: "main".into(),
                title: None,
                subscribers: 1200,
                subscribed: true,
            }],
            listing: crate::api::FeedListing::Subscribed,
            selected: Some(0),
        }));
        let text = rendered(&model);
        assert!(
            text.contains("main  (1200 subs)"),
            "the subscribed list still shows name and count"
        );
        assert!(
            !text.contains("◉"),
            "marking subscribed communities on the subscribed list is redundant"
        );
    }

    #[test]
    fn login_password_is_masked_in_the_compose_buffer() {
        assert_eq!(
            mask_login_password(":login alice s3cret"),
            ":login alice ******"
        );
        assert_eq!(
            mask_login_password(":login alice s3cret extra"),
            ":login alice ****** *****"
        );
        // The compose buffer holds the line without the `:` that entered
        // command mode; the mask must apply there too.
        assert_eq!(
            mask_login_password("login alice s3cret"),
            "login alice ******"
        );
    }

    #[test]
    fn login_masking_keeps_partial_and_unrelated_input_visible() {
        // The username is never masked, and there is nothing to mask until
        // the third token starts.
        assert_eq!(mask_login_password(":login alice"), ":login alice");
        assert_eq!(mask_login_password(":login ali"), ":login ali");
        // Non-login commands are echoed verbatim.
        assert_eq!(mask_login_password(":feed lemmy"), ":feed lemmy");
        assert_eq!(
            mask_login_password("not a login either"),
            "not a login either"
        );
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
    fn feed_limit_caps_at_the_server_maximum() {
        // Lemmy rejects `limit` above 50 with `couldnt_get_posts` instead of
        // clamping; a tall terminal must never send an over-limit request.
        assert_eq!(feed_limit_for_height(67), 50);
        assert_eq!(feed_limit_for_height(200), 50);
        assert_eq!(feed_limit_for_height(u16::MAX), 50);
        assert!(feed_limit_for_height(u16::MAX) <= MAX_FEED_LIMIT);
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
    fn thread_renders_only_as_a_modal() {
        let mut model = model(None, false);
        model.posts = vec![crate::api::PostView {
            id: crate::PostId(1),
            title: "Sole post".into(),
            body: None,
            url: None,
            community_id: crate::CommunityId(1),
            creator_id: crate::UserId(1),
            score: 12,
            comments: 7,
            published: None,
        }];
        let text = rendered(&model);
        assert!(
            !text.contains("Thread"),
            "no thread box without an open modal; rendered: {text}"
        );
        assert!(
            text.contains("Sole post"),
            "the feed must still render its posts; rendered: {text}"
        );

        model
            .modals
            .push(Modal::Thread(ThreadModal::new(crate::api::PostDetail {
                post: model.posts[0].clone(),
                comments: Vec::new(),
            })));
        let text = rendered(&model);
        assert!(
            text.contains("Thread"),
            "opening the thread must render the modal; rendered: {text}"
        );
    }

    #[test]
    fn thread_shows_comment_scores_without_ids_and_with_spacing() {
        let mut model = model(None, false);
        model
            .modals
            .push(Modal::Thread(ThreadModal::new(crate::api::PostDetail {
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
                        path: None,
                    },
                    crate::api::CommentView {
                        id: crate::CommentId(11),
                        post_id: crate::PostId(1),
                        content: "Another comment".into(),
                        creator_id: crate::UserId(2),
                        creator_name: "bob".into(),
                        score: -1,
                        path: None,
                    },
                ],
            })));
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
            "the thread header must not expose the post id; rendered: {text}"
        );
        let count = text.matches("Thread comments: 2").count();
        assert!(count >= 1, "the thread must still report its size");
    }

    #[test]
    fn thread_scroll_shifts_content_above_the_fold() {
        let mut model = model(None, false);
        let comments = (0..8)
            .map(|index| crate::api::CommentView {
                id: crate::CommentId(index + 1),
                post_id: crate::PostId(1),
                content: format!("comment number {index}"),
                creator_id: crate::UserId(2),
                creator_name: "alice".into(),
                score: index,
                path: None,
            })
            .collect();
        let mut thread = ThreadModal::new(crate::api::PostDetail {
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
        model.modals.push(Modal::Thread(thread.clone()));
        let at_top = rendered_at(&model, 140, 24);
        assert!(
            at_top.contains("Threaded post"),
            "the thread title is visible at the top"
        );
        thread.scroll = 40;
        model.modals.push(Modal::Thread(thread));
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
    fn help_floats_above_the_open_downloads_panel() {
        let text = rendered(&model(Some("profile".into()), true));
        assert!(
            text.contains("Help — \"profile\""),
            "open help must be visible while the downloads panel is open"
        );
        assert!(
            text.contains("Session downloads"),
            "the panel stays visible around the floating modal (it is content, not covered)"
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

    fn threaded_post() -> crate::api::PostDetail {
        crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 3,
                published: None,
            },
            comments: vec![
                crate::api::CommentView {
                    id: crate::CommentId(1),
                    post_id: crate::PostId(1),
                    content: "top".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "alice".into(),
                    score: 3,
                    path: Some("0.1".into()),
                },
                crate::api::CommentView {
                    id: crate::CommentId(2),
                    post_id: crate::PostId(1),
                    content: "reply".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "bob".into(),
                    score: -1,
                    path: Some("0.1.2".into()),
                },
                crate::api::CommentView {
                    id: crate::CommentId(3),
                    post_id: crate::PostId(1),
                    content: "nested reply".into(),
                    creator_id: crate::UserId(2),
                    creator_name: "carol".into(),
                    score: 0,
                    path: Some("0.1.2.3".into()),
                },
            ],
        }
    }

    #[test]
    fn thread_nests_replies_and_marks_threads() {
        let mut model = model(None, false);
        model
            .modals
            .push(Modal::Thread(ThreadModal::new(threaded_post())));
        let text = rendered_at(&model, 140, 48);
        assert!(
            text.contains("[3] alice: ▾ 2 replies"),
            "expanded thread marker with reply count; rendered: {text}"
        );
        assert!(
            text.contains("  [-1] bob: ▾ 1 reply"),
            "replies indent and carry their own marker; rendered: {text}"
        );
        assert!(
            text.contains("    [0] carol:"),
            "grandchildren indent deeper; rendered: {text}"
        );
        assert!(
            text.contains("Thread comments: 3"),
            "no hidden count when nothing is collapsed; rendered: {text}"
        );
    }

    #[test]
    fn collapsed_thread_hides_descendants_and_reports_hidden() {
        let mut model = model(None, false);
        let mut thread = ThreadModal::new(threaded_post());
        thread.collapsed.insert(crate::CommentId(1));
        thread.selected = Some(crate::CommentId(1));
        model.modals.push(Modal::Thread(thread));
        let text = rendered_at(&model, 140, 48);
        assert!(
            text.contains("[3] alice: ▸ 2 replies"),
            "collapsed marker; rendered: {text}"
        );
        assert!(
            !text.contains("nested reply") && !text.contains("reply"),
            "descendant content is hidden; rendered: {text}"
        );
        assert!(
            text.contains("Thread comments: 3 (2 hidden)"),
            "the header reports hidden replies; rendered: {text}"
        );
    }

    #[test]
    fn thread_cursor_header_is_highlighted() {
        let mut model = model(None, false);
        let mut thread = ThreadModal::new(crate::api::PostDetail {
            post: crate::api::PostView {
                id: crate::PostId(1),
                title: "Threaded post".into(),
                body: None,
                url: None,
                community_id: crate::CommunityId(1),
                creator_id: crate::UserId(1),
                score: 0,
                comments: 1,
                published: None,
            },
            comments: vec![crate::api::CommentView {
                id: crate::CommentId(1),
                post_id: crate::PostId(1),
                content: "body text".into(),
                creator_id: crate::UserId(2),
                creator_name: "alice".into(),
                score: 3,
                path: None,
            }],
        });
        thread.selected = Some(crate::CommentId(1));
        model.modals.push(Modal::Thread(thread));
        let width = 140u16;
        let height = 48u16;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal.draw(|frame| render(frame, &model)).expect("render");
        let buffer = terminal.backend().buffer();
        let content = &buffer.content;
        // Buffer cells hold one grapheme each; locate the header row by
        // scanning joined row text, then check that row for the highlight.
        let row = (0..height)
            .find(|&row| {
                let start = row as usize * width as usize;
                let end = start + width as usize;
                let text: String = content[start..end]
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                text.contains("[3] alice:")
            })
            .expect("comment header renders");
        let start = row as usize * width as usize;
        let end = start + width as usize;
        let highlighted = content[start..end]
            .iter()
            .any(|cell| cell.style().add_modifier.contains(Modifier::REVERSED));
        assert!(highlighted, "the focused comment header must be highlighted");
    }
}
