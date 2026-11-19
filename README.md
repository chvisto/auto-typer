# AutoTyper

AutoTyper is a portable Windows utility. Paste your text, choose a typing speed or finish time and click **Start typing**. During the countdown, focus the destination field. The typing view shows your progress and lets you adjust the remaining timing. Press **Esc** or click **Stop** to cancel.

Download **AutoTyper-windows-x64** from the latest successful [Windows build](https://github.com/chvisto/auto-typer/actions/workflows/windows.yml), then extract and run `AutoTyper.exe`. To remove it, close the app and delete the executable. You can also delete the ZIP if you kept it. The app keeps text and settings in memory and runs only while its window is open.

To build from source on Windows, use Rust 1.95 or newer and the matching Windows C++ build tools. Run `cargo build --release --locked` from this folder.
