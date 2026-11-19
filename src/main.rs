#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env, fs,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
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
const SURFACE: Color32 = Color32::from_gray(10);
const SEMIBOLD: &str = "Segoe Semibold";
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
            visuals.widgets.active.bg_stroke = Stroke::new(2.0, WHITE);
            set_typography(&cc.egui_ctx);
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            cc.egui_ctx.set_visuals_of(egui::Theme::Dark, visuals);
            cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
                style.spacing.item_spacing = egui::vec2(8.0, 6.0);
                style.spacing.interact_size.y = 24.0;
                style
                    .text_styles
                    .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
                style
                    .text_styles
                    .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
            });
            Ok(Box::new(AutoTyper::default()))
        }),
    )
}

fn set_typography(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let font_dir = env::var_os("WINDIR").map(|path| PathBuf::from(path).join("Fonts"));
    for (name, file, family) in [
        ("Segoe UI", "segoeui.ttf", egui::FontFamily::Proportional),
        (SEMIBOLD, "seguisb.ttf", egui::FontFamily::Name(SEMIBOLD.into())),
    ] {
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        let family = fonts.families.entry(family).or_insert(fallback);
        if let Some(directory) = &font_dir
            && let Ok(data) = fs::read(directory.join(file))
        {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
            family.insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
}

fn emphasis(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(egui::FontId::new(size, egui::FontFamily::Name(SEMIBOLD.into())))
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
                Pause {
                    enabled: true,
                    seconds: 5.0,
                },
                Pause {
                    enabled: false,
                    seconds: 0.5,
                },
                Pause {
                    enabled: false,
                    seconds: 1.0,
                },
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
            timing
                .pause_after(ch)
                .saturating_sub(now.saturating_duration_since(sent_at))
        });
        let available = match timing.pace {
            Pace::Wpm(_) => None,
            Pace::Duration(total) => {
                Some(total.saturating_sub(now.saturating_duration_since(started)))
            }
            Pace::FinishAt(finish) => Some(finish.duration_since(wall_now).unwrap_or_default()),
        };
        let typing_budget = if let Some(available) = available {
            if available.is_zero() {
                return Err(
                    "Set a finish time that leaves time to type after the countdown.".into(),
                );
            }
            let pauses = initial_pause
                + remaining[..count.saturating_sub(1)]
                    .iter()
                    .map(|&ch| timing.pause_after(ch))
                    .sum::<Duration>();
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
                None => {
                    typing
                        + Duration::from_secs_f64(seconds_per_char * rand::random_range(0.8..=1.2))
                }
            };
            deadlines.push(typing + pauses);
            pauses += timing.pause_after(ch);
        }
        Ok(Self {
            from: now,
            wall_from: wall_now,
            deadlines,
        })
    }

    fn next(&self, index: usize) -> Instant {
        self.from + self.deadlines[index]
    }

    fn clock_shifted(&self, now: Instant, wall_now: SystemTime) -> bool {
        let expected = self.wall_from + now.saturating_duration_since(self.from);
        wall_now
            .duration_since(expected)
            .unwrap_or_else(|error| error.duration())
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
        Ok(Self {
            stop: new_event(true)?,
            timing: new_event(false)?,
        })
    }

    fn wait(&self, deadline: Instant) -> Result<Wait, String> {
        let handles = [self.stop.as_raw_handle(), self.timing.as_raw_handle()];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let millis = remaining
                .as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: both handles remain owned for the entirety of the call.
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(
                    2,
                    handles.as_ptr(),
                    millis,
                    QS_HOTKEY,
                    MWMO_INPUTAVAILABLE,
                )
            };
            match result {
                WAIT_OBJECT_0 => return Ok(Wait::Stop),
                WAIT_TIMEOUT if Instant::now() >= deadline => return Ok(Wait::Due),
                WAIT_TIMEOUT => {}
                WAIT_FAILED => {
                    return Err(format!(
                        "The typing timer failed. {}",
                        io::Error::last_os_error()
                    ));
                }
                ready if ready == WAIT_OBJECT_0 + 1 => return Ok(Wait::Timing),
                ready if ready == WAIT_OBJECT_0 + 2 => {
                    let mut msg = MSG::default();
                    // SAFETY: the message is a writable output on the hotkey's owning thread.
                    if unsafe {
                        PeekMessageW(&mut msg, ptr::null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE)
                    } != 0
                        && msg.wParam == HOTKEY_ID as usize
                    {
                        return Ok(Wait::Stop);
                    }
                }
                _ => {
                    return Err(
                        "Windows returned an unexpected result from the typing timer.".into(),
                    );
                }
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
    capture: Option<(Instant, mpsc::Receiver<egui::ColorImage>)>,
    countdown_backdrop: Option<egui::TextureHandle>,
    reading: bool,
    timing_open: bool,
    applied_timing: Option<Timing>,
    timing_feedback: Option<Result<(), String>>,
    sent: usize,
    total: usize,
}

