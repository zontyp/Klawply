# CLAUDE.md — klawply

Guidance for Claude Code (and humans) working in this repo. Read this before
changing architecture, the OAuth flow, or the ChatGPT wire format.

---

## 1. Project goal

**klawply is a terminal AI agent that fills out job applications for you, driven
by your ChatGPT subscription.**

The intended loop:

1. **Connect** your ChatGPT subscription (Codex-style OAuth, no API key).
2. Read your **resume** once and distill it into structured fields.
3. You paste a **job posting URL**; klawply opens it in a debug Chrome, reads the
   page, asks the model which form controls to fill, and fills them over the
   Chrome DevTools Protocol.
4. It never clicks the final **Submit** — you review and submit yourself.

Design pillars:

- **All-Rust agent** — turn logic, state, and browser control are Rust; no Python
  or Node sidecar. (Playwright is JS/Python; we use `chromiumoxide` = CDP.)
- **Same TUI stack as pekchat-tui** — `ratatui` + `tokio` channels + a background
  worker thread. Green-on-black, seven-segment title (sci-fi watch aesthetic).
- **Uses the ChatGPT *subscription*, not the paid API** — the OAuth + backend are
  the reverse-engineered Codex path (see §5), the same one Hermes brokers.

Non-goals: multi-account orchestration, auto-submitting applications, scraping at
scale, or bypassing site anti-bot measures.

---

## 2. Repo layout

```
klawply/
├── Cargo.toml            # ratatui, tokio(multi-thread), reqwest(rustls,blocking),
│                         # chromiumoxide, sha2/rand/base64 (PKCE), webbrowser
├── CLAUDE.md             # this file
├── README.md            # user-facing docs
└── src/
    ├── main.rs           # runtime + event loop + terminal + clipboard (OSC 52)
    ├── config.rs         # ALL endpoints/ids/paths/model — the tuning surface
    ├── theme.rs          # green-phosphor palette
    ├── event.rs          # AgentCommand / AgentEvent (the two channels)
    ├── app.rs            # UI-side state machine + input + selection plumbing
    ├── agent.rs          # background worker: connect → resume → apply
    ├── auth.rs           # Codex OAuth (PKCE, loopback :1455, refresh, token store)
    ├── llm.rs            # ChatGPT backend calls + JSON parsing of replies
    ├── browser.rs        # chromiumoxide CDP: capture page, fill/click/select
    ├── fields.rs         # resume.txt + fields.json IO
    ├── selection.rs      # mouse-drag text selection (ported from pekchat)
    └── ui/
        ├── mod.rs        # rendering: connect / resume / chat screens
        └── seven_segment.rs  # the 3-row seven-segment font
```

---

## 3. System design

### 3.1 Threads & channels

Two threads plus a runtime. The UI thread never blocks on network or browser
work — it sends a command and reacts to events.

```
        ┌──────────────────────── process ────────────────────────┐
        │                                                          │
        │   UI thread (tokio multi-thread runtime, block_on)       │
        │   ┌────────────────────────────────────────────────┐    │
        │   │  event_loop  (main.rs::run)                     │    │
        │   │    tokio::select! {                             │    │
        │   │      input_rx  ← terminal reader thread         │    │
        │   │      event_rx  ← agent thread (AgentEvent)      │    │
        │   │    }                                            │    │
        │   │  → App::handle_event / App::on_key              │    │
        │   │  → ui::draw(frame, &mut app)                    │    │
        │   └───────────────┬───────────────▲────────────────┘    │
        │        cmd_tx (AgentCommand) │     │ event_tx (AgentEvent)│
        │                              ▼     │                     │
        │   agent thread (std::thread, owns Handle)                │
        │   ┌────────────────────────────────────────────────┐    │
        │   │  agent::run — blocking_recv() loop              │    │
        │   │    tokens: Option<Tokens>                       │    │
        │   │    browser: Option<BrowserSession>              │    │
        │   │    handle.block_on(async browser ops) ──────────┼──┐ │
        │   │    reqwest::blocking (auth + llm) ───────────────┼─┐│ │
        │   └────────────────────────────────────────────────┘ ││ │
        │                                                       ││ │
        │   terminal reader thread (std::thread)                ││ │
        │     crossterm::event::read() → input_tx               ││ │
        └───────────────────────────────────────────────────────┼┼─┘
                                                                 ││
                        auth.openai.com / chatgpt.com ◀──────────┘│  (HTTPS)
                        Chrome CDP  ws://localhost:9222 ◀──────────┘  (WebSocket)
```

