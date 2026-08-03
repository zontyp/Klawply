//! Drives a debug-mode Chrome over the DevTools Protocol using the pure-Rust
//! `chromiumoxide` client.
//!
//! If nothing is listening on the debug port we launch Chrome ourselves (headed,
//! with a persistent profile under the data dir so job-site logins survive
//! across runs); otherwise we attach to the running instance. We reuse one tab,
//! capture page HTML for the LLM, and execute the fill/click/select actions it
//! plans. Values are set via a small JS shim that also dispatches
//! `input`/`change` events, which is what React/Angular job forms need.

use chromiumoxide::Browser;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use futures::StreamExt;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::llm::Action;
use crate::{config, log};

pub type Result<T> = std::result::Result<T, String>;

/// A live connection to the debug Chrome plus the tab we drive.
pub struct BrowserSession {
    _browser: Browser,
    _handler: tokio::task::JoinHandle<()>,
    page: Page,
}

impl BrowserSession {
    /// Attach to Chrome's debug endpoint on `CDP_PORT`, launching Chrome first
    /// if nothing is listening there (unless `KLAWPLY_NO_LAUNCH` is set).
    pub async fn connect() -> Result<Self> {
        log::info(format!("browser ▸ connecting to CDP on :{}", config::CDP_PORT));
        let ws = match fetch_ws_url().await {
            Ok(ws) => ws,
            Err(_) if launch_enabled() => {
                log::info("browser ▸ no debug Chrome found; launching one");
                launch_chrome()?;
                wait_for_ws().await?
            }
            Err(e) => {
                return Err(format!(
                    "cannot reach Chrome debug port {} ({e}). Start Chrome with: \
                     google-chrome --remote-debugging-port={}",
                    config::CDP_PORT,
                    config::CDP_PORT,
                ));
            }
        };

        let (browser, mut handler) =
            Browser::connect(ws).await.map_err(|e| format!("CDP connect failed: {e}"))?;

        // The handler future must be polled for the connection to make
        // progress; park it on the runtime for the session's lifetime. Do NOT
        // break on individual event errors — dropping the handler cancels every
        // in-flight CDP command's response channel ("oneshot canceled"). Keep
        // driving until the stream itself ends (connection closed).
        let handler_task = tokio::spawn(async move {
            while handler.next().await.is_some() {}
        });

        // Reuse an existing blank tab if there is one; otherwise open one.
        let page = match browser.new_page("about:blank").await {
            Ok(p) => p,
            Err(e) => {
                handler_task.abort();
                return Err(format!("could not open a tab: {e}"));
            }
        };

        Ok(Self {
            _browser: browser,
            _handler: handler_task,
            page,
        })
    }

    /// Navigate the tab to `url` and wait for it to settle, then return the
    /// full rendered HTML for the LLM to read.
    pub async fn open_and_capture(&self, url: &str) -> Result<String> {
        log::debug(format!("browser ▸ navigating to {url}"));
        self.page
            .goto(url)
            .await
            .map_err(|e| format!("navigation to {url} failed: {e}"))?;
        // Best-effort: single-page apps may never fire a load event, so don't
        // fail the whole apply if this times out.
        let _ = self.page.wait_for_navigation().await;
        // Give client-rendered forms a moment to mount before we snapshot.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        self.page
            .content()
            .await
            .map_err(|e| format!("could not read page source: {e}"))
    }

    /// Execute one planned action. Errors are returned per-action so the caller
    /// can report which fields succeeded.
    pub async fn apply(&self, action: &Action) -> Result<()> {
        // Custom (non-native) dropdowns need a click-open then click-option
        // sequence with a wait in between, so they can't be a single eval.
        if action.action.eq_ignore_ascii_case("select_custom") {
            return self.select_custom(&action.selector, &action.value).await;
        }
        let script = build_action_script(action);
        let result = self
            .page
            .evaluate(script)
            .await
            .map_err(|e| format!("{}: eval failed: {e}", action.selector))?;
        check_ok(result.into_value().unwrap_or(json!({"ok": true})), &action.selector)
    }

