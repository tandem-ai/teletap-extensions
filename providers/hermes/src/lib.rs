//! `teletap/hermes`: Hermes Agent on teletap, as a provider extension. It used to
//! be `teletap-agents/src/install/hermes.rs` and Hermes's part of `hooks.rs`
//! (the "unknown agent" path) in teletap itself; it writes the same files, byte
//! for byte.
//!
//! - install: a plugin in `<hermes home>/plugins/teletap/` (`HERMES_HOME`, else
//!   `~/.hermes`). Hermes loads a user plugin only once it is in
//!   `plugins.enabled`, so install runs `hermes plugins enable teletap` when it
//!   can. The plugin serves the session's inbox and runs
//!   `teletap hook hermes <event>` itself (see `hermes-plugin.py`);
//! - hooks: which session event each call is, and what it prints (raw text).
//!
//! Docs: <https://hermes-agent.nousresearch.com/docs/user-guide/features/plugins>

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde_json::Value;
#[cfg(target_arch = "wasm32")]
use teletap_ext::Event;
use teletap_ext::{HookParsed, HookRendered, InstallRequest, InstallResult, door};

const PLUGIN: &str = include_str!("hermes-plugin.py");
const MANIFEST: &str = "name: teletap\nversion: \"1.0\"\ndescription: Answers from your team come back into this session\n";
/// What marks the plugin as ours.
const MARK: &str = "Written by `teletap install hermes`";

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

fn plugin_dir(req: &InstallRequest) -> String {
    let env = req
        .env
        .get("HERMES_HOME")
        .filter(|v| !v.is_empty())
        .cloned();
    let home = req
        .config_dir
        .clone()
        .or(env)
        .map(|d| d.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("{}/.hermes", req.home.trim_end_matches('/')));
    format!("{home}/plugins/teletap")
}

/// The plugin with this install's `teletap` binary and `TELETAP_HOME`.
fn plugin(req: &InstallRequest) -> String {
    let lit = |s: &str| serde_json::to_string(s).unwrap();
    PLUGIN
        .replace("\"__TELETAP_BIN__\"", &lit(&req.teletap_bin))
        .replace(
            "\"__TELETAP_HOME__\"",
            &lit(req.teletap_home.as_deref().unwrap_or_default()),
        )
}

/// Runs `hermes <args>` against the user's own Hermes (not with a config
/// dir). Not on PATH, refused or failed: false.
fn hermes(req: &InstallRequest, args: &[&str]) -> bool {
    let mut argv = vec!["hermes"];
    argv.extend_from_slice(args);
    req.config_dir.is_none() && door::exec(&argv).is_ok_and(|o| o.code == Some(0))
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let dir = plugin_dir(req);
    let mut report = InstallResult::default();
    for (name, text) in [
        ("plugin.yaml", MANIFEST.to_string()),
        ("__init__.py", plugin(req)),
    ] {
        let path = format!("{dir}/{name}");
        if door::write(&path, &text)? {
            report.files.push(path);
        }
    }
    // Only when the plugin is new or changed: a user who turned it off in
    // Hermes keeps it off until teletap itself changes it.
    if report.files.is_empty() {
        return Ok(report);
    }
    // Hermes loads a user plugin once it's enabled; its TUI and desktop app
    // also need the plugin allowed to start turns.
    let enabled = hermes(req, &["plugins", "enable", "teletap"])
        && hermes(
            req,
            &[
                "config",
                "set",
                "plugins.entries.teletap.allow_gateway_injection",
                "true",
            ],
        );
    report.notes.push(if enabled {
        "Hermes: the teletap plugin is in place and enabled; new Hermes sessions load it.".into()
    } else {
        "Hermes: the teletap plugin is in place; turn it on with `hermes plugins enable teletap` and \
         `hermes config set plugins.entries.teletap.allow_gateway_injection true`."
            .into()
    });
    Ok(report)
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let dir = plugin_dir(req);
    let mut report = InstallResult::default();
    // Only a plugin we wrote.
    if door::read(&format!("{dir}/__init__.py"))?.is_some_and(|t| t.contains(MARK)) {
        door::remove(&dir)?;
        report.files.push(dir);
        hermes(req, &["plugins", "disable", "teletap"]);
    }
    Ok(report)
}

// ---------------------------------------------------------------- hooks

/// The event word (`teletap hook hermes <event>`) → session event. The plugin
/// sends our short names; the rest is the table teletap's own hook took.
fn parse_event(name: &str) -> Option<&'static str> {
    let n: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match n.as_str() {
        "start" | "sessionstart" => "start",
        "prompt" | "userpromptsubmit" | "beforeagent" => "prompt",
        "stop" | "afteragent" => "stop",
        "idle" | "interrupt" => "idle",
        "step" | "posttooluse" | "aftertool" => "step",
        "blocked" | "permissionrequest" => "blocked",
        "end" | "sessionend" => "end",
        _ => return None,
    })
}

