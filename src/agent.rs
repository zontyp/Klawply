//! The background worker. Owns the tokens and the browser session, receives
//! [`AgentCommand`]s, and reports progress as [`AgentEvent`]s. Runs on its own
//! std thread so nothing here can block the UI; async browser work is driven
//! with a tokio runtime handle.

use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::auth::{self, Tokens};
use crate::browser::BrowserSession;
use crate::event::{AgentCommand, AgentEvent};
use crate::{config, fields, llm, log};

struct Agent {
    handle: Handle,
    tx: UnboundedSender<AgentEvent>,
    tokens: Option<Tokens>,
    browser: Option<BrowserSession>,
}

/// Entry point for the agent thread.
pub fn run(handle: Handle, mut rx: UnboundedReceiver<AgentCommand>, tx: UnboundedSender<AgentEvent>) {
    let mut agent = Agent {
        handle,
        tx,
        tokens: None,
        browser: None,
    };
    log::info("agent ▸ thread started");
    while let Some(cmd) = rx.blocking_recv() {
        log::debug(format!("agent ▸ command: {}", command_name(&cmd)));
        match cmd {
            AgentCommand::Connect => agent.connect(),
            AgentCommand::SubmitResume(text) => agent.submit_resume(&text),
            AgentCommand::UserMessage(text) => agent.user_message(&text),
            AgentCommand::SyncFromBrowser => agent.sync_from_browser(),
            AgentCommand::Quit => break,
        }
    }
    log::info("agent ▸ thread stopped");
}

fn command_name(cmd: &AgentCommand) -> &'static str {
    match cmd {
        AgentCommand::Connect => "Connect",
        AgentCommand::SubmitResume(_) => "SubmitResume",
        AgentCommand::UserMessage(_) => "UserMessage",
        AgentCommand::SyncFromBrowser => "SyncFromBrowser",
        AgentCommand::Quit => "Quit",
    }
}

impl Agent {
    fn emit(&self, event: AgentEvent) {
        let _ = self.tx.send(event);
    }

    fn status(&self, msg: impl Into<String>) {
        self.emit(AgentEvent::Status(msg.into()));
    }

    fn busy(&self, on: bool) {
        self.emit(AgentEvent::Busy(on));
    }

    // -- connect -----------------------------------------------------------

    fn connect(&mut self) {
        self.busy(true);
        self.status("connecting to ChatGPT…");

        // Fast path: reuse tokens from ~/.codex/auth.json (ours or the Codex
        // CLI's) unless the user explicitly forces a fresh login. Refresh first
        // if the access token has expired.
        if !force_login_env() {
            if let Some(tokens) = auth::load_existing() {
                log::info("connect ▸ found stored tokens; ensuring fresh");
                match auth::ensure_fresh(tokens) {
                    Ok(fresh) => {
                        log::info(format!("connect ▸ reusing login ({})", fresh.plan));
                        self.finish_connect(fresh);
                        return;
                    }
                    Err(e) => {
                        // Expired + not refreshable (rotated/revoked). Fall
                        // through to a fresh browser login instead of pretending
                        // we're connected.
                        log::warn(format!("connect ▸ stored tokens unusable: {e}"));
                        self.emit(AgentEvent::System(format!(
                            "Your saved ChatGPT login has expired ({e}). Signing in again…"
                        )));
                    }
                }
            }
        }
        self.interactive_login();
    }

    /// Always run the browser OAuth, ignoring any stored tokens.
    fn interactive_login(&mut self) {
        self.busy(true);
        log::info("connect ▸ starting interactive OAuth login");
        self.status("opening browser to log in to ChatGPT…");
        let tx = self.tx.clone();
        // `login` opens the browser and blocks on the loopback redirect; it
        // reports the authorize URL through the closure.
        let result = auth::login(|url| {
            let _ = tx.send(AgentEvent::ConnectPrompt {
                url: url.to_string(),
            });
        });
        match result {
            Ok(tokens) => {
                self.emit(AgentEvent::System("Logged in to ChatGPT.".into()));
                self.finish_connect(tokens);
            }
            Err(e) => {
                self.emit(AgentEvent::Error(format!("login failed: {e}")));
                self.busy(false);
            }
        }
    }

    fn finish_connect(&mut self, tokens: Tokens) {
        self.emit(AgentEvent::Connected {
            email: tokens.email.clone(),
            plan: tokens.plan.clone(),
        });
        self.tokens = Some(tokens);
        self.busy(false);

        if fields::resume_is_blank() {
            // No resume yet → resume screen asks the user to paste it.
            self.emit(AgentEvent::NeedResume);
            return;
        }

        // Resume is present → go to the chat window straight away (so the user
        // always sees the transcript + input box), THEN derive fields if we
        // never have. Emitting Ready first means the extraction's progress and
        // any errors are visible in the chat, not hidden behind a stuck screen.
        self.emit(AgentEvent::Ready);
        if fields::load_fields().is_empty() {
            self.emit(AgentEvent::System(
                "Found your saved resume — deriving application fields…".into(),
            ));
            let resume = fields::read_resume();
            self.run_extract(&resume);
        }
    }