impl AutoTyper {
    fn prepare_start(&mut self, ctx: &egui::Context) {
        let (send, receive) = mpsc::channel();
        self.capture = Some((Instant::now(), receive));
        self.status = Status::Starting;
        let repaint = ctx.clone();
        ctx.request_screenshot(move |image| {
            let _ = send.send(blurred_backdrop(&image));
            repaint.request_repaint();
        });
        ctx.request_repaint_after(Duration::from_millis(150));
    }

    fn stop(&mut self) {
        if self.capture.take().is_some() {
            self.status = Status::Stopped;
        } else if let Some(run) = &self.active {
            self.status = match signal(&run.events.stop) {
                Ok(()) => Status::Stopping,
                Err(error) => Status::Error(format!("Could not stop typing. {error}")),
            };
        }
    }

    fn start(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let text: Vec<char> = self.text.chars().collect();
        if text.is_empty() {
            return Err("Enter text to type.".into());
        }
        if text
            .iter()
            .any(|&ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        {
            return Err("The text contains an unsupported control character.".into());
        }
        let timing = self.options.timing(Local::now())?;
        let countdown = Duration::from_secs(u64::from(self.options.countdown));
        let events =
            Arc::new(Events::new().map_err(|error| format!("Could not start typing. {error}"))?);
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
                let result = run_job(
                    &text,
                    timing,
                    countdown,
                    &worker_events,
                    &worker_changes,
                    &report,
                );
                report(Update::Status(match result {
                    Ok(true) => Status::Completed,
                    Ok(false) => Status::Stopped,
                    Err(message) => Status::Error(message),
                }));
            })
            .map_err(|error| format!("Could not start typing. {error}"))?;
        self.active = Some(ActiveRun {
            events,
            changes,
            updates,
        });
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
            run.changes
                .send(timing)
                .map_err(|_| "The run has ended.".to_owned())?;
            signal(&run.events.timing).map_err(|error| format!("Could not apply timing. {error}"))
        });
        self.timing_feedback = result.err().map(Err);
    }
}

