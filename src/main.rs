#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText, Stroke};
use enigo::{Direction::Click, Enigo, Key, Keyboard, Settings};
use windows_sys::Win32::{
    Foundation::{HWND, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, SetEvent},
    UI::{
        Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_ESCAPE},
        WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId, MSG, MWMO_INPUTAVAILABLE,
            MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_HOTKEY, WM_HOTKEY,
        },
    },
};

const ACCENT: Color32 = Color32::from_rgb(0, 168, 255);
const BORDER: Color32 = Color32::from_rgb(30, 30, 30);
const MUTED: Color32 = Color32::from_gray(176);
const HOTKEY_ID: i32 = 1;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 520.0])
            .with_min_inner_size([400.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AutoTyper",
        options,
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.window_fill = Color32::BLACK;
            visuals.panel_fill = Color32::BLACK;
            visuals.extreme_bg_color = Color32::BLACK;
            visuals.window_stroke = Stroke::new(1.0, BORDER);
            visuals.override_text_color = Some(Color32::WHITE);
            visuals.selection.bg_fill = ACCENT.gamma_multiply(0.55);
            visuals.selection.stroke = Stroke::new(1.0, ACCENT);
            visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
            visuals.widgets.inactive.bg_fill = Color32::from_gray(12);
            visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(12);
            visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
            visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
            visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
            cc.egui_ctx.set_visuals(visuals);
            Ok(Box::new(AutoTyper::default()))
        }),
    )
}

#[derive(Clone, Copy, PartialEq)]
enum TimingMode {
    Wpm,
    TotalTime,
}

#[derive(Clone, Copy)]
struct Pause {
    enabled: bool,
    seconds: f64,
}

impl Pause {
    fn duration(self) -> Duration {
        Duration::from_secs_f64(if self.enabled { self.seconds } else { 0.0 })
    }
}

struct Options {
    mode: TimingMode,
    wpm: u32,
    total_seconds: f64,
    countdown: u32,
    pauses: [Pause; 3],
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: TimingMode::Wpm,
            wpm: 65,
            total_seconds: 60.0,
            countdown: 4,
            pauses: [
                Pause { enabled: true, seconds: 5.0 },
                Pause { enabled: false, seconds: 0.5 },
                Pause { enabled: false, seconds: 1.0 },
            ],
        }
    }
}

struct Job {
    keys: Vec<(char, Duration)>,
    countdown: Duration,
}

impl Job {
    fn plan(text: &str, options: &Options) -> Result<Self, String> {
        if text.is_empty() {
            return Err("Enter text to type.".into());
        }
        if options.pauses.iter().any(|pause| {
            pause.enabled && (!pause.seconds.is_finite() || pause.seconds < 0.0)
        }) {
            return Err("Enter a valid number of seconds for each enabled pause.".into());
        }
        let count = text.chars().count();
        let pauses = options.pauses.map(Pause::duration);
        let pause_after = |ch| match ch {
            '.' => pauses[0],
            ',' => pauses[1],
            '\n' => pauses[2],
            _ => Duration::ZERO,
        };

        // A pause applies only when another character follows.
        let budget = if options.mode == TimingMode::TotalTime {
            let total = Duration::try_from_secs_f64(options.total_seconds)
                .map_err(|_| "Enter a valid total time in seconds.".to_owned())?;
            let paused: Duration = text.chars().take(count - 1).map(pause_after).sum();
            if paused >= total {
                return Err(format!(
                    "Pauses take {:.2} s. Set a longer total time or shorten the pauses.",
                    paused.as_secs_f64()
                ));
            }
            Some(total - paused)
        } else {
            None
        };

        let seconds_per_char = 60.0 / (5.0 * f64::from(options.wpm));
        let mut typing_time = Duration::ZERO;
        let mut pause_time = Duration::ZERO;
        let mut keys = Vec::with_capacity(count);
        for (index, ch) in text.chars().enumerate() {
            if ch.is_control() && !matches!(ch, '\n' | '\t') {
                return Err("The text contains an unsupported control character.".into());
            }
            typing_time = match budget {
                Some(budget) => budget.mul_f64((index + 1) as f64 / count as f64),
                None => typing_time
                    + Duration::from_secs_f64(seconds_per_char * rand::random_range(0.8..=1.2)),
            };
            keys.push((ch, typing_time + pause_time));
            pause_time += pause_after(ch);
        }
        Ok(Self {
            keys,
            countdown: Duration::from_secs(u64::from(options.countdown)),
        })
    }
}

