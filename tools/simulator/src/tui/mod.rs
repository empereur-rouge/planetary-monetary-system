pub mod dashboard;

use crate::comms::types::AgentMessage;
use crate::metrics::aggregator::SharedMetrics;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::prelude::*;
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct TuiApp {
    metrics: SharedMetrics,
    start_time: Instant,
    chat_rx: mpsc::UnboundedReceiver<AgentMessage>,
    chat_log: Vec<String>,
}

impl TuiApp {
    pub fn new(
        metrics: SharedMetrics,
        chat_rx: mpsc::UnboundedReceiver<AgentMessage>,
    ) -> Self {
        Self {
            metrics,
            start_time: Instant::now(),
            chat_rx,
            chat_log: Vec::new(),
        }
    }

    pub fn run(&mut self, refresh_ms: u64) -> io::Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
        )?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        loop {
            // Drain chat messages
            while let Ok(msg) = self.chat_rx.try_recv() {
                self.chat_log.push(msg.summary());
                if self.chat_log.len() > 50 {
                    self.chat_log.remove(0);
                }
            }

            let snapshot = self.metrics.lock().unwrap().clone();
            let elapsed = self.start_time.elapsed();
            let chat_log = self.chat_log.clone();

            terminal.draw(|frame| {
                dashboard::render(frame, &snapshot, elapsed, &chat_log);
            })?;

            if event::poll(Duration::from_millis(refresh_ms))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        crossterm::terminal::disable_raw_mode()?;
        crossterm::execute!(
            terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen,
        )?;
        terminal.show_cursor()?;

        Ok(())
    }
}