Why this shape (mirrors pekchat-tui):

- The **agent thread is std, not async** — the turn logic is naturally
  sequential/blocking (OAuth token exchange, LLM HTTP, form fill). It uses
  `reqwest::blocking` for HTTP and a tokio `Handle` to `block_on` the async
  `chromiumoxide` calls. Keeping it off the runtime's worker threads avoids
  "blocking-in-async" foot-guns.
- The **UI runtime is multi-thread with `enable_all`** (unlike pekchat's
  current-thread runtime) because we have real IO drivers to power: the OAuth
  loopback server, HTTPS, and the CDP websocket (+ its `Handler` task).

### 3.2 Screen state machine (app.rs)

```
              start_connect()               NeedResume
   ┌────────┐  (Connect cmd)   ┌─────────┐  (resume.txt   ┌────────┐
   │Connect │ ───────────────▶ │ (agent) │ ─── blank) ───▶│ Resume │
   │ screen │                  │ connect │                │ screen │
   └────────┘                  └────┬────┘                └───┬────┘
       ▲                            │ Ready                   │ SubmitResume
       │ /login                     │ (resume present)        │ → extract → Ready
       │                            ▼                         ▼
       └──────────────────────  ┌────────┐ ◀──────────────────┘
                                │  Chat  │
                                │ screen │  paste URL → apply pipeline
                                └────────┘  free text → chat; "/login" → re-auth
```

`Screen` ∈ `{Connect, Resume, Chat}` lives in `app.rs`. Transitions are driven
**only** by `AgentEvent`s from the worker (`NeedResume`, `Ready`), never guessed
by the UI.

### 3.3 The "apply" pipeline (agent.rs::apply_to)

```
  user pastes URL
        │
        ▼
  ensure_browser()  ── first use ──▶ BrowserSession::connect()
        │                             (GET :9222/json/version → ws → CDP)
        ▼
  open_and_capture(url)   ── CDP ──▶  goto + wait_for_navigation + content()  → HTML
        │
        ▼
  fields = fields::load_fields()           (from ./fields.json)
        │
        ▼
  llm::plan_form_fill(tokens, HTML, fields)  ── HTTPS ──▶  ChatGPT backend
        │                                                  returns JSON [Action]
        ▼
  for action in actions:  browser.apply(action)  ── CDP ──▶ JS shim sets value +
        │                                                    dispatches input/change
        ▼
  browser.snapshot_values()  ── CDP ──▶  read back user-typed values
        │
        ▼
  fields::merge_fields(snapshot)   → learn corrections into ./fields.json
        │
        ▼
  emit Applied { url, filled, failed }   (never clicks Submit)
```

### 3.4 Data / files

```
  ~/.codex/auth.json     OAuth tokens (shared with Codex CLI)   ← auth.rs
  ./resume.txt           raw resume text (source of truth)      ← fields.rs
  ./fields.json          {snake_case_field: "value"} for forms  ← fields.rs
```

`resume.txt`/`fields.json` default to the **current working directory** (the
folder you launch from); override with `KLAWPLY_HOME`.

---

## 4. Module reference