impl eframe::App for AutoTyper {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((requested_at, receiver)) = &self.capture {
            let image = match receiver.try_recv() {
                Ok(image) => Some(Some(image)),
                Err(mpsc::TryRecvError::Empty) if requested_at.elapsed() < Duration::from_millis(150) => {
                    ctx.request_repaint_after(Duration::from_millis(150).saturating_sub(requested_at.elapsed()));
                    None
                }
                Err(_) => Some(None),
            };
            if let Some(image) = image {
                self.capture = None;
                self.countdown_backdrop = image.map(|image| {
                    ctx.load_texture("countdown backdrop", image, egui::TextureOptions::LINEAR)
                });
                if let Err(message) = self.start(ctx) {
                    self.status = Status::Error(message);
                }
            }
        }
        if let Some(run) = &self.active {
            loop {
                match run.updates.try_recv() {
                    Ok(Update::Progress(sent)) => self.sent = sent,
                    Ok(Update::Timing(result)) => {
                        self.timing_feedback = Some(result.map(|timing| {
                            self.applied_timing = Some(timing);
                        }));
                    }
                    Ok(Update::Status(
                        status @ (Status::Completed | Status::Stopped | Status::Error(_)),
                    )) => {
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
            ctx.request_repaint_after(Duration::from_millis(40));
        } else if self.options.mode == TimingMode::FinishAt
            && (!self.reading || self.timing_open)
            && ctx.input(|input| input.viewport().focused == Some(true))
        {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
        if !matches!(self.status, Status::Starting | Status::Countdown(_) | Status::Stopping) {
            self.countdown_backdrop = None;
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Status::Countdown(end) = self.status {
            self.countdown_view(ui, end);
            return;
        }
        egui::Panel::bottom("actions")
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(BLACK)
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ui, |ui| centered_column(ui, |ui| self.actions(ui)));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BLACK)
                    .inner_margin(egui::Margin::symmetric(22, 12)),
            )
            .show(ui, |ui| centered_column(ui, |ui| {
                if self.reading {
                    self.reading_view(ui);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("setup")
                        .show(ui, |ui| self.editor(ui));
                }
            }));
    }
}

impl AutoTyper {
    fn countdown_view(&mut self, ui: &mut egui::Ui, end: Instant) {
        let bounds = ui.max_rect();
        if let Some(backdrop) = &self.countdown_backdrop {
            ui.painter().image(
                backdrop.id(),
                bounds,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                WHITE,
            );
        }
        ui.painter().rect_filled(bounds, 0, Color32::from_black_alpha(125));

        let remaining = end.saturating_duration_since(Instant::now()).as_secs_f32();
        let fraction = (remaining / self.options.countdown.max(1) as f32).clamp(0.0, 1.0);
        let center = bounds.center() - egui::vec2(0.0, 43.0);
        let radius = (bounds.height() * 0.19).clamp(60.0, 96.0);
        ui.painter().circle_stroke(center, radius, Stroke::new(2.0, Color32::from_gray(74)));
        let arc: Vec<_> = (0..=96)
            .map(|step| {
                let angle = std::f32::consts::TAU * fraction * step as f32 / 96.0
                    - std::f32::consts::FRAC_PI_2;
                center + egui::vec2(angle.cos(), angle.sin()) * radius
            })
            .collect();
        ui.painter().add(egui::Shape::line(arc, Stroke::new(2.25, WHITE)));
        let timer = egui::Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(timer)
                .layout(egui::Layout::centered_and_justified(egui::Direction::TopDown)),
            |ui| {
                ui.add(egui::Label::new(emphasis(format!("{:.0}", remaining.ceil()), radius * 1.05))
                    .selectable(false));
            },
        );
        let caption = egui::Rect::from_min_max(
            egui::pos2(bounds.left() + 18.0, center.y + radius + 20.0),
            bounds.right_bottom() - egui::vec2(18.0, 8.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(caption).layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                ui.label(emphasis("Focus the destination field", 17.0));
                ui.add_space(10.0);
                if ui.add(egui::Button::new("Cancel countdown").min_size(egui::vec2(137.0, 34.0))).clicked() {
                    self.stop();
                }
                ui.label(RichText::new("Esc").monospace().weak().size(12.0));
            },
        );
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let enabled = self.active.is_none() && self.capture.is_none();
        ui.add_space(5.0);
        let editor_label = ui.horizontal(|ui| {
            let label = ui.label(emphasis("Text to type", 26.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.always_on_top, "Always on Top").changed() {
                    let level = if self.always_on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                }
            });
            label.id
        }).inner;
        ui.add_space(7.0);

