//! Firebase Realtime Database event streams (REST "server-sent events").
//!
//! [`watch`] keeps a local copy of a node up to date and calls back with the whole node
//! after every change. It reconnects automatically (network drops, token expiry).

use crate::firebase::Firebase;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::time::Duration;

/// Called with (current value of the node, true if this is the snapshot sent on (re)connect).
pub fn watch(
    fb: &Firebase,
    path: &str,
    mut on_change: impl FnMut(&Value, bool),
    mut on_error: impl FnMut(String),
) -> ! {
    let mut backoff = 1;
    loop {
        match fb.open_stream(path) {
            Ok(resp) => {
                backoff = 1;
                let mut mirror = Value::Null;
                let mut first = true;
                let mut event = String::new();
                for line in BufReader::new(resp).lines() {
                    let Ok(line) = line else { break };
                    if let Some(e) = line.strip_prefix("event:") {
                        event = e.trim().to_string();
                    } else if let Some(d) = line.strip_prefix("data:") {
                        match event.as_str() {
                            "put" | "patch" => {
                                let Ok(msg) = serde_json::from_str::<Value>(d.trim()) else {
                                    continue;
                                };
                                let p = msg["path"].as_str().unwrap_or("/");
                                apply(&mut mirror, p, msg["data"].clone(), event == "patch");
                                on_change(&mirror, first);
                                first = false;
                            }
                            "cancel" => {
                                on_error(format!(
                                    "Firebase refused /{path} (check the read rules)"
                                ));
                                break;
                            }
                            "auth_revoked" => break, // token expired: reconnect with a fresh one
                            _ => {}                  // keep-alive
                        }
                    }
                }
            }
            Err(e) => on_error(format!("{e:#}")),
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(30);
    }
}

/// Apply a put/patch event at `path` (e.g. "/", "/12", "/12/ax") to the local copy.
pub fn apply(root: &mut Value, path: &str, data: Value, patch: bool) {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut node = root;
    for seg in &segs {
        node = child_mut(node, seg);
    }
    if patch {
        if let Value::Object(m) = data {
            for (k, v) in m {
                let c = child_mut(node, &k);
                *c = v;
            }
        }
    } else {
        *node = data;
    }
}

fn child_mut<'a>(node: &'a mut Value, key: &str) -> &'a mut Value {
    // arrays (keys 0..n) are converted to objects so any index can be addressed
    if let Value::Array(a) = node {
        let m: serde_json::Map<String, Value> = a
            .drain(..)
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect();
        *node = Value::Object(m);
    }
    if !node.is_object() {
        *node = Value::Object(Default::default());
    }
    node.as_object_mut()
        .map(|m| m.entry(key.to_string()).or_insert(Value::Null))
        .expect("object")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn put_and_patch() {
        let mut v = Value::Null;
        apply(&mut v, "/", json!([{"ax": 1}, {"ax": 2}]), false);
        apply(&mut v, "/1", json!({"ax": 5}), false);
        assert_eq!(v["1"]["ax"], 5);
        apply(&mut v, "/", json!({"2": {"ax": 9}}), true);
        assert_eq!(v["2"]["ax"], 9);
        assert_eq!(v["0"]["ax"], 1);
    }
}
