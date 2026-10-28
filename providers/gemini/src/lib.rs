//! `teletap/gemini`: Gemini CLI on teletap, as a provider extension. It used to be
//! `teletap-agents/src/install/gemini.rs` and the Gemini parts of `hooks.rs` in
//! teletap itself; it writes the same files, byte for byte.
//!
//! - install: hooks and the MCP server `teletap` in `<config dir>/settings.json`
//!   (`~/.gemini`, or `$GEMINI_CLI_HOME/.gemini`). The `teletap-ext` server
//!   older versions set goes, on install and uninstall;
//! - hooks: which session event each Gemini hook is, and what it prints.
//!
//! Docs: <https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/reference.md>,
//! <https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md>.
//! Hook timeouts are in milliseconds; `trust: true` skips tool confirmations.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde_json::{Value, json};
#[cfg(target_arch = "wasm32")]
use teletap_ext::Event;
use teletap_ext::{HookParsed, HookRendered, InstallRequest, InstallResult, config as json, door};

/// (Gemini event, our event name, timeout in milliseconds).
const EVENTS: &[(&str, &str, u64)] = &[
    ("SessionStart", "start", 10_000),
    ("BeforeAgent", "prompt", 10_000),
    ("AfterAgent", "stop", 10_000),
    ("SessionEnd", "end", 5_000),
    ("AfterTool", "step", 10_000),
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

fn settings_path(req: &InstallRequest) -> String {
    let env = req
        .env
        .get("GEMINI_CLI_HOME")
        .filter(|v| !v.is_empty())
        .map(|h| format!("{}/.gemini", h.trim_end_matches('/')));
    let dir = req
        .config_dir
        .as_deref()
        .map(|d| d.trim_end_matches('/').to_string())
        .or(env)
        .unwrap_or_else(|| format!("{}/.gemini", req.home.trim_end_matches('/')));
    format!("{dir}/settings.json")
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let path = settings_path(req);
    let mut report = InstallResult::default();
    let mut settings = json::read(&path)?;
    add(&mut settings, req)?;
    if json::write(&path, &settings)? {
        report.files.push(path);
    }
    report.notes.push(
        "Gemini CLI: hooks (SessionStart, BeforeAgent, AfterAgent, SessionEnd, AfterTool) and the \
         `teletap` MCP server are set; restart Gemini to load them."
            .into(),
    );
    Ok(report)
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let path = settings_path(req);
    let mut report = InstallResult::default();
    if door::read(&path)?.is_none() {
        return Ok(report);
    }
    let mut settings = json::read(&path)?;
    remove(&mut settings);
    if json::write(&path, &settings)? {
        report.files.push(path);
    }
    Ok(report)
}

/// Our hooks and MCP servers, merged into `settings.json`; old servers gone.
fn add(settings: &mut Value, req: &InstallRequest) -> Result<()> {
    {
        let hooks = json::object_at(settings, "hooks")?;
        for (event, ours, timeout) in EVENTS {
            let handler = json!({
                "name": format!("teletap-{ours}"),
                "type": "command",
                "command": json::hook_command(req, "gemini", ours),
                "timeout": timeout,
            });
            json::set_hook(hooks, event, "gemini", handler, None)?;
        }
    }
    let servers = json::object_at(settings, "mcpServers")?;
    for (name, args) in json::MCP_SERVERS {
        let extra = [("trust", Value::Bool(true))];
        servers.insert(name.to_string(), json::mcp_entry(req, args, &extra));
    }
    json::remove_old_mcp(settings);
    Ok(())
}

/// Our hooks (every event) and MCP servers, gone from `settings.json`.
fn remove(settings: &mut Value) {
    json::remove_hooks(settings, "gemini");
    json::remove_mcp(settings);
}

// ---------------------------------------------------------------- hooks

/// Our event word (as installed) or Gemini's own hook name → session event.
fn parse_event(name: &str) -> Option<&'static str> {
    let n: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match n.as_str() {
        "start" | "sessionstart" => "start",
        "prompt" | "beforeagent" => "prompt",
        "stop" | "afteragent" => "stop",
        "step" | "aftertool" => "step",
        "end" | "sessionend" => "end",
        _ => return None,
    })
}

pub fn hook(name: &str, payload: &Value, _env: &BTreeMap<String, String>) -> HookParsed {
    // Cursor runs other agents' hooks too, with its own payload: only
    // Cursor's own hooks speak there.
    if payload.get("cursor_version").is_some() {
        return HookParsed::default();
    }
    let named = payload.get("hook_event_name").and_then(Value::as_str);
    let Some(kind) = parse_event(name).or_else(|| named.and_then(parse_event)) else {
        return HookParsed::default();
    };
    let str_at = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
    HookParsed {
        kind: Some(kind.into()),
        session_id: payload
            .get("session_id")
            .or_else(|| payload.get("thread_id"))
            .or_else(|| payload.get("conversation_id"))
            .and_then(Value::as_str)
            .map(str::to_string),
        key: None,
        cwd: str_at("cwd"),
        inbox: None,
    }
}

/// What the hook prints: the waiting messages as `additionalContext` on
/// `SessionStart`, `BeforeAgent` and `AfterTool`. A turn end (`AfterAgent`)
/// or a session end prints nothing.
pub fn render(kind: &str, handover: Option<String>, intro: Option<String>) -> String {
    match kind {
        "step" => handover.map_or_else(String::new, |t| additional_context("AfterTool", &t)),
        "prompt" => handover.map_or_else(String::new, |t| additional_context("BeforeAgent", &t)),
        "start" => {
            let text: Vec<String> = intro.into_iter().chain(handover).collect();
            if text.is_empty() {
                String::new()
            } else {
                additional_context("SessionStart", &text.join("\n\n"))
            }
        }
        _ => String::new(),
    }
}