        ui.add_enabled_ui(enabled, |ui| {
            let response = egui::Frame::new()
                .fill(SURFACE).stroke(Stroke::new(1.0, BORDER))
                .inner_margin(egui::Margin::symmetric(12, 9)).corner_radius(12)
                .show(ui, |ui| {
                    let response = egui::ScrollArea::vertical().id_salt("editor").max_height(121.0).show(ui, |ui| {
                        let response = ui.add(egui::TextEdit::multiline(&mut self.text)
                            .desired_rows(7).desired_width(f32::INFINITY)
                            .hint_text("Paste a paragraph or type here").frame(egui::Frame::new()))
                            .labelled_by(editor_label);
                        if response.changed() && self.text.contains('\r') {
                            self.text = self.text.replace("\r\n", "\n").replace('\r', "\n");
                        }
                        response
                    }).inner;
                    ui.separator();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!(
                            "{} characters  ·  {} words", self.text.chars().count(),
                            self.text.split_whitespace().count()
                        )).weak().size(12.0));
                    });
                    response
                });
            if response.inner.has_focus() {
                ui.painter().rect_stroke(response.response.rect, 12, Stroke::new(2.0, MUTED), egui::StrokeKind::Inside);
            }
            ui.add_space(11.0);
            timing_fields(ui, &mut self.options);
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        if self.reading && self.timing_open {
            egui::ScrollArea::vertical().id_salt("live-timing").max_height(213.0)
                .show(ui, |ui| timing_fields(ui, &mut self.options));
            ui.horizontal_wrapped(|ui| {
                if ui.button("Apply timing").clicked() {
                    self.apply_timing();
                }
                match &self.timing_feedback {
                    Some(Ok(())) => { ui.label(RichText::new("Timing updated").weak()); }
                    Some(Err(message)) => { ui.label(message); }
                    None => {}
                }
            });
            ui.add_space(5.0);
        }

        let status = match &self.status {
            Status::Ready | Status::Typing | Status::Completed | Status::Countdown(_) => None,
            Status::Starting => Some("Starting…"),
            Status::FocusPaused => Some("Focus the original target to resume"),
            Status::Stopping => Some("Stopping…"),
            Status::Stopped if !self.reading => Some("Stopped"),
            Status::Stopped => None,
            Status::Error(message) => Some(message.as_str()),
        };
        if let Some(status) = status {
            ui.label(status);
            ui.add_space(3.0);
        }

        ui.horizontal(|ui| {
            let running = self.active.is_some() || self.capture.is_some();
            if self.reading && !running {
                if ui.add(primary("Back to editor")).clicked() {
                    self.reading = false;
                    self.status = Status::Ready;
                }
            } else if running {
                if ui.add_enabled(!matches!(self.status, Status::Stopping), primary("Stop")).clicked() {
                    self.stop();
                }
                ui.label(RichText::new("Esc").monospace().weak().size(12.0));
                if self.reading {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.toggle_value(&mut self.timing_open, "Timing");
                    });
                }
            } else {
                if ui.add_enabled(!self.text.is_empty(), primary("Start typing")).clicked() {
                    self.prepare_start(ui.ctx());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let value = ui.add(egui::DragValue::new(&mut self.options.countdown)
                        .range(1..=60).suffix(" s"));
                    let label = ui.label(RichText::new("Countdown").weak().size(13.0));
                    value.labelled_by(label.id);
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
            ui.label(emphasis(heading, 26.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} / {} characters", self.sent, self.total))
                        .monospace()
                        .weak()
                        .size(12.0),
                );
            });
        });
        if let Some(Timing {
            pace: Pace::FinishAt(finish),
            ..
        }) = self.applied_timing
        {
            let finish: DateTime<Local> = finish.into();
            ui.label(
                RichText::new(format!("Finish by {}", finish.format("%a %H:%M:%S")))
                    .weak()
                    .size(12.0),
            );
        }
        ui.add(
            egui::ProgressBar::new(self.sent as f32 / self.total as f32)
                .desired_height(3.0)
                .fill(WHITE),
        );
        ui.add_space(13.0);

        egui::ScrollArea::vertical()
            .id_salt("paragraph")
            .show(ui, |ui| {
                let split = self
                    .text
                    .char_indices()
                    .nth(self.sent)
                    .map_or(self.text.len(), |(byte, _)| byte);
                let mut paragraph = egui::text::LayoutJob::default();
                for (part, color) in [(&self.text[..split], WHITE), (&self.text[split..], MUTED)] {
                    if !part.is_empty() {
                        paragraph.append(
                            part,
                            0.0,
                            egui::TextFormat {
                                font_id: egui::FontId::proportional(19.0),
                                color,
                                line_height: Some(29.0),
                                ..Default::default()
                            },
                        );
                    }
                }
                ui.add(egui::Label::new(paragraph).wrap());
            });
    }
}

fn centered_column(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let mut bounds = ui.available_rect_before_wrap();
    let inset = ((bounds.width() - 540.0) * 0.5).max(0.0);
    bounds.min.x += inset;
    bounds.max.x -= inset;
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(bounds).layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_min_width(bounds.width());
            add(ui);
        },
    );
}

fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(emphasis(label, 14.0).color(BLACK))
        .fill(WHITE)
        .min_size(egui::vec2(132.0, 38.0))
        .corner_radius(8)
}

