//! `teletap/cursor`: Cursor (IDE and CLI) on teletap, as a provider extension. It
//! used to be `teletap-agents/src/install/cursor.rs` and the Cursor parts of
//! `hooks.rs` in teletap itself; it writes the same files, byte for byte.
//!
//! - install: hooks in `<config dir>/hooks.json` and the MCP server `teletap`
//!   in `<config dir>/mcp.json` (`~/.cursor` by default). Both the editor and
//!   `cursor-agent` read them. The CLI's allow list (`cli-config.json`) gets
//!   `Mcp(teletap:*)` so the agent can use teletap's tools without asking each
//!   time. The `teletap-ext` server older versions set goes, on install and
//!   uninstall;
//! - hooks: which session event each Cursor hook is, and what it prints.
//!
//! Hooks are flat lists: `{"version": 1, "hooks": {"stop": [{command, timeout}]}}`.
//! `beforeSubmitPrompt` isn't hooked: it can't add context, so a message
//! handed over there would be lost.
//!
//! Docs: <https://cursor.com/docs/agent/hooks>, <https://cursor.com/docs/context/mcp>

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::{Context, Result};
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde_json::{Value, json};
#[cfg(target_arch = "wasm32")]
use teletap_ext::Event;
use teletap_ext::{HookParsed, HookRendered, InstallRequest, InstallResult, config as json, door};

/// Lets the CLI use teletap's MCP tools without a prompt.
const PERMISSION: &str = "Mcp(teletap:*)";

/// (Cursor event, our event name, timeout in seconds).
const EVENTS: &[(&str, &str, u64)] = &[
    ("sessionStart", "start", 10),
    ("postToolUse", "step", 10),
    // Also after a cancel (`status: "aborted"`): the chat is idle again.
    ("stop", "stop", 10),
    ("sessionEnd", "end", 5),
];

// The export exists in the WASM build only: host-side unit tests can't link
// Extism's imports, and test the logic below directly.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle(input: String) -> FnResult<String> {
    let answer = match Event::parse(&input)? {
        Event::Install(req) => serde_json::to_value(result(install(&req)))?,
        Event::Uninstall(req) => serde_json::to_value(result(uninstall(&req)))?,
        Event::Hook { name, payload, env } => serde_json::to_value(hook(&name, &payload, &env))?,
        Event::HookRender {
            kind,
            handover,
            intro,
        } => serde_json::to_value(HookRendered {
            stdout: render(&kind, handover, intro),
        })?,
        _ => Value::Null,
    };
    Ok(answer.to_string())
}

/// A config the person must fix is an answer, not a crash.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn result(r: Result<InstallResult>) -> InstallResult {
    r.unwrap_or_else(|e| InstallResult {
        error: Some(format!("{e:#}")),
        ..InstallResult::default()
    })
}

// ---------------------------------------------------------------- install

fn dir(req: &InstallRequest) -> String {
    match &req.config_dir {
        Some(d) => d.trim_end_matches('/').to_string(),
        None => format!("{}/.cursor", req.home.trim_end_matches('/')),
    }
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| json::is_our_hook(c, "cursor"))
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let dir = dir(req);
    let mut report = InstallResult::default();

    let path = format!("{dir}/hooks.json");
    let mut root = json::read(&path)?;
    add_hooks(&mut root, req, &path)?;
    if json::write(&path, &root)? {
        report.files.push(path);
    }

    let path = format!("{dir}/mcp.json");
    let mut root = json::read(&path)?;
    add_mcp(&mut root, req)?;
    if json::write(&path, &root)? {
        report.files.push(path);
    }

    let path = format!("{dir}/cli-config.json");
    let mut root = json::read(&path)?;
    add_permission(&mut root, &path)?;
    if json::write(&path, &root)? {
        report.files.push(path);
    }
    report.notes.push(
        "Cursor: hooks (sessionStart, postToolUse, stop, sessionEnd) and the `teletap` \
         MCP server are set for the editor and the CLI; new chats pick them up."
            .into(),
    );
    Ok(report)
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let dir = dir(req);
    let mut report = InstallResult::default();

    let path = format!("{dir}/hooks.json");
    if door::read(&path)?.is_some() {
        let mut root = json::read(&path)?;
        remove_hooks(&mut root);
        if json::write(&path, &root)? {
            report.files.push(path);
        }
    }
    let path = format!("{dir}/mcp.json");
    if door::read(&path)?.is_some() {
        let mut root = json::read(&path)?;
        if json::remove_mcp(&mut root) && json::write(&path, &root)? {
            report.files.push(path);
        }
    }
    let path = format!("{dir}/cli-config.json");
    if door::read(&path)?.is_some() {
        let mut root = json::read(&path)?;
        remove_permission(&mut root);
        if json::write(&path, &root)? {
            report.files.push(path);
        }
    }
    Ok(report)
}

