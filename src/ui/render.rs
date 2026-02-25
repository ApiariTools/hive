//! Rendering logic for the unified hive TUI.

use super::app::{App, ChatLine, Panel};
use crate::keeper::tui::theme;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
};

/// Main draw entry point.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Background fill.
    frame.render_widget(Block::default().style(theme::overlay_bg()), area);

    // Overall layout: tab bar (1) + body + input bar (3).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body
            Constraint::Length(3), // input bar
        ])
        .split(area);

    draw_tab_bar(frame, outer[0], app);
    draw_body(frame, outer[1], app);
    draw_input_bar(frame, outer[2], app);
}

// ── Tab Bar ──────────────────────────────────────────────

fn draw_tab_bar(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();

    for (i, ws) in app.workspaces.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", theme::muted()));
        }
        if i == app.active_tab {
            spans.push(Span::styled(format!(" {} ", ws.name), theme::highlight()));
        } else {
            spans.push(Span::styled(format!(" {} ", ws.name), theme::muted()));
        }
    }

    // Right-aligned status.
    let status_text = " hive ui  q:quit ";
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let padding = area.width as usize
        - used.min(area.width as usize)
        - status_text.len().min(area.width as usize);
    if padding > 0 {
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled(status_text, theme::subtitle()));

    let line = Line::from(spans);
    let bar = Paragraph::new(line);
    frame.render_widget(bar, area);
}

// ── Body ─────────────────────────────────────────────────

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    // Responsive split: ~30% left, ~70% right.
    let left_pct = if area.width > 120 {
        25
    } else if area.width > 80 {
        30
    } else {
        35
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Percentage(100 - left_pct),
        ])
        .split(area);

    draw_workers_panel(frame, columns[0], app);
    draw_chat_panel(frame, columns[1], app);
}

// ── Workers Panel ────────────────────────────────────────

fn draw_workers_panel(frame: &mut Frame, area: Rect, app: &App) {
    let border_style = if app.focus == Panel::Workers {
        theme::border_active()
    } else {
        theme::border()
    };

    let block = Block::default()
        .title(Span::styled(" Workers ", theme::title()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let workers = app.flat_workers();

    if workers.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            "  No active workers",
            theme::muted(),
        )))
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = workers
        .iter()
        .enumerate()
        .map(|(i, (_session, wt))| {
            let marker = if i == app.selected_worker {
                "▸ "
            } else {
                "  "
            };

            let status_icon = if wt.agent_alive {
                Span::styled("● ", theme::status_running())
            } else {
                Span::styled("○ ", theme::status_idle())
            };

            let id_style = if i == app.selected_worker {
                theme::selected()
            } else {
                theme::text()
            };

            let status_suffix = if wt.agent_alive {
                if wt.id.contains("tui") {
                    " (tui)"
                } else {
                    " (run)"
                }
            } else if wt.summary.is_some() {
                " (done)"
            } else {
                " (idle)"
            };

            let line = Line::from(vec![
                Span::styled(marker, id_style),
                status_icon,
                Span::styled(truncate(&wt.id, 20), id_style),
                Span::styled(status_suffix, theme::subtitle()),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

// ── Chat Panel ───────────────────────────────────────────

fn draw_chat_panel(frame: &mut Frame, area: Rect, app: &App) {
    let border_style = if app.focus == Panel::Chat {
        theme::border_active()
    } else {
        theme::border()
    };

    let title = if app.streaming {
        " Coordinator (streaming...) "
    } else {
        " Coordinator "
    };

    let block = Block::default()
        .title(Span::styled(title, theme::title()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.chat_history.is_empty() {
        return;
    }

    // Build lines from chat history.
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.chat_history {
        match entry {
            ChatLine::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You: ", theme::accent()),
                    Span::styled(text.as_str(), theme::text()),
                ]));
            }
            ChatLine::Assistant(text) => {
                lines.push(Line::from(Span::styled(
                    "Coordinator:",
                    theme::agent_color(),
                )));
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(format!("  {line}"), theme::text())));
                }
            }
            ChatLine::System(text) => {
                lines.push(Line::from(Span::styled(text.as_str(), theme::subtitle())));
            }
        }
        lines.push(Line::from("")); // blank separator
    }

    // Apply scroll from bottom.
    let visible_height = inner.height as usize;
    let total = lines.len();
    let skip = if total > visible_height {
        total
            - visible_height
            - (app.chat_scroll as usize).min(total.saturating_sub(visible_height))
    } else {
        0
    };

    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).take(visible_height).collect();

    let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ── Input Bar ────────────────────────────────────────────

fn draw_input_bar(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::border());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_spans = vec![
        Span::styled("> ", theme::accent()),
        Span::styled(app.input.as_str(), theme::text()),
    ];

    // Focus hint on the right.
    let hint = if app.focus == Panel::Chat {
        "[Tab: workers]"
    } else {
        "[Tab: chat]"
    };

    let input_line = Line::from(prompt_spans);
    let hint_line = Line::from(Span::styled(hint, theme::subtitle()));

    // Split inner into input area + hint.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(hint.len() as u16 + 1),
        ])
        .split(inner);

    let input_widget = Paragraph::new(input_line);
    frame.render_widget(input_widget, cols[0]);

    let hint_widget = Paragraph::new(hint_line);
    frame.render_widget(hint_widget, cols[1]);

    // Place the cursor.
    if app.focus == Panel::Chat {
        let cursor_x = cols[0].x + 2 + app.input_cursor as u16;
        let cursor_y = cols[0].y;
        frame.set_cursor_position((cursor_x.min(cols[0].right().saturating_sub(1)), cursor_y));
    }
}

// ── Helpers ──────────────────────────────────────────────

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}
