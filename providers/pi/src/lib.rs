//! `teletap/pi`: Pi on teletap, as a provider extension. It used to be
//! `teletap-agents/src/install/pi.rs` and Pi's part of `hooks.rs` (the
//! "unknown agent" path) in teletap itself; it writes the same file, byte for
//! byte.
//!
//! - install: an extension in `<agent dir>/extensions/teletap.ts`
//!   (`PI_CODING_AGENT_DIR`, else `~/.pi/agent`). Pi loads TypeScript
//!   extensions from there at start. Pi has no hooks of its own to
//!   configure; the extension serves the session's inbox and runs
//!   `teletap hook pi <event>` itself (see `pi-extension.ts`);
//! - launch (a cloud agent's run): the same extension in the run's folder,
//!   given to Pi with `--extension`; nothing in `~/.pi`;
//! - hooks: which session event each call is, and what it prints (raw text).
//!
//! Docs: <https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md>

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::Result;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde_json::Value;
#[cfg(target_arch = "wasm32")]
use teletap_ext::Event;
use teletap_ext::{
    HookParsed, HookRendered, InstallRequest, InstallResult, LaunchRequest, LaunchResult, door,
};

const TEMPLATE: &str = include_str!("pi-extension.ts");
/// What marks the file as ours.
const MARK: &str = "// Written by `teletap install pi`";

// The export exists in the WASM build only: host-side unit tests can't link
// Extism's imports, and test the logic below directly.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle(input: String) -> FnResult<String> {
    let answer = match Event::parse(&input)? {
        Event::Install(req) => serde_json::to_value(result(install(&req)))?,
        Event::Uninstall(req) => serde_json::to_value(result(uninstall(&req)))?,
        Event::Launch(req) => {
            serde_json::to_value(launch(&req).unwrap_or_else(|e| LaunchResult {
                error: Some(format!("{e:#}")),
                ..LaunchResult::default()
            }))?
        }
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

fn extension_path(req: &InstallRequest) -> String {
    let env = req
        .env
        .get("PI_CODING_AGENT_DIR")
        .filter(|v| !v.is_empty())
        .cloned();
    let dir = req
        .config_dir
        .clone()
        .or(env)
        .map(|d| d.trim_end_matches('/').to_string())
        .unwrap_or_else(|| format!("{}/.pi/agent", req.home.trim_end_matches('/')));
    format!("{dir}/extensions/teletap.ts")
}

/// The extension with this install's `teletap` binary and `TELETAP_HOME`.
fn extension(req: &InstallRequest) -> String {
    let lit = |s: &str| serde_json::to_string(s).unwrap();
    TEMPLATE
        .replace("\"__TELETAP_BIN__\"", &lit(&req.teletap_bin))
        .replace(
            "\"__TELETAP_HOME__\"",
            &lit(req.teletap_home.as_deref().unwrap_or_default()),
        )
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let path = extension_path(req);
    let mut report = InstallResult::default();
    if door::write(&path, &extension(req))? {
        report.files.push(path);
    }
    report
        .notes
        .push("Pi: the teletap extension is in place; new Pi sessions load it.".into());
    Ok(report)
}

/// The extension in the run's folder (the door's `{run_dir}/`), loaded by
/// flag: Pi's own folder isn't touched.
fn launch(req: &LaunchRequest) -> Result<LaunchResult> {
    let install = req.as_install();
    let path = extension_path(&install);
    door::write(&path, &extension(&install))?;
    Ok(LaunchResult {
        args: launch_args(&path),
        error: None,
    })
}

fn launch_args(path: &str) -> Vec<String> {
    vec!["--extension".into(), path.into()]
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let path = extension_path(req);
    let mut report = InstallResult::default();
    // Only a file we wrote.
    if door::read(&path)?.is_some_and(|t| t.contains(MARK)) && door::remove(&path)? {
        report.files.push(path);
    }
    Ok(report)
}

// ---------------------------------------------------------------- hooks

/// The event word (`teletap hook pi <event>`) → session event. The extension
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

/// The extension's hook call: `{hook_event_name, cwd?}` on stdin. Its inbox
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
                .map(|e| BTreeMap::from([("PI_CODING_AGENT_DIR".to_string(), e.to_string())]))
                .unwrap_or_default(),
            ..InstallRequest::default()
        }
    }

    #[test]
    fn extension_path_follows_pi() {
        assert_eq!(
            extension_path(&req(None, None)),
            "/Users/p/.pi/agent/extensions/teletap.ts"
        );
        assert_eq!(
            extension_path(&req(None, Some("/x/"))),
            "/x/extensions/teletap.ts"
        );
        assert_eq!(
            extension_path(&req(None, Some(""))),
            "/Users/p/.pi/agent/extensions/teletap.ts"
        );
        assert_eq!(
            extension_path(&req(Some("/tmp/agent"), Some("/x"))),
            "/tmp/agent/extensions/teletap.ts"
        );
    }

    #[test]
    fn a_launch_loads_the_extension_from_the_run() {
        let req = LaunchRequest {
            dir: "/w/.teletap/pi".into(),
            teletap_bin: "/usr/bin/teletap".into(),
            teletap_home: "/tmp/teletap-x".into(),
        };
        let install = req.as_install();
        let path = extension_path(&install);
        assert_eq!(path, "/w/.teletap/pi/extensions/teletap.ts");
        assert_eq!(
            launch_args(&path),
            ["--extension", "/w/.teletap/pi/extensions/teletap.ts"]
        );
        let text = extension(&install);
        assert!(text.contains(r#"const TELETAP = "/usr/bin/teletap";"#));
        assert!(text.contains(r#"const TELETAP_HOME_PIN = "/tmp/teletap-x";"#));
    }

    #[test]
    fn extension_fills_the_placeholders() {
        let mut r = req(None, None);
        r.teletap_home = Some("/tmp/my teletap".into());
        let text = extension(&r);
        assert!(!text.contains("__TELETAP_"), "placeholders filled");
        assert!(!text.contains("tool_call"), "no pre-tool handler");
        assert!(text.contains(MARK));
        assert!(text.contains(r#"const TELETAP = "/Users/p/.local/bin/teletap";"#));
        assert!(text.contains(r#"const TELETAP_HOME_PIN = "/tmp/my teletap";"#));
        assert!(extension(&req(None, None)).contains(r#"const TELETAP_HOME_PIN = "";"#));
    }

    #[test]
    fn reads_pi_hooks() {
        let env = BTreeMap::new();
        let p = hook(
            "start",
            &json!({"hook_event_name": "start", "cwd": "/w"}),
            &env,
        );
        assert_eq!(p.kind.as_deref(), Some("start"));
        assert_eq!(p.cwd.as_deref(), Some("/w"));
        assert!(p.session_id.is_none() && p.key.is_none() && p.inbox.is_none());
        for (word, kind) in [
            ("prompt", "prompt"),
            ("stop", "stop"),
            ("blocked", "blocked"),
            ("step", "step"),
            ("end", "end"),
        ] {
            let p = hook(word, &json!({"hook_event_name": word}), &env);
            assert_eq!(p.kind.as_deref(), Some(kind), "{word}");
        }
        let p = hook(
            "stop",
            &json!({"thread_id": "t", "conversation_id": "c"}),
            &env,
        );
        assert_eq!(p.session_id.as_deref(), Some("t"));
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
        assert_eq!(render("start", Some("m".into()), None), "m");
    }
}