fn timing_fields(ui: &mut egui::Ui, options: &mut Options) {
    ui.label(emphasis("Timing", 15.0));
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER))
        .inner_margin(4).corner_radius(10)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let width = (ui.available_width() - 8.0) / 3.0;
                for (mode, name) in [
                    (TimingMode::Wpm, "Target WPM"),
                    (TimingMode::Duration, "Total time"),
                    (TimingMode::FinishAt, "Finish by"),
                ] {
                    let selected = options.mode == mode;
                    let color = if selected { BLACK } else { MUTED };
                    let button = egui::Button::new(emphasis(name, 13.0).color(color))
                        .selected(selected)
                        .fill(if selected { WHITE } else { SURFACE })
                        .corner_radius(6);
                    if ui.add_sized([width, 30.0], button).clicked() {
                        options.mode = mode;
                    }
                }
            });
        });
    match options.mode {
        TimingMode::Wpm => {
            ui.horizontal(|ui| {
                let label = ui.label("Typing speed");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut options.wpm).range(1..=600).suffix(" WPM"))
                        .labelled_by(label.id);
                });
            });
        }
        TimingMode::Duration => {
            ui.horizontal(|ui| {
                let label = ui.label("Time after countdown");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::DragValue::new(&mut options.total_seconds)
                        .range(0.1..=86_400.0).speed(0.1).fixed_decimals(1).suffix(" s"))
                        .labelled_by(label.id);
                });
            });
        }
        TimingMode::FinishAt => {
            let now = Local::now();
            ui.horizontal_wrapped(|ui| {
                let label = ui.label("Finish by");
                ui.add(egui::TextEdit::singleline(&mut options.finish_at)
                    .desired_width(89.0).font(egui::TextStyle::Monospace).hint_text("HH:MM:SS"))
                    .labelled_by(label.id);
                ui.label(RichText::new(format!("PC clock {}", now.format("%H:%M:%S"))).weak().size(12.0));
            });
            if !options.finish_at.trim().is_empty() {
                let message = match next_finish(&options.finish_at, now) {
                    Ok(time) => time.format("%a, %b %e  ·  %H:%M:%S local").to_string(),
                    Err(message) => message,
                };
                ui.label(RichText::new(message).weak().size(12.0));
            }
        }
    }

    ui.add_space(5.0);
    ui.label(emphasis("Punctuation pauses", 15.0));
    egui::Frame::new().fill(SURFACE).stroke(Stroke::new(1.0, BORDER))
        .inner_margin(egui::Margin::symmetric(10, 5)).corner_radius(12)
        .show(ui, |ui| {
            for (index, (pause, label)) in options.pauses.iter_mut()
                .zip(["Periods", "Commas", "New lines"]).enumerate()
            {
                if index > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    let checkbox = ui.checkbox(&mut pause.enabled, label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled(pause.enabled, egui::DragValue::new(&mut pause.seconds)
                            .range(0.0..=600.0).speed(0.1).fixed_decimals(1).suffix(" s"))
                            .labelled_by(checkbox.id);
                    });
                });
            }
        });
}

fn blurred_backdrop(source: &egui::ColorImage) -> egui::ColorImage {
    let [width, height] = source.size;
    if width == 0 || height == 0 {
        return egui::ColorImage::new([1, 1], vec![BLACK]);
    }
    let stride = width.max(height).div_ceil(168).max(1);
    let size = [width.div_ceil(stride), height.div_ceil(stride)];
    let [cols, rows] = size;
    let mut pixels = Vec::with_capacity(cols * rows);
    for y in 0..rows {
        for x in 0..cols {
            let (x0, y0) = (x * stride, y * stride);
            let (x1, y1) = ((x0 + stride).min(width), (y0 + stride).min(height));
            let mut sum = 0;
            for sy in y0..y1 {
                for pixel in &source.pixels[sy * width + x0..sy * width + x1] {
                    sum += usize::from(pixel.r());
                }
            }
            pixels.push((sum / ((x1 - x0) * (y1 - y0))) as u32);
        }
    }

    // A small separable Gaussian softens the captured frame once per countdown.
    let taps = [1, 4, 6, 4, 1];
    let mut horizontal = vec![0; pixels.len()];
    for y in 0..rows {
        for x in 0..cols {
            horizontal[y * cols + x] = taps.iter().enumerate().map(|(offset, weight)| {
                let neighbor = (x + offset).saturating_sub(2).min(cols - 1);
                weight * pixels[y * cols + neighbor]
            }).sum::<u32>();
        }
    }
    let blurred = (0..pixels.len()).map(|index| {
        let (x, y) = (index % cols, index / cols);
        let weight = taps.iter().enumerate().map(|(offset, weight)| {
            let neighbor = (y + offset).saturating_sub(2).min(rows - 1);
            weight * horizontal[neighbor * cols + x]
        }).sum::<u32>();
        Color32::from_gray((weight / 256) as u8)
    }).collect();
    egui::ColorImage::new(size, blurred)
}

struct EscapeHotkey;