    /// Fill a custom dropdown built from `<div>`/`<li>` elements (an ARIA
    /// combobox/listbox, not a native `<select>`): click the trigger to open
    /// the menu, wait for options to render, then click the option whose visible
    /// text matches `value`.
    pub async fn select_custom(&self, trigger: &str, value: &str) -> Result<()> {
        let open = format!(
            r#"(function(){{
                var sel = {sel};
                var el = {query};
                if (!el) return {{ok:false, err:"dropdown not found"}};
                el.scrollIntoView({{block:"center"}});
                ["pointerdown","mousedown","mouseup","click"].forEach(function(t){{
                    el.dispatchEvent(new MouseEvent(t, {{bubbles:true}}));
                }});
                return {{ok:true}};
            }})()"#,
            sel = json!(trigger),
            query = safe_query("sel")
        );
        let opened = self
            .page
            .evaluate(open)
            .await
            .map_err(|e| format!("{trigger}: could not open dropdown: {e}"))?;
        check_ok(opened.into_value().unwrap_or(json!({"ok": true})), trigger)?;

        // Let the option list render (menus often mount in a portal/overlay).
        tokio::time::sleep(Duration::from_millis(450)).await;

        let pick = format!(
            r#"(function(){{
                var want = {val}.trim().toLowerCase();
                if (!want) return {{ok:false, err:"no value to match"}};
                var sel = '[role=option],[role=menuitem],[role=menuitemradio],li,' +
                          '[class*=option],[class*=Option],[class*=item],[class*=result]';
                var nodes = Array.prototype.slice.call(document.querySelectorAll(sel));
                var visible = nodes.filter(function(n){{
                    var r = n.getBoundingClientRect();
                    return r.width > 0 && r.height > 0;
                }});
                function txt(n){{ return (n.textContent||"").replace(/\s+/g," ").trim().toLowerCase(); }}
                var exact = visible.find(function(n){{ return txt(n) === want; }});
                var target = exact || visible.find(function(n){{ return txt(n).indexOf(want) >= 0; }});
                if (!target) return {{ok:false, err:"no option matching '"+want+"'"}};
                target.scrollIntoView({{block:"center"}});
                ["pointerdown","mousedown","mouseup","click"].forEach(function(t){{
                    target.dispatchEvent(new MouseEvent(t, {{bubbles:true}}));
                }});
                return {{ok:true}};
            }})()"#,
            val = json!(value)
        );
        let picked = self
            .page
            .evaluate(pick)
            .await
            .map_err(|e| format!("{trigger}: could not pick option: {e}"))?;
        check_ok(picked.into_value().unwrap_or(json!({"ok": true})), trigger)
    }

    /// Upload a local file to a file-input element. Uses CDP
    /// `DOM.setFileInputFiles`, which sets the input's files directly — no OS
    /// file-picker dialog — and works even on hidden inputs behind a styled
    /// "Upload" button.
    pub async fn upload_file(&self, selector: &str, path: &Path) -> Result<()> {
        let absolute = path
            .canonicalize()
            .map_err(|e| format!("resume file {}: {e}", path.display()))?;
        let element = self
            .page
            .find_element(selector)
            .await
            .map_err(|e| format!("{selector}: file input not found: {e}"))?;
        let params = SetFileInputFilesParams {
            files: vec![absolute.to_string_lossy().into_owned()],
            node_id: None,
            backend_node_id: Some(element.backend_node_id),
            object_id: None,
        };
        self.page
            .execute(params)
            .await
            .map_err(|e| format!("{selector}: upload failed: {e}"))?;
        log::debug(format!(
            "browser ▸ uploaded {} to {selector}",
            absolute.display()
        ));
        Ok(())
    }

    /// Snapshot the current values of every named/id'd input, textarea and
    /// select on the page. Used to learn values the *user* typed by hand so we
    /// can fold them back into `fields.json`.
    pub async fn snapshot_values(&self) -> Result<BTreeMap<String, String>> {
        let result = self
            .page
            .evaluate(SNAPSHOT_JS)
            .await
            .map_err(|e| format!("could not snapshot form values: {e}"))?;
        let map = result
            .into_value::<BTreeMap<String, String>>()
            .map_err(|e| format!("bad snapshot payload: {e}"))?;
        Ok(map
            .into_iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .collect())
    }
}