#[derive(Default)]
enum Status {
    #[default]
    Ready,
    Starting,
    Countdown(Instant),
    Typing { sent: usize, total: usize },
    Stopping,
    Stopped,
    Completed,
    Error(String),
}

struct ActiveRun {
    stop: Arc<OwnedHandle>,
    updates: mpsc::Receiver<Status>,
}

impl ActiveRun {
    fn cancel(&self) -> io::Result<()> {
        // SAFETY: the owned event remains valid while this reference is live.
        if unsafe { SetEvent(self.stop.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

#[derive(Default)]
struct AutoTyper {
    text: String,
    options: Options,
    always_on_top: bool,
    status: Status,
    active: Option<ActiveRun>,
}

impl AutoTyper {
    fn start(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let job = Job::plan(&self.text, &self.options)?;
        // SAFETY: null attributes and name create a private, initially unsignaled event.
        let raw = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if raw.is_null() {
            return Err(format!("Could not create the stop event. {}", io::Error::last_os_error()));
        }
        // SAFETY: CreateEventW returned a new handle owned by this process.
        let stop = Arc::new(unsafe { OwnedHandle::from_raw_handle(raw) });
        let worker_stop = Arc::clone(&stop);
        let ctx = ctx.clone();
        let (sender, updates) = mpsc::channel();
        thread::Builder::new()
            .name("typing".into())
            .spawn(move || {
                let report = |status| {
                    let _ = sender.send(status);
                    ctx.request_repaint();
                };
                let result = run_job(job, &worker_stop, &report);
                report(match result {
                    Ok(true) => Status::Completed,
                    Ok(false) => Status::Stopped,
                    Err(message) => Status::Error(message),
                });
            })
            .map_err(|error| format!("Could not start typing. {error}"))?;
        self.active = Some(ActiveRun { stop, updates });
        self.status = Status::Starting;
        Ok(())
    }
}

impl eframe::App for AutoTyper {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(run) = &self.active {
            loop {
                match run.updates.try_recv() {
                    Ok(status @ (Status::Completed | Status::Stopped | Status::Error(_))) => {
                        self.status = status;
                        self.active = None;
                        break;
                    }
                    Ok(status) => {
                        if !matches!(self.status, Status::Stopping) {
                            self.status = status;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.status = Status::Error("The typing thread ended unexpectedly.".into());
                        self.active = None;
                        break;
                    }
                }
            }
        }
        if let Status::Countdown(end) = self.status {
            let remaining = end.saturating_duration_since(Instant::now());
            let until_next_second = (remaining.as_secs_f64().fract() * 1000.0).ceil() as u64;
            ctx.request_repaint_after(Duration::from_millis(until_next_second.max(15)));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::BLACK).inner_margin(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("form").show(ui, |ui| {
                    self.form(ui);
                });
            });
    }
}

impl AutoTyper {
    fn form(&mut self, ui: &mut egui::Ui) {
        let running = self.active.is_some();
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 5.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("AutoTyper").size(25.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.checkbox(&mut self.always_on_top, "Always on Top").changed() {
                    let level = if self.always_on_top {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    };
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                }
            });
        });

        ui.add_enabled_ui(!running, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("Text to type").strong());
            egui::Frame::new()
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(6)
                .inner_margin(8)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("editor")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            if ui.add(egui::TextEdit::multiline(&mut self.text)
                                .desired_rows(7)
                                .desired_width(f32::INFINITY)
                                .hint_text("Paste or type your text here")
                                .frame(false))
                                .changed() && self.text.contains('\r')
                            {
                                self.text = self.text.replace("\r\n", "\n").replace('\r', "\n");
                            }
                        });
                });
            ui.label(RichText::new(format!(
                "{} characters   ·   {} words",
                self.text.chars().count(),
                self.text.split_whitespace().count()
            )).color(MUTED));

            ui.separator();
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.options.mode, TimingMode::Wpm, "Target WPM");
                ui.radio_value(&mut self.options.mode, TimingMode::TotalTime, "Total Completion Time");
            });
            ui.horizontal(|ui| match self.options.mode {
                TimingMode::Wpm => {
                    ui.label("Typing speed");
                    ui.add(egui::DragValue::new(&mut self.options.wpm).range(1..=600).suffix(" WPM"));
                }
                TimingMode::TotalTime => {
                    ui.label("Time after countdown");
                    ui.add(egui::DragValue::new(&mut self.options.total_seconds)
                        .range(0.1..=86_400.0).speed(0.1).fixed_decimals(1).suffix(" s"));
                }
            });

            ui.separator();
            egui::Grid::new("pauses").spacing([15.0, 5.0]).show(ui, |ui| {
                for (pause, label) in self.options.pauses.iter_mut().zip([
                    "Enable pause at periods",
                    "Enable pause at commas",
                    "Enable pause at new lines",
                ]) {
                    ui.checkbox(&mut pause.enabled, label);
                    ui.add_enabled(pause.enabled, egui::DragValue::new(&mut pause.seconds)
                        .range(0.0..=600.0).speed(0.1).fixed_decimals(1).suffix(" s"));
                    ui.end_row();
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Start countdown");
                ui.add(egui::DragValue::new(&mut self.options.countdown).range(1..=60).suffix(" s"));
            });
        });

        ui.add_space(5.0);
        ui.horizontal(|ui| {
            let start = egui::Button::new(RichText::new("Start Typing").strong().color(Color32::BLACK))
                .fill(ACCENT)
                .min_size(egui::vec2(118.0, 34.0));
            if ui.add_enabled(!running && !self.text.is_empty(), start).clicked()
                && let Err(error) = self.start(ui.ctx())
            {
                self.status = Status::Error(error);
            }
            let stop = egui::Button::new("Stop").min_size(egui::vec2(70.0, 34.0));
            if ui.add_enabled(running && !matches!(self.status, Status::Stopping), stop).clicked()
                && let Some(run) = &self.active
            {
                match run.cancel() {
                    Ok(()) => self.status = Status::Stopping,
                    Err(error) => {
                        ui.ctx().request_repaint();
                        self.status = Status::Error(format!("Could not stop typing. {error}"));
                    }
                }
            }
            ui.label(RichText::new("Esc stops typing").color(MUTED));
        });

        match &self.status {
            Status::Ready => { ui.label("Ready"); }
            Status::Starting => { ui.label("Starting…"); }
            Status::Countdown(end) => {
                let seconds = end.saturating_duration_since(Instant::now()).as_secs_f64().ceil();
                ui.label(RichText::new(format!("Click your target field · {seconds:.0} s")).color(ACCENT));
            }
            Status::Typing { sent, total } => {
                ui.add(egui::ProgressBar::new(*sent as f32 / *total as f32)
                    .fill(ACCENT.gamma_multiply(0.55))
                    .text(format!("Typing · {sent} / {total} characters")));
            }
            Status::Stopping => { ui.label("Stopping…"); }
            Status::Stopped => { ui.label("Stopped"); }
            Status::Completed => { ui.label(RichText::new("Completed").color(ACCENT)); }
            Status::Error(message) => { ui.label(RichText::new(message).color(Color32::LIGHT_RED)); }
        }
    }
}