| Module | Responsibility | Key entry points |
|--------|----------------|------------------|
| `main.rs` | Runtime, terminal init/restore, `select!` loop, terminal-event → app, OSC 52 clipboard | `run`, `handle_terminal_event`, `copy_to_clipboard` |
| `config.rs` | Every endpoint, client id, path, model, port | `oauth_*`, `CODEX_BASE_URL`, `model()`, `*_path()` |
| `event.rs` | The command/event enums crossing the thread boundary | `AgentCommand`, `AgentEvent` |
| `app.rs` | Screen enum, transcript, input buffer, selection, event reducer | `handle_event`, `on_key`, `*_selection`, `set_chat_view` |
| `agent.rs` | The turn engine on its own thread | `run`, `connect`, `interactive_login`, `submit_resume`, `apply_to`, `with_refresh` |
| `auth.rs` | Codex OAuth + token store | `login`, `refresh`, `load_existing`, `save`, `enrich_from_id_token` |
| `llm.rs` | ChatGPT backend wire format + reply parsing | `complete`, `extract_fields`, `plan_form_fill`, `chat` |
| `browser.rs` | CDP session | `connect`, `open_and_capture`, `apply`, `snapshot_values` |
| `fields.rs` | Local persistence | `read_resume`, `write_resume`, `load_fields`, `merge_fields` |
| `selection.rs` | Mouse selection lifecycle | `Selection::{new,drag,range,extract}` |
| `ui/mod.rs` | All rendering | `draw`, `wrap_transcript`, `highlight_line` |
| `ui/seven_segment.rs` | 3-row font | `render(text) -> [String; 3]` |

---

## 5. Implementation details that matter

### 5.1 ChatGPT OAuth (auth.rs) — reverse-engineered Codex flow

Constants (from `hermes_cli/auth.py`), centralised in `config.rs`:

- client id `app_EMoamEEZ73f0CkXaXp7hrann` (public, baked into the Codex CLI)
- authorize `https://auth.openai.com/oauth/authorize`
- token `https://auth.openai.com/oauth/token`
- redirect `http://localhost:1455/auth/callback` (port is fixed by the registration)
- scope `openid profile email offline_access`, PKCE **S256**
- extra authorize params: `id_token_add_organizations=true`,
  `codex_cli_simplified_flow=true`

Flow: bind the loopback listener **first**, open the browser, catch the redirect,
exchange `code`+`code_verifier` for tokens. `account_id` (→ `ChatGPT-Account-Id`
header) and `plan`/`email` come from decoding the `id_token` JWT payload claim
`https://api.openai.com/auth`.

Tokens persist to `~/.codex/auth.json` in the **Codex CLI's shape**
(`{tokens:{access_token,refresh_token,id_token,account_id}, last_refresh}`), so
the two tools are interchangeable. `load_existing()` is the fast path; set
`KLAWPLY_FORCE_LOGIN=1` or type `/login` to force a fresh browser login.

> ⚠️ Unofficial and against OpenAI ToS in some readings. It can break whenever
> OpenAI changes the endpoint. `load_existing` does **not** validate expiry —
> a stale access token still shows "connected"; the first backend call triggers
> `with_refresh` (401 → refresh → retry once).

### 5.2 ChatGPT backend (llm.rs) — the fragile part

`complete(system, user)` POSTs to `{CODEX_BASE_URL}/responses` with:

- `Authorization: Bearer <access_token>`, `ChatGPT-Account-Id: <account_id>`
- `OpenAI-Beta: responses=experimental`, `originator: codex_cli_rs`,
  `User-Agent: codex-cli`
- body: `{ model, instructions, input:[{role:user, content:[{type:input_text}]}],
  stream:false, tool_choice:"none" }`

`extract_output_text` handles the Responses shape
(`output[].content[].output_text`) plus a chat-completions fallback. **This is
the #1 thing to adjust if OpenAI drifts** — it's isolated on purpose.

Model default is **`gpt-5.5`** (`config::DEFAULT_MODEL`; Hermes selects it for the
Codex backend). Some accounts silently reject gpt-5.5 on this backend (empty
reply, no error); `complete` detects the empty completion and suggests
`KLAWPLY_MODEL=gpt-5.4` or `gpt-5.3-codex`.

Higher-level ops ask the model for **strict JSON** and parse tolerantly
(`json_slice` grabs the first `{`/`[` … last `}`/`]`, ignoring prose/fences):

- `extract_fields(resume, existing) -> BTreeMap<field, value>`
- `plan_form_fill(html, fields) -> Vec<Action>` where
  `Action { selector, action ∈ fill|click|select|check|uncheck, value, label }`

HTML is truncated (~120 KB) and resume (~24 KB) before sending.

### 5.3 Browser control (browser.rs) — CDP, not Playwright

