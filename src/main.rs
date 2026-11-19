#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Local, LocalResult, NaiveTime, TimeZone};
use eframe::egui::{self, Color32, RichText, Stroke};
use enigo::{Direction::Click, Enigo, Key, Keyboard, Settings};
use windows_sys::Win32::{
    Foundation::{HWND, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, SetEvent},
    UI::{
        Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_ESCAPE},
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, IsWindow, MSG, MWMO_INPUTAVAILABLE,
            MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_HOTKEY, WM_HOTKEY,
        },
    },
};

const BLACK: Color32 = Color32::BLACK;
const WHITE: Color32 = Color32::WHITE;
const MUTED: Color32 = Color32::from_gray(160);
const BORDER: Color32 = Color32::from_gray(30);
const HOTKEY_ID: i32 = 1;

fn main() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/autotyper.png"))
        .expect("the bundled AutoTyper icon must be a valid PNG");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_icon(icon)
            .with_inner_size([480.0, 540.0])
            .with_min_inner_size([390.0, 425.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AutoTyper",
        options,
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.window_fill = BLACK;
            visuals.panel_fill = BLACK;
            visuals.extreme_bg_color = BLACK;
            visuals.window_stroke = Stroke::new(1.0, BORDER);
            visuals.override_text_color = Some(WHITE);
            visuals.weak_text_color = Some(MUTED);
            visuals.selection.bg_fill = Color32::from_gray(55);
            visuals.selection.stroke = Stroke::new(1.0, WHITE);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
            visuals.widgets.inactive.bg_fill = Color32::from_gray(11);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(11);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, WHITE);
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            cc.egui_ctx.set_visuals_of(egui::Theme::Dark, visuals);
            cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
                style.spacing.item_spacing = egui::vec2(8.0, 6.0);
                style.spacing.interact_size.y = 24.0;
                style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
                style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            });
            Ok(Box::new(AutoTyper::default()))
        }),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum TimingMode {
    Wpm,
    Duration,
    FinishAt,
}

#[derive(Clone, Copy)]
struct Pause {
    enabled: bool,
    seconds: f64,
}

struct Options {
    mode: TimingMode,
    wpm: u32,
    total_seconds: f64,
    finish_at: String,
    countdown: u32,
    pauses: [Pause; 3],
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: TimingMode::Wpm,
            wpm: 65,
            total_seconds: 60.0,
            finish_at: String::new(),
            countdown: 4,
            pauses: [
                Pause { enabled: true, seconds: 5.0 },
                Pause { enabled: false, seconds: 0.5 },
                Pause { enabled: false, seconds: 1.0 },
            ],
        }
    }
}

impl Options {
    fn timing(&self, now: DateTime<Local>) -> Result<Timing, String> {
        let pace = match self.mode {
            TimingMode::Wpm => Pace::Wpm(self.wpm),
            TimingMode::Duration => Pace::Duration(
                Duration::try_from_secs_f64(self.total_seconds)
                    .map_err(|_| "Enter a valid duration in seconds.".to_owned())?,
            ),
            TimingMode::FinishAt => Pace::FinishAt(next_finish(&self.finish_at, now)?.into()),
        };
        let mut pauses = [Duration::ZERO; 3];
        for (configured, duration) in self.pauses.iter().zip(&mut pauses) {
            if configured.enabled {
                *duration = Duration::try_from_secs_f64(configured.seconds)
                    .map_err(|_| "Enter a valid duration for each enabled pause.".to_owned())?;
            }
        }
        Ok(Timing { pace, pauses })
    }
}

fn next_finish<Tz: TimeZone>(input: &str, now: DateTime<Tz>) -> Result<DateTime<Tz>, String> {
    let input = input.trim();
    let time = NaiveTime::parse_from_str(input, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(input, "%H:%M"))
        .map_err(|_| "Enter a 24-hour time such as 17:30:00.".to_owned())?;
    let today = now.date_naive();
    let zone = now.timezone();
    for day in [Some(today), today.succ_opt()].into_iter().flatten() {
        let candidates = match zone.from_local_datetime(&day.and_time(time)) {
            LocalResult::Single(time) => [Some(time), None],
            LocalResult::Ambiguous(first, second) => [Some(first), Some(second)],
            LocalResult::None => [None, None],
        };
        if let Some(finish) = candidates.into_iter().flatten().find(|time| *time > now) {
            return Ok(finish);
        }
    }
    Err("That time is unavailable. Choose a different finish time.".into())
}

