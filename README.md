# QE Utils — Unreal Engine Log Watcher (TUI)

A tiny cross-platform terminal UI to watch Unreal Engine project logs in real time. It reads the latest .log file of a selected project and provides a clean, interactive view with filtering and navigation.

This app is written in Rust using ratatui and crossterm.


## Features
- Project selection menu at startup (reads projects.json)
- Tails the current project log (starts from EOF to avoid flooding old lines)
- Optional timestamp display (first [ ... ] only); thread index [ .. ] is hidden
- Category styling and quick filtering:
  - Lines like `... LogRenderer: message` show the `LogRenderer:` category styled
  - Click on the category to filter by that category
  - The active filter is shown at the top-right; clear with `F`
- Smooth scrolling and basic color highlighting for warnings/errors
- Non-intrusive footer status (italic), e.g., shows the watched log path


## Controls
- Up/Down: scroll by 1 line
- PageUp/PageDown: scroll by 10 lines
- Home/End: jump to start/end
- C: clear the current view (and jump to the latest log tail)
- T: toggle timestamp visibility (first bracketed timestamp only)
- F: clear active category filter
- S: switch back to the project selection menu
- Q or Esc: quit
- Mouse: left-click on the category token (e.g., `LogRenderer:`) to filter by that category


## Configuration (projects.json)
Place a projects.json file either:
1) Next to the compiled binary, or
2) In the current working directory when running the app (useful for `cargo run`)

Example projects.json:
```json
{
  "projects": [
    {
      "key": "game",
      "name": "My UE Game",
      "uproject": "D:/UE/MyGame/MyGame.uproject"
    },
    {
      "key": "pcg",
      "name": "Procedural Tools",
      "uproject": "C:/Work/UE/PCG/PCG.uproject"
    }
  ]
}
```
Fields:
- key: short identifier used internally
- name: pretty name shown in the UI (optional; falls back to key)
- uproject: absolute or relative path to your .uproject file

The log file is resolved as `<uproject_dir>/Saved/Logs/<ProjectName>.log`.


## Building and Running (from source)
Prerequisites:
- Rust toolchain with Cargo (https://rustup.rs)

Build and run in debug mode:
```
cargo run
```
If your projects.json is not in the repo root, place it next to the produced binary (target\debug\qe-utils.exe on Windows), or run from the directory that contains projects.json.


## Install locally with Cargo
You can install the app to your Cargo bin directory using:
```
cargo install --path .
```
This compiles the project in release mode and places the binary into Cargo's bin folder (e.g., `%USERPROFILE%\.cargo\bin` on Windows). Ensure this folder is on your PATH.

After installation, you can run it from anywhere:
```
qe-utils
```
Remember to provide a valid projects.json either in the current directory or next to the installed binary.

To update the installed binary after changes, run the install command again:
```
cargo install --path . --force
```


## Notes and Troubleshooting
- If the UI shows "Watching: <path>" but no lines appear, the log may not have new content yet. Trigger activity in the project or ensure the path is correct.
- The app starts tailing from the end of the file (EOF) by design, so it doesn't flood old logs.
- Category detection expects a token like `Word:` with no spaces before the colon; lines without that form will still be shown (just without a clickable category).
- Terminal features vary; underline/italic rendering depends on your terminal emulator.
- On Windows, ensure the terminal supports mouse events. If clicks don't filter, try a different terminal (e.g., Windows Terminal or newer PowerShell consoles).


## License
This project is provided as-is for internal usage. Add a license file if you plan to distribute it.