/// Our entries in `hooks.json`; ours under an old event go.
fn add_hooks(root: &mut Value, req: &InstallRequest, path: &str) -> Result<()> {
    root.as_object_mut()
        .context("hooks.json is not a JSON object")?
        .entry("version")
        .or_insert(json!(1));
    let hooks = json::object_at(root, "hooks")?;
    for (event, ours, timeout) in EVENTS {
        let entry =
            json!({ "command": json::hook_command(req, "cursor", ours), "timeout": timeout });
        let list = hooks
            .entry(event.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!("hooks.{event} in {path} is not a list"))?;
        match list.iter().position(is_ours) {
            Some(i) => list[i] = entry,
            None => list.push(entry),
        }
    }
    Ok(())
}

/// Our entries gone from every event of `hooks.json`; emptied events go.
fn remove_hooks(root: &mut Value) {
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        for list in hooks.values_mut().filter_map(Value::as_array_mut) {
            list.retain(|e| !is_ours(e));
        }
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
}

/// Our MCP servers in `mcp.json`, old ones gone.
fn add_mcp(root: &mut Value, req: &InstallRequest) -> Result<()> {
    let servers = json::object_at(root, "mcpServers")?;
    for (name, args) in json::MCP_SERVERS {
        servers.insert(
            name.to_string(),
            json::mcp_entry(req, args, &[("type", Value::String("stdio".into()))]),
        );
    }
    json::remove_old_mcp(root);
    Ok(())
}

/// `Mcp(teletap:*)` in the CLI's `permissions.allow`.
fn add_permission(root: &mut Value, path: &str) -> Result<()> {
    let allow = json::object_at(root, "permissions")?
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .with_context(|| format!("permissions.allow in {path} is not a list"))?;
    if !allow.iter().any(|v| v == PERMISSION) {
        allow.push(json!(PERMISSION));
    }
    Ok(())
}

fn remove_permission(root: &mut Value) {
    if let Some(allow) = root
        .pointer_mut("/permissions/allow")
        .and_then(Value::as_array_mut)
    {
        allow.retain(|v| v != PERMISSION);
    }
}

// ---------------------------------------------------------------- hooks

/// Our event word (as installed) or Cursor's own hook name → session event.
fn parse_event(name: &str) -> Option<&'static str> {
    let n: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match n.as_str() {
        "start" | "sessionstart" => "start",
        "step" | "posttooluse" => "step",
        "stop" => "stop",
        "end" | "sessionend" => "end",
        _ => return None,
    })
}

/// A Cursor chat is its `conversation_id`, whatever process runs the hook:
/// the session key is always `cursor-<id>`.
pub fn hook(name: &str, payload: &Value, _env: &BTreeMap<String, String>) -> HookParsed {
    let named = payload.get("hook_event_name").and_then(Value::as_str);
    let Some(kind) = parse_event(name).or_else(|| named.and_then(parse_event)) else {
        return HookParsed::default();
    };
    let session_id = payload
        .get("session_id")
        .or_else(|| payload.get("thread_id"))
        .or_else(|| payload.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    HookParsed {
        kind: Some(kind.into()),
        key: Some(format!(
            "cursor-{}",
            session_id.as_deref().unwrap_or("unknown")
        )),
        session_id,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string),
        inbox: None,
    }
}