#[derive(Clone, Copy)]
enum Pace {
    Wpm(u32),
    Duration(Duration),
    FinishAt(SystemTime),
}

#[derive(Clone, Copy)]
struct Timing {
    pace: Pace,
    pauses: [Duration; 3],
}

impl Timing {
    fn pause_after(self, ch: char) -> Duration {
        match ch {
            '.' => self.pauses[0],
            ',' => self.pauses[1],
            '\n' => self.pauses[2],
            _ => Duration::ZERO,
        }
    }
}

struct Schedule {
    from: Instant,
    wall_from: SystemTime,
    deadlines: Vec<Duration>,
}

impl Schedule {
    fn plan(
        remaining: &[char],
        timing: Timing,
        started: Instant,
        now: Instant,
        wall_now: SystemTime,
        previous: Option<(char, Instant)>,
    ) -> Result<Self, String> {
        let count = remaining.len();
        let initial_pause = previous.map_or(Duration::ZERO, |(ch, sent_at)| {
            timing.pause_after(ch).saturating_sub(now.saturating_duration_since(sent_at))
        });
        let available = match timing.pace {
            Pace::Wpm(_) => None,
            Pace::Duration(total) => Some(total.saturating_sub(now.saturating_duration_since(started))),
            Pace::FinishAt(finish) => Some(finish.duration_since(wall_now).unwrap_or_default()),
        };
        let typing_budget = if let Some(available) = available {
            if available.is_zero() {
                return Err("Set a finish time that leaves time to type after the countdown.".into());
            }
            let pauses = initial_pause + remaining[..count.saturating_sub(1)]
                .iter().map(|&ch| timing.pause_after(ch)).sum::<Duration>();
            if available <= pauses {
                return Err(format!(
                    "The remaining pauses take {:.1} s. Set a later finish or shorten the pauses.",
                    pauses.as_secs_f64()
                ));
            }
            Some(available - pauses)
        } else {
            None
        };
        let seconds_per_char = match timing.pace {
            Pace::Wpm(wpm) => 60.0 / (5.0 * f64::from(wpm)),
            _ => 0.0,
        };

        let mut typing = Duration::ZERO;
        let mut pauses = initial_pause;
        let mut deadlines = Vec::with_capacity(count);
        for (index, &ch) in remaining.iter().enumerate() {
            typing = match typing_budget {
                Some(budget) => budget.mul_f64((index + 1) as f64 / count as f64),
                None => typing + Duration::from_secs_f64(seconds_per_char * rand::random_range(0.8..=1.2)),
            };
            deadlines.push(typing + pauses);
            pauses += timing.pause_after(ch);
        }
        Ok(Self { from: now, wall_from: wall_now, deadlines })
    }

    fn next(&self, index: usize) -> Instant {
        self.from + self.deadlines[index]
    }

    fn clock_shifted(&self, now: Instant, wall_now: SystemTime) -> bool {
        let expected = self.wall_from + now.saturating_duration_since(self.from);
        wall_now.duration_since(expected).unwrap_or_else(|error| error.duration())
            > Duration::from_millis(250)
    }
}

#[derive(Default)]
enum Status {
    #[default]
    Ready,
    Starting,
    Countdown(Instant),
    Typing,
    FocusPaused,
    Stopping,
    Stopped,
    Completed,
    Error(String),
}

enum Update {
    Status(Status),
    Progress(usize),
    Timing(Result<Timing, String>),
}

struct Events {
    stop: OwnedHandle,
    timing: OwnedHandle,
}

#[derive(Debug, PartialEq, Eq)]
enum Wait {
    Due,
    Stop,
    Timing,
}

impl Events {
    fn new() -> io::Result<Self> {
        Ok(Self { stop: new_event(true)?, timing: new_event(false)? })
    }