struct EscapeHotkey;

impl EscapeHotkey {
    fn register() -> Result<Self, String> {
        // SAFETY: a null window handle registers the key on this worker's message queue.
        if unsafe { RegisterHotKey(ptr::null_mut(), HOTKEY_ID, MOD_NOREPEAT, VK_ESCAPE.into()) } == 0 {
            return Err(format!("Could not register Esc. {}", io::Error::last_os_error()));
        }
        Ok(Self)
    }
}

impl Drop for EscapeHotkey {
    fn drop(&mut self) {
        // SAFETY: only the worker that registered the hotkey owns this guard.
        unsafe { UnregisterHotKey(ptr::null_mut(), HOTKEY_ID) };
    }
}

fn run_job(job: Job, stop: &OwnedHandle, report: impl Fn(Status)) -> Result<bool, String> {
    let _hotkey = EscapeHotkey::register()?;
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|error| format!("Could not initialize keyboard input. {error}"))?;
    let start = Instant::now() + job.countdown;
    report(Status::Countdown(start));
    if !wait_until(stop, start)? {
        return Ok(false);
    }

    let target = foreground_window();
    let mut process_id = 0;
    if !target.is_null() {
        // SAFETY: process_id is a writable output and target came from GetForegroundWindow.
        unsafe { GetWindowThreadProcessId(target, &mut process_id) };
    }
    if process_id == 0 || process_id == std::process::id() {
        return Err("Click the target text field before the countdown ends.".into());
    }

    let total = job.keys.len();
    report(Status::Typing { sent: 0, total });
    for (index, (ch, elapsed)) in job.keys.into_iter().enumerate() {
        if !wait_until(stop, start + elapsed)? {
            return Ok(false);
        }
        if foreground_window() != target {
            return Err("Stopped because the target window lost focus.".into());
        }
        let mut utf8 = [0; 4];
        let result = match ch {
            '\n' => enigo.key(Key::Return, Click),
            '\t' => enigo.key(Key::Tab, Click),
            _ => enigo.text(ch.encode_utf8(&mut utf8)),
        };
        result.map_err(|error| format!("Keyboard input failed. {error}"))?;
        if index + 1 < total {
            report(Status::Typing { sent: index + 1, total });
        }
    }
    Ok(true)
}

