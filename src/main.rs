#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod docx;
mod rich_text;
use std::{
    env, fs, io,
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
use rich_text::{Document, Heading, Style};
use windows_sys::Win32::{
    Foundation::{HWND, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, SetEvent},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
            MOD_NOREPEAT, RegisterHotKey, SendInput, UnregisterHotKey, VK_B, VK_CONTROL, VK_ESCAPE,
            VK_I, VK_LWIN, VK_MENU, VK_N, VK_OEM_2, VK_RWIN, VK_S, VK_SHIFT,
        },
        WindowsAndMessaging::{
            GUITHREADINFO, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextW,
            GetWindowThreadProcessId, IsWindow, MSG, MWMO_INPUTAVAILABLE,
            MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_HOTKEY, WM_HOTKEY,
        },
    },
};

const BLACK: Color32 = Color32::BLACK;
const WHITE: Color32 = Color32::WHITE;
const MUTED: Color32 = Color32::from_gray(160);
const BORDER: Color32 = Color32::from_gray(47);
const SURFACE: Color32 = Color32::from_gray(13);
const SEMIBOLD: &str = "Segoe Semibold";
const HOTKEY_ID: i32 = 1;
const TEXT_EDITOR: &str = "autotyper document";

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
        (
            SEMIBOLD,
            "seguisb.ttf",
            egui::FontFamily::Name(SEMIBOLD.into()),
        ),
    ] {
        let fallback = fonts.families[&egui::FontFamily::Proportional].clone();
        let family = fonts.families.entry(family).or_insert(fallback);
        if let Some(directory) = &font_dir
            && let Ok(data) = fs::read(directory.join(file))
        {
            fonts
                .font_data
                .insert(name.into(), Arc::new(egui::FontData::from_owned(data)));
            family.insert(0, name.into());
        }
    }
    ctx.set_fonts(fonts);
}

fn emphasis(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(egui::FontId::new(
        size,
        egui::FontFamily::Name(SEMIBOLD.into()),
    ))
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
    natural_writing: bool,
    natural_thinking: bool,
    thinking_seconds: [f64; 2],
    exhausted_typing: bool,
    exhaustion_reduction: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: TimingMode::Wpm,
            wpm: 65,
            total_seconds: 60.0,
            finish_at: String::new(),
            countdown: 4,
            natural_writing: false,
            natural_thinking: false,
            thinking_seconds: [5.0, 10.0],
            exhausted_typing: false,
            exhaustion_reduction: 20,
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
            TimingMode::Wpm => Pace::Wpm(
                self.wpm,
                if self.exhausted_typing { self.exhaustion_reduction } else { 0 },
            ),
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
        let thinking = if self.natural_thinking {
            let [minimum, maximum] = self.thinking_seconds;
            if !minimum.is_finite()
                || !maximum.is_finite()
                || minimum < 0.1
                || maximum > 600.0
                || minimum > maximum
            {
                return Err("Enter an ordered thinking range between 0.1 and 600 seconds.".into());
            }
            Some(self.thinking_seconds.map(Duration::from_secs_f64))
        } else {
            None
        };
        Ok(Timing {
            pace,
            pauses,
            natural_writing: self.natural_writing,
            thinking,
        })
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
    Wpm(u32, u8),
    Duration(Duration),
    FinishAt(SystemTime),
}

impl Pace {
    fn exhaustion_reduction(self) -> u8 {
        match self {
            Self::Wpm(_, reduction) => reduction,
            _ => 0,
        }
    }

    fn efficiency(self, tired: bool) -> f64 {
        if tired {
            1.0 - f64::from(self.exhaustion_reduction()) / 100.0
        } else {
            1.0
        }
    }
}

#[derive(Clone, Copy)]
struct Timing {
    pace: Pace,
    pauses: [Duration; 3],
    natural_writing: bool,
    thinking: Option<[Duration; 2]>,
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

#[derive(Clone, Copy, Default)]
struct NaturalStep {
    duration: Duration,
    covered_by: u8,
    weight_offset: f64,
    jitter_offset: f32,
}

fn word_spans(text: &[char]) -> impl Iterator<Item = std::ops::Range<usize>> + '_ {
    let mut next = 0;
    std::iter::from_fn(move || {
        while text.get(next).is_some_and(|ch| !ch.is_alphanumeric()) {
            next += 1;
        }
        let start = next;
        text.get(start)?;
        next += 1;
        while text.get(next).is_some_and(|&ch| {
            ch.is_alphanumeric()
                || (matches!(ch, '\'' | '’' | '-')
                    && text.get(next + 1).is_some_and(|ch| ch.is_alphanumeric()))
        }) {
            next += 1;
        }
        Some(start..next)
    })
}

fn shape_burst(steps: &mut [NaturalStep], pace: f64) {
    let last = steps.len().saturating_sub(1);
    let pickup = (steps.len() / 4).clamp(1, 7) as f64;
    let taper = (steps.len() / 5).clamp(1, 6) as f64;
    for (index, step) in steps.iter_mut().enumerate() {
        let launch = (1.0 - index as f64 / pickup).max(0.0).powi(2);
        let settle = (1.0 - (last - index) as f64 / taper).max(0.0).powi(2);
        step.weight_offset += pace + 0.62 * launch + 0.32 * settle;
    }
}