    fn wait(&self, deadline: Instant) -> Result<Wait, String> {
        let handles = [self.stop.as_raw_handle(), self.timing.as_raw_handle()];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let millis = remaining.as_nanos().div_ceil(1_000_000).min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: both handles remain owned for the entirety of the call.
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(2, handles.as_ptr(), millis, QS_HOTKEY, MWMO_INPUTAVAILABLE)
            };
            match result {
                WAIT_OBJECT_0 => return Ok(Wait::Stop),
                WAIT_TIMEOUT if Instant::now() >= deadline => return Ok(Wait::Due),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => return Err(format!("The typing timer failed. {}", io::Error::last_os_error())),
                ready if ready == WAIT_OBJECT_0 + 1 => return Ok(Wait::Timing),
                ready if ready == WAIT_OBJECT_0 + 2 => {
                    let mut msg = MSG::default();
                    // SAFETY: the message is a writable output on the hotkey's owning thread.
                    if unsafe { PeekMessageW(&mut msg, ptr::null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE) } != 0
                        && msg.wParam == HOTKEY_ID as usize
                    {
                        return Ok(Wait::Stop);
                    }
                }
                _ => return Err("Windows returned an unexpected result from the typing timer.".into()),
            }
        }
    }
}

fn new_event(manual_reset: bool) -> io::Result<OwnedHandle> {
    // SAFETY: null attributes and name create a private, initially unsignaled event.
    let raw = unsafe { CreateEventW(ptr::null(), manual_reset.into(), 0, ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateEventW returned a new, owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn signal(event: &OwnedHandle) -> io::Result<()> {
    // SAFETY: the owned handle remains live during the call.
    if unsafe { SetEvent(event.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct ActiveRun {
    events: Arc<Events>,
    changes: mpsc::Sender<Timing>,
    updates: mpsc::Receiver<Update>,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        let _ = signal(&self.events.stop);
    }
}

#[derive(Default)]
struct AutoTyper {
    text: String,
    options: Options,
    always_on_top: bool,
    status: Status,
    active: Option<ActiveRun>,
    reading: bool,
    timing_open: bool,
    applied_timing: Option<Timing>,
    timing_feedback: Option<Result<(), String>>,
    sent: usize,
    total: usize,
}

impl AutoTyper {
    fn start(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let text: Vec<char> = self.text.chars().collect();
        if text.is_empty() {
            return Err("Enter text to type.".into());
        }
        if text.iter().any(|&ch| ch.is_control() && !matches!(ch, '\n' | '\t')) {
            return Err("The text contains an unsupported control character.".into());
        }
        let timing = self.options.timing(Local::now())?;
        let countdown = Duration::from_secs(u64::from(self.options.countdown));
        let events = Arc::new(Events::new().map_err(|error| format!("Could not start typing. {error}"))?);
        let worker_events = Arc::clone(&events);
        let (sender, updates) = mpsc::channel();
        let (changes, worker_changes) = mpsc::channel();
        let ctx = ctx.clone();
        let total = text.len();
        thread::Builder::new()
            .name("typing".into())
            .spawn(move || {
                let report = |update| {
                    let _ = sender.send(update);
                    ctx.request_repaint();
                };
                let result = run_job(&text, timing, countdown, &worker_events, &worker_changes, &report);
                report(Update::Status(match result {
                    Ok(true) => Status::Completed,
                    Ok(false) => Status::Stopped,
                    Err(message) => Status::Error(message),
                }));
            })
            .map_err(|error| format!("Could not start typing. {error}"))?;
        self.active = Some(ActiveRun { events, changes, updates });
        self.applied_timing = Some(timing);
        self.timing_feedback = None;
        self.timing_open = false;
        self.sent = 0;
        self.total = total;
        self.status = Status::Starting;
        Ok(())
    }

    fn apply_timing(&mut self) {
        let Some(run) = &self.active else { return };
        let result = self.options.timing(Local::now()).and_then(|timing| {
            run.changes.send(timing).map_err(|_| "The run has ended.".to_owned())?;
            signal(&run.events.timing).map_err(|error| format!("Could not apply timing. {error}"))
        });
        self.timing_feedback = result.err().map(Err);
    }
}

impl eframe::App for AutoTyper {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(run) = &self.active {
            loop {
                match run.updates.try_recv() {
                    Ok(Update::Progress(sent)) => self.sent = sent,
                    Ok(Update::Timing(result)) => {
                        self.timing_feedback = Some(result.map(|timing| {
                            self.applied_timing = Some(timing);
                        }));
                    }
                    Ok(Update::Status(status @ (Status::Completed | Status::Stopped | Status::Error(_)))) => {
                        self.status = status;
                        self.active = None;
                        self.timing_open = false;
                        break;
                    }
                    Ok(Update::Status(status)) => {
                        if matches!(status, Status::Typing) {
                            self.reading = true;
                        }
                        if !matches!(self.status, Status::Stopping) {
                            self.status = status;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        break;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.status = Status::Error("The typing thread ended unexpectedly.".into());
                        self.active = None;
                        self.timing_open = false;
                        break;
                    }
                }
            }
        }
        if matches!(self.status, Status::Countdown(_)) {
            ctx.request_repaint_after(Duration::from_millis(75));
        } else if self.options.mode == TimingMode::FinishAt && (!self.reading || self.timing_open) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::bottom("actions")
            .resizable(false)
            .frame(egui::Frame::new().fill(BLACK).inner_margin(egui::Margin::symmetric(22, 12)))
            .show(ui, |ui| self.actions(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BLACK).inner_margin(egui::Margin::symmetric(22, 12)))
            .show(ui, |ui| {
                if self.reading {
                    self.reading_view(ui);
                } else {
                    egui::ScrollArea::vertical().id_salt("setup").show(ui, |ui| self.editor(ui));
                }
            });
    }
}

impl AutoTyper {
    fn editor(&mut self, ui: &mut egui::Ui) {
        let enabled = self.active.is_none();
        ui.horizontal(|ui| {
            ui.label(RichText::new("AutoTyper").size(28.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.always_on_top, "Always on Top").changed() {
                    let level = if self.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                }
            });
        });
        ui.add_space(10.0);

        ui.add_enabled_ui(enabled, |ui| {
            let editor_label = ui.horizontal(|ui| {
                let label = ui.label(RichText::new("Text to type").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!(
                        "{} characters  ·  {} words", self.text.chars().count(), self.text.split_whitespace().count()
                    )).weak().size(12.0));
                });
                label.id
            }).inner;
            egui::Frame::new().stroke(Stroke::new(1.0, BORDER))
                .inner_margin(egui::Margin::symmetric(10, 6)).corner_radius(8)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("editor").max_height(123.0).show(ui, |ui| {
                        if ui.add(egui::TextEdit::multiline(&mut self.text)
                            .desired_rows(7).desired_width(f32::INFINITY)
                            .hint_text("Paste a paragraph or type here").frame(egui::Frame::new()))
                            .labelled_by(editor_label).changed() && self.text.contains('\r')
                        {
                            self.text = self.text.replace("\r\n", "\n").replace('\r', "\n");
                        }
                    });
                });
            ui.add_space(10.0);
            timing_fields(ui, &mut self.options);
            ui.add_space(5.0);
            ui.separator();
            ui.horizontal(|ui| {
                let label = ui.label("Start countdown");
                ui.add(egui::DragValue::new(&mut self.options.countdown).range(1..=60).suffix(" s"))
                    .labelled_by(label.id);
            });
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(ui.available_width());
        ui.separator();
        if self.reading && self.timing_open {
            egui::ScrollArea::vertical().id_salt("live-timing").max_height(215.0).show(ui, |ui| {
                timing_fields(ui, &mut self.options);
                if ui.button("Apply timing").clicked() {
                    self.apply_timing();
                }
                match &self.timing_feedback {
                    Some(Ok(())) => { ui.label(RichText::new("Timing updated").weak()); }
                    Some(Err(message)) => { ui.label(message); }
                    None => {}
                }
            });
            ui.separator();
        }

        let status = match &self.status {
            Status::Ready | Status::Typing | Status::Completed => None,
            Status::Starting => Some("Starting…".to_owned()),
            Status::Countdown(end) => Some(format!(
                "Click your target field · {:.0} s",
                end.saturating_duration_since(Instant::now()).as_secs_f64().ceil()
            )),
            Status::FocusPaused => Some("Focus the original target to resume".to_owned()),
            Status::Stopping => Some("Stopping…".to_owned()),
            Status::Stopped if !self.reading => Some("Stopped".to_owned()),
            Status::Stopped => None,
            Status::Error(message) => Some(message.clone()),
        };
        if let Some(status) = status {
            ui.label(status);
        }

        ui.horizontal(|ui| {
            let running = self.active.is_some();
            if self.reading && !running {
                if ui.add(primary("Back to editor")).clicked() {
                    self.reading = false;
                    self.status = Status::Ready;
                }
            } else if running {
                if ui.add_enabled(!matches!(self.status, Status::Stopping), primary("Stop")).clicked()
                    && let Some(run) = &self.active
                {
                    self.status = match signal(&run.events.stop) {
                        Ok(()) => Status::Stopping,
                        Err(error) => Status::Error(format!("Could not stop typing. {error}")),
                    };
                }
                ui.label(RichText::new("Esc").monospace().weak().size(12.0));
                if self.reading {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.toggle_value(&mut self.timing_open, "Timing");
                    });
                }
            } else {
                let start = ui.add_enabled(!self.text.is_empty(), primary("Start typing"));
                if start.clicked() && let Err(message) = self.start(ui.ctx()) {
                    self.status = Status::Error(message);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Esc to stop").weak().size(12.0));
                });
            }
        });
    }

    fn reading_view(&self, ui: &mut egui::Ui) {
        let heading = match self.status {
            Status::Completed => "Completed",
            Status::Stopped | Status::Error(_) => "Stopped",
            Status::FocusPaused => "Paused",
            Status::Stopping => "Stopping",
            _ => "Typing",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(heading).size(26.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} / {} characters", self.sent, self.total)).monospace().weak().size(12.0));
            });
        });
        if let Some(Timing { pace: Pace::FinishAt(finish), .. }) = self.applied_timing {
            let finish: DateTime<Local> = finish.into();
            ui.label(RichText::new(format!("Finish by {}", finish.format("%a %H:%M:%S"))).weak().size(12.0));
        }
        ui.add(egui::ProgressBar::new(self.sent as f32 / self.total as f32).desired_height(3.0).fill(WHITE));
        ui.add_space(13.0);

        egui::ScrollArea::vertical().id_salt("paragraph").show(ui, |ui| {
            let split = self.text.char_indices().nth(self.sent).map_or(self.text.len(), |(byte, _)| byte);
            let mut paragraph = egui::text::LayoutJob::default();
            for (part, color) in [(&self.text[..split], WHITE), (&self.text[split..], MUTED)] {
                if !part.is_empty() {
                    paragraph.append(part, 0.0, egui::TextFormat {
                        font_id: egui::FontId::proportional(19.0), color, line_height: Some(29.0),
                        ..Default::default()
                    });
                }
            }
            ui.add(egui::Label::new(paragraph).wrap());
        });
    }
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(BLACK))
        .fill(WHITE).min_size(egui::vec2(114.0, 36.0)).corner_radius(7)
}

