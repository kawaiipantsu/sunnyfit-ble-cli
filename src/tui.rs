use crate::{config::Config, metrics::Workout};
use anyhow::{bail, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Sparkline},
    Terminal,
};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal},
    time::Duration,
};
pub struct Ui {
    terminal: Terminal<CrosstermBackend<io::Stderr>>,
    history: VecDeque<u64>,
}
pub enum Action {
    Quit,
    Pause,
    Marker,
    None,
}
fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
}
fn metric(v: Option<f64>, unit: &str) -> String {
    v.map(|v| format!("{v:.1} {unit}"))
        .unwrap_or_else(|| format!("— {unit}"))
}
pub fn clock(s: f64) -> String {
    let n = s.max(0.) as u64;
    format!("{:02}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60)
}
impl Ui {
    pub fn new() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            bail!("--workout needs a terminal on stdin and stderr; use capture for headless recording");
        }
        enable_raw_mode()?;
        if let Err(e) = execute!(io::stderr(), EnterAlternateScreen) {
            restore();
            return Err(e.into());
        }
        let old = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            old(info);
        }));
        match Terminal::new(CrosstermBackend::new(io::stderr())) {
            Ok(terminal) => Ok(Self {
                terminal,
                history: VecDeque::new(),
            }),
            Err(e) => {
                restore();
                Err(e.into())
            }
        }
    }
    pub fn input(&self) -> Result<Action> {
        if event::poll(Duration::ZERO)? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Release {
                    return Ok(match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
                        KeyCode::Char('c')
                            if k.modifiers.contains(event::KeyModifiers::CONTROL) =>
                        {
                            Action::Quit
                        }
                        KeyCode::Char(' ') | KeyCode::Char('p') => Action::Pause,
                        KeyCode::Char('m') => Action::Marker,
                        _ => Action::None,
                    });
                }
            }
        }
        Ok(Action::None)
    }
    pub fn draw(
        &mut self,
        w: &Workout,
        c: &Config,
        demo: bool,
        raw: u64,
        unknown: u64,
        redis: &str,
    ) -> Result<()> {
        self.history
            .push_back(w.current.cadence_rpm.unwrap_or(0.) as u64);
        if self.history.len() > 120 {
            self.history.pop_front();
        }
        let history: Vec<u64> = self.history.iter().copied().collect();
        let accent = Color::Rgb(92, 225, 204);
        let gold = Color::Rgb(255, 200, 87);
        let muted = Color::Rgb(145, 158, 180);
        self.terminal.draw(|f|{
            let area=f.area();if area.width<60||area.height<20{f.render_widget(Paragraph::new("SUNNYFIT\nResize to at least 60 × 20\nq to save and exit").block(Block::bordered()),area);return;}
            let rows=Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(3),Constraint::Length(5),Constraint::Length(5),Constraint::Min(4),Constraint::Length(3),Constraint::Length(3)]).split(area);
            let state=if w.paused{"PAUSED"}else if w.stale{"WAITING FOR DATA"}else if w.current.speed_kmh.is_some_and(|v|v>0.) || w.current.cadence_rpm.is_some_and(|v|v>0.) {"IN MOTION"}else{"READY"};
            f.render_widget(Paragraph::new(Line::from(vec![Span::styled("  S U N N Y F I T  ",Style::default().fg(accent).add_modifier(Modifier::BOLD)),Span::raw(if demo{" / DEMO • SIMULATED DATA  "}else{" / DESK RIDE  "}),Span::styled(format!("{} · {state}",w.connection),Style::default().fg(gold))])).block(Block::bordered().border_style(Style::default().fg(accent))),rows[0]);
            for (row,items) in [(rows[1],vec![("SPEED",metric(w.current.speed_kmh,"km/h")),("CADENCE",metric(w.current.cadence_rpm,"rpm")),("DISTANCE",metric(w.distance_m.map(|v|v/1000.),"km"))]),(rows[2],vec![("SESSION / ACTIVE",format!("{} / {}",clock(w.elapsed_s),clock(w.active_s))),("ENERGY · ESTIMATE",metric(w.calories_kcal,"kcal")),("POWER / HEART",format!("{} / {}",metric(w.current.power_w,"W"),metric(w.current.heart_rate_bpm,"bpm")))])]{
                let cols=Layout::horizontal([Constraint::Percentage(33),Constraint::Percentage(34),Constraint::Percentage(33)]).split(row);
                for (i,(title,value)) in items.into_iter().enumerate(){f.render_widget(Paragraph::new(format!("\n {value}")).style(Style::default().fg(accent).add_modifier(Modifier::BOLD)).block(Block::bordered().title(format!(" {title} ")).border_style(Style::default().fg(muted))),cols[i]);}
            }
            f.render_widget(Sparkline::default().data(&history).style(Style::default().fg(accent)).block(Block::bordered().title(" CADENCE · recent samples · gaps shown at baseline ")),rows[3]);
            let goal=c.goal_minutes as f64*60.;let ratio=(w.active_s/goal).clamp(0.,1.);
            f.render_widget(Gauge::default().ratio(ratio).gauge_style(Style::default().fg(gold)).label(format!("{} active / {} min goal  •  {}",clock(w.active_s),c.goal_minutes,if ratio>=1.{"Goal reached. Every turn counts."}else if w.stale{"Waiting for live measurements"}else if w.current.cadence_rpm.unwrap_or(0.)>=c.target_cadence{"Smooth and steady. You're on pace."}else{"Find a comfortable rhythm."})).block(Block::bordered()),rows[4]);
            let text=vec![Line::from(format!(" q save & quit   space pause totals   m mark   raw {raw} · undecoded {unknown} · Redis {redis}")),Line::styled(" — unavailable/stale · calibrated/estimated values are labelled in saved metrics",Style::default().fg(muted))];
            f.render_widget(Paragraph::new(text).block(Block::default().borders(Borders::TOP)),rows[5]);
        })?;
        Ok(())
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        restore();
        let _ = self.terminal.show_cursor();
    }
}