fn natural_cadence(text: &[char], mut sample: impl FnMut() -> f64) -> Vec<NaturalStep> {
    let word_key = |span: std::ops::Range<usize>| {
        text[span]
            .iter()
            .flat_map(|ch| ch.to_lowercase())
            .collect::<String>()
    };
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for span in word_spans(text) {
        *counts.entry(word_key(span)).or_default() += 1;
    }
    let peak = counts.values().copied().max().unwrap_or(1) as f64;
    let mut steps = vec![NaturalStep::default(); text.len()];
    let mut previous: Option<(usize, String)> = None;
    let mut words_in_burst = 0usize;
    let mut burst_start = 0;
    let mut burst_target = 4 + (sample() * 5.0) as usize;
    let mut burst_weight = 0.0;
    for span in word_spans(text) {
        let key = word_key(span.clone());
        let count = counts[&key];
        let familiarity = if count > 1 {
            (count as f64 / peak).sqrt()
        } else {
            0.0
        };
        let complexity = (span.len().saturating_sub(7) as f64 * 0.04).min(0.33);
        let rare = 1.0 - familiarity;
        if let Some((end, previous_word)) = &previous {
            let separator = &text[*end..span.start];
            if separator.iter().any(|ch| ch.is_whitespace()) {
                let newlines = separator.iter().filter(|&&ch| ch == '\n').count();
                let periods = separator.iter().filter(|&&ch| ch == '.').count();
                let abbreviation = (previous_word.len() == 1
                    && previous_word.as_bytes()[0].is_ascii_alphabetic())
                    || matches!(
                        previous_word.as_str(),
                        "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "vs" | "fig"
                    );
                let sentence = periods > 1
                    || (periods == 1 && !abbreviation)
                    || separator
                        .iter()
                        .any(|&ch| matches!(ch, '!' | '?' | '。' | '！' | '？'));
                let clause = separator
                    .iter()
                    .any(|&ch| matches!(ch, ',' | ';' | ':' | '—' | '–'));
                let chance = sample();
                let draw = sample();
                let tail = draw.powi(2);
                let transition = matches!(
                    key.as_str(),
                    "but" | "because" | "although" | "however" | "therefore" | "meanwhile"
                );
                let joined_phrase = matches!(
                    previous_word.as_str(),
                    "a" | "an" | "the" | "to" | "of" | "and" | "or" | "in" | "for"
                );
                let pressure =
                    (words_in_burst.saturating_sub(burst_target.saturating_sub(2)) as f64 * 0.095)
                        .min(0.48);
                let hesitation = 0.03
                    + pressure
                    + 0.06 * rare
                    + 0.36 * complexity
                    + if transition { 0.12 } else { 0.0 };
                let seconds = match newlines {
                    2.. => 0.85 + 1.15 * tail,
                    1 => 0.48 + 0.92 * tail,
                    _ if sentence => 0.42 + 1.30 * tail,
                    _ if clause && words_in_burst >= 2 && chance < 0.82 => 0.16 + 0.60 * tail,
                    _ if periods == 0
                        && !joined_phrase
                        && words_in_burst >= 3
                        && (chance < hesitation || words_in_burst >= burst_target + 2) =>
                    {
                        if draw > 0.94 {
                            0.72 + 1.28 * ((draw - 0.94) / 0.06).powi(2)
                        } else {
                            0.14 + 0.44 * tail + 0.5 * complexity
                        }
                    }
                    _ => 0.0,
                }
                .min(2.0);
                let step = &mut steps[span.start - 1];
                step.duration = Duration::from_secs_f64(seconds);
                step.covered_by = u8::from(periods > 0)
                    | (u8::from(separator.contains(&',')) << 1)
                    | (u8::from(newlines > 0) << 2);
                if seconds > 0.0 {
                    shape_burst(&mut steps[burst_start..span.start], burst_weight);
                    burst_start = span.start;
                    words_in_burst = 0;
                    burst_target = 4 + (sample() * 5.0) as usize;
                    burst_weight = (sample() - 0.5) * 0.38;
                }
            }
        }
        for (letter, step) in steps[span.clone()].iter_mut().enumerate() {
            let ease_in = if complexity > 0.0 {
                0.08 * 3usize.saturating_sub(letter) as f64 * (0.35 + 0.65 * rare)
            } else {
                0.0
            };
            step.weight_offset = -0.08 * familiarity + complexity * (0.45 + 0.55 * rare) + ease_in;
        }
        words_in_burst += 1;
        previous = Some((span.end, key));
    }
    shape_burst(&mut steps[burst_start..], burst_weight);
    // Slow changes in rhythm should carry across neighboring keystrokes.
    let mut drift = 0.0;
    for step in &mut steps {
        drift = drift * 0.82 + (sample() - 0.5) * 0.10;
        step.jitter_offset = (drift + (sample() - 0.5) * 0.08) as f32;
    }
    steps
}

struct Schedule {
    from: Instant,
    wall_from: SystemTime,
    deadlines: Vec<Duration>,
    typing_deadlines: Vec<Duration>,
}

impl Schedule {
    fn plan(
        text: &[char],
        natural: &[NaturalStep],
        timing: Timing,
        started: Instant,
        now: Instant,
        wall_now: SystemTime,
        previous: Option<(usize, Instant)>,
        tired: bool,
    ) -> Result<Self, String> {
        let efficiency = timing.pace.efficiency(tired);
        let first = previous.map_or(0, |(index, _)| index + 1);
        let count = text.len() - first;
        let configured = timing
            .pauses
            .iter()
            .enumerate()
            .fold(0u8, |mask, (index, pause)| {
                mask | (u8::from(!pause.is_zero()) << index)
            });
        let gaps = |index: usize| {
            let fixed = timing.pause_after(text[index]);
            let extra = natural
                .get(index)
                .filter(|pause| timing.natural_writing && pause.covered_by & configured == 0)
                .map_or(Duration::ZERO, |pause| pause.duration.div_f64(efficiency));
            (fixed, extra)
        };
        let weight = |index: usize| {
            if timing.natural_writing {
                natural.get(index).map_or(1.0, |step| {
                    (1.0 + step.weight_offset) * (1.0 + f64::from(step.jitter_offset))
                })
            } else {
                1.0
            }
        };
        let (mut fixed_elapsed, mut natural_elapsed) =
            previous.map_or((Duration::ZERO, Duration::ZERO), |(index, sent_at)| {
                let (fixed, extra) = gaps(index);
                let elapsed = now.saturating_duration_since(sent_at);
                (
                    fixed.saturating_sub(elapsed),
                    extra.saturating_sub(elapsed.saturating_sub(fixed)),
                )
            });
        let available = match timing.pace {
            Pace::Wpm(..) => None,
            Pace::Duration(total) => {
                Some(total.saturating_sub(now.saturating_duration_since(started)))
            }
            Pace::FinishAt(finish) => Some(finish.duration_since(wall_now).unwrap_or_default()),
        };
        let timed_budget = if let Some(available) = available {
            if available.is_zero() {
                return Err(
                    "Set a finish time that leaves time to type after the countdown.".into(),
                );
            }
            let (fixed_total, natural_total) =
                (first..text.len().saturating_sub(1)).map(gaps).fold(
                    (fixed_elapsed, natural_elapsed),
                    |(fixed, extra), (f, e)| (fixed + f, extra + e),
                );
            if available <= fixed_total {
                return Err(format!(
                    "The remaining pauses take {:.1} s. Set a later finish or shorten the pauses.",
                    fixed_total.as_secs_f64()
                ));
            }
            let after_fixed = available - fixed_total;
            let natural_budget = natural_total.min(after_fixed.mul_f64(0.35));
            Some((after_fixed - natural_budget, natural_total, natural_budget))
        } else {
            None
        };
        let seconds_per_char = match timing.pace {
            Pace::Wpm(wpm, _) => 60.0 / (5.0 * f64::from(wpm) * efficiency),
            _ => 0.0,
        };
        let typing_budget = timed_budget.map(|(typing, _, _)| typing).or_else(|| {
            timing
                .natural_writing
                .then(|| Duration::from_secs_f64(seconds_per_char * count as f64))
        });

        let mut typing = Duration::ZERO;
        let mut typed_weight = 0.0;
        let total_weight = if typing_budget.is_some() {
            (first..text.len()).map(weight).sum::<f64>()
        } else {
            0.0
        };
        let mut deadlines = Vec::with_capacity(count);
        let track_typing = timing.thinking.is_some() || timing.pace.exhaustion_reduction() > 0;
        let mut typing_deadlines = if track_typing {
            Vec::with_capacity(count)
        } else {
            Vec::new()
        };
        for index in first..text.len() {
            let scheduled_natural = match timed_budget {
                Some((_, total, budget)) if !total.is_zero() => {
                    budget.mul_f64(natural_elapsed.as_secs_f64() / total.as_secs_f64())
                }
                _ => natural_elapsed,
            };
            let weight = weight(index);
            typed_weight += weight;
            typing = match typing_budget {
                Some(budget) => budget.mul_f64(typed_weight / total_weight),
                None => {
                    typing
                        + Duration::from_secs_f64(
                            seconds_per_char * weight * rand::random_range(0.8..=1.2),
                        )
                }
            };
            deadlines.push(typing + fixed_elapsed + scheduled_natural);
            if track_typing {
                typing_deadlines.push(typing);
            }
            let (fixed, extra) = gaps(index);
            fixed_elapsed += fixed;
            natural_elapsed += extra;
        }
        if let Some((last, available)) = deadlines.last_mut().zip(available) {
            *last = available;
        }
        Ok(Self {
            from: now,
            wall_from: wall_now,
            deadlines,
            typing_deadlines,
        })
    }

