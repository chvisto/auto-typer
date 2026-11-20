# AutoTyper

AutoTyper is a portable Windows utility. Paste your text, choose a typing speed or finish time and click **Start typing**. During the countdown, focus the destination field. The typing view shows your progress and lets you adjust the remaining timing. Press **Esc** or click **Stop** to pause. Use **Resume** or **Ctrl+Enter** to continue in the original field, or **Back to editor** to end the run.

Download **AutoTyper-windows-x64** from the latest successful [Windows build](https://github.com/chvisto/auto-typer/actions/workflows/windows.yml), then extract and run `AutoTyper.exe`. To remove it, close the app and delete the executable. You can also delete the ZIP if you kept it. The app keeps text and settings in memory and runs only while its window is open.

To build from source on Windows, use Rust 1.95 or newer and the matching Windows C++ build tools. Run `cargo build --release --locked` from this folder.

## Local MCP

Point a local MCP client at the installed executable and pass \`--mcp\`. For Codex on Windows, run this from PowerShell:

\`\`\`powershell
codex mcp add autotyper -- "$env:LOCALAPPDATA\Programs\AutoTyper.exe" --mcp
\`\`\`

Other clients can use the executable's absolute path as the stdio command and \`["--mcp"]\` as its arguments. Tool discovery stays in the background. The first tool call opens a separate **AutoTyper · MCP** window for that client. The session ends when the client disconnects or you close its window. The installer-free app opens no network port.

| Tool | What it does |
| --- | --- |
| \`autotyper_state\` | Reads progress, current timing, and zero-based paragraph boundaries. Use \`include_text: true\` to return draft text. |
| \`autotyper_set_draft\` | Replaces the draft with \`text\` and optional matching \`html\`, or imports an absolute \`path\` to a supported file. |
| \`autotyper_configure\` | Changes timing, formatting and always-on-top. Supply \`paragraph\` and \`settings\` for a paragraph override. |
| \`autotyper_playback\` | Starts, pauses, resumes or cancels the run. |

For example, \`{"paragraph":0,"settings":{"mode":"duration","total_seconds":12}}\` gives the first paragraph 12 seconds and \`{"paragraph":1,"settings":{"mode":"wpm","wpm":80}}\` sets the second paragraph to 80 WPM. Line breaks at a boundary count toward the next paragraph's timing. On a paragraph-timed draft, unconfigured paragraphs use the default settings. Without overrides, the timing applies to the whole draft. An agent can update a current or upcoming paragraph during a paragraph-timed run and inspect timing feedback with \`autotyper_state\`.

Before the agent calls start, focus its AutoTyper window, then click the destination field during the countdown. Resume waits for the original field. Close the app and remove the client entry when uninstalling. In Codex, use \`codex mcp remove autotyper\`.