fn timing_fields(ui: &mut egui::Ui, options: &mut Options) {
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(&mut options.mode, TimingMode::Wpm, "Target WPM");
        ui.radio_value(&mut options.mode, TimingMode::Duration, "Total time");
        ui.radio_value(&mut options.mode, TimingMode::FinishAt, "Finish by");
    });
    match options.mode {
        TimingMode::Wpm => {
            ui.horizontal(|ui| {
                let label = ui.label("Typing speed");
                ui.add(egui::DragValue::new(&mut options.wpm).range(1..=600).suffix(" WPM")).labelled_by(label.id);
            });
        }
        TimingMode::Duration => {
            ui.horizontal(|ui| {
                let label = ui.label("Time after countdown");
                ui.add(egui::DragValue::new(&mut options.total_seconds)
                    .range(0.1..=86_400.0).speed(0.1).fixed_decimals(1).suffix(" s")).labelled_by(label.id);
            });
        }
        TimingMode::FinishAt => {
            let now = Local::now();
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Finish by");
                ui.add(egui::TextEdit::singleline(&mut options.finish_at)
                    .desired_width(90.0).font(egui::TextStyle::Monospace).hint_text("HH:MM:SS"))
                    .labelled_by(label.id);
                ui.label(RichText::new(format!("PC clock {}", now.format("%H:%M:%S"))).weak().size(12.0));
            });
            if !options.finish_at.trim().is_empty() {
                let message = match next_finish(&options.finish_at, now) {
                    Ok(finish) => format!("{}", finish.format("%a, %b %e  ·  %H:%M:%S local")),
                    Err(message) => message,
                };
                ui.label(RichText::new(message).weak().size(12.0));
            }
        }
    }
    ui.add_space(4.0);
    egui::Grid::new("punctuation").spacing([14.0, 3.0]).show(ui, |ui| {
        for (pause, label) in options.pauses.iter_mut().zip(["Pause at periods", "Pause at commas", "Pause at new lines"]) {
            let checkbox = ui.checkbox(&mut pause.enabled, label);
            ui.add_enabled(pause.enabled, egui::DragValue::new(&mut pause.seconds)
                .range(0.0..=600.0).speed(0.1).fixed_decimals(1).suffix(" s")).labelled_by(checkbox.id);
            ui.end_row();
        }
    });
}

