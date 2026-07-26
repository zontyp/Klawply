//! Talks to the ChatGPT subscription backend (`chatgpt.com/backend-api/codex`)
//! using the OAuth access token, the same way the Codex CLI does.
//!
//! The wire format here is the reverse-engineered Codex "Responses" API. It is
//! centralised in `complete()` so that, if OpenAI changes the shape, there is a
//! single place to adjust. Everything above `complete()` is provider-agnostic:
//! we ask the model for strict JSON and parse it.

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};

use crate::auth::Tokens;
use crate::{config, log};

pub type Result<T> = std::result::Result<T, String>;

/// One browser action the model wants us to perform to fill a form.
#[derive(Debug, Clone, Deserialize)]
pub struct Action {
    /// CSS selector for the target element.
    pub selector: String,
    /// One of: `fill`, `click`, `select`, `select_custom`, `check`, `uncheck`,
    /// `upload`.
    #[serde(default = "default_action")]
    pub action: String,
    /// Value to type or option to select. Ignored for click/check.
    #[serde(default)]
    pub value: String,
    /// Human-readable label for the transcript ("First name").
    #[serde(default)]
    pub label: String,
}

fn default_action() -> String {
    "fill".to_string()
}

// ---------------------------------------------------------------------------
// High-level operations
// ---------------------------------------------------------------------------

/// Turn resume text (plus whatever we already know) into a flat map of
/// application fields for `fields.json`.
pub fn extract_fields(
    tokens: &Tokens,
    resume: &str,
    existing: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let system = "You extract structured job-application data from a resume. \
        Respond with ONE JSON object only — no prose, no markdown fences. Keys are \
        snake_case field names a job form would ask for (first_name, last_name, \
        full_name, email, phone, address, city, state, country, postal_code, \
        linkedin, github, portfolio, website, current_company, current_title, \
        years_experience, work_authorization, requires_sponsorship, \
        willing_to_relocate, desired_salary, summary, skills, education, \
        experience). Values are plain strings. Omit keys you cannot determine. \
        Keep any existing values unless the resume clearly contradicts them.";
    let user = format!(
        "Existing fields (JSON):\n{}\n\nResume text:\n\"\"\"\n{}\n\"\"\"",
        serde_json::to_string(existing).unwrap_or_else(|_| "{}".into()),
        truncate(resume, 24_000),
    );
    let reply = complete(tokens, system, &user)?;
    let obj = parse_json_object(&reply)?;
    Ok(obj
        .into_iter()
        .filter_map(|(k, v)| value_to_string(&v).map(|s| (k, s)))
        .collect())
}

/// Given the page HTML and the user's fields, decide which form controls to
/// fill. Returns an ordered list of actions.
pub fn plan_form_fill(
    tokens: &Tokens,
    page_html: &str,
    fields: &BTreeMap<String, String>,
) -> Result<Vec<Action>> {
    let system = "You are a browser automation planner for job-application forms. \
        You are given the HTML of a page and the applicant's data. Produce a JSON \
        ARRAY of actions to complete the form, in order. Each action is an object: \
        {\"selector\": <css selector>, \"action\": one of \
        \"fill\"|\"click\"|\"select\"|\"select_custom\"|\"check\"|\"uncheck\"|\
        \"upload\", \"value\": <string>, \"label\": <short human label>}. Prefer \
        stable selectors (id, name, aria-label, placeholder). Use \"select\" for \
        native <select> elements with the visible option text as value. \
        For CUSTOM dropdowns that are NOT a native <select> — e.g. a clickable \
        <div>/<button> with role=\"combobox\"/\"listbox\" or an aria-haspopup that \
        opens a list of <div>/<li> options (common for Gender, Country, etc.) — \
        use action \"select_custom\" with the selector of the trigger element and \
        the desired option's visible text as value; klawply will open it and click \
        the matching option. \
        For a resume/CV file-upload field, target the <input type=\"file\"> element \
        (it may be hidden behind a styled button — still select the input itself) \
        and use action \"upload\" with an empty value; klawply attaches the local \
        resume document automatically. Only include fields you can confidently map \
        from the applicant data. Do NOT click the final submit button. Respond \
        with the JSON array only — no prose, no markdown fences.";
    let user = format!(
        "Applicant data (JSON):\n{}\n\nPage HTML:\n\"\"\"\n{}\n\"\"\"",
        serde_json::to_string(fields).unwrap_or_else(|_| "{}".into()),
        truncate(page_html, 120_000),
    );
    let reply = complete(tokens, system, &user)?;
    let arr = parse_json_array(&reply)?;
    Ok(arr
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Action>(v).ok())
        .filter(|a| !a.selector.trim().is_empty())
        .collect())
}