fn additional_context(event: &str, text: &str) -> String {
    json!({
        "hookSpecificOutput": { "hookEventName": event, "additionalContext": text }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/usr/local/bin/teletap hook";

    fn opts() -> InstallRequest {
        InstallRequest {
            teletap_bin: "/usr/local/bin/teletap".into(),
            home: "/Users/p".into(),
            ..InstallRequest::default()
        }
    }

    #[test]
    fn install_and_uninstall() {
        let original = json!({"theme": "Dracula", "mcpServers": {"gh": {"command": "gh-mcp"}}});
        let mut s = original.clone();
        add(&mut s, &opts()).unwrap();
        assert_eq!(s["theme"], "Dracula");
        for (event, ours, _) in EVENTS {
            let h = &s["hooks"][event][0]["hooks"][0];
            assert_eq!(h["name"], format!("teletap-{ours}"), "{event}");
            assert_eq!(h["command"], format!("{CMD} gemini {ours}"), "{event}");
            assert!(h["timeout"].as_u64().unwrap() >= 1000, "ms");
        }
        assert_eq!(
            s["mcpServers"]["teletap"],
            json!({"command": "/usr/local/bin/teletap", "args": ["mcp"], "trust": true})
        );
        assert!(s["mcpServers"].get("teletap-ext").is_none());
        assert_eq!(s["mcpServers"]["gh"]["command"], "gh-mcp");

        let once = s.clone();
        add(&mut s, &opts()).unwrap();
        assert_eq!(s, once, "idempotent");
        remove(&mut s);
        assert_eq!(s, original);
    }

    #[test]
    fn the_old_teletap_ext_server_goes() {
        let ext = json!({"command": "/old/teletap", "args": ["mcp", "--ext"]});
        let mut s = json!({"mcpServers": {"teletap-ext": ext, "ext": {"command": "/bin/other"}}});
        add(&mut s, &opts()).unwrap();
        assert!(s["mcpServers"].get("teletap-ext").is_none(), "on install");
        s["mcpServers"]["teletap-ext"] = ext;
        remove(&mut s);
        assert_eq!(
            s["mcpServers"],
            json!({"ext": {"command": "/bin/other"}}),
            "on uninstall"
        );
        // Someone else's `teletap-ext` server stays.
        let mut theirs = json!({"mcpServers": {"teletap-ext": {"command": "/bin/not-teletap"}}});
        let before = theirs.clone();
        remove(&mut theirs);
        assert_eq!(theirs, before);
    }

    #[test]
    fn event_names() {
        for (n, k) in [
            ("start", "start"),
            ("SessionStart", "start"),
            ("prompt", "prompt"),
            ("BeforeAgent", "prompt"),
            ("stop", "stop"),
            ("AfterAgent", "stop"),
            ("end", "end"),
            ("SessionEnd", "end"),
            ("step", "step"),
            ("AfterTool", "step"),
        ] {
            assert_eq!(parse_event(n), Some(k), "{n}");
        }
        for stale in ["tool", "BeforeTool"] {
            assert_eq!(parse_event(stale), None, "{stale}");
        }
    }

    #[test]
    fn reads_gemini_hooks() {
        let env = BTreeMap::new();
        let p = hook(
            "stop",
            &json!({"session_id": "g1", "cwd": "/w", "hook_event_name": "AfterAgent"}),
            &env,
        );
        assert_eq!(p.kind.as_deref(), Some("stop"));
        assert_eq!(p.session_id.as_deref(), Some("g1"));
        assert_eq!(p.cwd.as_deref(), Some("/w"));
        assert!(p.key.is_none() && p.inbox.is_none());
        // The event can also come from stdin alone.
        assert_eq!(
            hook("auto", &json!({"hook_event_name": "BeforeAgent"}), &env)
                .kind
                .as_deref(),
            Some("prompt")
        );
        assert!(hook("BeforeTool", &json!({}), &env).kind.is_none());
        assert!(
            hook("stop", &json!({"cursor_version": "2.6.0"}), &env)
                .kind
                .is_none()
        );
    }

    #[test]
    fn renders_what_the_hook_prints() {
        for kind in ["stop", "prompt", "end", "start", "step"] {
            assert_eq!(render(kind, None, None), "", "{kind}");
        }
        assert_eq!(render("stop", Some("m".into()), None), "");
        assert_eq!(
            render("prompt", Some("m".into()), None),
            r#"{"hookSpecificOutput":{"hookEventName":"BeforeAgent","additionalContext":"m"}}"#
        );
        assert_eq!(
            render("step", Some("m".into()), None),
            r#"{"hookSpecificOutput":{"hookEventName":"AfterTool","additionalContext":"m"}}"#
        );
        assert_eq!(
            render("start", Some("m".into()), Some("hi".into())),
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"hi\n\nm"}}"#
        );
    }

    #[test]
    fn settings_follow_gemini() {
        let req = |dir: Option<&str>, env: Option<&str>| InstallRequest {
            config_dir: dir.map(str::to_string),
            home: "/Users/p".into(),
            env: env
                .map(|e| BTreeMap::from([("GEMINI_CLI_HOME".to_string(), e.to_string())]))
                .unwrap_or_default(),
            ..InstallRequest::default()
        };
        assert_eq!(
            settings_path(&req(None, None)),
            "/Users/p/.gemini/settings.json"
        );
        assert_eq!(
            settings_path(&req(None, Some("/g/"))),
            "/g/.gemini/settings.json"
        );
        assert_eq!(
            settings_path(&req(None, Some(""))),
            "/Users/p/.gemini/settings.json"
        );
        assert_eq!(
            settings_path(&req(Some("/tmp/c/"), Some("/g"))),
            "/tmp/c/settings.json"
        );
    }
}
