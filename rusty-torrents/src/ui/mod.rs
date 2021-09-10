use std::io::Stdout;
use std::sync::Arc;
use std::io::stdout;
use std::time::Instant;

use tui::Frame;
use tui::terminal::Terminal;
use tui::backend::CrosstermBackend;

use tokio::sync::oneshot::Sender;

use tui::text::{Span, Spans};
use tui::style::{Color, Style};
use tui::layout::{Constraint, Layout, Rect};
use tui::widgets::{Block, Borders, Cell, Gauge, Row, Table, TableState, Tabs};

use crossterm::execute;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyCode, MouseEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};

use crate::engine::TorrentInfo;

pub struct App {
    selected_tab: usize,
    
    files_state: TableState,
    peers_state: TableState,
    piece_state: TableState,

    total_size: usize,
    total_downloaded: usize,

    stop_tx: Sender<()>,

    start_time: Instant,
    torrent_info: Arc<TorrentInfo>
}

impl App {
    pub fn new(stop_tx: Sender<()>, torrent_info: Arc<TorrentInfo>) -> App {
        let (total_size, total_downloaded) = {
            let mut total = 0;
            let mut downloaded = 0;

            let pieces = torrent_info.torrent_pieces();
            let pieces = pieces.try_read().unwrap();
            
            for piece in pieces.iter() {
                total += piece.get_len();
                downloaded += piece.get_downloaded();
            }

            (total, downloaded)
        };

        App {
            selected_tab: 0,

            files_state: TableState::default(),
            peers_state: TableState::default(),
            piece_state: TableState::default(),

            total_size,
            total_downloaded,

            stop_tx,
            
            start_time: Instant::now(),
            torrent_info
        }
    }