/// Ask Chrome for its websocket debugger URL via the HTTP `/json/version`
/// endpoint. `Browser::connect` wants the `ws://` URL directly.
async fn fetch_ws_url() -> Result<String> {
    let client = reqwest::Client::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(config::cdp_version_url())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    value
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no webSocketDebuggerUrl in /json/version".into())
}

/// Interpret the `{ok, err}` object returned by our JS shims.
fn check_ok(value: serde_json::Value, ctx: &str) -> Result<()> {
    if value.get("ok").and_then(|v| v.as_bool()).unwrap_or(true) {
        Ok(())
    } else {
        let err = value.get("err").and_then(|v| v.as_str()).unwrap_or("unknown");
        Err(format!("{ctx}: {err}"))
    }
}

/// Whether klawply may launch Chrome itself (default yes; `KLAWPLY_NO_LAUNCH=1`
/// opts out for users who manage Chrome themselves).
fn launch_enabled() -> bool {
    !matches!(
        std::env::var("KLAWPLY_NO_LAUNCH").ok().as_deref(),
        Some("1") | Some("true")
    )
}

/// Spawn Chrome in debug mode with a persistent klawply profile. The child is
/// intentionally detached (not tracked) so the browser outlives the app and can
/// be reused on the next run.
fn launch_chrome() -> Result<()> {
    let profile = config::data_dir().join("chrome-profile");
    let port = config::CDP_PORT.to_string();
    let mut args = vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", profile.display()),
        // Chrome 111+ rejects CDP websockets unless the origin is allow-listed.
        "--remote-allow-origins=*".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    if matches!(
        std::env::var("KLAWPLY_CHROME_HEADLESS").ok().as_deref(),
        Some("1") | Some("true")
    ) {
        args.push("--headless=new".to_string());
    }

    for binary in chrome_candidates() {
        log::debug(format!("browser ▸ trying to launch {binary}"));
        match Command::new(&binary)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_child) => {
                log::info(format!("browser ▸ launched {binary} on :{port}"));
                return Ok(());
            }
            // Not this binary — try the next candidate.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("failed to launch {binary}: {e}")),
        }
    }
    Err("could not find Chrome/Chromium to launch. Install it, set KLAWPLY_CHROME=/path/to/chrome, \
         or start it yourself with --remote-debugging-port=9222"
        .into())
}

