//! For providers: an agent's JSON config edited through the door the way
//! teletap's own installer did it. Only our entries change; everything else
//! keeps its place.

use anyhow::{Context, Result, bail};

use crate::{InstallRequest, door};
use serde_json::{Map, Value};

/// Quotes a word for `sh -c` when it needs it.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=:,@%".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `[TELETAP_HOME=… ]<teletap> hook <agent> <event>`, run by the agent's shell.
pub fn hook_command(req: &InstallRequest, agent: &str, event: &str) -> String {
    let mut cmd = String::new();
    if let Some(home) = &req.teletap_home {
        cmd.push_str(&format!("TELETAP_HOME={} ", shell_quote(home)));
    }
    cmd.push_str(&format!(
        "{} hook {agent} {event}",
        shell_quote(&req.teletap_bin)
    ));
    cmd
}

/// True for a hook command teletap wrote (any teletap path or home).
pub fn is_our_hook(command: &str, agent: &str) -> bool {
    let words: Vec<String> = command
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect();
    words.windows(3).any(|w| {
        let bin = w[0].rsplit('/').next().unwrap_or("");
        bin == "teletap" && w[1] == "hook" && w[2] == agent
    })
}

/// Our MCP servers: (name, args after the teletap binary). Their tools run
/// without asking.
pub const MCP_SERVERS: &[(&str, &[&str])] = &[("teletap", &["mcp"])];

/// MCP servers older providers set (`teletap-ext`: extension tools, gone):
/// install and uninstall remove them.
pub const OLD_MCP_SERVERS: &[&str] = &["teletap-ext"];

/// True when an MCP server entry runs the teletap binary.
pub fn is_our_mcp(entry: &Value) -> bool {
    let bin = entry
        .get("command")
        .and_then(Value::as_str)
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("");
    bin == "teletap"
}

/// An MCP server entry running `teletap <args>`, for JSON configs.
pub fn mcp_entry(req: &InstallRequest, args: &[&str], extra: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in extra.iter().filter(|(k, _)| *k == "type") {
        m.insert(k.to_string(), v.clone());
    }
    m.insert("command".into(), Value::String(req.teletap_bin.clone()));
    m.insert(
        "args".into(),
        Value::Array(args.iter().map(|a| Value::String(a.to_string())).collect()),
    );
    if let Some(home) = &req.teletap_home {
        let mut env = Map::new();
        env.insert("TELETAP_HOME".into(), Value::String(home.clone()));
        m.insert("env".into(), Value::Object(env));
    }
    for (k, v) in extra.iter().filter(|(k, _)| *k != "type") {
        m.insert(k.to_string(), v.clone());
    }
    Value::Object(m)
}

/// Reads a JSON object file through the door; missing or empty is `{}`.
pub fn read(path: &str) -> Result<Value> {
    match door::read(path)? {
        Some(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Some(text) => {
            let v: Value = serde_json::from_str(&text)
                .with_context(|| format!("{path} is not valid JSON; not touching it"))?;
            if !v.is_object() {
                bail!("{path} is not a JSON object; not touching it");
            }
            Ok(v)
        }
        None => Ok(Value::Object(Map::new())),
    }
}

/// Writes `value` (pretty, with a final newline) when it differs.
pub fn write(path: &str, value: &Value) -> Result<bool> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(door::write(path, &text)?)
}

/// `obj[key]` as an object, created if missing.
pub fn object_at<'a>(obj: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    let map = obj.as_object_mut().context("expected a JSON object")?;
    let slot = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    slot.as_object_mut()
        .with_context(|| format!("`{key}` is not an object"))
}

/// Claude hook shape: `hooks.<Event> = [{matcher?, hooks: [{type, command,
/// timeout}]}]`. Puts our handler in place (or appends a group).
pub fn set_hook(
    hooks: &mut Map<String, Value>,
    event: &str,
    agent: &str,
    handler: Value,
    matcher: Option<&str>,
) -> Result<()> {
    let groups = hooks
        .entry(event.to_string())
        .or_insert_with(|| Value::Array(vec![]))
        .as_array_mut()
        .with_context(|| format!("hooks.{event} is not an array"))?;
    let mut placed = false;
    for group in groups.iter_mut() {
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut i = 0;
        while i < handlers.len() {
            let ours = handlers[i]
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|c| is_our_hook(c, agent));
            if ours && !placed {
                handlers[i] = handler.clone();
                placed = true;
                i += 1;
            } else if ours {
                handlers.remove(i);
            } else {
                i += 1;
            }
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|h| !h.is_empty())
    });
    if !placed {
        let mut group = serde_json::json!({ "hooks": [handler] });
        if let Some(m) = matcher {
            group["matcher"] = m.into();
        }
        groups.push(group);
    }
    Ok(())
}

fn remove_from_groups(groups: &mut Value, agent: &str) {
    let Some(groups) = groups.as_array_mut() else {
        return;
    };
    for group in groups.iter_mut() {
        if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
            handlers.retain(|h| {
                !h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| is_our_hook(c, agent))
            });
        }
    }
    groups.retain(|g| {
        g.get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|h| !h.is_empty())
    });
}

/// Removes our handlers from every event; drops emptied groups/events.
pub fn remove_hooks(root: &mut Value, agent: &str) {
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for groups in hooks.values_mut() {
        remove_from_groups(groups, agent);
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        root.as_object_mut().unwrap().remove("hooks");
    }
}

/// Removes our MCP servers, old ones included; drops an emptied table.
pub fn remove_mcp(root: &mut Value) -> bool {
    MCP_SERVERS
        .iter()
        .fold(remove_old_mcp(root), |any, (name, _)| {
            remove_mcp_server(root, name) | any
        })
}

/// Removes our old MCP servers (`OLD_MCP_SERVERS`); drops an emptied table.
pub fn remove_old_mcp(root: &mut Value) -> bool {
    OLD_MCP_SERVERS
        .iter()
        .fold(false, |any, name| remove_mcp_server(root, name) | any)
}

/// Removes `mcpServers.<name>` when it is ours; drops an emptied table.
pub fn remove_mcp_server(root: &mut Value, name: &str) -> bool {
    let Some(servers) = root.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    if !servers.get(name).is_some_and(is_our_mcp) {
        return false;
    }
    servers.remove(name);
    if servers.is_empty() {
        root.as_object_mut().unwrap().remove("mcpServers");
    }
    true
}