/// What the hook prints: `stop` continues with `followup_message`,
/// `postToolUse` and `sessionStart` take `additional_context`.
pub fn render(kind: &str, handover: Option<String>, intro: Option<String>) -> String {
    let out = match kind {
        "stop" => handover.map(|t| json!({ "followup_message": t })),
        "step" => handover.map(|t| json!({ "additional_context": t })),
        "start" => {
            let text: Vec<String> = intro.into_iter().chain(handover).collect();
            (!text.is_empty()).then(|| json!({ "additional_context": text.join("\n\n") }))
        }
        _ => None,
    };
    out.map(|v| v.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/usr/local/bin/teletap hook";
    const PATH: &str = "/c/hooks.json";

    fn opts() -> InstallRequest {
        InstallRequest {
            teletap_bin: "/usr/local/bin/teletap".into(),
            home: "/Users/p".into(),
            ..InstallRequest::default()
        }
    }

    #[test]
    fn keeps_other_hooks() {
        let theirs = json!({"version": 1, "hooks": {"stop": [{"command": "/opt/orca/hook.sh", "timeout": 10}]}});
        let mut h = theirs.clone();
        add_hooks(&mut h, &opts(), PATH).unwrap();
        assert_eq!(h["hooks"]["stop"].as_array().unwrap().len(), 2);
        assert_eq!(
            h["hooks"]["sessionStart"][0],
            json!({"command": format!("{CMD} cursor start"), "timeout": 10})
        );
        assert_eq!(h["hooks"]["sessionEnd"][0]["timeout"], 5);
        assert!(
            h["hooks"].get("beforeSubmitPrompt").is_none(),
            "it can't add context"
        );
        let once = h.clone();
        add_hooks(&mut h, &opts(), PATH).unwrap();
        assert_eq!(h, once, "idempotent");
        remove_hooks(&mut h);
        assert_eq!(h, theirs);

        // The old `teletap-ext` server goes.
        let mut m = json!({"mcpServers": {
            "gh": {"command": "gh-mcp"},
            "teletap-ext": {"type": "stdio", "command": "/old/teletap", "args": ["mcp", "--ext"]},
        }});
        add_mcp(&mut m, &opts()).unwrap();
        assert_eq!(
            m,
            json!({"mcpServers": {
                "gh": {"command": "gh-mcp"},
                "teletap": {"type": "stdio", "command": "/usr/local/bin/teletap", "args": ["mcp"]},
            }})
        );
        let once = m.clone();
        add_mcp(&mut m, &opts()).unwrap();
        assert_eq!(m, once, "idempotent");
        assert!(json::remove_mcp(&mut m));
        assert_eq!(m, json!({"mcpServers": {"gh": {"command": "gh-mcp"}}}));

        let mut c = json!({});
        add_permission(&mut c, "/c/cli-config.json").unwrap();
        add_permission(&mut c, "/c/cli-config.json").unwrap();
        assert_eq!(c["permissions"]["allow"], json!(["Mcp(teletap:*)"]));
        remove_permission(&mut c);
        assert_eq!(c, json!({"permissions": {"allow": []}}));
    }

    #[test]
    fn a_new_hooks_file_starts_at_version_1() {
        let mut h = json!({});
        add_hooks(&mut h, &opts(), PATH).unwrap();
        let keys: Vec<_> = h.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["version", "hooks"]);
        let events: Vec<_> = h["hooks"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            events,
            ["sessionStart", "postToolUse", "stop", "sessionEnd"]
        );
        let mut bad = json!({"hooks": {"stop": {}}});
        let e = add_hooks(&mut bad, &opts(), PATH).unwrap_err();
        assert_eq!(e.to_string(), "hooks.stop in /c/hooks.json is not a list");
    }

    #[test]
    fn event_names() {
        for (n, k) in [
            ("start", "start"),
            ("sessionStart", "start"),
            ("step", "step"),
            ("postToolUse", "step"),
            ("stop", "stop"),
            ("end", "end"),
            ("sessionEnd", "end"),
        ] {
            assert_eq!(parse_event(n), Some(k), "{n}");
        }
        for stale in ["tool", "preToolUse", "beforeSubmitPrompt"] {
            assert_eq!(parse_event(stale), None, "{stale}");
        }
    }

    #[test]
    fn the_chat_is_the_key() {
        let env = BTreeMap::new();
        let p = hook(
            "stop",
            &json!({"conversation_id": "c-42", "hook_event_name": "stop", "status": "aborted", "cursor_version": "2.6.0"}),
            &env,
        );
        assert_eq!(p.kind.as_deref(), Some("stop"));
        assert_eq!(p.session_id.as_deref(), Some("c-42"));
        assert_eq!(p.key.as_deref(), Some("cursor-c-42"));
        assert_eq!(
            hook("x", &json!({"hook_event_name": "postToolUse"}), &env)
                .key
                .as_deref(),
            Some("cursor-unknown")
        );
        assert!(hook("tool", &json!({}), &env).kind.is_none());
    }

    #[test]
    fn renders_what_the_hook_prints() {
        for kind in ["stop", "step", "start", "end", "prompt"] {
            assert_eq!(render(kind, None, None), "", "{kind}");
        }
        assert_eq!(
            render("stop", Some("m".into()), None),
            r#"{"followup_message":"m"}"#
        );
        assert_eq!(
            render("step", Some("m".into()), None),
            r#"{"additional_context":"m"}"#
        );
        assert_eq!(
            render("start", Some("m".into()), Some("hi".into())),
            r#"{"additional_context":"hi\n\nm"}"#
        );
        assert_eq!(render("end", Some("m".into()), None), "");
    }

    #[test]
    fn config_dir() {
        assert_eq!(dir(&opts()), "/Users/p/.cursor");
        let req = InstallRequest {
            config_dir: Some("/tmp/c/".into()),
            ..opts()
        };
        assert_eq!(dir(&req), "/tmp/c");
    }
}