/// Candidate Chrome/Chromium binaries, honouring `KLAWPLY_CHROME` first.
fn chrome_candidates() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(explicit) = std::env::var("KLAWPLY_CHROME") {
        if !explicit.trim().is_empty() {
            names.push(explicit);
        }
    }
    names.extend(
        [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "chrome",
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/snap/bin/chromium",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    names
}

/// Poll the debug port until Chrome is ready (or we give up after ~15s).
async fn wait_for_ws() -> Result<String> {
    for _ in 0..75 {
        if let Ok(ws) = fetch_ws_url().await {
            return Ok(ws);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err("Chrome was launched but its debug port never became ready".into())
}

/// A JS expression that resolves the selector held in the JS variable named
/// `var_name`. Falls back to `getElementById` when `querySelector` throws —
/// real job forms use ids that aren't valid CSS selectors (e.g. ones starting
/// with a digit like `#280cb238-…`), which would otherwise throw a `SyntaxError`.
fn safe_query(var_name: &str) -> String {
    format!(
        "(function(){{ try {{ return document.querySelector({v}); }} \
         catch (e) {{ return ({v} && {v}[0] === '#') \
         ? document.getElementById({v}.slice(1)) : null; }} }})()",
        v = var_name
    )
}

/// Build a self-contained JS expression for one action, with all user values
/// JSON-escaped so they can't break out of the string.
fn build_action_script(action: &Action) -> String {
    let sel = json!(action.selector).to_string();
    let val = json!(action.value).to_string();
    let act = json!(action.action.to_lowercase()).to_string();
    format!(
        r#"(function(){{
            var sel = {sel}, action = {act}, value = {val};
            var el = {query};
            if (!el) return {{ok:false, err:"element not found"}};
            // The model sometimes targets a wrapper (a custom element like
            // <spl-form-element> or a label/div) rather than the control itself.
            // Drill into the real input/textarea/select so value-setting works.
            var TAG = (el.tagName || "").toUpperCase();
            if (TAG !== "INPUT" && TAG !== "TEXTAREA" && TAG !== "SELECT"
                && !el.isContentEditable && el.querySelector) {{
                var inner = el.querySelector(
                    "input, textarea, select, [contenteditable=true],[contenteditable='']");
                if (inner) {{ el = inner; TAG = (el.tagName || "").toUpperCase(); }}
            }}
            el.scrollIntoView({{block:"center"}});
            if (action === "click") {{ el.click(); return {{ok:true}}; }}
            if (action === "check" || action === "uncheck") {{
                el.checked = (action === "check");
                el.dispatchEvent(new Event("input", {{bubbles:true}}));
                el.dispatchEvent(new Event("change", {{bubbles:true}}));
                return {{ok:true}};
            }}
            if (action === "select" && el.options) {{
                var opts = Array.prototype.slice.call(el.options);
                var match = opts.find(function(o){{
                    return o.value === value || (o.text || "").trim() === value;
                }});
                if (!match) return {{ok:false, err:"no matching option"}};
                el.value = match.value;
                el.dispatchEvent(new Event("change", {{bubbles:true}}));
                return {{ok:true}};
            }}
            // default: fill. Contenteditable rich-text fields have no value.
            if (el.isContentEditable) {{
                el.focus();
                el.textContent = value;
                el.dispatchEvent(new Event("input", {{bubbles:true}}));
                el.dispatchEvent(new Event("change", {{bubbles:true}}));
                return {{ok:true}};
            }}
            // Find the native `value` setter on el's OWN prototype chain. Picking
            // it by tag name (HTMLInputElement.prototype) throws "Illegal
            // invocation" when el is a subclass/custom element; walking el's real
            // chain also intentionally skips React's own-property override.
            function nativeSetter(node){{
                for (var p = Object.getPrototypeOf(node); p; p = Object.getPrototypeOf(p)) {{
                    var d = Object.getOwnPropertyDescriptor(p, "value");
                    if (d && typeof d.set === "function") return d.set;
                }}
                return null;
            }}
            el.focus();
            var setter = nativeSetter(el);
            try {{
                if (setter) setter.call(el, value); else el.value = value;
            }} catch (e) {{
                try {{ el.value = value; }}
                catch (e2) {{ return {{ok:false, err:"cannot set value: " + e2}}; }}
            }}
            el.dispatchEvent(new Event("input", {{bubbles:true}}));
            el.dispatchEvent(new Event("change", {{bubbles:true}}));
            return {{ok:true}};
        }})()"#,
        query = safe_query("sel")
    )
}