    // -- resume ------------------------------------------------------------

    fn submit_resume(&mut self, text: &str) {
        if let Err(e) = fields::write_resume(text) {
            self.emit(AgentEvent::Error(format!("could not save resume: {e}")));
            return;
        }
        self.run_extract(text);
    }

    /// Send resume text to the model and populate fields.json. Shared by the
    /// resume screen and the auto-derive-on-connect path.
    fn run_extract(&mut self, resume: &str) {
        self.busy(true);
        self.status("reading your resume with ChatGPT…");
        let existing = fields::load_fields();
        let resume = resume.to_string();
        let result = self.with_refresh(|t| llm::extract_fields(t, &resume, &existing));
        match result {
            Ok(extracted) => match fields::merge_fields(&extracted) {
                Ok(changed) => {
                    let n = changed.len();
                    self.emit(AgentEvent::FieldsUpdated { changed });
                    self.emit(AgentEvent::System(format!(
                        "Saved {n} field(s) to fields.json from your resume."
                    )));
                    self.emit(AgentEvent::Ready);
                }
                Err(e) => self.emit(AgentEvent::Error(format!("could not save fields: {e}"))),
            },
            Err(e) => {
                let hint = if auth::is_relogin_error(&e) || is_auth_error(&e) {
                    " — type /login to reconnect your ChatGPT account."
                } else {
                    ""
                };
                self.emit(AgentEvent::Error(format!("resume parse failed: {e}{hint}")));
            }
        }
        self.busy(false);
    }

    // -- chat / apply ------------------------------------------------------

    fn user_message(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.eq_ignore_ascii_case("/login") {
            self.interactive_login();
        } else if trimmed.eq_ignore_ascii_case("/sync") || trimmed.eq_ignore_ascii_case("/learn") {
            self.sync_from_browser();
        } else if is_url(trimmed) {
            self.apply_to(trimmed);
        } else {
            self.chat(trimmed);
        }
    }

    fn chat(&mut self, message: &str) {
        self.busy(true);
        // Stream the reply so it renders token-by-token instead of appearing all
        // at once after a long wait.
        let tx = self.tx.clone();
        let result = self.with_refresh(|t| {
            let mut on_delta = |delta: &str| {
                let _ = tx.send(AgentEvent::AssistantChunk(delta.to_string()));
            };
            llm::chat_streaming(t, "", message, &mut on_delta)
        });
        if let Err(e) = result {
            self.emit(AgentEvent::Error(e));
        }
        self.busy(false);
    }

    fn apply_to(&mut self, url: &str) {
        self.busy(true);
        log::info(format!("apply ▸ {url}"));
        if let Err(e) = self.ensure_browser() {
            log::error(format!("apply ▸ browser: {e}"));
            self.emit(AgentEvent::Error(e));
            self.busy(false);
            return;
        }

        self.status("opening the job page in Chrome…");
        let html = match self.block_browser(|b, h| h.block_on(b.open_and_capture(url))) {
            Ok(html) => html,
            Err(e) => {
                log::error(format!("apply ▸ open page: {e}"));
                self.emit(AgentEvent::Error(format!("could not open page: {e}")));
                self.busy(false);
                return;
            }
        };
        log::debug(format!("apply ▸ captured {} bytes of HTML", html.len()));

        self.status("asking ChatGPT which fields to fill…");
        let form_fields = fields::load_fields();
        let actions = match self.with_refresh(|t| llm::plan_form_fill(t, &html, &form_fields)) {
            Ok(actions) => actions,
            Err(e) => {
                log::error(format!("apply ▸ plan: {e}"));
                self.emit(AgentEvent::Error(format!("could not plan form fill: {e}")));
                self.busy(false);
                return;
            }
        };
        log::info(format!("apply ▸ planned {} action(s)", actions.len()));

        if actions.is_empty() {
            self.emit(AgentEvent::System(
                "ChatGPT found no fields it could confidently fill on this page.".into(),
            ));
            self.busy(false);
            return;
        }

        let total = actions.len();
        let mut filled = 0usize;
        let mut failed = Vec::new();
        for (i, action) in actions.iter().enumerate() {
            let label = if action.label.is_empty() {
                action.selector.clone()
            } else {
                action.label.clone()
            };
            // Live progress so the user sees each field being filled.
            self.status(format!("filling {}/{}: {label}", i + 1, total));
            let outcome = if action.action.eq_ignore_ascii_case("upload") {
                self.upload_resume(&action.selector)
            } else {
                self.block_browser(|b, h| h.block_on(b.apply(action)))
            };
            match outcome {
                Ok(()) => {
                    filled += 1;
                    log::debug(format!("apply ▸ did '{}' ({})", label, action.selector));
                }
                Err(e) => {
                    log::warn(format!("apply ▸ field failed: {e}"));
                    failed.push(format!("{label}: {e}"));
                }
            }
        }

        self.emit(AgentEvent::Applied {
            url: url.to_string(),
            filled,
            failed,
        });
        self.emit(AgentEvent::System(
            "Fill in anything I missed, then type /sync to save your entries to fields.json.".into(),
        ));
        self.busy(false);
    }

