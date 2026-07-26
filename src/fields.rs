//! Persistent user data: the resume text (`resume.txt`) and the structured
//! application fields the agent fills forms from (`fields.json`).
//!
//! `fields.json` is deliberately a flat-ish JSON object (`{"first_name": "...",
//! "email": "...", ...}`) so the LLM can read and rewrite it freely and the user
//! can hand-edit it. Values are always strings.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::config;

/// Read `resume.txt`. Returns an empty string if it doesn't exist.
pub fn read_resume() -> String {
    std::fs::read_to_string(config::resume_path()).unwrap_or_default()
}

pub fn resume_is_blank() -> bool {
    read_resume().trim().is_empty()
}

pub fn write_resume(text: &str) -> Result<(), String> {
    config::ensure_data_dir();
    std::fs::write(config::resume_path(), text).map_err(|e| e.to_string())
}

/// Load `fields.json` as an ordered name→value map. Corrupt or missing files
/// yield an empty map rather than an error, so the app always starts.
pub fn load_fields() -> BTreeMap<String, String> {
    let Ok(raw) = std::fs::read_to_string(config::fields_path()) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(&raw) else {
        return BTreeMap::new();
    };
    obj.into_iter()
        .filter_map(|(k, v)| value_to_string(&v).map(|s| (k, s)))
        .collect()
}

/// Overwrite `fields.json` with `fields`.
pub fn save_fields(fields: &BTreeMap<String, String>) -> Result<(), String> {
    config::ensure_data_dir();
    let mut obj = Map::new();
    for (k, v) in fields {
        obj.insert(k.clone(), Value::String(v.clone()));
    }
    let json = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    std::fs::write(config::fields_path(), json).map_err(|e| e.to_string())
}

/// Merge `updates` into the stored fields (updates win) and persist. Returns the
/// keys that were newly added or changed, for user-facing reporting.
pub fn merge_fields(updates: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let mut current = load_fields();
    let mut changed = Vec::new();
    for (k, v) in updates {
        if v.trim().is_empty() {
            continue;
        }
        if current.get(k).map(|old| old != v).unwrap_or(true) {
            changed.push(k.clone());
        }
        current.insert(k.clone(), v.clone());
    }
    save_fields(&current)?;
    Ok(changed)
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