impl EscapeHotkey {
    fn register() -> Result<Self, String> {
        // SAFETY: the null window handle registers Esc on this worker's message queue.
        if unsafe { RegisterHotKey(ptr::null_mut(), HOTKEY_ID, MOD_NOREPEAT, VK_ESCAPE.into()) }
            == 0
        {
            return Err(format!(
                "Could not register Esc. {}",
                io::Error::last_os_error()
            ));
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
    let mut schedule = Schedule::plan(
        text,
        timing,
        started,
        started,
        SystemTime::now() + countdown,
        None,
    )?;
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
        schedule = Schedule::plan(
            text,
            timing,
            started,
            Instant::now(),
            SystemTime::now(),
            None,
        )?;
    }

    let mut sent = 0;
    let mut next = 0;
    let mut previous = None;
    let mut focus_paused = false;
    report(Update::Status(Status::Typing));
    while sent < text.len() {
        let due = schedule.next(next);
        let check_focus = Instant::now() + Duration::from_millis(125);
        let deadline = if focus_paused {
            check_focus
        } else {
            due.min(check_focus)
        };
        match events.wait(deadline)? {
            Wait::Stop => return Ok(false),
            Wait::Timing => {
                if let Some(requested) = changes.try_iter().last() {
                    match Schedule::plan(
                        &text[sent..],
                        requested,
                        started,
                        Instant::now(),
                        SystemTime::now(),
                        previous,
                    ) {
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
        if focus_paused
            || (matches!(timing.pace, Pace::FinishAt(_)) && schedule.clock_shifted(now, wall_now))
        {
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

    #[test]
    fn backdrop_blurs_pixels_and_limits_texture_size() {
        let mut source = egui::ColorImage::new([7, 7], vec![BLACK; 49]);
        source.pixels[24] = WHITE;
        let backdrop = blurred_backdrop(&source);
        assert_eq!(backdrop.size, [7, 7]);
        assert!((1..255).contains(&backdrop.pixels[24].r()));
        assert!(backdrop.pixels[23].r() > 0);
        assert_eq!(backdrop.pixels[0], BLACK);

        let wide = egui::ColorImage::new([600, 300], vec![BLACK; 600 * 300]);
        let downsampled = blurred_backdrop(&wide);
        assert_eq!(downsampled.size, [150, 75]);
        assert!(downsampled.pixels.iter().all(|&pixel| pixel == BLACK));
    }

    fn planned(text: &str, timing: Timing) -> Result<Schedule, String> {
        let start = Instant::now();
        Schedule::plan(
            &text.chars().collect::<Vec<_>>(),
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
        )
    }

    #[test]
    fn exact_duration_reserves_only_pauses_between_characters() {
        let timing = Timing {
            pace: Pace::Duration(Duration::from_secs(20)),
            pauses: [5, 1, 2].map(Duration::from_secs),
        };
        let schedule = planned("é.,\nx.", timing).unwrap();
        let deadlines: Vec<_> = schedule
            .deadlines
            .iter()
            .map(Duration::as_secs_f64)
            .collect();
        assert_eq!(deadlines, [2.0, 4.0, 11.0, 14.0, 18.0, 20.0]);
    }

    #[test]
    fn duration_rejects_pauses_without_time_to_type() {
        let timing = Timing {
            pace: Pace::Duration(Duration::from_secs(5)),
            pauses: [5, 0, 0].map(Duration::from_secs),
        };
        assert!(planned("One. Two.", timing).is_err());
    }

    #[test]
    fn timing_changes_preserve_the_original_start_and_remaining_punctuation_pause() {
        let started = Instant::now();
        let now = started + Duration::from_secs(6);
        let timing = Timing {
            pace: Pace::Duration(Duration::from_secs(10)),
            pauses: [3, 0, 0].map(Duration::from_secs),
        };
        let schedule = Schedule::plan(
            &['a', 'b'],
            timing,
            started,
            now,
            SystemTime::UNIX_EPOCH,
            Some(('.', now - Duration::from_secs(1))),
        )
        .unwrap();
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
        let timing = Timing {
            pace: Pace::FinishAt(finish.into()),
            pauses: [Duration::ZERO; 3],
        };
        let schedule = Schedule::plan(&['a'], timing, started, started, wall_now, None).unwrap();
        assert_eq!(schedule.deadlines, [Duration::from_secs(30)]);
        assert!(!schedule.clock_shifted(
            started + Duration::from_secs(1),
            wall_now + Duration::from_secs(1)
        ));
        assert!(schedule.clock_shifted(
            started + Duration::from_secs(1),
            wall_now + Duration::from_secs(2)
        ));
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
        assert_eq!(
            events
                .wait(Instant::now() + Duration::from_secs(2))
                .unwrap(),
            Wait::Timing
        );
        signal(&events.stop).unwrap();
        assert_eq!(
            events
                .wait(Instant::now() + Duration::from_secs(2))
                .unwrap(),
            Wait::Stop
        );
    }
}