    pub fn draw(mut self) {
        let mut stdout = stdout();

        enable_raw_mode().unwrap();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture).unwrap();
    
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend).unwrap();
    
        terminal.clear().unwrap();

        let tick_rate = std::time::Duration::from_millis(200);
        let mut last_tick = std::time::Instant::now();
    
        loop {
            let mut should_draw = false;
            let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or_else(|| std::time::Duration::from_secs(0));
    
            if let Ok(event) = crossterm::event::poll(timeout) {
                if event {
                    if let Ok(event) = crossterm::event::read() {
                        match event {
                            Event::Key(key) => {
                                match key.code {
                                    KeyCode::Up => {
                                        self.scroll_list(true);
                                        should_draw = true;
                                    }
                                    KeyCode::Down => {
                                        self.scroll_list(false);
                                        should_draw = true;
                                    }
                                    KeyCode::Left => {
                                        if self.selected_tab == 0 {
                                            self.selected_tab = 2;
                                        }
                                        else {
                                            self.selected_tab -= 1;
                                        }
        
                                        should_draw = true;
                                    }
                                    KeyCode::Right => {
                                        if self.selected_tab == 2 {
                                            self.selected_tab = 0;
                                        }
                                        else {
                                            self.selected_tab += 1;
                                        }
        
                                        should_draw = true;
                                    }
                                    KeyCode::Esc | KeyCode::Char('q') => {
                                        self.stop_tx.send(()).unwrap();

                                        disable_raw_mode().unwrap();
        
                                        execute!(
                                            terminal.backend_mut(),
                                            LeaveAlternateScreen,
                                            DisableMouseCapture
                                        ).unwrap();
        
                                        terminal.show_cursor().unwrap();
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                            Event::Mouse(e) => {
                                match e.kind {
                                    MouseEventKind::ScrollUp => {
                                        self.scroll_list(true);
                                        should_draw = true;
                                    }
                                    MouseEventKind::ScrollDown => {
                                        self.scroll_list(false);
                                        should_draw = true;
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
    
            if last_tick.elapsed() >= tick_rate {
                should_draw = true;
                last_tick = std::time::Instant::now();
            }

            if should_draw {
                terminal.draw(|f| {
                    let chunks = Layout::default()
                        .constraints(
                            [
                                Constraint::Min(10),
                                Constraint::Percentage(14),
                                Constraint::Percentage(60)
                            ].as_ref()
                        )
                        .split(f.size())
                    ;

                    self.draw_general_info(f, chunks[0]);
                    self.draw_tabs(f, chunks[1], chunks[2]);
                }).unwrap();
            }
        }
    }

    fn scroll_list(&mut self, up: bool) {
        let (state, max) = {
            if self.selected_tab == 0 {
                (&mut self.files_state, self.torrent_info.data().get_files().len())
            }
            else if self.selected_tab == 1 {
                let max = {
                    if let Ok(lock) = self.torrent_info.torrent_peers().try_read() {
                        lock.len()
                    }
                    else {
                        50
                    }
                };

                (&mut self.peers_state, max)
            }
            else {
                (&mut self.piece_state, self.torrent_info.data().info().pieces().len())
            }
        };

        let selected = state.selected().unwrap_or(0);
        let new_selected = {
            if up {
                if selected == 0 {
                    max
                }
                else {
                    selected - 1
                }
            }
            else if selected >= max {
                0
            }
            else {
                selected + 1
            }
        };

        state.select(Some(new_selected));
    }

    fn draw_general_info(&mut self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let chunks = Layout::default()
            .constraints([
                Constraint::Percentage(70),
                Constraint::Max(30)
            ].as_ref())
            .split(area)
        ;

        let name = self.torrent_info.data().get_name();
        
        let progress = {
            if let Ok(pieces) = self.torrent_info.torrent_pieces().try_read() {
                let mut downloaded = 0;

                for piece in pieces.iter() {
                    downloaded += piece.get_downloaded();
                }

                self.total_downloaded = downloaded;
            }

            ((self.total_downloaded as f32 / self.total_size as f32) * 100.0) as u16
        };

        let rate = {
            let now = Instant::now();
            let start_time = self.start_time;
            let elapsed = now.checked_sub(start_time.elapsed()).unwrap().elapsed();

            if elapsed.as_secs() > 0 {
                if let Ok(lock) = self.torrent_info.total_downloaded.try_read() {
                    *lock / elapsed.as_secs() as usize
                }
                else {
                    0
                }
            }
            else {
                0
            }
        };

        let gauge = Gauge::default()
            .percent(progress)
            .block(Block::default().borders(Borders::ALL))
            .gauge_style(Style::default().fg(Color::Magenta).bg(Color::White))
        ;
        
        let peers = {
            if let Ok(peers) = self.torrent_info.torrent_peers().try_read() {
                peers.len()
            }
            else {
                0
            }
        };

        let row = Row::new(vec![
            Cell::from(Span::styled(name, Style::default())),
            Cell::from(Span::styled(App::bytes_data(self.total_size as u32), Style::default())),
            Cell::from(Span::styled(App::bytes_data(self.total_downloaded as u32), Style::default())),
            Cell::from(Span::styled(App::bytes_rate(rate as u32), Style::default())),
            Cell::from(Span::styled(peers.to_string(), Style::default()))
        ]);

        let table = Table::new(vec![row])
            .block(Block::default().borders(Borders::ALL))
            .header(Row::new(vec!["Name", "Size", "Downloaded", "DL Rate", "Peers"]).bottom_margin(1))
            .widths(&[
                Constraint::Percentage(35),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(10),
                Constraint::Percentage(35)
            ])
        ;

        f.render_widget(table, chunks[0]);
        f.render_widget(gauge, chunks[1]);
    }

    fn draw_tabs(&mut self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect, tabs_area: Rect) {
        let tab_titles = vec![
            Spans::from(Span::styled("Files", Style::default())),
            Spans::from(Span::styled("Peers", Style::default())),
            Spans::from(Span::styled("Pieces", Style::default()))
        ];

        let tabs = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::ALL))
            .highlight_style(Style::default().fg(Color::Yellow))
            .select(self.selected_tab)
        ;

        f.render_widget(tabs, area);

        match self.selected_tab {
            0 => self.draw_files_tab(f, tabs_area),
            1 => self.draw_peers_tab(f, tabs_area),
            2 => self.draw_pieces_tab(f, tabs_area),
            _ => {}
        }
    }

    fn draw_files_tab(&mut self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let mut rows = Vec::new();

        for (file, size) in self.torrent_info.data().get_files() {
            rows.push(Row::new(vec![file.clone(), App::bytes_data(*size as u32)]));
        }

        let table = Table::new(rows)
            .block(Block::default().borders(Borders::ALL))
            .header(Row::new(vec!["Name", "Size"]).bottom_margin(1))
            .widths(&[
                Constraint::Percentage(60),
                Constraint::Percentage(40)
            ])
            .highlight_symbol("> ")
            .highlight_style(Style::default().fg(Color::Yellow))
        ;

        f.render_stateful_widget(table, area, &mut self.files_state);
    }

    fn draw_peers_tab(&mut self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let mut rows = Vec::new();

        let peers = self.torrent_info.torrent_peers();
        let peers_lock = peers.try_read();

        if let Ok(peers) = peers_lock {
            for peer in peers.iter() {
                if let Ok(lock) = peer.try_read() {
                    let now = Instant::now();
                    let start_time = lock.start_time();
                    let elapsed = now.checked_sub(start_time.elapsed()).unwrap().elapsed();
                    let rate = {
                        if elapsed.as_secs() > 0 {
                            lock.downloaded_total() / elapsed.as_secs() as usize
                        }
                        else {
                            0
                        }
                    };

                    rows.push(Row::new(vec![
                        lock.address().to_string(),
                        format!("{}", lock.status()),
                        App::bytes_data(lock.downloaded_total() as u32),
                        App::bytes_data(lock.uploaded_total() as u32),
                        App::bytes_rate(rate as u32),
                        {
                            if let Some(message) = lock.last_message_sent() {
                                format!("{}", message)
                            }
                            else {
                                String::from("-")
                            }
                        },
                        {
                            if let Some(message) = lock.last_message_received() {
                                format!("{}", message)
                            }
                            else {
                                String::from("-")
                            }
                        }
                    ]));
                }
            }
    
            let table = Table::new(rows)
                .block(Block::default().borders(Borders::ALL))
                .header(Row::new(vec![
                    "Address",
                    "Status",
                    "Downloaded",
                    "Uploaded",
                    "DL Rate",
                    "Last Message Sent",
                    "Last Message Received"
                ]).bottom_margin(1))
                .widths(&[
                    Constraint::Percentage(20),
                    Constraint::Percentage(10),
                    Constraint::Percentage(10),
                    Constraint::Percentage(10),
                    Constraint::Percentage(10),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20)
                ])
                .highlight_symbol("> ")
                .highlight_style(Style::default().fg(Color::Yellow))
            ;
            
            f.render_stateful_widget(table, area, &mut self.peers_state);
        }
    }

    fn draw_pieces_tab(&mut self, f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
        let mut rows = Vec::new();

        let pieces = self.torrent_info.torrent_pieces();
        let pieces_lock = pieces.try_read();

        if let Ok(pieces) = pieces_lock {
            for (i, piece) in pieces.iter().enumerate() {
                rows.push(Row::new(vec![
                    i.to_string(),
                    App::bytes_data(piece.get_len() as u32),
                    App::bytes_data(piece.get_downloaded() as u32),
                    if piece.requested() {String::from("Yes")} else {String::from("No")},
                    if piece.finished() {String::from("Yes")} else {String::from("No")}
                ]));
            }
    
            let table = Table::new(rows)
                .block(Block::default().borders(Borders::ALL))
                .header(Row::new(vec!["Number", "Size", "Downloaded", "Requested", "Completed"]).bottom_margin(1))
                .widths(&[
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20)
                ])
                .highlight_symbol("> ")
                .highlight_style(Style::default().fg(Color::Yellow))
            ;
            
            f.render_stateful_widget(table, area, &mut self.piece_state);
        }
    }

    fn bytes_data(bytes: u32) -> String {
        let mut bytes = bytes;

        if bytes > 1024 {
            bytes /= 1024;

            if bytes < 1024 {
                return format!("{}KB", bytes);
            }
            else {
                bytes /= 1024;

                if bytes < 1024 {
                    return format!("{}MB", bytes);
                }
                else {
                    bytes /= 1024;
                    
                    return format!("{}GB", bytes);
                }
            }
        }
        else {
            return format!("{}B", bytes);
        }

        String::new()
    }

    fn bytes_rate(bytes: u32) -> String {
        let mut bytes = bytes;

        if bytes > 1024 {
            bytes /= 1024;

            if bytes < 1024 {
                return format!("{}KB/s", bytes);
            }
            else {
                bytes /= 1024;

                if bytes < 1024 {
                    return format!("{}MB/s", bytes);
                }
                else {
                    bytes /= 1024;
                    
                    return format!("{}GB/s", bytes);
                }
            }
        }
        else {
            return format!("{}B/s", bytes);
        }

        String::new()
    }
}
