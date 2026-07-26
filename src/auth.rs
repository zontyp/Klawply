//! Codex-style ChatGPT OAuth (PKCE, loopback redirect).
//!
//! This mirrors what the OpenAI Codex CLI does and what Hermes brokers
//! (`hermes_cli/auth.py`): open the system browser to `auth.openai.com`, let the
//! user log in with their ChatGPT subscription, and catch the redirect on a
//! fixed loopback port. The resulting access/refresh tokens drive the ChatGPT
//! backend at `chatgpt.com/backend-api/codex`.
//!
//! Note: this endpoint is unofficial and reverse-engineered; OpenAI can change
//! it at any time, and driving a subscription programmatically may be against
//! their terms. Tokens are stored (and imported) at `~/.codex/auth.json`, the
//! same file the Codex CLI uses, so the two stay interchangeable.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;

use crate::{config, log};

pub type Result<T> = std::result::Result<T, String>;

/// The credentials we care about — a subset of what the token endpoint returns.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: String,
    /// `chatgpt_account_id` claim, sent as the `ChatGPT-Account-Id` header.
    #[serde(default)]
    pub account_id: String,
    /// Best-effort, for display only ("plus", "pro", ...).
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub email: String,
}

impl Tokens {
    #[allow(dead_code)] // used by callers/tests that gate on a complete token pair
    pub fn is_usable(&self) -> bool {
        !self.access_token.is_empty() && !self.refresh_token.is_empty()
    }
}

