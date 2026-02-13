use crate::metrics::MetricsSnapshot;
use ratatui::prelude::*;
use ratatui::widgets::*;
use std::time::Duration;

pub fn render(
    frame: &mut Frame,
    snap: &MetricsSnapshot,
    elapsed: Duration,
    chat_log: &[String],
) {
    let area = frame.area();

    // Main layout: Header | Middle | Chat | Events
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Header
            Constraint::Length(8),  // Charts row
            Constraint::Min(6),    // Middle (DAG + Agents)
            Constraint::Length(8), // Chat
            Constraint::Length(8), // Event log
        ])
        .split(area);

    // ═══ Header ═══
    let elapsed_str = format!(
        "{:02}:{:02}",
        elapsed.as_secs() / 60,
        elapsed.as_secs() % 60
    );
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " pms-simulator ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("(Gemini Pro)  |  "),
        Span::styled(elapsed_str, Style::default().fg(Color::Yellow)),
        Span::raw("  |  "),
        Span::styled(
            format!("{:.1} TX/s", snap.tps),
            Style::default().fg(Color::Green),
        ),
        Span::raw("  |  "),
        Span::raw(format!("{} TX total", snap.total_tx)),
        Span::raw("  |  "),
        Span::styled(
            format!("{} errs", snap.total_errors),
            Style::default().fg(if snap.total_errors > 0 {
                Color::Red
            } else {
                Color::DarkGray
            }),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" Dashboard "));
    frame.render_widget(header, main_chunks[0]);

    // ═══ Charts row ═══
    let chart_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(main_chunks[1]);

    // TPS Sparkline
    let tps_data: Vec<u64> = snap
        .tps_history
        .iter()
        .map(|v| (*v * 10.0) as u64)
        .collect();
    let tps_spark = Sparkline::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" TPS (60s) "),
        )
        .data(&tps_data)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(tps_spark, chart_chunks[0]);

    // Latency bars
    let lat_bars = BarChart::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Latency "),
        )
        .bar_width(8)
        .bar_gap(2)
        .bar_style(Style::default().fg(Color::Yellow))
        .data(&[
            ("p50", snap.latency_p50_ms as u64),
            ("p95", snap.latency_p95_ms as u64),
            ("p99", snap.latency_p99_ms as u64),
        ]);
    frame.render_widget(lat_bars, chart_chunks[1]);

    // ═══ Middle: DAG Status + Agent Table ═══
    let mid_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(main_chunks[2]);

    // DAG Status
    let dag_text = vec![
        Line::from(format!("Tips: {}", snap.tips_count)),
        Line::from(format!("Supply: {}", snap.circulating_supply)),
        Line::from(format!("UTXOs: {}", snap.utxo_count)),
    ];
    let dag_panel = Paragraph::new(dag_text)
        .block(Block::default().borders(Borders::ALL).title(" DAG Status "));
    frame.render_widget(dag_panel, mid_chunks[0]);

    // Agent Table
    let header_cells = ["Name", "TX", "Err", "Lat(ms)", "Balance"]
        .iter()
        .map(|h| Cell::from(*h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header_row = Row::new(header_cells).height(1);

    let mut agent_rows: Vec<Row> = snap
        .agent_stats
        .values()
        .map(|stat| {
            Row::new(vec![
                Cell::from(stat.name.clone()),
                Cell::from(stat.tx_count.to_string()),
                Cell::from(stat.error_count.to_string()).style(
                    if stat.error_count > 0 {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default()
                    },
                ),
                Cell::from(format!("{:.0}", stat.last_latency_ms)),
                Cell::from(stat.balance.clone()),
            ])
        })
        .collect();
    agent_rows.sort_by(|a, b| {
        // Sort by name
        format!("{:?}", a).cmp(&format!("{:?}", b))
    });

    let table = Table::new(
        agent_rows,
        [
            Constraint::Length(12),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(header_row)
    .block(Block::default().borders(Borders::ALL).title(" Agents "));
    frame.render_widget(table, mid_chunks[1]);

    // ═══ Chat Panel ═══
    let chat_lines: Vec<Line> = chat_log
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|s| Line::from(Span::styled(s.as_str(), Style::default().fg(Color::Magenta))))
        .collect();
    let chat_panel = Paragraph::new(chat_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Agent Chat (P2P) "),
    );
    frame.render_widget(chat_panel, main_chunks[3]);

    // ═══ Event Log ═══
    let event_lines: Vec<Line> = snap
        .recent_events
        .iter()
        .rev()
        .take(6)
        .rev()
        .map(|s| Line::from(s.as_str()))
        .collect();
    let events_panel = Paragraph::new(event_lines)
        .block(Block::default().borders(Borders::ALL).title(" Events "));
    frame.render_widget(events_panel, main_chunks[4]);
}