    /// Read the live form from the debug Chrome, normalise the values to clean
    /// field names, and fold them into fields.json — the way manual entries get
    /// remembered for next time.
    fn sync_from_browser(&mut self) {
        if self.browser.is_none() {
            self.emit(AgentEvent::Error(
                "no page open yet — paste a job link first, fill the form, then /sync.".into(),
            ));
            return;
        }
        self.busy(true);
        self.status("reading what you entered on the page…");
        let observed = match self.block_browser(|b, h| h.block_on(b.snapshot_values())) {
            Ok(o) => o,
            Err(e) => {
                self.emit(AgentEvent::Error(e));
                self.busy(false);
                return;
            }
        };
        if observed.is_empty() {
            self.emit(AgentEvent::System("Nothing filled on the page yet to learn.".into()));
            self.busy(false);
            return;
        }

        self.status("saving your entries to fields.json…");
        let existing = fields::load_fields();
        let mapped = match self.with_refresh(|t| llm::map_form_values(t, &observed, &existing)) {
            Ok(m) => m,
            Err(e) => {
                self.emit(AgentEvent::Error(format!("could not save entries: {e}")));
                self.busy(false);
                return;
            }
        };
        match fields::merge_fields(&mapped) {
            Ok(changed) if changed.is_empty() => {
                self.emit(AgentEvent::System(
                    "Your entries already match fields.json — nothing new to save.".into(),
                ));
            }
            Ok(changed) => {
                self.emit(AgentEvent::System(format!(
                    "Saved {} field(s) from the page: {}",
                    changed.len(),
                    changed.join(", ")
                )));
                self.emit(AgentEvent::FieldsUpdated { changed });
            }
            Err(e) => self.emit(AgentEvent::Error(e)),
        }
        self.busy(false);
    }

    /// Upload the local resume document to a file-input `selector`. Resolves the
    /// document from the klawply folder and gives an actionable error if none is
    /// there, so the model can request an upload without knowing local paths.
    fn upload_resume(&self, selector: &str) -> Result<(), String> {
        let Some(path) = config::resume_document_path() else {
            return Err(format!(
                "no resume file to upload — put resume.pdf (or .docx) in {}",
                config::data_dir().display()
            ));
        };
        log::info(format!("apply ▸ uploading {} to {selector}", path.display()));
        self.block_browser(|b, h| h.block_on(b.upload_file(selector, &path)))
    }

    // -- helpers -----------------------------------------------------------

    /// Ensure we have a live browser session, connecting on first use (and
    /// launching Chrome in debug mode if it isn't already running).
    fn ensure_browser(&mut self) -> Result<(), String> {
        if self.browser.is_some() {
            return Ok(());
        }
        self.status("connecting to Chrome (launching it if needed)…");
        let session = self.handle.block_on(BrowserSession::connect())?;
        self.browser = Some(session);
        Ok(())
    }

    /// Run a closure against the live browser session with the runtime handle.
    fn block_browser<T>(
        &self,
        f: impl FnOnce(&BrowserSession, &Handle) -> Result<T, String>,
    ) -> Result<T, String> {
        let browser = self.browser.as_ref().ok_or("browser not connected")?;
        f(browser, &self.handle)
    }

    /// Call an LLM op with the current tokens; on a 401 refresh once and retry.
    fn with_refresh<T>(
        &mut self,
        f: impl Fn(&Tokens) -> llm::Result<T>,
    ) -> Result<T, String> {
        let tokens = self.tokens.clone().ok_or("not connected to ChatGPT")?;
        match f(&tokens) {
            Ok(v) => Ok(v),
            Err(e) if is_auth_error(&e) => {
                self.status("refreshing ChatGPT session…");
                let refreshed = auth::refresh(&tokens)?;
                let out = f(&refreshed);
                self.tokens = Some(refreshed);
                out
            }
            Err(e) => Err(e),
        }
    }
}

fn is_url(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://")
}

/// Whether `KLAWPLY_FORCE_LOGIN` asks us to skip the stored-token fast path.
fn force_login_env() -> bool {
    std::env::var("KLAWPLY_FORCE_LOGIN")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn is_auth_error(e: &str) -> bool {
    e.contains("401") || e.contains("invalid_token") || e.contains("Unauthorized")
}
