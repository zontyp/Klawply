//! Paths, endpoints and tunables. Everything reverse-engineered from the Codex
//! CLI / Hermes lives here so it is easy to adjust when OpenAI moves things.

use std::path::PathBuf;

// ---- Codex OAuth (values taken from hermes_cli/auth.py) ---------------------

/// Public client id the Codex CLI registers with OpenAI. This is not a secret;
/// it is baked into the CLI and identifies the "app" during the OAuth dance.
pub const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const OAUTH_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const OAUTH_SCOPE: &str = "openid profile email offline_access";
/// Fixed loopback port Codex registers as its redirect target. It must match
/// exactly or OpenAI rejects the redirect_uri.
pub const OAUTH_REDIRECT_PORT: u16 = 1455;
pub const OAUTH_REDIRECT_PATH: &str = "/auth/callback";
pub const OAUTH_USER_AGENT: &str = "codex-cli";

pub fn oauth_redirect_uri() -> String {
    format!(
        "http://localhost:{OAUTH_REDIRECT_PORT}{OAUTH_REDIRECT_PATH}"
    )
}

// ---- ChatGPT backend (inference) -------------------------------------------

/// The consumer Codex backend that serves your ChatGPT subscription. Requests
/// carry the OAuth access token as a bearer plus the `ChatGPT-Account-Id`
/// header (see hermes agent/account_usage.py).
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// Default model. `gpt-5.5` is what Hermes selects for the ChatGPT Codex
/// backend (`hermes_cli/moa_config.py`: provider `openai-codex`, model
/// `gpt-5.5`); it's served over the Responses API we use here. Override with
/// `KLAWPLY_MODEL`. If an account silently rejects it, `gpt-5.4` or
/// `gpt-5.3-codex` are the documented fallbacks.
pub const DEFAULT_MODEL: &str = "gpt-5.5";

pub fn model() -> String {
    std::env::var("KLAWPLY_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

// ---- Chrome / CDP -----------------------------------------------------------

/// Where klawply expects Chrome to be listening in debug mode. Start Chrome
/// with: `google-chrome --remote-debugging-port=9222`.
pub const CDP_PORT: u16 = 9222;

pub fn cdp_version_url() -> String {
    format!("http://localhost:{CDP_PORT}/json/version")
}

// ---- Local storage ----------------------------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Where `resume.txt` and `fields.json` live. Defaults to the directory klawply
/// was launched from (i.e. the klawply folder when you `cargo run` there), so
/// the files are easy to find and edit. Override with `KLAWPLY_HOME`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("KLAWPLY_HOME") {
        return PathBuf::from(dir);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub fn fields_path() -> PathBuf {
    data_dir().join("fields.json")
}

pub fn resume_path() -> PathBuf {
    data_dir().join("resume.txt")
}

/// The resume *document* to upload to file-upload fields (as opposed to the
/// plain-text `resume.txt` used for field extraction). Looked up as
/// `KLAWPLY_RESUME_FILE`, else the first of a few common filenames in the data
/// dir. Returns `None` if the user hasn't placed one.
pub fn resume_document_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("KLAWPLY_RESUME_FILE") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let dir = data_dir();
    for name in [
        "resume.pdf",
        "resume.docx",
        "resume.doc",
        "resume.rtf",
        "cv.pdf",
        "cv.docx",
    ] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The Codex CLI's shared token file: `~/.codex/auth.json`. We import from it as
/// a fast path (same as Hermes) and, after a fresh login, write our tokens back
/// so the CLI and klawply stay in sync.
pub fn codex_auth_path() -> PathBuf {
    if let Some(codex_home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(codex_home).join("auth.json");
    }
    home().join(".codex").join("auth.json")
}

/// Create the data dir if it does not exist. Best-effort.
pub fn ensure_data_dir() {
    let _ = std::fs::create_dir_all(data_dir());
}