fn foreground_window() -> HWND {
    // SAFETY: this function takes no arguments and returns an opaque handle.
    unsafe { GetForegroundWindow() }
}

fn wait_until(stop: &OwnedHandle, deadline: Instant) -> Result<bool, String> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let millis = remaining.as_nanos().div_ceil(1_000_000).min(u128::from(u32::MAX - 1)) as u32;
        let handle = stop.as_raw_handle();
        // SAFETY: handle points to a live event, borrowed for the duration of the wait.
        let result = unsafe { MsgWaitForMultipleObjectsEx(1, &handle, millis, QS_HOTKEY, MWMO_INPUTAVAILABLE) };
        match result {
            WAIT_OBJECT_0 => return Ok(false),
            WAIT_TIMEOUT if Instant::now() >= deadline => return Ok(true),
            WAIT_TIMEOUT => continue,
            WAIT_FAILED => return Err(format!("The typing timer failed. {}", io::Error::last_os_error())),
            ready if ready == WAIT_OBJECT_0 + 1 => {
                let mut msg = MSG::default();
                // SAFETY: the message is a writable output on the hotkey's owning thread.
                if unsafe { PeekMessageW(&mut msg, ptr::null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE) } != 0
                    && msg.wParam == HOTKEY_ID as usize
                {
                    return Ok(false);
                }
            }
            _ => return Err("Windows returned an unexpected result from the typing timer.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_duration_distributes_typing_budget_around_pauses() {
        let mut options = Options { mode: TimingMode::TotalTime, total_seconds: 20.0, ..Options::default() };
        options.pauses[1] = Pause { enabled: true, seconds: 1.0 };
        options.pauses[2] = Pause { enabled: true, seconds: 2.0 };
        let job = Job::plan("é.,\nx.", &options).unwrap();
        let deadlines: Vec<_> = job.keys.iter().map(|(_, time)| time.as_secs_f64()).collect();
        assert_eq!(deadlines, [2.0, 4.0, 11.0, 14.0, 18.0, 20.0]);
    }

    #[test]
    fn duration_rejects_pauses_with_no_typing_time() {
        let options = Options { mode: TimingMode::TotalTime, total_seconds: 5.0, ..Options::default() };
        assert!(Job::plan("One. Two.", &options).is_err());
    }

    #[test]
    fn wpm_delays_stay_in_the_jitter_range() {
        let options = Options::default();
        let text = "abcd".repeat(30);
        let mut previous = Duration::ZERO;
        let nominal = 60.0 / (5.0 * f64::from(options.wpm));
        for (_, next) in Job::plan(&text, &options).unwrap().keys {
            let ratio = (next - previous).as_secs_f64() / nominal;
            assert!((0.799999..=1.200001).contains(&ratio));
            previous = next;
        }
    }
}