    fn next(&self, index: usize) -> Instant {
        self.from + self.deadlines[index]
    }

    fn typing_interval(&self, index: usize) -> Duration {
        self.typing_deadlines
            .get(index)
            .map_or(Duration::ZERO, |&end| {
                end - index
                    .checked_sub(1)
                    .map_or(Duration::ZERO, |previous| self.typing_deadlines[previous])
            })
    }

    fn thinking_at(&self, index: usize, left: Duration, spent: Duration) -> Option<Instant> {
        let interval = self.typing_interval(index).saturating_sub(spent);
        (left < interval).then(|| self.next(index) - (interval - left))
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
    Thinking,
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

    fn wait_through(&self, deadline: Instant) -> Result<bool, String> {
        let mut timing_changed = false;
        loop {
            match self.wait(deadline)? {
                Wait::Stop => return Ok(false),
                Wait::Timing => timing_changed = true,
                Wait::Due => break,
            }
        }
        if timing_changed {
            signal(&self.timing).map_err(|error| format!("Could not apply timing. {error}"))?;
        }
        Ok(true)
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
    text: Document,
    plain_text: bool,
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
        let styles =
            (!self.plain_text && self.text.has_styles()).then(|| self.text.styles().to_vec());
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
                    styles.as_deref(),
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
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.text.clear_pending_paste();
        if self.active.is_some() || self.capture.is_some() || self.reading {
            return;
        }
        let open = input.events.iter().any(|event| {
            matches!(event, egui::Event::Key {
                key: egui::Key::O, pressed: true, repeat: false, modifiers, ..
            } if modifiers.ctrl && !modifiers.alt && !modifiers.shift)
        });
        let dropped = std::mem::take(&mut input.dropped_files);
        if open || !dropped.is_empty() {
            let selection = match dropped.as_slice() {
                [] => docx::choose(foreground_window()),
                [file] => Ok(Some(file.path().to_owned())),
                _ => Err("Drop one Word document at a time.".into()),
            };
            match selection.and_then(|path| path.as_deref().map(docx::read).transpose()) {
                Ok(Some(document)) => {
                    self.text.append_document(document);
                    self.status = Status::Ready;
                }
                Ok(None) => {}
                Err(message) => self.status = Status::Error(message),
            }
            return;
        }
        if ctx.memory(|memory| memory.has_focus(egui::Id::unique(TEXT_EDITOR)))
            && let Some(plain) = input.events.iter().find_map(|event| {
                if let egui::Event::Paste(text) = event {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            && let Some(html) = rich_text::clipboard_html()
        {
            self.text.prepare_paste(plain, &html);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((requested_at, receiver)) = &self.capture {
            let image = match receiver.try_recv() {
                Ok(image) => Some(Some(image)),
                Err(mpsc::TryRecvError::Empty)
                    if requested_at.elapsed() < Duration::from_millis(150) =>
                {
                    ctx.request_repaint_after(
                        Duration::from_millis(150).saturating_sub(requested_at.elapsed()),
                    );
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
        if !matches!(
            self.status,
            Status::Starting | Status::Countdown(_) | Status::Stopping
        ) {
            self.countdown_backdrop = None;
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

impl AutoTyper {
    fn show(&mut self, ui: &mut egui::Ui) {
        if let Status::Countdown(end) = self.status {
            self.countdown_view(ui, end);
            return;
        }
        if !self.reading
            && self.active.is_none()
            && self.capture.is_none()
            && !self.text.is_empty()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter))
        {
            self.prepare_start(ui.ctx());
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
            .show(ui, |ui| {
                centered_column(ui, |ui| {
                    if self.reading {
                        self.reading_view(ui);
                    } else {
                        self.editor(ui);
                    }
                })
            });
    }

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
        ui.painter()
            .rect_filled(bounds, 0, Color32::from_black_alpha(125));

        let remaining = end.saturating_duration_since(Instant::now()).as_secs_f32();
        let fraction = (remaining / self.options.countdown.max(1) as f32).clamp(0.0, 1.0);
        let center = bounds.center() - egui::vec2(0.0, 28.0);
        let number_rect =
            egui::Rect::from_center_size(center - egui::vec2(0.0, 26.0), egui::vec2(238.0, 115.0));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(number_rect).layout(
                egui::Layout::centered_and_justified(egui::Direction::TopDown),
            ),
            |ui| {
                ui.add(
                    egui::Label::new(emphasis(format!("{:02}", remaining.ceil() as u32), 96.0))
                        .selectable(false),
                );
            },
        );
        let track =
            egui::Rect::from_center_size(center + egui::vec2(0.0, 42.0), egui::vec2(120.0, 2.0));
        ui.painter().rect_filled(track, 1, Color32::from_gray(76));
        ui.painter().rect_filled(
            egui::Rect::from_min_size(
                track.min,
                egui::vec2(track.width() * fraction, track.height()),
            ),
            1,
            WHITE,
        );
        let caption = egui::Rect::from_min_max(
            egui::pos2(bounds.left() + 18.0, center.y + 61.0),
            bounds.right_bottom() - egui::vec2(18.0, 8.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(caption)
                .layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                ui.label(emphasis("Click where you want to type", 17.0));
                ui.add_space(10.0);
                if ui
                    .add(egui::Button::new("Cancel  ·  Esc").min_size(egui::vec2(119.0, 34.0)))
                    .clicked()
                {
                    self.stop();
                }
            },
        );
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        let enabled = self.active.is_none() && self.capture.is_none();
        let editor_label = compact_row(ui, 27.0, |ui| {
            let label = ui.label(emphasis("Text", 17.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .checkbox(&mut self.always_on_top, "Always on top")
                    .changed()
                {
                    let level = if self.always_on_top {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    };
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
                }
            });
            label.id
        });
        let text_height = (ui.available_height() - 60.0).max(48.0);
        ui.add_enabled_ui(enabled, |ui| {
            let frame = egui::Frame::new()
                .fill(SURFACE)
                .stroke(Stroke::new(1.0, BORDER))
                .inner_margin(egui::Margin::symmetric(12, 9))
                .corner_radius(9)
                .show(ui, |ui| {
                    let response = egui::ScrollArea::vertical()
                        .id_salt("editor")
                        .max_height(text_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, width: f32| {
                                assert_eq!(buffer.type_id(), std::any::TypeId::of::<Document>());
                                // SAFETY: this editor only passes its Document to the layouter.
                                let document = unsafe { &*(buffer as *const dyn egui::TextBuffer as *const Document) };
                                let mut job = document.layout(14.0, usize::MAX, true);
                                job.wrap.max_width = width;
                                ui.fonts_mut(|fonts| fonts.layout_job(job))
                            };
                            ui.add(egui::TextEdit::multiline(&mut self.text)
                                .id(egui::Id::unique(TEXT_EDITOR))
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("Paste, type or drop a Word document")
                                .frame(egui::Frame::new())
                                .layouter(&mut layouter))
                                .labelled_by(editor_label)
                                .on_hover_text("Drop a .docx file or press Ctrl+O. Imported text goes below any existing text.")
                        })
                        .inner;
                    ui.separator();
                    compact_row(ui, 24.0, |ui| {
                        if self.text.has_styles() {
                            let mut preserve = !self.plain_text;
                            if ui.checkbox(&mut preserve, "Send bold & italic")
                                .on_hover_text("Preserves bold and italic. Titles and headings use supported document styles. Start with a plain caret.").changed()
                            {
                                self.plain_text = !preserve;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} words  ·  {} characters",
                                    self.text.split_whitespace().count(),
                                    self.text.chars().count()
                                ))
                                .weak()
                                .size(12.0),
                            );
                        });
                    });
                    response
                });
            if frame.inner.has_focus() {
                ui.painter().rect_stroke(
                    frame.response.rect,
                    9,
                    Stroke::new(1.0, MUTED),
                    egui::StrokeKind::Inside,
                );
            }
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui) {
        if !self.reading {
            let ready = self.active.is_none() && self.capture.is_none();
            ui.add_enabled_ui(ready, |ui| timing_fields(ui, &mut self.options));
            ui.separator();
        } else if self.timing_open {
            timing_fields(ui, &mut self.options);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Apply timing").clicked() {
                    self.apply_timing();
                }
                match &self.timing_feedback {
                    Some(Ok(())) => {
                        ui.label(RichText::new("Timing updated").weak());
                    }
                    Some(Err(message)) => {
                        ui.label(message);
                    }
                    None => {}
                }
            });
            ui.add_space(5.0);
        }

        let status = match &self.status {
            Status::Ready
            | Status::Typing
            | Status::Thinking
            | Status::Completed
            | Status::Countdown(_) => None,
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

        compact_row(ui, 38.0, |ui| {
            let running = self.active.is_some() || self.capture.is_some();
            if self.reading && !running {
                if ui.add(primary("Back to editor")).clicked() {
                    self.reading = false;
                    self.status = Status::Ready;
                }
            } else if running {
                if ui
                    .add_enabled(!matches!(self.status, Status::Stopping), primary("Stop"))
                    .clicked()
                {
                    self.stop();
                }
                ui.label(RichText::new("Esc").monospace().weak().size(12.0));
                if self.reading {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.toggle_value(&mut self.timing_open, "Timing");
                    });
                }
            } else {
                let label = ui.label(RichText::new("Countdown").weak());
                ui.add(
                    egui::DragValue::new(&mut self.options.countdown)
                        .range(1..=60)
                        .suffix(" s"),
                )
                .labelled_by(label.id);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.text.is_empty(), primary("Start typing"))
                        .on_hover_text("Ctrl+Enter")
                        .clicked()
                    {
                        self.prepare_start(ui.ctx());
                    }
                    ui.label(RichText::new("Ctrl+Enter").weak().size(11.0));
                });
            }
        });
    }

    fn reading_view(&self, ui: &mut egui::Ui) {
        let heading = match self.status {
            Status::Completed => "Completed",
            Status::Stopped | Status::Error(_) => "Stopped",
            Status::FocusPaused => "Paused",
            Status::Thinking => "Thinking",
            Status::Stopping => "Stopping",
            _ => "Typing",
        };
        compact_row(ui, 31.0, |ui| {
            ui.label(emphasis(heading, 23.0));
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
                let paragraph = self.text.layout(19.0, self.sent, !self.plain_text);
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
        egui::UiBuilder::new()
            .max_rect(bounds)
            .layout(egui::Layout::top_down(egui::Align::Min)),
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

fn compact_row<R>(ui: &mut egui::Ui, height: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // The right-aligned child may fill this row, so give it a bounded height.
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    )
    .inner
}

fn timing_fields(ui: &mut egui::Ui, options: &mut Options) {
    let choices = [
        (TimingMode::Wpm, "Pace"),
        (TimingMode::Duration, "Duration"),
        (TimingMode::FinishAt, "Finish by"),
    ];
    let label = match options.mode {
        TimingMode::Wpm => "Pace",
        TimingMode::Duration => "Duration",
        TimingMode::FinishAt => "Finish by",
    };
    compact_row(ui, 31.0, |ui| {
        let timing = ui.label(emphasis("Timing", 14.0));
        egui::ComboBox::from_id_salt("timing mode")
            .width(92.0)
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (mode, label) in choices {
                    ui.selectable_value(&mut options.mode, mode, label);
                }
            })
            .response
            .labelled_by(timing.id);

        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match options.mode {
                TimingMode::Wpm => {
                    ui.add(
                        egui::DragValue::new(&mut options.wpm)
                            .range(1..=600)
                            .suffix(" WPM"),
                    );
                    ui.label(RichText::new("Speed").weak());
                }
                TimingMode::Duration => {
                    ui.add(
                        egui::DragValue::new(&mut options.total_seconds)
                            .range(0.1..=86_400.0)
                            .speed(0.1)
                            .fixed_decimals(1)
                            .suffix(" s"),
                    );
                    ui.label(RichText::new("Typing time").weak());
                }
                TimingMode::FinishAt => {
                    ui.add(
                        egui::TextEdit::singleline(&mut options.finish_at)
                            .desired_width(82.0)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("HH:MM:SS"),
                    );
                    ui.label(RichText::new("Local time").weak());
                }
            },
        );
    });
    if options.mode == TimingMode::FinishAt {
        let now = Local::now();
        let clock = now.format("%H:%M:%S").to_string();
        let hint = if options.finish_at.trim().is_empty() {
            format!("PC clock {clock}")
        } else {
            match next_finish(&options.finish_at, now) {
                Ok(time) => time.format("%a, %b %e  ·  %H:%M:%S local").to_string(),
                Err(message) => message,
            }
        };
        ui.label(RichText::new(hint).weak().size(12.0));
    }

    let names = ["Periods", "Commas", "New lines"];
    let enabled = options
        .pauses
        .iter()
        .zip(names)
        .filter(|(pause, _)| pause.enabled);
    let summary = match enabled.clone().count() {
        0 => "Off".to_owned(),
        1 => enabled
            .map(|(pause, name)| format!("{name} · {:.1} s", pause.seconds))
            .next()
            .unwrap(),
        count => format!("{count} enabled"),
    };
    let mut label = if options.natural_writing {
        format!("Pauses    {summary}  ·  Natural writing")
    } else {
        format!("Pauses    {summary}")
    };
    if options.natural_thinking {
        label.push_str("  ·  Natural thinking");
    }
    let button = ui.add(egui::Button::new(label).min_size(egui::vec2(ui.available_width(), 31.0)));
    egui::Popup::from_toggle_button_response(&button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(216.0);
            for (pause, label) in options.pauses.iter_mut().zip(names) {
                compact_row(ui, 28.0, |ui| {
                    let checkbox = ui.checkbox(&mut pause.enabled, label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled(
                            pause.enabled,
                            egui::DragValue::new(&mut pause.seconds)
                                .range(0.0..=600.0)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" s"),
                        )
                        .labelled_by(checkbox.id);
                    });
                });
            }
            ui.checkbox(&mut options.natural_writing, "Natural writing")
                .on_hover_text("Types in bursts, pauses at thought breaks, and eases into longer words. WPM excludes pauses. Timed modes fit pauses into the remaining time.");
            ui.checkbox(&mut options.natural_thinking, "Natural thinking")
                .on_hover_text("Takes a random break every 6–12 seconds of typing. Timed modes take a break when the full wait fits.");
            if options.natural_thinking {
                let [minimum, maximum] = &mut options.thinking_seconds;
                compact_row(ui, 28.0, |ui| {
                    let min = ui.label("Min");
                    ui.add(egui::DragValue::new(minimum).range(0.1..=*maximum).fixed_decimals(1).suffix(" s"))
                        .labelled_by(min.id);
                    let max = ui.label("Max");
                    ui.add(egui::DragValue::new(maximum).range(*minimum..=600.0).fixed_decimals(1).suffix(" s"))
                        .labelled_by(max.id);
                });
            }
            if options.mode == TimingMode::Wpm {
                compact_row(ui, 28.0, |ui| {
                    let enabled = ui.checkbox(&mut options.exhausted_typing, "Exhausted typing")
                        .on_hover_text("After a random stretch of typing, temporarily lowers WPM and lengthens enabled natural pauses. The percentage sets the speed reduction.");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled(options.exhausted_typing,
                            egui::DragValue::new(&mut options.exhaustion_reduction).range(1..=90).suffix("%"),
                        ).labelled_by(enabled.id);
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
            horizontal[y * cols + x] = taps
                .iter()
                .enumerate()
                .map(|(offset, weight)| {
                    let neighbor = (x + offset).saturating_sub(2).min(cols - 1);
                    weight * pixels[y * cols + neighbor]
                })
                .sum::<u32>();
        }
    }
    let blurred = (0..pixels.len())
        .map(|index| {
            let (x, y) = (index % cols, index / cols);
            let weight = taps
                .iter()
                .enumerate()
                .map(|(offset, weight)| {
                    let neighbor = (y + offset).saturating_sub(2).min(rows - 1);
                    weight * horizontal[neighbor * cols + x]
                })
                .sum::<u32>();
            Color32::from_gray((weight / 256) as u8)
        })
        .collect();
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

fn next_thinking_interval() -> Duration {
    Duration::from_secs_f64(rand::random_range(6.0..12.0))
}

fn thinking_length([minimum, maximum]: [Duration; 2], efficiency: f64) -> Duration {
    let length = if minimum == maximum {
        minimum
    } else {
        Duration::from_secs_f64(rand::random_range(
            minimum.as_secs_f64()..=maximum.as_secs_f64(),
        ))
    };
    length.div_f64(efficiency)
}

#[derive(Clone, Copy)]
struct Fatigue {
    tired: bool,
    remaining: Duration,
}

impl Fatigue {
    fn new(tired: bool) -> Self {
        let seconds = if tired {
            rand::random_range(6.0..12.0)
        } else {
            rand::random_range(12.0..24.0)
        };
        Self { tired, remaining: Duration::from_secs_f64(seconds) }
    }

    fn advance(&mut self, elapsed: Duration) -> bool {
        self.remaining = self.remaining.saturating_sub(elapsed);
        if self.remaining.is_zero() {
            *self = Self::new(!self.tired);
            true
        } else {
            false
        }
    }
}

fn run_job(
    text: &[char],
    styles: Option<&[Style]>,
    mut timing: Timing,
    countdown: Duration,
    events: &Events,
    changes: &mpsc::Receiver<Timing>,
    report: impl Fn(Update),
) -> Result<bool, String> {
    let _hotkey = EscapeHotkey::register()?;
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|error| format!("Could not initialize keyboard input. {error}"))?;
    let mut natural = timing
        .natural_writing
        .then(|| natural_cadence(text, rand::random::<f64>));
    let started = Instant::now() + countdown;
    let mut schedule = Schedule::plan(
        text,
        natural.as_deref().unwrap_or_default(),
        timing,
        started,
        started,
        SystemTime::now() + countdown,
        None,
        false,
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
            natural.as_deref().unwrap_or_default(),
            timing,
            started,
            Instant::now(),
            SystemTime::now(),
            None,
            false,
        )?;
    }

    let mut sent = 0;
    let mut next = 0;
    let mut previous = None;
    let mut focus_paused = false;
    let mut thinking_left = timing.thinking.map(|_| next_thinking_interval());
    let mut thinking_spent = Duration::ZERO;
    let mut fatigue = (timing.pace.exhaustion_reduction() > 0).then(|| Fatigue::new(false));
    let mut active_style = Style::default();
    let mut paragraph = Heading::Body;
    let headings =
        styles.filter(|styles| styles.iter().any(|style| style.heading != Heading::Body));
    let editor = headings.map(|_| HeadingEditor::detect(target));
    report(Update::Status(Status::Typing));
    while sent < text.len() {
        let due = schedule.next(next);
        let thinking_at =
            thinking_left.and_then(|left| schedule.thinking_at(next, left, thinking_spent));
        let check_focus = Instant::now() + Duration::from_millis(125);
        let deadline = if focus_paused {
            check_focus
        } else {
            thinking_at.unwrap_or(due).min(check_focus)
        };
        match events.wait(deadline)? {
            Wait::Stop => return Ok(false),
            Wait::Timing => {
                if let Some(requested) = changes.try_iter().last() {
                    if requested.natural_writing {
                        natural.get_or_insert_with(|| natural_cadence(text, rand::random::<f64>));
                    }
                    let next_fatigue = (requested.pace.exhaustion_reduction() > 0)
                        .then(|| fatigue.unwrap_or_else(|| Fatigue::new(false)));
                    match Schedule::plan(
                        text,
                        natural.as_deref().unwrap_or_default(),
                        requested,
                        started,
                        Instant::now(),
                        SystemTime::now(),
                        previous,
                        next_fatigue.is_some_and(|phase| phase.tired),
                    ) {
                        Ok(updated) => {
                            schedule = updated;
                            thinking_left = requested
                                .thinking
                                .map(|_| thinking_left.unwrap_or_else(next_thinking_interval));
                            thinking_spent = Duration::ZERO;
                            fatigue = next_fatigue;
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
            schedule = Schedule::plan(
                text,
                natural.as_deref().unwrap_or_default(),
                timing,
                started,
                now,
                wall_now,
                previous,
                fatigue.is_some_and(|phase| phase.tired),
            )?;
            next = 0;
            thinking_spent = Duration::ZERO;
            if focus_paused {
                focus_paused = false;
                report(Update::Status(Status::Typing));
            }
            continue;
        }
        if let Some(at) = thinking_at
            && now >= at
            && let Some(range) = timing.thinking
        {
            thinking_spent += thinking_left.unwrap_or_default();
            thinking_left = Some(next_thinking_interval());
            let tired = fatigue.is_some_and(|phase| phase.tired);
            let length = thinking_length(range, timing.pace.efficiency(tired));
            let end = now + length;
            let after = if matches!(timing.pace, Pace::Wpm(..)) {
                None
            } else {
                let Ok(updated) = Schedule::plan(
                    text,
                    natural.as_deref().unwrap_or_default(),
                    timing,
                    started,
                    end,
                    wall_now + length,
                    previous,
                    false,
                ) else {
                    continue;
                };
                Some(updated)
            };
            report(Update::Status(Status::Thinking));
            if !events.wait_through(end)? {
                return Ok(false);
            }
            if let Some(updated) = after {
                schedule = updated;
                next = 0;
                thinking_spent = Duration::ZERO;
            } else {
                let elapsed = Instant::now().saturating_duration_since(now);
                if fatigue.as_mut().is_some_and(|phase| phase.tired && phase.advance(elapsed)) {
                    schedule = Schedule::plan(
                        text, natural.as_deref().unwrap_or_default(), timing, started,
                        Instant::now(), SystemTime::now(), previous, false,
                    )?;
                    next = 0;
                    thinking_spent = Duration::ZERO;
                } else {
                    schedule.from += elapsed;
                }
            }
            report(Update::Status(Status::Typing));
            continue;
        }
        if now < due {
            continue;
        }

        let ch = text[sent];
        if let Some(styles) = styles {
            let mut style = styles[sent];
            if let Some(editor) = editor {
                if ch != '\n'
                    && (sent == 0 || text[sent - 1] == '\n')
                    && (style.heading != paragraph || style.heading != Heading::Body)
                {
                    if !editor.apply(target, style.heading, &mut enigo, events)? {
                        return Ok(false);
                    }
                    paragraph = style.heading;
                }
                if editor == HeadingEditor::Other && style.heading != Heading::Body {
                    style.bold = true;
                }
            }
            change_style(target, active_style, style)?;
            active_style = style;
            if foreground_window() != target {
                continue;
            }
        }
        let mut utf8 = [0; 4];
        let result = match ch {
            '\n' => enigo.key(Key::Return, Click),
            '\t' => enigo.key(Key::Tab, Click),
            ch => enigo.text(ch.encode_utf8(&mut utf8)),
        };
        result.map_err(|error| format!("Keyboard input failed. {error}"))?;
        if let Some(left) = &mut thinking_left {
            *left = left.saturating_sub(
                schedule
                    .typing_interval(next)
                    .saturating_sub(thinking_spent),
            );
        }
        thinking_spent = Duration::ZERO;
        let phase_changed = fatigue.as_mut().is_some_and(|phase| {
            let elapsed = if phase.tired {
                let before = next.checked_sub(1).map_or(Duration::ZERO, |index| schedule.deadlines[index]);
                schedule.deadlines[next] - before
            } else {
                schedule.typing_interval(next)
            };
            phase.advance(elapsed)
        });
        previous = Some((sent, Instant::now()));
        sent += 1;
        next += 1;
        report(Update::Progress(sent));
        if phase_changed && sent < text.len() {
            schedule = Schedule::plan(
                text, natural.as_deref().unwrap_or_default(), timing, started,
                Instant::now(), SystemTime::now(), previous,
                fatigue.is_some_and(|phase| phase.tired),
            )?;
            next = 0;
        }
    }
    if foreground_window() == target {
        change_style(target, active_style, Style::default())?;
    }
    Ok(true)
}

fn change_style(target: HWND, previous: Style, next: Style) -> Result<(), String> {
    if previous.bold == next.bold && previous.italic == next.italic {
        return Ok(());
    }
    send_shortcut(
        target,
        &[VK_CONTROL],
        &[
            (VK_B, previous.bold != next.bold),
            (VK_I, previous.italic != next.italic),
        ],
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeadingEditor {
    Word,
    Docs,
    Other,
}

impl HeadingEditor {
    fn detect(target: HWND) -> Self {
        if window_class(target) == "OpusApp" {
            return Self::Word;
        }
        if window_title(target).contains(" - Google Docs") {
            return Self::Docs;
        }
        Self::Other
    }

    fn apply(
        self,
        target: HWND,
        heading: Heading,
        enigo: &mut Enigo,
        events: &Events,
    ) -> Result<bool, String> {
        match (self, heading) {
            (Self::Other, _) => {}
            (Self::Word, Heading::Body) => {
                send_shortcut(target, &[VK_CONTROL, VK_SHIFT], &[(VK_N, true)])?;
            }
            (Self::Word, Heading::Level(level @ 1..=3))
            | (Self::Docs, Heading::Level(level @ 1..=6)) => {
                send_shortcut(
                    target,
                    &[VK_CONTROL, VK_MENU],
                    &[(u16::from(b'0' + level), true)],
                )?;
            }
            (Self::Docs, Heading::Body) => {
                send_shortcut(target, &[VK_CONTROL, VK_MENU], &[(u16::from(b'0'), true)])?;
            }
            (Self::Docs, Heading::Level(_)) => {
                return Err("Google Docs supports heading levels 1 through 6.".into());
            }
            (Self::Docs, Heading::Title) => {
                send_shortcut(target, &[VK_MENU], &[(VK_OEM_2, true)])?;
                if !wait_for_style_input(events, || foreground_window() == target)? {
                    return Ok(false);
                }
                type_style_command(enigo, "Apply title")?;
                if !wait_for_style_input(events, || foreground_window() == target)? {
                    return Ok(false);
                }
                enigo
                    .key(Key::DownArrow, Click)
                    .map_err(style_input_error)?;
                enigo.key(Key::Return, Click).map_err(style_input_error)?;
            }
            (Self::Word, heading) => {
                let initial_caret = focused_control(target);
                send_shortcut(target, &[VK_CONTROL, VK_SHIFT], &[(VK_S, true)])?;
                if !wait_for_style_input(events, || {
                    let foreground = foreground_window();
                    foreground == target || word_styles_popup(foreground, target)
                })? {
                    return Ok(false);
                }
                let style_box = focused_control(foreground_window());
                if style_box.is_null() || style_box == initial_caret {
                    return Err("Word did not open its Apply Styles field.".into());
                }
                let name = match heading {
                    Heading::Title => "Title".into(),
                    Heading::Level(level) => format!("Heading {level}"),
                    Heading::Body => unreachable!(),
                };
                type_style_command(enigo, &name)?;
                enigo.key(Key::Return, Click).map_err(style_input_error)?;
            }
        }
        if matches!(self, Self::Other) {
            return Ok(true);
        }
        wait_for_style_input(events, || foreground_window() == target)
    }
}

fn window_class(window: HWND) -> String {
    let mut name = [0u16; 64];
    // SAFETY: name is a writable UTF-16 buffer and the handle is opaque.
    let len = unsafe { GetClassNameW(window, name.as_mut_ptr(), name.len() as i32) };
    String::from_utf16_lossy(&name[..len as usize])
}

fn window_title(window: HWND) -> String {
    let mut title = [0u16; 512];
    // SAFETY: title is a writable UTF-16 buffer and the handle is opaque.
    let len = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
    String::from_utf16_lossy(&title[..len as usize])
}

fn word_styles_popup(foreground: HWND, target: HWND) -> bool {
    let mut expected = 0;
    let mut actual = 0;
    // SAFETY: both process IDs are writable and the handles are opaque.
    unsafe {
        GetWindowThreadProcessId(target, &mut expected);
        GetWindowThreadProcessId(foreground, &mut actual);
    }
    expected != 0 && expected == actual && window_title(foreground) == "Apply Styles"
}

fn focused_control(window: HWND) -> HWND {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: GetGUIThreadInfo writes to the initialized structure for the target thread.
    let thread = unsafe { GetWindowThreadProcessId(window, ptr::null_mut()) };
    if thread != 0 && unsafe { GetGUIThreadInfo(thread, &mut info) } != 0 {
        info.hwndFocus
    } else {
        ptr::null_mut()
    }
}

fn wait_for_style_input(
    events: &Events,
    focus_is_valid: impl FnOnce() -> bool,
) -> Result<bool, String> {
    if !events.wait_through(Instant::now() + Duration::from_millis(180))? {
        return Ok(false);
    }
    if focus_is_valid() {
        Ok(true)
    } else {
        Err("The target changed while applying a paragraph style.".into())
    }
}

fn type_style_command(enigo: &mut Enigo, text: &str) -> Result<(), String> {
    enigo.text(text).map_err(style_input_error)
}

fn style_input_error(error: enigo::InputError) -> String {
    format!("Could not apply the paragraph style. {error}")
}

fn send_shortcut(target: HWND, modifiers: &[u16], keys: &[(u16, bool)]) -> Result<(), String> {
    let key_input = |key, up| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                ..Default::default()
            },
        },
    };
    let mut inputs = Vec::with_capacity(2 * (modifiers.len() + keys.len()));
    inputs.extend(modifiers.iter().map(|&key| key_input(key, false)));
    for &(key, changed) in keys {
        if changed {
            inputs.push(key_input(key, false));
            inputs.push(key_input(key, true));
        }
    }
    inputs.extend(modifiers.iter().rev().map(|&key| key_input(key, true)));

    // SAFETY: these are Win32 virtual key values and the input buffer lives through SendInput.
    let some_key_held = [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .into_iter()
        .chain(
            keys.iter()
                .filter(|(_, changed)| *changed)
                .map(|(key, _)| *key),
        )
        .any(|key| unsafe { GetAsyncKeyState(i32::from(key)) } < 0);
    if some_key_held || foreground_window() != target {
        return Err("Formatting stopped because the target or keyboard state changed.".into());
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        if sent > 0 {
            let releases: Vec<_> = keys
                .iter()
                .filter(|(_, changed)| *changed)
                .map(|(key, _)| *key)
                .chain(modifiers.iter().rev().copied())
                .map(|key| key_input(key, true))
                .collect();
            unsafe {
                SendInput(
                    releases.len() as u32,
                    releases.as_ptr(),
                    std::mem::size_of::<INPUT>() as i32,
                )
            };
        }
        return Err("Windows could not send the formatting shortcut.".into());
    }
    Ok(())
}

fn foreground_window() -> HWND {
    // SAFETY: this function takes no arguments and returns an opaque handle.
    unsafe { GetForegroundWindow() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_keeps_its_controls_in_view_with_empty_and_long_text() {
        use egui::epaint::Shape;

        for size in [[390.0, 425.0], [480.0, 540.0], [880.0, 640.0]] {
            for scale in [1.0, 1.5, 2.0] {
                for text in [String::new(), "A paragraph that wraps.\n".repeat(120)] {
                    for mode in [TimingMode::Wpm, TimingMode::Duration, TimingMode::FinishAt] {
                        let ctx = egui::Context::default();
                        set_typography(&ctx);
                        let mut app = AutoTyper::default();
                        egui::TextBuffer::insert_text(
                            &mut app.text,
                            &text,
                            egui::text::CharIndex(0),
                        );
                        app.options.mode = mode;
                        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size.into());
                        let mut input = egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        };
                        input
                            .viewports
                            .get_mut(&egui::ViewportId::ROOT)
                            .unwrap()
                            .native_pixels_per_point = Some(scale);
                        ctx.run_ui(input.clone(), |ui| app.show(ui));
                        let output = ctx.run_ui(input, |ui| app.show(ui));
                        for label in [
                            "Timing",
                            "Pauses    Periods · 5.0 s",
                            "Countdown",
                            "Start typing",
                        ] {
                            assert!(
                                output.shapes.iter().any(|clipped| {
                                    let Shape::Text(text) = &clipped.shape else {
                                        return false;
                                    };
                                    text.galley.text() == label
                                        && clipped
                                            .clip_rect
                                            .intersect(screen)
                                            .expand(1.0)
                                            .contains_rect(clipped.shape.visual_bounding_rect())
                                }),
                                "missing or clipped {label} at {size:?}, {scale}x"
                            );
                        }
                    }
                }
            }
        }
    }

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
            &[],
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
            false,
        )
    }

    #[test]
    fn exact_duration_reserves_only_pauses_between_characters() {
        let timing = Timing {
            pace: Pace::Duration(Duration::from_secs(20)),
            pauses: [5, 1, 2].map(Duration::from_secs),
            natural_writing: false,
            thinking: None,
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
            natural_writing: false,
            thinking: None,
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
            natural_writing: false,
            thinking: None,
        };
        let schedule = Schedule::plan(
            &['.', 'a', 'b'],
            &[],
            timing,
            started,
            now,
            SystemTime::UNIX_EPOCH,
            Some((0, now - Duration::from_secs(1))),
            false,
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
            natural_writing: false,
            thinking: None,
        };
        let schedule =
            Schedule::plan(&['a'], &[], timing, started, started, wall_now, None, false).unwrap();
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
    fn natural_writing_only_pauses_at_boundaries_and_fits_the_deadline() {
        let text: Vec<_> = "First we write, and then continue.\n\nAnother thought! Last one."
            .chars()
            .collect();
        let natural = natural_cadence(&text, || 0.2);
        let after_comma = text.iter().position(|&ch| ch == ',').unwrap() + 1;
        let after_newline = text.iter().rposition(|&ch| ch == '\n').unwrap();
        assert_eq!(natural[0].duration, Duration::ZERO);
        assert_eq!(natural[after_comma].covered_by, 0b010);
        assert_eq!(natural[after_newline].covered_by, 0b101);
        assert!(natural[after_comma].duration > Duration::ZERO);
        assert!(natural[after_newline].duration > natural[after_comma].duration);
        assert!(
            natural
                .iter()
                .all(|pause| pause.duration <= Duration::from_secs(2))
        );

        let start = Instant::now();
        let duration = Duration::from_secs(3);
        let timing = Timing {
            pace: Pace::Duration(duration),
            pauses: [Duration::ZERO, Duration::from_secs(1), Duration::ZERO],
            natural_writing: true,
            thinking: None,
        };
        let schedule = Schedule::plan(
            &text,
            &natural,
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
            false,
        )
        .unwrap();
        let deadlines = schedule.deadlines;
        assert_eq!(deadlines.last().copied(), Some(duration));
        assert!(deadlines.windows(2).all(|pair| pair[0] < pair[1]));
        let mut without_automatic_comma = natural;
        without_automatic_comma[after_comma].duration = Duration::ZERO;
        let without = Schedule::plan(
            &text,
            &without_automatic_comma,
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
            false,
        )
        .unwrap();
        assert_eq!(deadlines, without.deadlines);
        assert!(deadlines[after_comma] - deadlines[after_comma - 1] >= Duration::from_secs(1));
    }

    #[test]
    fn familiar_phrases_flow_and_long_words_ease_in() {
        let text: Vec<_> = "The same thing the same thing the same thing the synchronization"
            .chars()
            .collect();
        let spans: Vec<_> = word_spans(&text).collect();
        let cadence = natural_cadence(&text, || 0.0);
        let before = |index: usize| cadence[spans[index].start - 1].duration;
        assert_eq!(before(1), Duration::ZERO);
        assert_eq!(before(2), Duration::ZERO);
        assert!(before(3) > Duration::ZERO);
        assert_eq!(before(4), Duration::ZERO);
        assert_eq!(before(5), Duration::ZERO);
        let last = spans.last().unwrap();
        assert!(cadence[last.start].weight_offset > cadence[last.start + 4].weight_offset);

        let start = Instant::now();
        let timing = Timing {
            pace: Pace::Wpm(65, 0),
            pauses: [Duration::ZERO; 3],
            natural_writing: true,
            thinking: None,
        };
        let schedule = Schedule::plan(
            &text,
            &cadence,
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
            false,
        )
        .unwrap();
        let interval = |index: usize| schedule.deadlines[index] - schedule.deadlines[index - 1];
        let middle = interval(spans[1].start + 1);
        assert!(schedule.deadlines[0] >= middle.mul_f64(1.35));
        assert!(interval(spans[3].start - 1) >= middle.mul_f64(1.15));
    }

    #[test]
    fn abbreviations_and_decimals_do_not_start_a_sentence_pause() {
        let text: Vec<_> = "Dr. Smith saw 3.14 grams. Then left.".chars().collect();
        let cadence = natural_cadence(&text, || 0.0);
        let starts: Vec<_> = word_spans(&text).map(|span| span.start).collect();
        assert_eq!(cadence[starts[1] - 1].duration, Duration::ZERO);
        assert_eq!(cadence[starts[4] - 1].duration, Duration::ZERO);
        assert!(cadence[starts[6] - 1].duration >= Duration::from_millis(420));
    }

    #[test]
    fn a_long_unpunctuated_passage_gets_a_break_even_without_an_early_hesitation() {
        let text: Vec<_> = "notes ".repeat(18).chars().collect();
        let cadence = natural_cadence(&text, || 0.999);
        let breaks: Vec<_> = cadence
            .iter()
            .filter(|step| !step.duration.is_zero())
            .collect();
        assert_eq!(breaks.len(), 1);
        assert!(breaks[0].duration >= Duration::from_millis(400));
    }

    #[test]
    fn natural_wpm_measures_typing_without_pauses() {
        let text: Vec<_> = "sun. synchronization sun sun\n\nsun".chars().collect();
        let natural = natural_cadence(&text, || 0.9);
        let start = Instant::now();
        let timing = Timing {
            pace: Pace::Wpm(120, 0),
            pauses: [Duration::from_secs(1), Duration::ZERO, Duration::ZERO],
            natural_writing: true,
            thinking: None,
        };
        let schedule = Schedule::plan(
            &text,
            &natural,
            timing,
            start,
            start,
            SystemTime::UNIX_EPOCH,
            None,
            false,
        )
        .unwrap();
        let pauses = Duration::from_secs(1)
            + natural
                .iter()
                .filter(|step| step.covered_by & 1 == 0)
                .map(|step| step.duration)
                .sum::<Duration>();
        assert!(pauses > Duration::from_secs(1));
        assert_eq!(
            schedule.deadlines.last().copied(),
            Some(Duration::from_millis(100 * text.len() as u64) + pauses)
        );
    }

    #[test]
    fn thinking_uses_active_typing_seconds_at_different_speeds() {
        let mut options = Options::default();
        assert!(options.timing(Local::now()).unwrap().thinking.is_none());
        options.natural_thinking = true;
        let range = options.timing(Local::now()).unwrap().thinking.unwrap();
        assert_eq!(range, [5, 10].map(Duration::from_secs));
        assert_eq!(thinking_length([range[0]; 2], 1.0), range[0]);
        assert!((0..32).all(|_| (range[0]..=range[1]).contains(&thinking_length(range, 1.0))));

        let mut text = vec!['a'; 360];
        text[0] = ',';
        let start = Instant::now();
        for wpm in [60, 120, 600] {
            let timing = Timing {
                pace: Pace::Wpm(wpm, 0),
                pauses: [Duration::ZERO, Duration::from_secs(4), Duration::ZERO],
                natural_writing: true,
                thinking: Some(range),
            };
            let schedule = Schedule::plan(
                &text,
                &[],
                timing,
                start,
                start,
                SystemTime::UNIX_EPOCH,
                None,
                false,
            )
            .unwrap();
            let mut left = Duration::from_millis(6050);
            let at = (0..text.len())
                .find_map(|index| {
                    let at = schedule.thinking_at(index, left, Duration::ZERO);
                    left = left.saturating_sub(schedule.typing_interval(index));
                    at
                })
                .unwrap();
            assert_eq!(at - start, Duration::from_millis(10050));
        }
    }

    #[test]
    fn exhaustion_is_pace_only_and_cycles_back_to_full_efficiency() {
        let mut options = Options::default();
        assert_eq!(options.timing(Local::now()).unwrap().pace.exhaustion_reduction(), 0);
        options.exhausted_typing = true;
        let pace = options.timing(Local::now()).unwrap().pace;
        assert_eq!(pace.exhaustion_reduction(), 20);
        let mut phase = Fatigue { tired: false, remaining: Duration::from_secs(4) };
        assert!(!phase.advance(Duration::from_secs(3)));
        assert_eq!(65.0 * pace.efficiency(phase.tired), 65.0);
        assert!(phase.advance(Duration::from_secs(1)));
        assert_eq!(65.0 * pace.efficiency(phase.tired), 52.0);
        assert!(phase.advance(Duration::from_secs(12)));
        assert_eq!(65.0 * pace.efficiency(phase.tired), 65.0);
        options.mode = TimingMode::Duration;
        assert_eq!(options.timing(Local::now()).unwrap().pace.exhaustion_reduction(), 0);
    }

    #[test]
    fn exhaustion_slows_keys_and_natural_breaks_without_extending_fixed_pauses() {
        let text = ['a', '.', 'b'];
        let mut natural = [NaturalStep::default(); 3];
        natural[0].duration = Duration::from_secs(4);
        let start = Instant::now();
        let timing = Timing {
            pace: Pace::Wpm(120, 20),
            pauses: [Duration::from_secs(2), Duration::ZERO, Duration::ZERO],
            natural_writing: true,
            thinking: Some([Duration::from_secs(8); 2]),
        };
        let plan = |tired| Schedule::plan(&text, &natural, timing, start, start, SystemTime::UNIX_EPOCH, None, tired).unwrap();
        assert_eq!(plan(false).deadlines, [100, 4200, 6300].map(Duration::from_millis));
        assert_eq!(plan(true).deadlines, [125, 5250, 7375].map(Duration::from_millis));
        assert_eq!(thinking_length(timing.thinking.unwrap(), timing.pace.efficiency(true)), Duration::from_secs(10));
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
        assert!(
            !events
                .wait_through(Instant::now() + Duration::from_secs(10))
                .unwrap()
        );
    }
}
