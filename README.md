# klawply

> ⚠️ **Alpha / experimental.** klawply drives your **ChatGPT subscription**
> through an unofficial, reverse-engineered endpoint (the same one the Codex CLI
> uses) and automates a real Chrome browser. It can consume your subscription
> quota quickly and may break whenever OpenAI changes things. Use it on your own
> applications, review every form before submitting, and mind OpenAI's terms.

klawply is a terminal AI agent that applies to jobs for you. It's a green-on-black
seven-segment TUI built on the same stack as **pekchat-tui** (ratatui + tokio
channels, with a background agent thread).

```
  _  _        _         _  _  _        _
 |_/ |_  |_| |_| |  |_| |_ |_ |    |_| . . .
 | \ |_  | | | | |_ | | |    |    |         KLAWPLY
```

## What it does

1. **Connect** — on launch it connects your ChatGPT subscription via Codex-style
   OAuth (PKCE + loopback). Your browser opens to `auth.openai.com`; log in and
   it redirects back automatically. Tokens are stored at `~/.codex/auth.json`
   (shared with the Codex CLI, so an existing login is reused instantly).
   If `~/.codex/auth.json` already exists (from the Codex CLI or a previous
   run), klawply reuses it and connects **without** prompting. To log in fresh —
   e.g. to switch accounts — launch with `KLAWPLY_FORCE_LOGIN=1` or type
   `/login` in the chat box.
2. **Resume** — if `resume.txt` is blank, klawply asks you to paste
   your resume. It sends the text to ChatGPT and populates
   `~/.klawply/fields.json` with structured application fields.
3. **Apply** — paste a job posting link into the chat box. klawply opens it in
   debug-mode Chrome (launching Chrome if needed), captures the page source, asks
   ChatGPT which form controls to fill, and fills them via the Chrome DevTools
   Protocol. If the page has an **"Upload resume"** field, ChatGPT flags it and
   klawply attaches your `resume.pdf`/`.docx` over CDP (no OS file dialog). It
   also reads back anything you typed by hand and folds it into `fields.json`.

klawply never clicks the final **Submit** — you review and submit yourself.

## Prerequisites

- **Rust** (edition 2024, rustc 1.85+) — install via [rustup](https://rustup.rs).
- A **ChatGPT** subscription (Plus/Pro/Team).
- **Google Chrome / Chromium** installed. klawply **launches it for you** in debug
  mode (headed, with a persistent profile at `<data-dir>/chrome-profile`, so
  job-site logins persist across runs). You don't have to start it yourself.
  - To manage Chrome yourself instead, start it with
    `google-chrome --remote-debugging-port=9222` and set `KLAWPLY_NO_LAUNCH=1`.
  - If the binary isn't on `PATH`, point to it with `KLAWPLY_CHROME=/path/to/chrome`.

## Install

```sh
git clone https://github.com/zontyp/Klawply.git
cd Klawply
cargo build --release        # first build is a few minutes (chromiumoxide is large)
```

The binary is then at `target/release/klawply` (or just use `cargo run` below).

## Run

```sh
cargo run                    # from the cloned folder
# or, after `cargo build --release`:
./target/release/klawply
```

On first launch klawply opens your browser to log in to ChatGPT (unless a Codex
CLI login already exists at `~/.codex/auth.json`, which it reuses). `resume.txt`
and `fields.json` are created in whatever folder you run it from.

<details>
<summary>Legacy: run with an existing debug Chrome instead of auto-launch</summary>

```sh
google-chrome --remote-debugging-port=9222 &
KLAWPLY_NO_LAUNCH=1 cargo run
```
</details>

## Keys & commands

- **Connect screen** — `Enter` connect · `c` copy the login link · `q` quit
- **Resume screen** — paste your resume, then `Enter` (or `Ctrl+D`) to save
- **Chat screen** — `Enter` send · drag to select + `Ctrl+C` to copy ·
  `PgUp`/`PgDn` scroll · `Ctrl+C` quit
- **Type in the chat box:**
  - a **job link** → klawply opens it and fills the form
  - **`/sync`** → read what you typed on the page and save it to `fields.json`
  - **`/login`** → sign in to ChatGPT again (switch accounts)
  - anything else → a normal question to the assistant

## Files

| Path | Purpose |
|------|---------|
| `./resume.txt` | Your resume text (source of truth for extraction). Saved in the folder you launch klawply from. |
| `./fields.json` | Structured application fields the agent fills forms from. Saved alongside `resume.txt`. |
| `./resume.pdf` *(or `.docx`)* | Optional resume **document**. When a job page has an "Upload resume" field, klawply attaches this file. Put it in the klawply folder (or point to it with `KLAWPLY_RESUME_FILE`). |
| `~/.codex/auth.json` | ChatGPT OAuth tokens (shared with the Codex CLI). |

## Configuration (env)

| Variable | Default | Meaning |
|----------|---------|---------|
| `KLAWPLY_MODEL` | `gpt-5.5` | Model id sent to the backend (Hermes uses `gpt-5.5` for the Codex backend; `gpt-5.4` / `gpt-5.3-codex` are fallbacks). |
| `KLAWPLY_FORCE_LOGIN` | `0` | Set to `1` to skip the stored-token fast path and force a fresh browser login. |
| `KLAWPLY_NO_LAUNCH` | `0` | Set to `1` to stop klawply launching Chrome (you start it yourself). |
| `KLAWPLY_CHROME` | (auto) | Path to the Chrome/Chromium binary if not on `PATH`. |
| `KLAWPLY_CHROME_HEADLESS` | `0` | Set to `1` to launch Chrome headless (you won't see the form fill). |
| `KLAWPLY_RESUME_FILE` | (auto) | Path to the resume document to upload; defaults to `resume.pdf`/`.docx` in the data dir. |
| `KLAWPLY_LOG` | `info` | Log level: `off\|error\|warn\|info\|debug`. Logs go to `<data-dir>/klawply.log`. |
| `KLAWPLY_HOME` | current dir | Where `resume.txt` / `fields.json` are stored. |
| `CODEX_HOME` | `~/.codex` | Where `auth.json` lives. |

## How it works (modules)

| Module | Role |
|--------|------|
| `auth.rs` | Codex-style ChatGPT OAuth (PKCE, loopback on :1455, refresh). |
| `llm.rs` | Calls `chatgpt.com/backend-api/codex/responses`; resume→fields, page→form-fill planning. |
| `browser.rs` | `chromiumoxide` CDP client — capture page source, fill/click/select, snapshot values. |
| `agent.rs` | Background worker: connect → resume → apply, on its own thread. |
| `app.rs` / `event.rs` | UI state machine and the command/event channels. |
| `ui/` | ratatui rendering + the seven-segment font. |

## Caveats

- The ChatGPT backend and its request shape are **reverse-engineered** and
  unofficial; `llm.rs::complete()` centralizes the wire format so it's easy to
  patch when it drifts.
- `KLAWPLY_MODEL` defaults to `gpt-5.5` (the model Hermes selects for the Codex
  backend). Some ChatGPT accounts intermittently reject it on the Codex backend
  with no error — klawply detects the empty reply and suggests `gpt-5.4` /
  `gpt-5.3-codex`.
- Form filling is heuristic — always review the page before submitting.