/// On-disk shape of `~/.codex/auth.json`, compatible with the Codex CLI.
#[derive(Debug, Serialize, Deserialize, Default)]
struct AuthFile {
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: StoredTokens,
    #[serde(default)]
    last_refresh: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct StoredTokens {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    account_id: String,
}

// ---------------------------------------------------------------------------
// Import / persist
// ---------------------------------------------------------------------------

/// Try to load already-valid tokens from `~/.codex/auth.json` (written by a
/// previous klawply login or by the Codex CLI itself).
pub fn load_existing() -> Option<Tokens> {
    let raw = std::fs::read_to_string(config::codex_auth_path()).ok()?;
    let parsed: AuthFile = serde_json::from_str(&raw).ok()?;
    let st = parsed.tokens;
    if st.access_token.is_empty() || st.refresh_token.is_empty() {
        return None;
    }
    let mut tokens = Tokens {
        access_token: st.access_token,
        refresh_token: st.refresh_token,
        id_token: st.id_token,
        account_id: st.account_id,
        ..Default::default()
    };
    enrich_from_id_token(&mut tokens);
    Some(tokens)
}

/// Persist tokens back to `~/.codex/auth.json`.
pub fn save(tokens: &Tokens) -> Result<()> {
    let file = AuthFile {
        openai_api_key: None,
        tokens: StoredTokens {
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            id_token: tokens.id_token.clone(),
            account_id: tokens.account_id.clone(),
        },
        last_refresh: Some(now_iso()),
    };
    let path = config::codex_auth_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Login (PKCE + loopback)
// ---------------------------------------------------------------------------

/// Run the interactive login. `on_url` is called once with the authorize URL so
/// the UI can display it (we also try to open the system browser). Blocks on a
/// dedicated thread until the browser redirects back or `timeout` elapses.
pub fn login<F: Fn(&str)>(on_url: F) -> Result<Tokens> {
    let verifier = random_b64url(64);
    let challenge = B64URL.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64url(32);

    // Bind the loopback listener *before* we send the user to the browser, so
    // we never miss the redirect.
    let listener = TcpListener::bind(("127.0.0.1", config::OAUTH_REDIRECT_PORT))
        .map_err(|e| format!("cannot bind localhost:{}: {e}", config::OAUTH_REDIRECT_PORT))?;

    let authorize_url = build_authorize_url(&challenge, &state);
    log::info(format!(
        "auth ▸ login: waiting for redirect on localhost:{}",
        config::OAUTH_REDIRECT_PORT
    ));
    on_url(&authorize_url);
    let _ = webbrowser::open(&authorize_url);

    let code = wait_for_redirect(&listener, &state)?;
    log::info("auth ▸ received callback; exchanging code");
    let tokens = exchange_code(&code, &verifier)?;
    save(&tokens)?;
    log::info(format!("auth ▸ login ok (plan={})", tokens.plan));
    Ok(tokens)
}

fn build_authorize_url(challenge: &str, state: &str) -> String {
    let enc = urlencoding::encode;
    format!(
        "{base}?response_type=code&client_id={cid}&redirect_uri={redir}&scope={scope}\
         &code_challenge={chal}&code_challenge_method=S256&id_token_add_organizations=true\
         &codex_cli_simplified_flow=true&state={state}",
        base = config::OAUTH_AUTHORIZE_URL,
        cid = enc(config::OAUTH_CLIENT_ID),
        redir = enc(&config::oauth_redirect_uri()),
        scope = enc(config::OAUTH_SCOPE),
        chal = enc(challenge),
        state = enc(state),
    )
}

/// Accept a single HTTP request on the loopback listener, validate `state`, and
/// pull the `code` out of the query string.
fn wait_for_redirect(listener: &TcpListener, expected_state: &str) -> Result<String> {
    // The browser may make a favicon or preflight request first; loop until we
    // see the callback with a `code`.
    loop {
        let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let Some(target) = request.lines().next().and_then(|l| l.split_whitespace().nth(1)) else {
            respond(&mut stream, "Waiting for OpenAI…");
            continue;
        };
        if !target.starts_with(config::OAUTH_REDIRECT_PATH) {
            respond(&mut stream, "Waiting for OpenAI…");
            continue;
        }
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = parse_query(query);
        if let Some(err) = params.get("error") {
            respond(&mut stream, "Login failed. You can close this tab.");
            return Err(format!("OpenAI returned error: {err}"));
        }
        match (params.get("code"), params.get("state")) {
            (Some(code), Some(state)) if state == expected_state => {
                respond(
                    &mut stream,
                    "klawply is connected. You can close this tab and return to the terminal.",
                );
                return Ok(code.clone());
            }
            (Some(_), Some(_)) => {
                respond(&mut stream, "State mismatch. You can close this tab.");
                return Err("OAuth state mismatch — possible CSRF, aborting.".into());
            }
            _ => {
                respond(&mut stream, "Waiting for OpenAI…");
            }
        }
    }
}

fn respond(stream: &mut std::net::TcpStream, message: &str) {
    let body = format!(
        "<!doctype html><html><body style='background:#000;color:#0f6;\
         font-family:monospace;text-align:center;padding-top:20vh'>\
         <h1 style='color:#0f6'>klawply</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------
// Token exchange / refresh
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
}

fn exchange_code(code: &str, verifier: &str) -> Result<Tokens> {
    let redirect_uri = config::oauth_redirect_uri();
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", config::OAUTH_CLIENT_ID),
        ("code_verifier", verifier),
    ];
    let resp = post_token(&form)?;
    let mut tokens = Tokens {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        id_token: resp.id_token,
        ..Default::default()
    };
    enrich_from_id_token(&mut tokens);
    Ok(tokens)
}

/// Refresh an access token. Uses the refresh token; keeps the old refresh token
/// if the response doesn't rotate it (some responses omit it).
pub fn refresh(existing: &Tokens) -> Result<Tokens> {
    if existing.refresh_token.is_empty() {
        return Err("no refresh token; please reconnect".into());
    }
    log::info("auth ▸ refreshing access token");
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", existing.refresh_token.as_str()),
        ("client_id", config::OAUTH_CLIENT_ID),
    ];
    let resp = post_token(&form).inspect_err(|e| log::warn(format!("auth ▸ refresh failed: {e}")))?;
    let mut tokens = Tokens {
        access_token: resp.access_token,
        refresh_token: if resp.refresh_token.is_empty() {
            existing.refresh_token.clone()
        } else {
            resp.refresh_token
        },
        id_token: if resp.id_token.is_empty() {
            existing.id_token.clone()
        } else {
            resp.id_token
        },
        ..Default::default()
    };
    enrich_from_id_token(&mut tokens);
    save(&tokens)?;
    Ok(tokens)
}

/// Blocking POST to the token endpoint. Runs a short-lived blocking reqwest
/// client so it can be called from either sync or async contexts off the UI
/// thread.
fn post_token(form: &[(&str, &str)]) -> Result<TokenResponse> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(config::OAUTH_USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(config::OAUTH_TOKEN_URL)
        .form(form)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("token endpoint returned {status}: {text}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("bad token response: {e} — {text}"))
}

// ---------------------------------------------------------------------------
// Expiry / freshness
// ---------------------------------------------------------------------------

/// Refresh this many seconds before the access token's `exp` to avoid using a
/// token that expires mid-request.
const EXPIRY_SKEW_SECS: i64 = 120;

/// True when the access token's `exp` claim is in the past (within skew). If we
/// can't read the claim we assume it's fine and let a 401 drive a refresh.
pub fn access_is_expired(tokens: &Tokens) -> bool {
    match decode_jwt_payload(&tokens.access_token)
        .and_then(|c| c.get("exp").and_then(|v| v.as_i64()))
    {
        Some(exp) => now_unix() + EXPIRY_SKEW_SECS >= exp,
        None => false,
    }
}

/// Return usable tokens: if the access token is still valid, use them as-is;
/// otherwise refresh (which also persists). Errors here mean re-login is needed.
pub fn ensure_fresh(tokens: Tokens) -> Result<Tokens> {
    if access_is_expired(&tokens) {
        refresh(&tokens)
    } else {
        Ok(tokens)
    }
}

/// Whether an error means the stored refresh token is dead and the user must
/// sign in again (rotated/reused token, revoked grant, etc.).
pub fn is_relogin_error(e: &str) -> bool {
    let e = e.to_lowercase();
    e.contains("refresh_token_reused")
        || e.contains("invalid_grant")
        || e.contains("already been used")
        || e.contains("sign in again")
        || e.contains("signing in again")
        || e.contains("missing refresh token")
        || e.contains("no refresh token")
}

// ---------------------------------------------------------------------------
// id_token claims
// ---------------------------------------------------------------------------

/// Pull `chatgpt_account_id`, plan and email out of the id_token JWT. The
/// account id is required for the `ChatGPT-Account-Id` header.
fn enrich_from_id_token(tokens: &mut Tokens) {
    let Some(claims) = decode_jwt_payload(&tokens.id_token) else {
        return;
    };
    if let Some(email) = claims.get("email").and_then(|v| v.as_str()) {
        tokens.email = email.to_string();
    }
    if let Some(auth) = claims.get("https://api.openai.com/auth") {
        if tokens.account_id.is_empty() {
            if let Some(id) = auth.get("chatgpt_account_id").and_then(|v| v.as_str()) {
                tokens.account_id = id.to_string();
            }
        }
        if let Some(plan) = auth.get("chatgpt_plan_type").and_then(|v| v.as_str()) {
            tokens.plan = plan.to_string();
        }
    }
}

fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = B64URL.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

fn random_b64url(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    B64URL.encode(buf)
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((
                k.to_string(),
                urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string()),
            ))
        })
        .collect()
}

/// Seconds-precision UTC timestamp without pulling in a date crate.
fn now_iso() -> String {
    format!("{}", now_unix())
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
