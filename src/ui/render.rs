//! Rendering logic for the unified hive TUI.

use super::app::{App, ChatLine, Panel, SidebarItem};
use crate::keeper::discovery::WorktreeInfo;
use crate::keeper::tui::theme;
use chrono::Local;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

/// Main draw entry point.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Background fill.
    frame.render_widget(Block::default().style(theme::overlay_bg()), area);

    // Overall layout: tab bar (1) + body.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // body
        ])
        .split(area);

    draw_tab_bar(frame, outer[0], app);
    draw_body(frame, outer[1], app);
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
    let status_text = " hive ui  ^b n/p:tab  q:quit ";
    let used: usize = spans.iter().map(|s| s.width()).sum();
    let padding = (area.width as usize)
        .saturating_sub(used.min(area.width as usize))
        .saturating_sub(status_text.len().min(area.width as usize));
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
    // Responsive split: wider sidebar for 3-line rows.
    let left_pct = if area.width > 120 {
        20
    } else if area.width > 80 {
        24
    } else {
        30
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_pct),
            Constraint::Percentage(100 - left_pct),
        ])
        .split(area);

    draw_sidebar(frame, columns[0], app);
    draw_content_panel(frame, columns[1], app);
}

// ── Sidebar (borderless, swarm-style) ────────────────────

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // list
            Constraint::Length(1), // divider
            Constraint::Length(1), // status bar
        ])
        .split(area);

    draw_sidebar_list(frame, chunks[0], app);
    draw_sidebar_divider(frame, chunks[1]);
    draw_sidebar_status_bar(frame, chunks[2], app);
}

fn draw_sidebar_divider(frame: &mut Frame, area: Rect) {
    let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let divider = Paragraph::new("\u{2500}".repeat(inner.width as usize))
        .style(Style::default().fg(theme::WAX));
    frame.render_widget(divider, inner);
}

fn draw_sidebar_list(frame: &mut Frame, area: Rect, app: &App) {
    let workers = app.flat_workers();
    let viewport = area.height as usize;

    // Build item heights: Chat=2, each Worker=3
    let mut heights: Vec<usize> = vec![2]; // Chat item
    if workers.is_empty() {
        heights.push(1); // "No workers" line
    } else {
        heights.extend(std::iter::repeat_n(3, workers.len()));
    }

    // Cumulative top positions
    let mut tops = Vec::with_capacity(heights.len());
    let mut cumulative = 0usize;
    for &h in &heights {
        tops.push(cumulative);
        cumulative += h;
    }
    let total_height = cumulative;

    // Find selected item index (0 = Chat, 1+ = workers)
    let selected_item = match app.sidebar_selection {
        SidebarItem::Chat => 0,
        SidebarItem::Worker(i) => {
            if workers.is_empty() {
                0
            } else {
                i + 1
            }
        }
    };

    // Adjust scroll to keep selected item visible
    let mut scroll = app.sidebar_scroll.get();
    if selected_item < heights.len() {
        let sel_top = tops[selected_item];
        let sel_bottom = sel_top + heights[selected_item];

        if sel_top < scroll {
            scroll = sel_top;
        } else if sel_bottom > scroll + viewport {
            scroll = sel_bottom.saturating_sub(viewport);
        }
    }

    if total_height <= viewport {
        scroll = 0;
    } else {
        scroll = scroll.min(total_height - viewport);
    }
    app.sidebar_scroll.set(scroll);

    // Render visible items
    let mut render_y = area.y;
    let viewport_bottom = area.y + area.height;

    // Item 0: Chat
    {
        let item_top = tops[0];
        let item_bottom = item_top + heights[0];
        if item_bottom > scroll && render_y < viewport_bottom {
            let skip_top = scroll.saturating_sub(item_top);
            let available = (viewport_bottom - render_y) as usize;
            let render_h = (heights[0] - skip_top).min(available);
            if render_h > 0 {
                let rect = Rect::new(area.x, render_y, area.width, render_h as u16);
                render_y += render_h as u16;
                draw_chat_sidebar_item(frame, rect, app);
            }
        }
    }

    if workers.is_empty() {
        // "No workers" placeholder
        if render_y < viewport_bottom {
            let rect = Rect::new(area.x, render_y, area.width, 1);
            let line = Line::from(Span::styled("  No active workers", theme::muted()));
            frame.render_widget(Paragraph::new(line), rect);
        }
    } else {
        for (i, (_, wt)) in workers.iter().enumerate() {
            let item_idx = i + 1;
            let item_top = tops[item_idx];
            let item_bottom = item_top + heights[item_idx];

            if item_bottom <= scroll {
                continue;
            }
            if render_y >= viewport_bottom {
                break;
            }

            let skip_top = scroll.saturating_sub(item_top);
            let available = (viewport_bottom - render_y) as usize;
            let render_h = (heights[item_idx] - skip_top).min(available);

            if render_h == 0 {
                continue;
            }

            let rect = Rect::new(area.x, render_y, area.width, render_h as u16);
            render_y += render_h as u16;

            let is_selected = app.sidebar_selection == SidebarItem::Worker(i);
            draw_worker_sidebar_row(frame, rect, wt, is_selected, i);
        }
    }
}