struct EscapeHotkey;

impl EscapeHotkey {
    fn register() -> Result<Self, String> {
        // SAFETY: the null window handle registers Esc on this worker's message queue.
        if unsafe { RegisterHotKey(ptr::null_mut(), HOTKEY_ID, MOD_NOREPEAT, VK_ESCAPE.into()) } == 0 {
            return Err(format!("Could not register Esc. {}", io::Error::last_os_error()));
        }
        Ok(Self)
    }
}

impl Drop for EscapeHotkey {
    fn drop(&mut self) {
        // SAFETY: only the thread that registered the hotkey owns this guard.
        unsafe { UnregisterHotKey(ptr::null_mut(), HOTKEY_ID) };
    }
}

fn run_job(
    text: &[char],
    mut timing: Timing,
    countdown: Duration,
    events: &Events,
    changes: &mpsc::Receiver<Timing>,
    report: impl Fn(Update),
) -> Result<bool, String> {
    let _hotkey = EscapeHotkey::register()?;
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|error| format!("Could not initialize keyboard input. {error}"))?;
    let started = Instant::now() + countdown;
    let mut schedule = Schedule::plan(text, timing, started, started, SystemTime::now() + countdown, None)?;
    report(Update::Status(Status::Countdown(started)));
    if events.wait(started)? == Wait::Stop {
        return Ok(false);
    }

    let target = foreground_window();
    let mut process_id = 0;
    if !target.is_null() {
        // SAFETY: process_id is writable and target came from GetForegroundWindow.
        unsafe { GetWindowThreadProcessId(target, &mut process_id) };
    }
    if process_id == 0 || process_id == std::process::id() {
        return Err("Click the target text field before the countdown ends.".into());
    }

    if matches!(timing.pace, Pace::FinishAt(_)) {
        schedule = Schedule::plan(text, timing, started, Instant::now(), SystemTime::now(), None)?;
    }

    let mut sent = 0;
    let mut next = 0;
    let mut previous = None;
    let mut focus_paused = false;
    report(Update::Status(Status::Typing));
    while sent < text.len() {
        let due = schedule.next(next);
        let check_focus = Instant::now() + Duration::from_millis(125);
        let deadline = if focus_paused { check_focus } else { due.min(check_focus) };
        match events.wait(deadline)? {
            Wait::Stop => return Ok(false),
            Wait::Timing => {
                if let Some(requested) = changes.try_iter().last() {
                    match Schedule::plan(&text[sent..], requested, started, Instant::now(), SystemTime::now(), previous) {
                        Ok(updated) => {
                            schedule = updated;
                            timing = requested;
                            next = 0;
                            report(Update::Timing(Ok(timing)));
                        }
                        Err(message) => report(Update::Timing(Err(message))),
                    }
                }
                continue;
            }
            Wait::Due => {}
        }

        if foreground_window() != target {
            // SAFETY: IsWindow checks the opaque handle without dereferencing it.
            if unsafe { IsWindow(target) } == 0 {
                return Err("The target window was closed.".into());
            }
            if !focus_paused {
                focus_paused = true;
                report(Update::Status(Status::FocusPaused));
            }
            continue;
        }
        let now = Instant::now();
        let wall_now = SystemTime::now();
        if focus_paused || (matches!(timing.pace, Pace::FinishAt(_)) && schedule.clock_shifted(now, wall_now)) {
            schedule = Schedule::plan(&text[sent..], timing, started, now, wall_now, previous)?;
            next = 0;
            if focus_paused {
                focus_paused = false;
                report(Update::Status(Status::Typing));
            }
            continue;
        }
        if now < due {
            continue;
        }

        let ch = text[sent];
        let mut utf8 = [0; 4];
        let result = match ch {
            '\n' => enigo.key(Key::Return, Click),
            '\t' => enigo.key(Key::Tab, Click),
            ch => enigo.text(ch.encode_utf8(&mut utf8)),
        };
        result.map_err(|error| format!("Keyboard input failed. {error}"))?;
        previous = Some((ch, Instant::now()));
        sent += 1;
        next += 1;
        report(Update::Progress(sent));
    }
    Ok(true)
}