/// The plugin's hook call: `{hook_event_name}` on stdin. Its inbox
/// (`TELETAP_AGENT_INBOX`) reaches teletapd from the teletap CLI, not from here.
pub fn hook(name: &str, payload: &Value, _env: &BTreeMap<String, String>) -> HookParsed {
    // Cursor runs other agents' hooks with its own payload: only Cursor's
    // own hooks speak there.
    if payload.get("cursor_version").is_some() {
        return HookParsed::default();
    }
    let named = payload.get("hook_event_name").and_then(Value::as_str);
    let Some(kind) = parse_event(name).or_else(|| named.and_then(parse_event)) else {
        return HookParsed::default();
    };
    let text = |k: &str| payload.get(k).and_then(Value::as_str).map(str::to_string);
    HookParsed {
        kind: Some(kind.into()),
        session_id: text("session_id")
            .or_else(|| text("thread_id"))
            .or_else(|| text("conversation_id")),
        key: None,
        cwd: text("cwd"),
        inbox: None,
    }
}

/// What the hook prints: the waiting messages as plain text on `prompt`, the
/// intro and them on `start`; nothing otherwise.
pub fn render(kind: &str, handover: Option<String>, intro: Option<String>) -> String {
    match kind {
        "prompt" => handover.unwrap_or_default(),
        "start" => intro
            .into_iter()
            .chain(handover)
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(dir: Option<&str>, env: Option<&str>) -> InstallRequest {
        InstallRequest {
            config_dir: dir.map(str::to_string),
            teletap_bin: "/Users/p/.local/bin/teletap".into(),
            home: "/Users/p".into(),
            env: env
                .map(|e| BTreeMap::from([("HERMES_HOME".to_string(), e.to_string())]))
                .unwrap_or_default(),
            ..InstallRequest::default()
        }
    }

    #[test]
    fn plugin_dir_follows_hermes() {
        assert_eq!(
            plugin_dir(&req(None, None)),
            "/Users/p/.hermes/plugins/teletap"
        );
        assert_eq!(plugin_dir(&req(None, Some("/h/"))), "/h/plugins/teletap");
        assert_eq!(
            plugin_dir(&req(None, Some(""))),
            "/Users/p/.hermes/plugins/teletap"
        );
        assert_eq!(
            plugin_dir(&req(Some("/tmp/hermes"), Some("/h"))),
            "/tmp/hermes/plugins/teletap"
        );
    }

    #[test]
    fn plugin_fills_the_placeholders() {
        let mut r = req(None, None);
        r.teletap_home = Some("/tmp/my teletap".into());
        let code = plugin(&r);
        assert!(!code.contains("__TELETAP_"), "placeholders filled");
        assert!(!code.contains("pre_tool_call"), "no pre-tool hook");
        assert!(code.contains(MARK));
        assert!(code.contains(r#"TELETAP = "/Users/p/.local/bin/teletap""#));
        assert!(code.contains(r#"TELETAP_HOME_PIN = "/tmp/my teletap""#));
        assert!(plugin(&req(None, None)).contains(r#"TELETAP_HOME_PIN = """#));
        assert!(MANIFEST.contains("name: teletap"));
    }

    #[test]
    fn reads_hermes_hooks() {
        let env = BTreeMap::new();
        for (word, kind) in [
            ("start", "start"),
            ("prompt", "prompt"),
            ("stop", "stop"),
            ("blocked", "blocked"),
            ("step", "step"),
            ("end", "end"),
        ] {
            let p = hook(word, &json!({"hook_event_name": word}), &env);
            assert_eq!(p.kind.as_deref(), Some(kind), "{word}");
            assert!(p.session_id.is_none() && p.key.is_none() && p.cwd.is_none());
            assert!(p.inbox.is_none());
        }
        let p = hook("stop", &json!({"conversation_id": "c", "cwd": "/w"}), &env);
        assert_eq!(p.session_id.as_deref(), Some("c"));
        assert_eq!(p.cwd.as_deref(), Some("/w"));
        assert!(hook("tool", &json!({}), &env).kind.is_none());
        assert!(
            hook("stop", &json!({"cursor_version": "1"}), &env)
                .kind
                .is_none()
        );
        assert_eq!(
            hook("x", &json!({"hook_event_name": "SessionEnd"}), &env)
                .kind
                .as_deref(),
            Some("end")
        );
    }

    #[test]
    fn renders_what_the_hook_prints() {
        for kind in ["stop", "step", "idle", "blocked", "end"] {
            assert_eq!(
                render(kind, Some("m".into()), Some("hi".into())),
                "",
                "{kind}"
            );
        }
        for kind in ["prompt", "start", "stop", "step", "end", "idle", "blocked"] {
            assert_eq!(render(kind, None, None), "", "{kind}");
        }
        assert_eq!(render("prompt", Some("m".into()), None), "m");
        assert_eq!(
            render("start", Some("m".into()), Some("hi".into())),
            "hi\n\nm"
        );
        assert_eq!(render("start", None, Some("hi".into())), "hi");
    }
}
