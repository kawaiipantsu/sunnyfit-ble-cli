use crate::{
    config::Config,
    metrics::Workout,
    training::{self, Training},
};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Gauge, Paragraph, Tabs, Widget, Wrap},
    Frame, Terminal,
};
use std::{
    collections::VecDeque,
    io::{self, Stderr},
    time::Duration,
};
const TEAL: Color = Color::Rgb(72, 225, 195);
const GOLD: Color = Color::Rgb(255, 201, 100);
const DIM: Color = Color::Rgb(110, 136, 163);
const BG: Color = Color::Rgb(12, 20, 32);
const TABS: [&str; 9] = [
    "Ride",
    "Gauges",
    "Intervals",
    "Ghost",
    "Routes",
    "Town",
    "Games",
    "Desk",
    "Progress",
];
pub enum Action {
    None,
    Quit,
    Pause,
    Marker,
    Focus,
    ResetFocus,
}
pub struct Ui {
    terminal: Terminal<CrosstermBackend<Stderr>>,
    history: VecDeque<Option<f64>>,
    last_second: Option<u64>,
    tab: usize,
    game: usize,
}
fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
}
fn panel(title: impl Into<String>) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", title.into()))
        .border_style(Style::default().fg(DIM))
}
pub fn clock(v: f64) -> String {
    let s = v.max(0.) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}