/// A conversational reply for the chat window, streamed fragment-by-fragment
/// via `on_delta`.
pub fn chat_streaming(
    tokens: &Tokens,
    history: &str,
    user_message: &str,
    on_delta: &mut dyn FnMut(&str),
) -> Result<String> {
    let system = "You are klawply, a concise terminal agent that helps a user apply \
        to jobs. Keep replies short and practical.";
    let user = format!("{history}\nUser: {user_message}");
    complete_streaming(tokens, system, &user, on_delta)
}

/// Normalise the raw values read back from a form (`identifier -> value`) into
/// canonical `fields.json` keys, so manually-entered values become reusable.
pub fn map_form_values(
    tokens: &Tokens,
    observed: &BTreeMap<String, String>,
    existing: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    if observed.is_empty() {
        return Ok(BTreeMap::new());
    }
    let system = "You normalise job-application form inputs into a reusable applicant \
        profile. You are given raw form values (the form's field identifier → the \
        value currently entered) and the existing profile's key names. Output ONE \
        JSON object mapping canonical snake_case field names to values. Reuse an \
        existing profile key whenever the meaning matches (e.g. a form field \
        'Gender' or 'candidate[sex]' → 'gender'; 'Email address' → 'email'). Prefer \
        standard names (first_name, last_name, full_name, email, phone, gender, \
        address, city, state, country, postal_code, linkedin_profile_url, \
        github_profile_url, current_company, current_title, years_experience, \
        desired_salary). Drop UI noise (search boxes, empty values, one-time codes). \
        Values are plain strings. Respond with the JSON object only.";
    let user = format!(
        "Existing profile keys: {}\n\nObserved form values (JSON):\n{}",
        existing.keys().cloned().collect::<Vec<_>>().join(", "),
        serde_json::to_string(observed).unwrap_or_else(|_| "{}".into()),
    );
    let reply = complete(tokens, system, &user)?;
    let obj = parse_json_object(&reply)?;
    Ok(obj
        .into_iter()
        .filter_map(|(k, v)| value_to_string(&v).map(|s| (k, s)))
        .collect())
}

// ---------------------------------------------------------------------------
// Wire format (Codex Responses API) — the one place to adjust if it changes.
// ---------------------------------------------------------------------------

/// Send one system+user turn and return the full assistant text. Blocking;
/// call it off the UI thread.
pub fn complete(tokens: &Tokens, system: &str, user: &str) -> Result<String> {
    stream_request(tokens, system, user, &mut |_| {})
}

/// Like [`complete`], but invokes `on_delta` for each text fragment as it
/// streams in — so the UI can render the reply token-by-token.
pub fn complete_streaming(
    tokens: &Tokens,
    system: &str,
    user: &str,
    on_delta: &mut dyn FnMut(&str),
) -> Result<String> {
    stream_request(tokens, system, user, on_delta)
}

/// Build the Responses-API request body. `stream` is always true because the
/// Codex backend rejects `stream:false` ("Stream must be set to true").
fn request_body(system: &str, user: &str) -> serde_json::Value {
    json!({
        "model": config::model(),
        "instructions": system,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": user }],
        }],
        "stream": true,
        "store": false,
        "tool_choice": "none",
        "parallel_tool_calls": false,
    })
}