/// Collect current values keyed by the best identifier we can find.
const SNAPSHOT_JS: &str = r#"(function(){
    var out = {};
    var nodes = document.querySelectorAll("input, textarea, select");
    for (var i = 0; i < nodes.length; i++) {
        var el = nodes[i];
        var type = (el.type || "").toLowerCase();
        if (type === "password" || type === "hidden" || type === "file") continue;
        var key = el.name || el.id || el.getAttribute("aria-label") || el.placeholder;
        if (!key) continue;
        var val = (type === "checkbox" || type === "radio")
            ? (el.checked ? "true" : "")
            : (el.value || "");
        if (val) out[key] = val;
    }
    return out;
})()"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the CDP upload path against a real (headless) Chrome. Ignored by
    /// default because it needs a Chrome binary + a free debug port; run with:
    /// `KLAWPLY_CHROME_HEADLESS=1 cargo test upload_sets_file_input -- --ignored`
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn upload_sets_file_input() {
        let session = BrowserSession::connect().await.expect("connect/launch chrome");
        session
            .open_and_capture("data:text/html,<input type=file id=f>")
            .await
            .expect("navigate");

        let tmp = std::env::temp_dir().join("klawply_test_resume.pdf");
        std::fs::write(&tmp, b"%PDF-1.4 test").unwrap();

        session.upload_file("#f", &tmp).await.expect("upload");

        let name = session
            .page
            .evaluate("document.querySelector('#f').files[0] ? document.querySelector('#f').files[0].name : ''")
            .await
            .expect("eval")
            .into_value::<String>()
            .expect("string");
        assert!(name.contains("klawply_test_resume"), "got file name: {name:?}");
    }

    /// Verifies custom (`<div>`-based) dropdown selection end-to-end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn select_custom_picks_option() {
        let session = BrowserSession::connect().await.expect("connect");
        session.open_and_capture("about:blank").await.expect("nav");
        let setup = r#"(function(){
            document.body.innerHTML =
              '<div id=t role=combobox>Pick</div>' +
              '<div id=m style="display:none">' +
              '<div role=option>Male</div><div role=option>Female</div></div>';
            var t=document.getElementById('t'), m=document.getElementById('m');
            t.addEventListener('click', function(){ m.style.display='block'; });
            Array.prototype.forEach.call(m.querySelectorAll('[role=option]'), function(o){
                o.addEventListener('click', function(){ window.__p = o.textContent; });
            });
            return true;
        })()"#;
        session.page.evaluate(setup).await.expect("setup");
        session.select_custom("#t", "Female").await.expect("select_custom");
        let picked = session.page
            .evaluate("window.__p || ''").await.expect("eval")
            .into_value::<String>().expect("string");
        assert_eq!(picked, "Female");
    }

    /// Regression for the SmartRecruiters "Illegal invocation" failure: a field
    /// wrapped in a custom element whose id is the target. The fill shim must
    /// drill into the inner <input> and set its value without throwing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn fills_input_inside_custom_element_wrapper() {
        let session = BrowserSession::connect().await.expect("connect");
        session.open_and_capture("about:blank").await.expect("nav");
        // A custom element (unknown tag → HTMLElement, not HTMLInputElement)
        // wrapping a real input — mirrors <spl-form-element> on SmartRecruiters.
        session
            .page
            .evaluate(
                r#"(function(){
                    document.body.innerHTML =
                      '<spl-form-element id="first-name-input"><input type=text></spl-form-element>';
                    return true;
                })()"#,
            )
            .await
            .expect("setup");

        let action = Action {
            selector: "#first-name-input".into(),
            action: "fill".into(),
            value: "Karan".into(),
            label: "First name".into(),
        };
        session.apply(&action).await.expect("apply fill");

        let got = session
            .page
            .evaluate("document.querySelector('#first-name-input input').value")
            .await
            .expect("eval")
            .into_value::<String>()
            .expect("string");
        assert_eq!(got, "Karan");
    }

    /// Regression for the "not a valid selector" failure: an id that starts with
    /// a digit is not a valid CSS selector, so querySelector throws. The shim
    /// must fall back to getElementById and still fill the field.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn fills_field_with_digit_leading_id() {
        let session = BrowserSession::connect().await.expect("connect");
        session.open_and_capture("about:blank").await.expect("nav");
        session
            .page
            .evaluate(
                r#"(function(){
                    document.body.innerHTML =
                      '<input id="280cb238-6f0b-49d0-afb0-ba9dc3fdf24a" type=text>';
                    return true;
                })()"#,
            )
            .await
            .expect("setup");

        let action = Action {
            selector: "#280cb238-6f0b-49d0-afb0-ba9dc3fdf24a".into(),
            action: "fill".into(),
            value: "hello".into(),
            label: String::new(),
        };
        session.apply(&action).await.expect("apply fill");

        let got = session
            .page
            .evaluate("document.getElementById('280cb238-6f0b-49d0-afb0-ba9dc3fdf24a').value")
            .await
            .expect("eval")
            .into_value::<String>()
            .expect("string");
        assert_eq!(got, "hello");
    }
}