fn foreground_window() -> HWND {
    // SAFETY: this function takes no arguments and returns an opaque handle.
    unsafe { GetForegroundWindow() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(text: &str, timing: Timing) -> Result<Schedule, String> {
        let start = Instant::now();
        Schedule::plan(&text.chars().collect::<Vec<_>>(), timing, start, start, SystemTime::UNIX_EPOCH, None)
    }

    #[test]
    fn exact_duration_reserves_only_pauses_between_characters() {
        let timing = Timing {
            pace: Pace::Duration(Duration::from_secs(20)),
            pauses: [5, 1, 2].map(Duration::from_secs),
        };
        let schedule = planned("é.,\nx.", timing).unwrap();
        let deadlines: Vec<_> = schedule.deadlines.iter().map(Duration::as_secs_f64).collect();
        assert_eq!(deadlines, [2.0, 4.0, 11.0, 14.0, 18.0, 20.0]);
    }

    #[test]
    fn duration_rejects_pauses_without_time_to_type() {
        let timing = Timing { pace: Pace::Duration(Duration::from_secs(5)), pauses: [5, 0, 0].map(Duration::from_secs) };
        assert!(planned("One. Two.", timing).is_err());
    }

    #[test]
    fn timing_changes_preserve_the_original_start_and_remaining_punctuation_pause() {
        let started = Instant::now();
        let now = started + Duration::from_secs(6);
        let timing = Timing { pace: Pace::Duration(Duration::from_secs(10)), pauses: [3, 0, 0].map(Duration::from_secs) };
        let schedule = Schedule::plan(&['a', 'b'], timing, started, now, SystemTime::UNIX_EPOCH, Some(('.', now - Duration::from_secs(1)))).unwrap();
        assert_eq!(schedule.deadlines, [3, 4].map(Duration::from_secs));
    }

    #[test]
    fn finish_by_uses_the_next_local_occurrence_and_detects_clock_changes() {
        let zone = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 18, 23, 59, 45).unwrap();
        let finish = next_finish("00:00:15", now).unwrap();
        assert_eq!((finish - now).num_seconds(), 30);
        assert!(next_finish("27:60", now).is_err());

        let wall_now: SystemTime = now.into();
        let started = Instant::now();
        let timing = Timing { pace: Pace::FinishAt(finish.into()), pauses: [Duration::ZERO; 3] };
        let schedule = Schedule::plan(&['a'], timing, started, started, wall_now, None).unwrap();
        assert_eq!(schedule.deadlines, [Duration::from_secs(30)]);
        assert!(!schedule.clock_shifted(started + Duration::from_secs(1), wall_now + Duration::from_secs(1)));
        assert!(schedule.clock_shifted(started + Duration::from_secs(1), wall_now + Duration::from_secs(2)));
    }

    #[test]
    fn wpm_delays_stay_in_the_jitter_range() {
        let timing = Options::default().timing(Local::now()).unwrap();
        let text = "abcd".repeat(30);
        let mut previous = Duration::ZERO;
        let nominal = 60.0 / (5.0 * 65.0);
        for next in planned(&text, timing).unwrap().deadlines {
            let ratio = (next - previous).as_secs_f64() / nominal;
            assert!((0.799999..=1.200001).contains(&ratio));
            previous = next;
        }
    }

    #[test]
    fn stop_interrupts_a_future_deadline() {
        let events = Events::new().unwrap();
        signal(&events.timing).unwrap();
        assert_eq!(events.wait(Instant::now() + Duration::from_secs(2)).unwrap(), Wait::Timing);
        signal(&events.stop).unwrap();
        assert_eq!(events.wait(Instant::now() + Duration::from_secs(2)).unwrap(), Wait::Stop);
    }
}
