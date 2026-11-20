use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    sync::{Arc, OnceLock, mpsc},
    thread,
};

use chrono::Local;
use eframe::egui::{self, TextBuffer};
use serde_json::{Value, json};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

use crate::{
    AutoTyper, Document, Options, Pace, Status, TimingEdit, file_import, foreground_window,
    paragraph_ends,
};

const MAX_MESSAGE: u64 = 1024 * 1024;
const VERSION: &str = "2025-11-25";
type ToolResult = Result<Value, String>;

struct Call {
    name: String,
    arguments: Value,
    reply: mpsc::Sender<ToolResult>,
}

pub struct Session {
    calls: mpsc::Receiver<Call>,
    first: Option<Call>,
    wake: Arc<OnceLock<egui::Context>>,
}

impl Session {
    pub fn start() -> io::Result<Option<Self>> {
        let (out, calls) = mpsc::channel();
        let wake: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        let repaint = Arc::clone(&wake);
        thread::Builder::new()
            .name("mcp stdio".into())
            .spawn(move || {
                let _ = serve(
                    &mut io::stdin().lock(),
                    &mut io::stdout().lock(),
                    |name, arguments| {
                        let (reply, response) = mpsc::channel();
                        out.send(Call {
                            name,
                            arguments,
                            reply,
                        })
                        .map_err(|_| "AutoTyper closed.")?;
                        if let Some(ctx) = repaint.get() {
                            ctx.request_repaint();
                        }
                        response
                            .recv()
                            .map_err(|_| "AutoTyper closed while handling the request.")?
                    },
                );
                drop(out);
                if let Some(ctx) = repaint.get() {
                    ctx.request_repaint();
                }
            })?;
        // A client can list tools without opening a desktop window.
        Ok(calls.recv().ok().map(|first| Self {
            calls,
            first: Some(first),
            wake,
        }))
    }

    pub fn wake_with(&self, context: egui::Context) {
        let _ = self.wake.set(context);
    }

    pub fn process(&mut self, app: &mut AutoTyper, ctx: &egui::Context) -> bool {
        loop {
            let call = match self
                .first
                .take()
                .map(Ok)
                .unwrap_or_else(|| self.calls.try_recv())
            {
                Ok(call) => call,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            };
            let _ = call
                .reply
                .send(call_tool(app, ctx, &call.name, call.arguments));
        }
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code,"message":message}})
}

fn serve(
    input: &mut impl BufRead,
    output: &mut impl Write,
    mut call: impl FnMut(String, Value) -> ToolResult,
) -> io::Result<()> {
    let mut initialized = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut *input)
            .take(MAX_MESSAGE + 1)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        if line.len() as u64 > MAX_MESSAGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds 1 MiB",
            ));
        }
        let response = match serde_json::from_slice::<Value>(&line) {
            Err(_) => error(Value::Null, -32700, "Parse error"),
            Ok(request) => {
                let id = request.get("id").cloned();
                let Some(method) = request["method"]
                    .as_str()
                    .filter(|_| request["jsonrpc"] == "2.0")
                else {
                    write_message(
                        output,
                        &error(id.unwrap_or(Value::Null), -32600, "Invalid request"),
                    )?;
                    continue;
                };
                let Some(id) = id else { continue };
                if !(id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()) {
                    write_message(output, &error(Value::Null, -32600, "Invalid request ID"))?;
                    continue;
                }
                let params = &request["params"];
                let result = match method {
                    "initialize" if params["protocolVersion"].is_string() => {
                        initialized = true;
                        let requested = params["protocolVersion"].as_str().unwrap_or(VERSION);
                        let protocol = match requested {
                            "2024-11-05" | "2025-03-26" | "2025-06-18" => requested,
                            _ => VERSION,
                        };
                        json!({
                            "protocolVersion":protocol,
                            "capabilities":{"tools":{}},
                            "serverInfo":{"name":"autotyper","version":env!("CARGO_PKG_VERSION")},
                            "instructions":"AutoTyper opens a local window on the first tool call. Set a draft and timing, focus AutoTyper before starting, then click the intended text field during the countdown. Resume uses the original field."
                        })
                    }
                    "initialize" => {
                        write_message(output, &error(id, -32602, "Missing protocolVersion"))?;
                        continue;
                    }
                    "ping" => json!({}),
                    _ if !initialized => {
                        write_message(output, &error(id, -32002, "Initialize first"))?;
                        continue;
                    }
                    "tools/list" => json!({"tools":tools()}),
                    "tools/call" => {
                        let Some(name) = params["name"].as_str() else {
                            write_message(output, &error(id, -32602, "Missing tool name"))?;
                            continue;
                        };
                        let arguments = params
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| json!({}));
                        let outcome = if arguments.is_object() {
                            call(name.to_owned(), arguments)
                        } else {
                            Err("Tool arguments must be an object.".into())
                        };
                        match outcome {
                            Ok(content) => {
                                json!({"content":[{"type":"text","text":content.to_string()}],"structuredContent":content})
                            }
                            Err(message) => {
                                json!({"content":[{"type":"text","text":message}],"isError":true})
                            }
                        }
                    }
                    _ => {
                        write_message(output, &error(id, -32601, "Method not found"))?;
                        continue;
                    }
                };
                json!({"jsonrpc":"2.0","id":id,"result":result})
            }
        };
        write_message(output, &response)?;
    }
}