/// 2-line chat entry in the sidebar.
fn draw_chat_sidebar_item(frame: &mut Frame, area: Rect, app: &App) {
    let is_selected = app.sidebar_selection == SidebarItem::Chat;

    let row_style = if is_selected {
        Style::default().bg(ratatui::style::Color::Rgb(58, 50, 42))
    } else {
        Style::default().bg(theme::COMB)
    };
    frame.render_widget(Paragraph::new("").style(row_style), area);

    let selector = if is_selected { "\u{25b8}" } else { " " };
    let text_style = if is_selected {
        theme::selected()
    } else {
        theme::text()
    };

    let icon = if app.streaming {
        Span::styled("\u{25c9} ", theme::status_running())
    } else {
        Span::styled("\u{25c8} ", theme::accent())
    };

    // Line 1: " ▸ ◈ Coordinator"
    if area.height >= 1 {
        let line1 = Line::from(vec![
            Span::styled(format!(" {selector}"), text_style),
            Span::styled(" ", Style::default()),
            icon,
            Span::styled("Coordinator", text_style),
        ]);
        frame.render_widget(
            Paragraph::new(line1).style(row_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    // Line 2: "     ready|streaming..."
    if area.height >= 2 {
        let status_text = if app.streaming {
            "streaming..."
        } else {
            "ready"
        };
        let line2 = Line::from(vec![
            Span::styled("    ", Style::default()),
            Span::styled(
                status_text,
                Style::default().fg(ratatui::style::Color::Rgb(100, 97, 90)),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line2).style(row_style),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

/// 3-line worker row matching swarm's style.
fn draw_worker_sidebar_row(
    frame: &mut Frame,
    area: Rect,
    wt: &WorktreeInfo,
    selected: bool,
    idx: usize,
) {
    let wt_color = theme::SIDEBAR_COLORS[idx % theme::SIDEBAR_COLORS.len()];
    let bar = Span::styled("\u{258c}", Style::default().fg(wt_color));

    let row_style = if selected {
        Style::default().bg(ratatui::style::Color::Rgb(58, 50, 42))
    } else {
        Style::default().bg(theme::COMB)
    };

    // Background fill
    frame.render_widget(Paragraph::new("").style(row_style), area);

    let status_icon = if wt.agent_alive {
        if wt.agent_session_status.as_deref() == Some("waiting") {
            Span::styled("\u{25cf} ", theme::status_waiting())
        } else {
            Span::styled("\u{25cf} ", theme::status_running())
        }
    } else if wt.summary.is_some() {
        Span::styled("\u{25c6} ", theme::status_done())
    } else {
        Span::styled("\u{25cb} ", theme::status_idle())
    };

    let selector = if selected { "\u{25b8}" } else { " " };
    let selector_style = if selected {
        theme::selected()
    } else {
        theme::muted()
    };
    let id_style = if selected {
        theme::selected()
    } else {
        theme::text()
    };

    // Line 1: ▌▸ ● worker-id  #PR
    if area.height >= 1 {
        let id_max = (area.width as usize).saturating_sub(10);
        let mut line1_spans = vec![
            bar.clone(),
            Span::styled(selector, selector_style),
            Span::styled(" ", Style::default()),
            status_icon,
            Span::styled(truncate(&wt.id, id_max), id_style),
        ];

        if let Some(ref pr) = wt.pr {
            let pr_badge = format!(" #{}", pr.number);
            let pr_style = match pr.state.as_str() {
                "MERGED" => theme::success(),
                "OPEN" => Style::default().fg(theme::MINT),
                _ => theme::muted(),
            };
            line1_spans.push(Span::styled(pr_badge, pr_style));
        }

        frame.render_widget(
            Paragraph::new(Line::from(line1_spans)).style(row_style),
            Rect::new(area.x, area.y, area.width, 1),
        );
    }

    // Line 2: ▌  agent-kind · 5m
    if area.height >= 2 {
        let ago = format_time_ago(wt);
        let line2 = Line::from(vec![
            bar.clone(),
            Span::styled("  ", Style::default()),
            Span::styled(&wt.agent_kind, theme::agent_color()),
            Span::styled(
                format!(" \u{00b7} {ago}"),
                Style::default().fg(ratatui::style::Color::Rgb(80, 77, 70)),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line2).style(row_style),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }

    // Line 3: ▌  summary/prompt text
    if area.height >= 3 {
        let max_len = (area.width as usize).saturating_sub(3);
        let (text, style) = if let Some(ref summary) = wt.summary {
            (
                truncate(summary, max_len),
                Style::default().fg(ratatui::style::Color::Rgb(140, 137, 130)),
            )
        } else if !wt.prompt.is_empty() {
            // Show first line of prompt as fallback
            let first = wt.prompt.lines().next().unwrap_or(&wt.prompt);
            (
                truncate(first, max_len),
                Style::default().fg(ratatui::style::Color::Rgb(90, 87, 80)),
            )
        } else {
            (String::new(), theme::muted())
        };

        let line3 = Line::from(vec![
            bar,
            Span::styled("  ", Style::default()),
            Span::styled(text, style),
        ]);
        frame.render_widget(
            Paragraph::new(line3).style(row_style),
            Rect::new(area.x, area.y + 2, area.width, 1),
        );
    }
}

fn draw_sidebar_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    // Pending confirmation takes priority.
    if let Some(ref action) = app.pending_action {
        let msg = match action {
            super::app::PendingAction::CloseWorker(id) => format!(" Close {}? y/n", id),
        };
        let line = Line::from(Span::styled(
            msg,
            Style::default()
                .fg(theme::NECTAR)
                .add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Flash message.
    if let Some(ref flash) = app.flash_message {
        let line = Line::from(Span::styled(&flash.text[..], theme::accent()));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    // Context-sensitive hints.
    let hints = if app.is_chat_selected() {
        Line::from(vec![
            Span::styled(" j", theme::key_hint()),
            Span::styled("/", theme::key_desc()),
            Span::styled("k", theme::key_hint()),
            Span::styled(" nav  ", theme::key_desc()),
            Span::styled("l", theme::key_hint()),
            Span::styled(" focus  ", theme::key_desc()),
            Span::styled("q", theme::key_hint()),
            Span::styled(" quit", theme::key_desc()),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ↵", theme::key_hint()),
            Span::styled(" enter  ", theme::key_desc()),
            Span::styled("x", theme::key_hint()),
            Span::styled(" close  ", theme::key_desc()),
            Span::styled("p", theme::key_hint()),
            Span::styled(" PR  ", theme::key_desc()),
            Span::styled("c", theme::key_hint()),
            Span::styled(" copy", theme::key_desc()),
        ])
    };
    frame.render_widget(Paragraph::new(hints), area);
}

// ── Content Panel (contextual) ───────────────────────────

fn draw_content_panel(frame: &mut Frame, area: Rect, app: &App) {
    match app.sidebar_selection {
        SidebarItem::Chat => {
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),    // chat
                    Constraint::Length(3), // input bar
                ])
                .split(area);

            draw_chat_panel(frame, right[0], app);
            draw_input_bar(frame, right[1], app, "> ");
        }
        SidebarItem::Worker(_) => {
            draw_worker_output(frame, area, app);
        }
    }
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
            .saturating_sub(visible_height)
            .saturating_sub((app.chat_scroll as usize).min(total.saturating_sub(visible_height)))
    } else {
        0
    };

    let visible_lines: Vec<Line> = lines.into_iter().skip(skip).take(visible_height).collect();

    let paragraph = Paragraph::new(visible_lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

// ── Input Bar ────────────────────────────────────────────

fn draw_input_bar(frame: &mut Frame, area: Rect, app: &App, prompt: &str) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::border());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let prompt_spans = vec![
        Span::styled(prompt, theme::accent()),
        Span::styled(app.input.as_str(), theme::text()),
    ];

    // Focus hint on the right.
    let hint = if app.focus == Panel::Chat {
        "[h: sidebar]"
    } else {
        "[l: focus]"
    };

    let input_line = Line::from(prompt_spans);
    let hint_line = Line::from(Span::styled(hint, theme::subtitle()));

    // Split inner into input area + hint.
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length((hint.len() as u16).saturating_add(1)),
        ])
        .split(inner);

    let input_widget = Paragraph::new(input_line);
    frame.render_widget(input_widget, cols[0]);

    let hint_widget = Paragraph::new(hint_line);
    frame.render_widget(hint_widget, cols[1]);

    // Place the cursor.
    if app.focus == Panel::Chat {
        let prompt_width = prompt.len() as u16;
        let cursor_x = cols[0].x + prompt_width + app.input_cursor as u16;
        let cursor_y = cols[0].y;
        frame.set_cursor_position((cursor_x.min(cols[0].right().saturating_sub(1)), cursor_y));
    }
}

// ── Worker Output (tmux pane capture) ─────────────────────

fn draw_worker_output(frame: &mut Frame, area: Rect, app: &App) {
    let border_style = if app.focus == Panel::Chat {
        theme::border_active()
    } else {
        theme::border()
    };

    let workers = app.flat_workers();
    let worker_idx = match app.sidebar_selection {
        SidebarItem::Worker(i) => i,
        _ => return,
    };

    let Some((_, wt)) = workers.get(worker_idx) else {
        return;
    };

    // Build title: id + status.
    let status_str = if wt.agent_alive {
        if wt.agent_session_status.as_deref() == Some("waiting") {
            "waiting"
        } else {
            "running"
        }
    } else {
        "stopped"
    };
    let title_parts = format!(" {}  {} ", wt.id, status_str);

    let block = Block::default()
        .title(Span::styled(title_parts, theme::title()))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Parse ANSI-colored pane content into styled lines.
    let all_lines: Vec<Line> = app
        .pane_content
        .split('\n')
        .map(|l| parse_ansi_line(l))
        .collect();

    // Trim trailing empty lines.
    let trimmed_len = all_lines
        .iter()
        .rposition(|l| l.width() > 0)
        .map(|i| i + 1)
        .unwrap_or(0);
    let lines = &all_lines[..trimmed_len];

    let visible_height = inner.height as usize;
    let total = lines.len();

    // Always pinned to bottom (keys are forwarded to the pane directly).
    let skip = total.saturating_sub(visible_height);

    let visible_lines: Vec<Line> = lines
        .iter()
        .skip(skip)
        .take(visible_height)
        .cloned()
        .collect();

    let paragraph = Paragraph::new(visible_lines);
    frame.render_widget(paragraph, inner);
}

// ── ANSI parsing ─────────────────────────────────────────

/// Parse a line with ANSI escape sequences into a ratatui Line with styles.
fn parse_ansi_line(input: &str) -> Line<'static> {
    use ratatui::style::Color;

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_style = Style::default();
    let mut buf = String::new();
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            // Flush buffered text.
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), current_style));
            }
            chars.next(); // consume '['
            // Read CSI parameters.
            let mut params = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == ';' {
                    params.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            // Read final byte.
            let final_byte = chars.next().unwrap_or('m');
            if final_byte == 'm' {
                // SGR — apply styles.
                let codes: Vec<u32> = params
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                let mut i = 0;
                while i < codes.len() {
                    match codes[i] {
                        0 => current_style = Style::default(),
                        1 => current_style = current_style.add_modifier(Modifier::BOLD),
                        3 => current_style = current_style.add_modifier(Modifier::ITALIC),
                        4 => current_style = current_style.add_modifier(Modifier::UNDERLINED),
                        7 => current_style = current_style.add_modifier(Modifier::REVERSED),
                        22 => current_style = current_style.remove_modifier(Modifier::BOLD),
                        23 => current_style = current_style.remove_modifier(Modifier::ITALIC),
                        24 => current_style = current_style.remove_modifier(Modifier::UNDERLINED),
                        27 => current_style = current_style.remove_modifier(Modifier::REVERSED),
                        // Standard foreground colors.
                        30 => current_style = current_style.fg(Color::Black),
                        31 => current_style = current_style.fg(Color::Red),
                        32 => current_style = current_style.fg(Color::Green),
                        33 => current_style = current_style.fg(Color::Yellow),
                        34 => current_style = current_style.fg(Color::Blue),
                        35 => current_style = current_style.fg(Color::Magenta),
                        36 => current_style = current_style.fg(Color::Cyan),
                        37 => current_style = current_style.fg(Color::White),
                        39 => current_style.fg = None,
                        // Standard background colors.
                        40 => current_style = current_style.bg(Color::Black),
                        41 => current_style = current_style.bg(Color::Red),
                        42 => current_style = current_style.bg(Color::Green),
                        43 => current_style = current_style.bg(Color::Yellow),
                        44 => current_style = current_style.bg(Color::Blue),
                        45 => current_style = current_style.bg(Color::Magenta),
                        46 => current_style = current_style.bg(Color::Cyan),
                        47 => current_style = current_style.bg(Color::White),
                        49 => current_style.bg = None,
                        // 256-color and RGB.
                        38 if i + 1 < codes.len() => {
                            if codes[i + 1] == 5 && i + 2 < codes.len() {
                                current_style =
                                    current_style.fg(Color::Indexed(codes[i + 2] as u8));
                                i += 2;
                            } else if codes[i + 1] == 2 && i + 4 < codes.len() {
                                current_style = current_style.fg(Color::Rgb(
                                    codes[i + 2] as u8,
                                    codes[i + 3] as u8,
                                    codes[i + 4] as u8,
                                ));
                                i += 4;
                            }
                        }
                        48 if i + 1 < codes.len() => {
                            if codes[i + 1] == 5 && i + 2 < codes.len() {
                                current_style =
                                    current_style.bg(Color::Indexed(codes[i + 2] as u8));
                                i += 2;
                            } else if codes[i + 1] == 2 && i + 4 < codes.len() {
                                current_style = current_style.bg(Color::Rgb(
                                    codes[i + 2] as u8,
                                    codes[i + 3] as u8,
                                    codes[i + 4] as u8,
                                ));
                                i += 4;
                            }
                        }
                        // Bright foreground.
                        90..=97 => {
                            current_style =
                                current_style.fg(Color::Indexed((codes[i] - 90 + 8) as u8));
                        }
                        // Bright background.
                        100..=107 => {
                            current_style =
                                current_style.bg(Color::Indexed((codes[i] - 100 + 8) as u8));
                        }
                        _ => {} // ignore unknown
                    }
                    i += 1;
                }
                if codes.is_empty() {
                    // "\x1b[m" with no params = reset.
                    current_style = Style::default();
                }
            }
        } else {
            buf.push(ch);
        }
    }

    // Flush remaining text.
    if !buf.is_empty() {
        spans.push(Span::styled(buf, current_style));
    }

    if spans.is_empty() {
        Line::from("")
    } else {
        Line::from(spans)
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

/// Format elapsed time as "now/5m/2h/3d".
fn format_time_ago(wt: &WorktreeInfo) -> String {
    let Some(created) = wt.created_at else {
        return String::new();
    };
    let elapsed = Local::now().signed_duration_since(created);
    if elapsed.num_minutes() < 1 {
        "now".to_string()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h", elapsed.num_hours())
    } else {
        format!("{}d", elapsed.num_days())
    }
}