/// POST to the Codex `/responses` endpoint and consume the SSE stream,
/// reassembling `output_text` deltas (and forwarding each to `on_delta`).
fn stream_request(
    tokens: &Tokens,
    system: &str,
    user: &str,
    on_delta: &mut dyn FnMut(&str),
) -> Result<String> {
    let body = request_body(system, user);
    let url = format!("{}/responses", config::CODEX_BASE_URL);
    log::debug(format!(
        "llm ▸ POST {url} model={} account={} prompt_bytes={}",
        config::model(),
        !tokens.account_id.is_empty(),
        user.len(),
    ));

    let client = reqwest::blocking::Client::builder()
        .user_agent(config::OAUTH_USER_AGENT)
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;

    let mut req = client
        .post(&url)
        .bearer_auth(&tokens.access_token)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "codex_cli_rs")
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json");
    if !tokens.account_id.is_empty() {
        req = req.header("ChatGPT-Account-Id", &tokens.account_id);
    }

    let resp = req.json(&body).send().map_err(|e| {
        log::error(format!("llm ▸ request failed: {e}"));
        e.to_string()
    })?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        log::error(format!("llm ▸ {status}: {}", truncate(&text, 500)));
        return Err(format!("ChatGPT backend returned {status}: {}", truncate(&text, 500)));
    }

    let out = read_sse_stream(resp, on_delta)?;
    if out.trim().is_empty() {
        // The Codex backend has been known to silently reject the gpt-5.5 family
        // on some accounts (no events, no error). Point the user at the fallbacks.
        let model = config::model();
        let hint = if model.contains("gpt-5.5") {
            format!(
                " — the ChatGPT Codex backend may be silently rejecting '{model}' on this \
                 account. Try KLAWPLY_MODEL=gpt-5.4 or gpt-5.3-codex."
            )
        } else {
            String::new()
        };
        log::warn(format!("llm ▸ empty completion (model={model})"));
        return Err(format!("empty completion from backend{hint}"));
    }
    log::debug(format!("llm ▸ ok, {} chars", out.chars().count()));
    Ok(out)
}

/// Read an SSE response line-by-line, accumulating `output_text` deltas and
/// forwarding each to `on_delta`. Falls back to the full text carried by a
/// `response.completed` event if no deltas arrive.
fn read_sse_stream(
    resp: reqwest::blocking::Response,
    on_delta: &mut dyn FnMut(&str),
) -> Result<String> {
    let mut reader = BufReader::new(resp);
    let mut line = String::new();
    let mut deltas = String::new();
    let mut completed = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if read == 0 {
            break; // stream closed
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty.contains("output_text") && ty.ends_with("delta") {
            if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                deltas.push_str(d);
                on_delta(d);
            }
        } else if ty == "response.completed" || ty == "response.incomplete" {
            if let Some(t) = v.get("response").map(extract_output_text) {
                if !t.is_empty() {
                    completed = t;
                }
            }
        }
    }
    Ok(if deltas.is_empty() { completed } else { deltas })
}

/// Pull the assistant text out of a Responses-API payload. Handles the common
/// `output: [{type:"message", content:[{type:"output_text", text}]}]` shape and
/// a couple of fallbacks.
fn extract_output_text(value: &Value) -> String {
    // Convenience field some responses include.
    if let Some(s) = value.get("output_text").and_then(|v| v.as_str()) {
        if !s.is_empty() {
            return s.to_string();
        }
    }
    let mut collected = String::new();
    if let Some(items) = value.get("output").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for part in parts {
                        let ty = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if ty == "output_text" || ty == "text" {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                collected.push_str(t);
                            }
                        }
                    }
                }
            }
        }
    }
    if !collected.is_empty() {
        return collected;
    }
    // Last-ditch: some deployments return chat-completions shape.
    if let Some(t) = value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
    {
        return t.to_string();
    }
    collected
}

// ---------------------------------------------------------------------------
// JSON parsing helpers (tolerant of stray prose / code fences)
// ---------------------------------------------------------------------------

fn parse_json_object(text: &str) -> Result<serde_json::Map<String, Value>> {
    let slice = json_slice(text, '{', '}').ok_or("model did not return a JSON object")?;
    match serde_json::from_str::<Value>(slice) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err("model output was not a valid JSON object".into()),
    }
}

fn parse_json_array(text: &str) -> Result<Vec<Value>> {
    let slice = json_slice(text, '[', ']').ok_or("model did not return a JSON array")?;
    match serde_json::from_str::<Value>(slice) {
        Ok(Value::Array(a)) => Ok(a),
        _ => Err("model output was not a valid JSON array".into()),
    }
}

/// Extract the substring from the first `open` to the last matching `close`.
fn json_slice(text: &str, open: char, close: char) -> Option<&str> {
    let start = text.find(open)?;
    let end = text.rfind(close)?;
    if end > start {
        Some(&text[start..=end])
    } else {
        None
    }
}

fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…[truncated {} bytes]", &s[..end], s.len() - end)
    }
}