- Attaches to the debug Chrome on `--remote-debugging-port=9222`; if nothing is
  listening it **launches Chrome itself** (headed, persistent profile under the
  data dir; `KLAWPLY_NO_LAUNCH=1` opts out, `KLAWPLY_CHROME` overrides the
  binary). `fetch_ws_url` reads `/json/version` →
  `webSocketDebuggerUrl` → `Browser::connect(ws)`. The `Handler` stream is parked
  on a spawned task for the session's life.
- One reused tab. `open_and_capture` = goto + wait + `content()`.
- `apply` runs a **self-contained JS shim** per action (values JSON-escaped) that
  sets the native value setter and dispatches `input`+`change` — required for
  React/Angular forms to register the change. `select` matches option by value or
  visible text; `check`/`uncheck` toggle + dispatch.
- `snapshot_values` reads every visible named/id'd input/textarea/select back into
  a map, so user-typed corrections flow into `fields.json`.

### 5.4 TUI (ui/, main.rs)

- **Seven-segment font**: `render()` maps each char to segments a–g and emits 3
  rows; the connect screen shows `KLAWPLY` in it, the long instruction in bold.
- **Transcript is pre-wrapped** into concrete visual lines (`wrap_transcript`)
  rather than letting `Paragraph` wrap, so a mouse cell maps back to exact
  characters for selection. `app.set_chat_view(lines, area, top)` publishes them.
- **Selection/copy** (ported from pekchat): mouse Down = anchor, Drag = activate +
  highlight (reverse-video), Up = copy via OSC 52. Ctrl+C copies when a selection
  is active, else quits. `content_pos` maps cell → `(line, col)` using area+scroll.
- **Scroll model**: `app.scroll` counts lines up from the bottom (0 = pinned);
  the renderer converts to a `top` line index.
- **Input**: bracketed paste is enabled so multi-line resume/link pastes arrive as
  one `Event::Paste`. On the resume screen, **Enter submits** (Ctrl+D/Ctrl+S too);
  a <40 ms "paste burst" heuristic treats fast-arriving Enters as literal newlines
  so non-bracketed pastes don't submit early. Ctrl+S is intentionally *not* the
  only submit key — terminals eat it as XOFF flow control.

### 5.5 Auth-refresh retry (agent.rs::with_refresh)

Every LLM op goes through `with_refresh(|tokens| ...)`: run once; on an error that
looks like `401`/`invalid_token`/`Unauthorized`, call `auth::refresh`, persist,
and retry once. Refresh failures propagate so the UI can suggest `/login`.

---

## 6. Build, run, verify

```sh
cargo build              # ~2 min cold (chromiumoxide is heavy), fast after
cargo test               # selection + seven-segment unit tests
cargo run                # launches the TUI; also launches Chrome on first apply
```

Env: `KLAWPLY_MODEL`, `KLAWPLY_HOME`, `KLAWPLY_FORCE_LOGIN`, `KLAWPLY_LOG`
(`off|error|warn|info|debug` → `<data-dir>/klawply.log`), `KLAWPLY_NO_LAUNCH`,
`KLAWPLY_CHROME`, `KLAWPLY_CHROME_HEADLESS`, `CODEX_HOME`.

Smoke test without a TTY of your own:
`KLAWPLY_HOME=/tmp/x timeout 4 script -qec ./target/debug/klawply /dev/null`
(exit 124 = it stayed up; grep stderr for `panic`).

---

## 7. Conventions & gotchas

- **`config.rs` is the tuning surface.** Endpoints, ids, ports, model, and paths
  live there — don't hardcode them elsewhere.
- **Errors are `Result<T, String>`** across agent/auth/llm/browser — deliberately
  simple for a TUI; map with `.map_err(|e| e.to_string())`.
- **The UI never does IO.** If you need network/disk from a keypress, send an
  `AgentCommand` and handle it in `agent.rs`; report back with `AgentEvent`.
- **Screen transitions come from events**, not from the UI assuming success.
- **Never read/print `~/.codex/auth.json` contents** — it holds live tokens.
- **klawply must never auto-submit** an application. Fill only; the user submits.
- The reverse-engineered backend (auth + `llm::complete`) is the most likely thing
  to break; keep it isolated and easy to patch, and prefer surfacing an actionable
  error over a silent failure.