fn metric(v: Option<f64>, unit: &str) -> String {
    v.map(|v| format!("{v:.1} {unit}"))
        .unwrap_or_else(|| format!("— {unit}"))
}
fn gauge(f: &mut Frame, area: Rect, title: &str, ratio: f64, label: String, color: Color) {
    f.render_widget(
        Gauge::default()
            .ratio(ratio.clamp(0., 1.))
            .use_unicode(true)
            .gauge_style(Style::default().fg(color).bg(Color::Rgb(26, 39, 55)))
            .label(label)
            .block(panel(title)),
        area,
    );
}
struct SpeedGraph<'a> {
    values: &'a VecDeque<Option<f64>>,
}
impl Widget for SpeedGraph<'_> {
    fn render(self, area: Rect, b: &mut Buffer) {
        if area.width < 5 || area.height < 3 {
            return;
        }
        let valid: Vec<f64> = self.values.iter().flatten().copied().collect();
        let max = valid.iter().copied().reduce(f64::max).unwrap_or(0.);
        let min = valid.iter().copied().reduce(f64::min).unwrap_or(0.);
        let top = (max * 1.18).max(10.);
        let h = area.height - 1;
        for value in [min, max] {
            let y = area.bottom() - 1 - (value / top * h as f64).round() as u16;
            for x in area.x..area.right() {
                b[(x, y)].set_symbol("┄").set_fg(Color::Rgb(40, 65, 79));
            }
        }
        let width = area.width as usize;
        let skip = self.values.len().saturating_sub(width);
        for (i, value) in self.values.iter().skip(skip).enumerate() {
            let Some(value) = value else {
                continue;
            };
            let x = area.x + i as u16;
            let level = (value / top * h as f64).clamp(0., h as f64);
            let y = area.bottom() - 1 - level.floor() as u16;
            for fill in y + 1..area.bottom() {
                let t = (area.bottom() - fill) as f64 / area.height as f64;
                b[(x, fill)].set_symbol("█").set_fg(Color::Rgb(
                    (15. + t * 20.) as u8,
                    (45. + t * 120.) as u8,
                    (60. + t * 100.) as u8,
                ));
            }
            b[(x, y)]
                .set_symbol(if level.fract() > 0.5 { "▀" } else { "▁" })
                .set_fg(TEAL);
        }
        b.set_string(
            area.x + 1,
            area.y,
            format!("MAX {max:.1}  ·  MIN {min:.1} km/h"),
            Style::default().fg(GOLD),
        );
    }
}
impl Ui {
    pub fn new() -> Result<Self> {
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
                last_second: None,
                tab: 0,
                game: 0,
            }),
            Err(e) => {
                restore();
                Err(e.into())
            }
        }
    }
    pub fn input(&mut self) -> Result<Action> {
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Tab => self.tab = (self.tab + 1) % TABS.len(),
                    KeyCode::BackTab => self.tab = (self.tab + TABS.len() - 1) % TABS.len(),
                    KeyCode::Char(n @ '1'..='9') => self.tab = n as usize - '1' as usize,
                    KeyCode::Char('g') => self.game = (self.game + 1) % 2,
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(Action::Quit),
                    KeyCode::Char(' ') | KeyCode::Char('p') => return Ok(Action::Pause),
                    KeyCode::Char('m') => return Ok(Action::Marker),
                    KeyCode::Enter if self.tab == 7 => return Ok(Action::Focus),
                    KeyCode::Char('r') if self.tab == 7 => return Ok(Action::ResetFocus),
                    _ => {}
                }
            }
        }
        Ok(Action::None)
    }
    pub fn draw(&mut self, w: &Workout, c: &Config, demo: bool, t: &Training) -> Result<()> {
        let second = w.elapsed_s as u64;
        if self.last_second != Some(second) {
            self.history.push_back(w.current.speed_kmh);
            if self.history.len() > 240 {
                self.history.pop_front();
            }
            self.last_second = Some(second);
        }
        let tab = self.tab;
        let game = self.game;
        let history = &self.history;
        self.terminal
            .draw(|f| draw(f, w, c, demo, t, tab, game, history))?;
        Ok(())
    }
}
impl Drop for Ui {
    fn drop(&mut self) {
        restore();
        let _ = self.terminal.show_cursor();
    }
}
#[allow(clippy::too_many_arguments)]
fn draw(
    f: &mut Frame,
    w: &Workout,
    c: &Config,
    demo: bool,
    t: &Training,
    tab: usize,
    game: usize,
    history: &VecDeque<Option<f64>>,
) {
    let area = f.area();
    f.render_widget(
        Block::default().style(Style::default().bg(BG).fg(Color::Rgb(215, 230, 240))),
        area,
    );
    if area.width < 64 || area.height < 24 {
        f.render_widget(
            Paragraph::new("SUNNYFIT\nResize to at least 64 × 24\nq to save and exit")
                .block(panel("Desk ride")),
            area,
        );
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(3),
        Constraint::Length(2),
    ])
    .split(area);
    let state = if w.paused {
        "PAUSED"
    } else if w.stale {
        "WAITING FOR DATA"
    } else {
        "LIVE"
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  S U N N Y F I T  ",
                Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if demo && w.source == "service" {
                    "DEMO · SERVICE STREAM  "
                } else if demo {
                    "DEMO · SIMULATED  "
                } else if w.source == "service" {
                    "SERVICE STREAM  "
                } else {
                    "DESK CYCLING CLUB  "
                },
                Style::default().fg(GOLD),
            ),
            Span::raw(format!(
                "{state}  ·  {}  ·  {}",
                w.connection,
                clock(w.elapsed_s)
            )),
        ]))
        .block(panel("Ride a little. Feel the difference.")),
        rows[0],
    );
    f.render_widget(
        Tabs::new(TABS)
            .select(tab)
            .divider("·")
            .style(Style::default().fg(DIM))
            .highlight_style(Style::default().fg(TEAL).add_modifier(Modifier::BOLD)),
        rows[1],
    );
    let cards = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(rows[2]);
    for (i, (name, value)) in [
        ("SPEED", metric(w.current.speed_kmh, "km/h")),
        ("CADENCE", metric(w.current.cadence_rpm, "rpm")),
        ("DISTANCE", metric(w.distance_m.map(|d| d / 1000.), "km")),
        ("HEART RATE", metric(w.current.heart_rate_bpm, "bpm")),
    ]
    .iter()
    .enumerate()
    {
        f.render_widget(
            Paragraph::new(format!(" {value}"))
                .style(
                    Style::default()
                        .fg(if i == 3 {
                            Color::Rgb(255, 136, 157)
                        } else {
                            TEAL
                        })
                        .add_modifier(Modifier::BOLD),
                )
                .block(panel(*name)),
            cards[i],
        );
    }
    match tab {
        0 => {
            let cols = Layout::horizontal([Constraint::Percentage(67), Constraint::Percentage(33)])
                .split(rows[3]);
            let block = panel("SPEED · last 240 seconds · gaps stay empty");
            let inner = block.inner(cols[0]);
            f.render_widget(block, cols[0]);
            f.render_widget(SpeedGraph { values: history }, inner);
            let text = vec![
                Line::styled(
                    format!(" ACTIVE  {}", clock(w.active_s)),
                    Style::default().fg(TEAL),
                ),
                Line::raw(format!(" ENERGY  {}", metric(w.calories_kcal, "kcal"))),
                Line::raw(format!(" AVERAGE {}", metric(w.average_speed_kmh, "km/h"))),
                Line::raw(format!(" PEAK    {}", metric(w.max_speed_kmh, "km/h"))),
                Line::raw(format!(" POWER   {}", metric(w.current.power_w, "W"))),
                Line::raw(""),
                Line::styled(
                    crate::food::message(w.calories_kcal),
                    Style::default().fg(GOLD),
                ),
                Line::raw(""),
                Line::styled("Tab opens your next adventure →", Style::default().fg(DIM)),
            ];
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(panel("TODAY'S RIDE")),
                cols[1],
            );
        }
        1 => {
            let parts = Layout::vertical([Constraint::Ratio(1, 3); 3]).split(rows[3]);
            gauge(
                f,
                parts[0],
                "SPEED · scale 50 km/h",
                w.current.speed_kmh.unwrap_or(0.) / 50.,
                metric(w.current.speed_kmh, "km/h"),
                TEAL,
            );
            gauge(
                f,
                parts[1],
                "CADENCE · comfortable target",
                w.current.cadence_rpm.unwrap_or(0.) / (c.target_cadence * 1.5),
                format!(
                    "{}  / target {:.0} rpm",
                    metric(w.current.cadence_rpm, "rpm"),
                    c.target_cadence
                ),
                GOLD,
            );
            gauge(
                f,
                parts[2],
                "ACTIVE TIME",
                w.active_s / (c.goal_minutes as f64 * 60.),
                format!("{} / {} min", clock(w.active_s), c.goal_minutes),
                Color::Rgb(152, 141, 255),
            );
        }
        2 => intervals(f, rows[3], w, t),
        3 => ghost(f, rows[3], w, t),
        4 => route_view(f, rows[3], c, t),
        5 => town(f, rows[3], w),
        6 => games(f, rows[3], w, t, game),
        7 => desk(f, rows[3], c, t),
        8 => progress(f, rows[3], c, t),
        _ => {}
    }
    gauge(
        f,
        rows[4],
        "SESSION GOAL",
        w.active_s / (c.goal_minutes as f64 * 60.),
        format!(
            "{} / {} min  ·  {}",
            clock(w.active_s),
            c.goal_minutes,
            t.toast
        ),
        TEAL,
    );
    f.render_widget(
        Paragraph::new(vec![
            Line::raw(
                " Tab / Shift-Tab views · 1–9 jump · space pause totals · m mark · q save & quit",
            ),
            Line::styled(
                " — unavailable/stale · energy may be estimated · routes are simulated, no GPS",
                Style::default().fg(DIM),
            ),
        ]),
        rows[5],
    );
}
fn intervals(f: &mut Frame, area: Rect, w: &Workout, t: &Training) {
    let mut lines = vec![
        Line::styled(
            format!(
                " {}  ·  {} / {}",
                t.plan.name,
                clock(t.interval_s),
                clock(t.plan.duration())
            ),
            Style::default().fg(TEAL),
        ),
        Line::raw(""),
    ];
    if let Some((lap, index, step, left)) = t.plan.current(t.interval_s) {
        lines.push(Line::styled(
            format!(
                " NOW  {} · round {}/{} · {} remaining",
                step.name,
                lap + 1,
                t.plan.repeats,
                clock(left)
            ),
            Style::default().fg(GOLD),
        ));
        lines.push(Line::raw(format!(
            " Target {:.0}–{:.0} rpm  ·  current {}",
            step.cadence_min,
            step.cadence_max,
            metric(w.current.cadence_rpm, "rpm")
        )));
        for (i, s) in t.plan.steps.iter().enumerate() {
            lines.push(Line::raw(format!(
                " {} {:22} {}  {:.0}–{:.0} rpm",
                if i == index { "▶" } else { " " },
                s.name,
                clock(s.seconds as f64),
                s.cadence_min,
                s.cadence_max
            )));
        }
    } else {
        lines.push(Line::styled(
            " PLAN COMPLETE · Easy spin, then enjoy the rest of your day.",
            Style::default().fg(GOLD),
        ));
    }
    lines.push(Line::raw(format!(
        "\n Time inside target: {} · freezes on pause or lost bike data",
        clock(t.in_target_s)
    )));
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel(
                "INTERVAL STUDIO · target guidance, no resistance control",
            )),
        area,
    );
}
fn ghost(f: &mut Frame, area: Rect, w: &Workout, t: &Training) {
    let Some(distance) = t.ghost.distance_at(w.active_s) else {
        f.render_widget(Paragraph::new("\n Race your previous ride.\n\n Start with --ghost latest, --ghost best or --ghost /path/to/workout\n A saved, non-demo session with distance creates your ghost.").wrap(Wrap{trim:false}).block(panel("GHOST RIDER")),area);
        return;
    };
    let yours = w.distance_m.unwrap_or(0.);
    let gap = yours - distance;
    let scale = yours.max(distance).max(100.);
    let width = area.width.saturating_sub(18) as f64;
    let track = |d: f64| {
        format!(
            "{}o─o",
            " ".repeat((d / scale * width).clamp(0., width) as usize)
        )
    };
    let text=format!("\n YOU   {}\n\n GHOST {}\n\n {:+.0} m {} · against {} · matched by active riding time\n Ghost {:.2} km · you {:.2} km",track(yours),track(distance),gap,if gap>=0.{"ahead"}else{"behind"},t.ghost.label,distance/1000.,yours/1000.);
    f.render_widget(
        Paragraph::new(text)
            .style(Style::default().fg(TEAL))
            .block(panel("GHOST RIDER · race your own rhythm")),
        area,
    );
}
fn route_view(f: &mut Frame, area: Rect, c: &Config, t: &Training) {
    let r = training::route(&c.route);
    let parts = Layout::vertical([Constraint::Length(3), Constraint::Min(4)]).split(area);
    let d = t.route_distance.min(r.length_m);
    gauge(
        f,
        parts[0],
        r.name,
        d / r.length_m,
        format!(
            "{:.2} / {:.0} km · {:.2} km remaining",
            d / 1000.,
            r.length_m / 1000.,
            (r.length_m - d) / 1000.
        ),
        TEAL,
    );
    let mut lines = vec![
        Line::styled(
            r"       /\       /\          .       ~     __",
            Style::default().fg(TEAL),
        ),
        Line::raw(r"  ____/  \_____/  \____  __/ \__  ________/  \____"),
        Line::raw(""),
    ];
    for (name, at) in r.stops {
        lines.push(Line::styled(
            format!(
                " {} {:18} {:>7.1} km  {}",
                if d >= *at { "◆" } else { "◇" },
                name,
                at / 1000.,
                if d >= *at {
                    "checkpoint reached"
                } else {
                    "ahead"
                }
            ),
            Style::default().fg(if d >= *at { TEAL } else { DIM }),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        " Fictional route distances · accumulated riding distance · no GPS or elevation",
    ));
    f.render_widget(
        Paragraph::new(lines).block(panel("POSTCARDS FROM YOUR DESK")),
        parts[1],
    );
}
fn town(f: &mut Frame, area: Rect, w: &Workout) {
    let skyline = [
        "        /\\             _______              /\\             _______    ",
        "       /  \\    ____    | CAFE  |      __    /  \\   ____     | BOOKS |   ",
        "      | [] |  | [] |   | [] [] |     |  |  | [] | | [] |    | [] [] |   ",
        "  ____|____|__|____|___|___o___|_____|__|__|____|_|____|____|___o___|___",
    ];
    let inner = panel("SUNNY TOWN · scenery moves with your riding distance").inner(area);
    let width = inner.width as usize;
    let shift = (w.distance_m.unwrap_or(0.) / 3.) as usize;
    let mut lines = vec![
        Line::styled(
            "       .             ☀                .              .",
            Style::default().fg(GOLD),
        ),
        Line::raw(""),
    ];
    for s in skyline {
        let chars: Vec<char> = s.chars().collect();
        let text: String = (0..width)
            .map(|i| chars[(i + shift) % chars.len()])
            .collect();
        lines.push(Line::styled(text, Style::default().fg(DIM)));
    }
    let rider = if w.current.cadence_rpm.unwrap_or(0.) > 0.
        && ((w.elapsed_s * 3.) as u64).is_multiple_of(2)
    {
        "     _o\n   _<,_\n  (*)/(*)"
    } else {
        "     _o\n   _<,_\n  (o)/(o)"
    };
    for s in rider.lines() {
        lines.push(Line::styled(
            format!("{}{}", " ".repeat(width / 3), s),
            Style::default().fg(TEAL),
        ));
    }
    let road: String = (0..width)
        .map(|i| if (i + shift) % 8 < 4 { '─' } else { ' ' })
        .collect();
    lines.push(Line::raw(road));
    lines.push(Line::raw(format!(
        " Town odyssey: {} · every 3 m scrolls the street",
        metric(w.distance_m, "m")
    )));
    f.render_widget(
        Paragraph::new(lines).block(panel(
            "SUNNY TOWN · scenery moves with your riding distance",
        )),
        area,
    );
}
fn games(f: &mut Frame, area: Rect, w: &Workout, t: &Training, game: usize) {
    let live = !w.paused && !w.stale && w.connection == "ready";
    if game == 0 {
        let target = Training::gate_target((t.game_s / 8.) as u64);
        let text=format!("\n CADENCE GATES                   SCORE {}\n\n         ╭──────────────────────────╮\n         │       {:.0}–{:.0} rpm         │\n         ╰──────────────────────────╯\n\n Match the band when the gate arrives in {:.1}s.\n Current {}  ·  {}\n\n g switches to Cadence Flight. Games freeze when bike data is stale.",t.gate_score,target-10.,target+10.,8.-t.game_s%8.,metric(w.current.cadence_rpm,"rpm"),if live{"IN PLAY"}else{"WAITING"});
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(GOLD))
                .block(panel("ARCADE · small moves, big scores")),
            area,
        );
    } else {
        let inner = panel("CADENCE FLIGHT · g changes game").inner(area);
        let h = inner.height.saturating_sub(3).max(3) as usize;
        let width = inner.width.max(10) as usize;
        let rpm = w.current.cadence_rpm.unwrap_or(0.);
        let target = Training::flight_target((t.game_s / 6.) as u64);
        let bird = ((1. - rpm / 120.).clamp(0., 1.) * (h - 1) as f64) as usize;
        let gap = ((1. - target / 120.) * (h - 1) as f64) as usize;
        let x = ((1. - t.game_s % 6. / 6.) * (width - 5) as f64) as usize + 3;
        let mut lines = vec![Line::styled(
            format!(
                " CLEARED {} · MISSED {} · target {:.0} ± 15 rpm",
                t.flight_score, t.flight_misses, target
            ),
            Style::default().fg(GOLD),
        )];
        for y in 0..h {
            let mut row = vec![' '; width];
            if y.abs_diff(gap) > 1 {
                row[x] = '║';
            }
            if y == bird {
                row[3] = '>';
                row[4] = 'o';
            }
            lines.push(Line::styled(
                row.iter().collect::<String>(),
                Style::default().fg(TEAL),
            ));
        }
        lines.push(Line::raw(
            " Cadence lifts your glider. Pass the opening when the wall reaches you.",
        ));
        f.render_widget(
            Paragraph::new(lines).block(panel("CADENCE FLIGHT · g changes game")),
            area,
        );
    }
}
fn desk(f: &mut Frame, area: Rect, c: &Config, t: &Training) {
    let cycle = (c.focus_minutes + c.break_minutes) as f64 * 60.;
    let within = t.focus_s % cycle;
    let focus = within < c.focus_minutes as f64 * 60.;
    let remaining = if focus {
        c.focus_minutes as f64 * 60. - within
    } else {
        cycle - within
    };
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let text=format!("\n {}        {} {}\n\n Focus {} min / break {} min · completed blocks {}\n\n Today's movement: {} / {} min\n Movement reminder: {} min · cadence alert: below {:.0} rpm for {}s\n\n Enter pauses/resumes focus · r restarts the Pomodoro clock\n Focus time continues through bike disconnects.\n\n {}",if focus{"FOCUS TIME"}else{"TAKE A BREAK"},clock(remaining),if t.focus_paused{"PAUSED"}else{""},c.focus_minutes,c.break_minutes,(t.focus_s/cycle)as u64,clock(t.progress.daily(&day)),c.daily_goal_minutes,c.movement_reminder_minutes,c.cadence_alert_rpm,c.cadence_alert_seconds,t.toast);
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(TEAL))
            .block(panel("DESK COMPANION")),
        area,
    );
}
fn progress(f: &mut Frame, area: Rect, c: &Config, t: &Training) {
    let day = chrono::Local::now().format("%Y-%m-%d").to_string();
    let xp = t.progress.xp();
    let mut lines = vec![
        Line::styled(
            format!(
                " LEVEL {}    {} XP    {:.2} km    {} active",
                1 + xp / 500,
                xp,
                t.progress.distance() / 1000.,
                clock(t.progress.active())
            ),
            Style::default().fg(GOLD),
        ),
        Line::raw(""),
        Line::raw(format!(
            " Daily challenge: {} / {} min",
            clock(t.progress.daily(&day)),
            Training::daily_target()
        )),
        Line::raw(format!(" Daily active goal: {} min", c.daily_goal_minutes)),
        Line::raw(""),
    ];
    for (name, earned) in t.progress.achievements() {
        lines.push(Line::styled(
            format!(" {} {}", if earned { "★" } else { "☆" }, name),
            Style::default().fg(if earned { TEAL } else { DIM }),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::raw(
        " XP: 10 per active minute + 25 per kilometre · demo progress is temporary",
    ));
    f.render_widget(
        Paragraph::new(lines).block(panel("YOUR LITTLE VICTORIES")),
        area,
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_views_render_at_supported_sizes() {
        let c = Config::default();
        let dir = std::env::temp_dir();
        let t = Training::new(&c, &dir, "render", true, &dir).unwrap();
        let w = Workout::default();
        for (width, height) in [(64, 24), (124, 36)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for tab in 0..9 {
                terminal
                    .draw(|f| draw(f, &w, &c, true, &t, tab, 0, &VecDeque::new()))
                    .unwrap();
            }
        }
    }
}