fn write_message(output: &mut impl Write, response: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *output, response)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn tools() -> Value {
    json!([
        {"name":"autotyper_state","description":"Read draft, zero-based paragraph boundaries and settings, run status and progress. Offsets are Unicode character indices. Separators at a boundary count toward the following paragraph.","inputSchema":{"type":"object","properties":{"include_text":{"type":"boolean","default":false}},"additionalProperties":false}},
        {"name":"autotyper_set_draft","description":"Replace the draft with text and optional matching HTML, or import an absolute path to a .doc, .docx or .txt file. Replacing a draft clears its paragraph timings.","inputSchema":{"type":"object","properties":{"text":{"type":"string"},"html":{"type":"string"},"path":{"type":"string"}},"oneOf":[{"required":["text"],"not":{"required":["path"]}},{"required":["path"],"not":{"anyOf":[{"required":["text"]},{"required":["html"]}]}}],"additionalProperties":false}},
        {"name":"autotyper_configure","description":"Patch timing defaults or a zero-based paragraph. The first paragraph override enables separate timing for every paragraph of this draft. During a run, omitted paragraph applies timing to the current paragraph; a supplied index updates that current or upcoming paragraph in a paragraph-timed run. Changes to active timing are queued; inspect state for timing feedback.","inputSchema":{
            "type":"object","properties":{
                "paragraph":{"type":"integer","minimum":0},
                "plain_text":{"type":"boolean","description":"Send plain text instead of applying formatting."},
                "always_on_top":{"type":"boolean"},
                "settings":{"type":"object","additionalProperties":false,"properties":{
                    "mode":{"enum":["wpm","duration","finish_at"]},
                    "wpm":{"type":"integer","minimum":1,"maximum":600},
                    "total_seconds":{"type":"number","minimum":0.1,"maximum":86400},
                    "finish_at":{"type":"string","description":"Local clock time, HH:MM or HH:MM:SS."},
                    "countdown":{"type":"integer","minimum":1,"maximum":60},
                    "pauses":{"type":"array","description":"Exactly three entries in order: periods, commas, new lines.","minItems":3,"maxItems":3,"items":{"type":"object","properties":{"enabled":{"type":"boolean"},"seconds":{"type":"number","minimum":0,"maximum":600}},"required":["enabled","seconds"],"additionalProperties":false}},
                    "natural_writing":{"type":"boolean"},
                    "natural_thinking":{"type":"boolean"},
                    "thinking_seconds":{"type":"array","description":"Minimum and maximum break in seconds, in order.","minItems":2,"maxItems":2,"items":{"type":"number","minimum":0.1,"maximum":600}},
                    "exhausted_typing":{"type":"boolean"},
                    "exhaustion_reduction":{"type":"integer","minimum":1,"maximum":90}
                }}
            },"additionalProperties":false
        }},
        {"name":"autotyper_playback","description":"Start a countdown, pause, resume, or cancel. Start requires the AutoTyper window in the foreground so you can select the destination during the countdown. Resume waits for the original field.","inputSchema":{"type":"object","properties":{"action":{"enum":["start","pause","resume","cancel"]}},"required":["action"],"additionalProperties":false}}
    ])
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StateArgs {
    #[serde(default)]
    include_text: bool,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftArgs {
    text: Option<String>,
    html: Option<String>,
    path: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureArgs {
    paragraph: Option<usize>,
    settings: Option<serde_json::Map<String, Value>>,
    plain_text: Option<bool>,
    always_on_top: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybackArgs {
    action: String,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| format!("Invalid arguments. {error}"))
}

fn call_tool(app: &mut AutoTyper, ctx: &egui::Context, name: &str, arguments: Value) -> ToolResult {
    match name {
        "autotyper_state" => Ok(state(app, parse::<StateArgs>(arguments)?.include_text)),
        "autotyper_set_draft" => {
            if app.active.is_some() || app.capture.is_some() {
                return Err("Cancel or finish the run before replacing the draft.".into());
            }
            let args: DraftArgs = parse(arguments)?;
            let document = match (args.text, args.html, args.path) {
                (Some(text), html, None) => {
                    let mut document = Document::default();
                    if let Some(html) = html {
                        document.prepare_paste(&text, &html);
                    }
                    document.insert_text(&text, egui::text::CharIndex(0));
                    document
                }
                (None, None, Some(path)) if path.is_absolute() => file_import::read(&path)?,
                _ => return Err("Supply text with optional HTML, or an absolute file path.".into()),
            };
            app.text = document;
            app.paragraph_settings.clear();
            app.sent = 0;
            app.total = 0;
            app.reading = false;
            app.timing_open = false;
            app.status = Status::Ready;
            Ok(state(app, false))
        }
        "autotyper_configure" => {
            configure(app, ctx, parse(arguments)?)?;
            Ok(state(app, false))
        }
        "autotyper_playback" => {
            match parse::<PlaybackArgs>(arguments)?.action.as_str() {
                "start" if app.active.is_none() && app.capture.is_none() => {
                    if app.text.is_empty() {
                        return Err("Load a draft before starting.".into());
                    }
                    let mut foreground_process = 0;
                    // SAFETY: Windows validates the handle and writes into a live PID buffer.
                    unsafe {
                        GetWindowThreadProcessId(foreground_window(), &mut foreground_process)
                    };
                    if foreground_process != std::process::id() {
                        return Err("Focus the AutoTyper window before starting, then select the destination field during the countdown.".into());
                    }
                    app.reading = false;
                    app.prepare_start(ctx);
                }
                "start" => return Err("A run is already in progress.".into()),
                "pause" if matches!(app.status, Status::Paused(_) | Status::Stopping) => {}
                "pause" if app.active.is_some() => app.stop(),
                "pause" => return Err("There is no active run to pause.".into()),
                "resume" if matches!(app.status, Status::Paused(_)) => app.resume(),
                "resume" => return Err("There is no paused run to resume.".into()),
                "cancel" => {
                    app.active = None;
                    app.capture = None;
                    app.status = Status::Stopped;
                    app.timing_open = false;
                }
                _ => return Err("Use start, pause, resume or cancel.".into()),
            }
            Ok(state(app, false))
        }
        _ => Err("Unknown AutoTyper tool.".into()),
    }
}

fn configure(app: &mut AutoTyper, ctx: &egui::Context, args: ConfigureArgs) -> Result<(), String> {
    if args.plain_text.is_some() && (app.active.is_some() || app.capture.is_some()) {
        return Err("Set formatting before starting the run.".into());
    }
    if let Some(patch) = args.settings {
        if app.capture.is_some() {
            return Err("Wait until the run starts before changing timing.".into());
        }
        let index = args.paragraph;
        let current = if let Some(index) = index {
            let count = paragraph_ends(&app.text.chars().collect::<Vec<_>>()).len();
            if index >= count {
                return Err(format!("Paragraph {index} is out of range for this draft."));
            }
            if let Some(run) = &app.active
                && (index < app.running_paragraph
                    || app.paragraph_settings.is_empty()
                    || index >= run.paragraph_count)
            {
                return Err("Edit a current or future paragraph in a paragraph-timed run.".into());
            }
            app.paragraph_settings.get(&index).unwrap_or(&app.options)
        } else {
            &app.options
        };
        let mut settings = serde_json::to_value(current).map_err(|error| error.to_string())?;
        settings
            .as_object_mut()
            .expect("Options serialize as an object")
            .extend(patch);
        let options: Options = parse(settings)?;
        let [minimum, maximum] = options.thinking_seconds;
        if !(1..=600).contains(&options.wpm)
            || !(1..=60).contains(&options.countdown)
            || !(0.1..=86_400.0).contains(&options.total_seconds)
            || !(1..=90).contains(&options.exhaustion_reduction)
            || !(0.1..=600.0).contains(&minimum)
            || !(minimum..=600.0).contains(&maximum)
            || options
                .pauses
                .iter()
                .any(|pause| !(0.0..=600.0).contains(&pause.seconds))
        {
            return Err("One or more timing settings are outside the supported range.".into());
        }
        let timing = options.timing(Local::now())?;
        if app.active.is_some() {
            app.send_timing(TimingEdit {
                paragraph: index,
                timing,
            })?;
            app.timing_feedback = None;
        }
        if let Some(index) = index {
            app.paragraph_settings.insert(index, options);
        } else {
            app.options = options;
        }
    }
    if let Some(plain_text) = args.plain_text {
        app.plain_text = plain_text;
    }
    if let Some(top) = args.always_on_top {
        app.always_on_top = top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
    }
    Ok(())
}

fn state(app: &AutoTyper, include_text: bool) -> Value {
    let (status, message) = match &app.status {
        Status::Ready => ("ready", None),
        Status::Starting => ("starting", None),
        Status::Countdown(_) => ("countdown", None),
        Status::Typing => ("typing", None),
        Status::Thinking => ("thinking", None),
        Status::FocusPaused => ("waiting_for_target", None),
        Status::Paused(message) => ("paused", message.as_deref()),
        Status::Stopping => ("pausing", None),
        Status::Stopped => ("stopped", None),
        Status::Completed => ("completed", None),
        Status::Error(message) => ("error", Some(message.as_str())),
    };
    let text: Vec<char> = app.text.chars().collect();
    let mut start = 0;
    let paragraphs: Vec<_> = paragraph_ends(&text).into_iter().enumerate().map(|(index, end)| {
        let mut entry = json!({"index":index,"start_char":start,"end_char":end,"settings":app.paragraph_settings.get(&index)});
        if include_text {
            entry["text"] = json!(text[start..end].iter().collect::<String>());
        }
        start = end;
        entry
    }).collect();
    let countdown_remaining = if let Status::Countdown(end) = app.status {
        Some(
            end.saturating_duration_since(std::time::Instant::now())
                .as_secs_f64(),
        )
    } else {
        None
    };
    let applied = app.applied_timing.map(|timing| {
        let pace = match timing.pace {
            Pace::Wpm(wpm, reduction) => json!({"mode":"wpm","wpm":wpm,"exhaustion_reduction":reduction}),
            Pace::Duration(duration) => json!({"mode":"duration","total_seconds":duration.as_secs_f64()}),
            Pace::FinishAt(at) => json!({"mode":"finish_at","finish_at":chrono::DateTime::<Local>::from(at).to_rfc3339()}),
        };
        json!({"pace":pace,"pauses_seconds":timing.pauses.map(|pause| pause.as_secs_f64()),
            "natural_writing":timing.natural_writing,
            "thinking_seconds":timing.thinking.map(|range| range.map(|duration| duration.as_secs_f64()))})
    });
    json!({
        "status":status,"message":message,"sent":app.sent,"total":text.len(),
        "countdown_seconds_remaining":countdown_remaining,"applied_timing":applied,
        "current_paragraph":app.active.as_ref().filter(|_| !app.paragraph_settings.is_empty()).map(|_| app.running_paragraph),
        "settings":app.options,"paragraphs":paragraphs,"plain_text":app.plain_text,
        "always_on_top":app.always_on_top,"has_styles":app.text.has_styles(),
        "timing_feedback":app.timing_feedback.as_ref().map(|result| match result {
            Ok(()) => json!({"applied":true}),
            Err(message) => json!({"applied":false,"message":message})
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_negotiates_and_lists_tools_without_opening_the_window() {
        let messages = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        );
        let mut output = Vec::new();
        serve(&mut messages.as_bytes(), &mut output, |_, _| {
            panic!("unexpected app call")
        })
        .unwrap();
        let responses: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["result"]["protocolVersion"], VERSION);
        assert_eq!(responses[1]["result"]["tools"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn settings_patch_a_unicode_paragraph_without_changing_its_neighbor() {
        let mut app = AutoTyper::default();
        let context = egui::Context::default();
        call_tool(
            &mut app,
            &context,
            "autotyper_set_draft",
            json!({"text":"Café.\n\nSecond.\n"}),
        )
        .unwrap();
        configure(
            &mut app,
            &context,
            parse(json!({"paragraph":1,"settings":{"mode":"duration","total_seconds":3.0}}))
                .unwrap(),
        )
        .unwrap();
        let snapshot = state(&app, true);
        assert_eq!(snapshot["paragraphs"][0]["end_char"], 5);
        assert_eq!(snapshot["paragraphs"][0]["settings"], Value::Null);
        assert_eq!(snapshot["paragraphs"][1]["text"], "\n\nSecond.\n");
        assert_eq!(snapshot["paragraphs"][1]["settings"]["total_seconds"], 3.0);
        assert_eq!(app.options.wpm, 65);
    }
}
