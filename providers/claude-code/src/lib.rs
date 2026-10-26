//! `teletap/claude-code`: Claude Code on teletap, as a provider extension. It used
//! to be `teletap-agents/src/install/claude.rs` and the Claude parts of
//! `hooks.rs` in teletap itself; it writes the same files, byte for byte.
//!
//! - install: hooks and permissions in `<config dir>/settings.json`, the MCP
//!   servers `teletap` and `teletap-ext` (extension tools) in the user-scope
//!   `mcpServers` of `.claude.json` (through `claude mcp …` when no config
//!   dir is given and `claude` is on PATH: a running Claude rewrites that
//!   file all the time). Only `teletap`'s tools are allowed without asking;
//!   extension tools are asked about on first use;
//! - launch (a cloud agent's run): the same servers in the run's
//!   `mcp.json`, given with `--mcp-config`, and `teletap`'s tools allowed with
//!   `--allowedTools`; nothing in `~/.claude`. Both flags take several
//!   values, so each is passed as `--flag=value`: the person's prompt that
//!   follows stays a prompt;
//! - hooks: which session event each Claude hook is, and what it prints.
//!
//! Docs: <https://code.claude.com/docs/en/hooks>, <https://code.claude.com/docs/en/mcp>.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code, unused_imports))]

use std::collections::BTreeMap;

use anyhow::{Result, bail};
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde_json::{Value, json};
#[cfg(target_arch = "wasm32")]
use teletap_ext::Event;
use teletap_ext::{
    HookParsed, HookRendered, Inbox, InstallRequest, InstallResult, LaunchRequest, LaunchResult,
    config as json, door,
};

/// (Claude event, our event name, timeout in seconds, matcher).
///
/// `Stop` doesn't run when the user interrupts a turn; the `idle_prompt`
/// notification (the prompt sat idle for a while) tells teletapd it is idle again.
const EVENTS: &[(&str, &str, u64, Option<&str>)] = &[
    ("SessionStart", "start", 10, None),
    ("UserPromptSubmit", "prompt", 10, None),
    ("Stop", "stop", 10, None),
    ("Notification", "idle", 5, Some("idle_prompt")),
    // Prints nothing (the person decides); tells teletapd not to type in now.
    ("PermissionRequest", "blocked", 5, None),
    ("SessionEnd", "end", 5, None),
];

/// What a session may run without asking: the agent-facing commands only and
/// teletap's MCP tools (not `mcp__teletap-ext__*`: extension tools are asked about).
const PERMISSIONS: &[&str] = &[
    "Bash(teletap send:*)",
    "Bash(teletap inbox:*)",
    "Bash(teletap who:*)",
    "Bash(teletap show:*)",
    "Bash(teletap wait:*)",
    "Bash(teletap sessions:*)",
    "mcp__teletap__*",
];

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

struct Files {
    settings: String,
    claude_json: String,
}

fn files(req: &InstallRequest) -> Files {
    let home = req.home.trim_end_matches('/');
    let default_dir = format!("{home}/.claude");
    let env = req
        .env
        .get("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| v.trim_end_matches('/').to_string());
    let dir = req
        .config_dir
        .as_deref()
        .map(|d| d.trim_end_matches('/').to_string())
        .or_else(|| env.clone())
        .unwrap_or_else(|| default_dir.clone());
    // `.claude.json` sits in the config dir when one is set, else in $HOME.
    let claude_json = if env.is_none() && dir == default_dir {
        format!("{home}/.claude.json")
    } else {
        format!("{dir}/.claude.json")
    };
    Files {
        settings: format!("{dir}/settings.json"),
        claude_json,
    }
}

/// How the user-scope MCP server gets into `.claude.json`: the file itself
/// (an explicit config dir, or no `claude` on PATH), or Claude's CLI.
fn via_cli(req: &InstallRequest) -> Result<bool> {
    Ok(req.config_dir.is_none() && door::which("claude")?.is_some())
}

fn run_claude(args: &[&str]) -> Result<()> {
    let mut argv = vec!["claude"];
    argv.extend_from_slice(args);
    let out = door::exec(&argv)?;
    if out.code != Some(0) {
        bail!(
            "`claude {}` failed ({:?}): {}{}",
            args[..args.len().min(4)].join(" "),
            out.code,
            out.stderr.trim(),
            out.stdout.trim(),
        );
    }
    Ok(())
}

fn install(req: &InstallRequest) -> Result<InstallResult> {
    let f = files(req);
    let mut report = InstallResult::default();

    let mut settings = json::read(&f.settings)?;
    {
        let hooks = json::object_at(&mut settings, "hooks")?;
        for (event, ours, timeout, matcher) in EVENTS {
            let handler = json!({
                "type": "command",
                "command": json::hook_command(req, "claude", ours),
                "timeout": timeout,
            });
            json::set_hook(hooks, event, "claude", handler, *matcher)?;
        }
    }
    {
        let perms = json::object_at(&mut settings, "permissions")?;
        let Some(allow) = perms
            .entry("allow")
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
        else {
            bail!("permissions.allow is not an array");
        };
        for p in PERMISSIONS {
            if !allow.iter().any(|v| v == p) {
                allow.push(Value::String(p.to_string()));
            }
        }
    }
    if json::write(&f.settings, &settings)? {
        report.files.push(f.settings.clone());
    }

    if via_cli(req)? {
        // Only read the file, to skip the CLI when nothing changes.
        let calls = cli_installs(json::read(&f.claude_json).ok().as_ref(), req);
        for args in &calls {
            run_claude(&args.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        if !calls.is_empty() {
            report.files.push(f.claude_json.clone());
        }
    } else {
        let mut cj = json::read(&f.claude_json)?;
        add_mcp(&mut cj, req)?;
        if json::write(&f.claude_json, &cj)? {
            report.files.push(f.claude_json.clone());
        }
    }
    report.notes.push(
        "Claude Code: hooks (SessionStart, UserPromptSubmit, Stop, Notification, \
         PermissionRequest, SessionEnd), \
         permissions and the `teletap` and `teletap-ext` MCP servers are set; new sessions \
         pick them up."
            .into(),
    );
    Ok(report)
}

/// Our MCP servers as `.claude.json` holds them.
fn mcp_entries(req: &InstallRequest) -> Vec<(&'static str, Value)> {
    json::MCP_SERVERS
        .iter()
        .map(|(name, args, _)| {
            let extra = [("type", Value::String("stdio".into()))];
            (*name, json::mcp_entry(req, args, &extra))
        })
        .collect()
}

/// A cloud agent's run: our servers in `<run>/mcp.json` (the door's
/// `{run_dir}/`), teletap's tools allowed.
fn launch(req: &LaunchRequest) -> Result<LaunchResult> {
    let (path, text) = launch_config(req);
    door::write(&path, &text)?;
    Ok(LaunchResult {
        args: launch_args(&path),
        error: None,
    })
}

fn launch_config(req: &LaunchRequest) -> (String, String) {
    let mut cfg = json!({});
    add_mcp(&mut cfg, &req.as_install()).expect("an object");
    let path = format!("{}/mcp.json", req.dir.trim_end_matches('/'));
    let text = serde_json::to_string_pretty(&cfg).expect("JSON") + "\n";
    (path, text)
}

fn launch_args(path: &str) -> Vec<String> {
    vec![
        format!("--mcp-config={path}"),
        "--allowedTools=mcp__teletap__*".into(),
    ]
}

/// Our MCP servers in `.claude.json` itself.
fn add_mcp(cj: &mut Value, req: &InstallRequest) -> Result<()> {
    let servers = json::object_at(cj, "mcpServers")?;
    for (name, entry) in mcp_entries(req) {
        servers.insert(name.into(), entry);
    }
    Ok(())
}

/// The `claude mcp …` calls that put our servers in `.claude.json` (as
/// read): none for a server already in place.
fn cli_installs(cj: Option<&Value>, req: &InstallRequest) -> Vec<Vec<String>> {
    let mut calls = vec![];
    for (name, entry) in mcp_entries(req) {
        let current = cj.and_then(|cj| cj.get("mcpServers")?.get(name));
        if current == Some(&entry) {
            continue;
        }
        if current.is_some() {
            calls.push(argv(&["mcp", "remove", "--scope", "user", name]));
        }
        let entry = entry.to_string();
        calls.push(argv(&["mcp", "add-json", "--scope", "user", name, &entry]));
    }
    calls
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn uninstall(req: &InstallRequest) -> Result<InstallResult> {
    let f = files(req);
    let mut report = InstallResult::default();

    if door::read(&f.settings)?.is_some() {
        let mut settings = json::read(&f.settings)?;
        json::remove_hooks(&mut settings, "claude");
        if let Some(perms) = settings
            .get_mut("permissions")
            .and_then(Value::as_object_mut)
        {
            if let Some(allow) = perms.get_mut("allow").and_then(Value::as_array_mut) {
                allow.retain(|v| !PERMISSIONS.iter().any(|p| v == p));
                if allow.is_empty() {
                    perms.remove("allow");
                }
            }
            if perms.is_empty() {
                settings.as_object_mut().unwrap().remove("permissions");
            }
        }
        if json::write(&f.settings, &settings)? {
            report.files.push(f.settings.clone());
        }
    }
    if door::read(&f.claude_json)?.is_some() {
        let mut cj = json::read(&f.claude_json)?;
        if via_cli(req)? {
            // Our entries only: a user's own `teletap` server stays.
            let mut removed = false;
            for (name, ..) in json::MCP_SERVERS {
                if json::remove_mcp_server(&mut cj, name) {
                    run_claude(&["mcp", "remove", "--scope", "user", name])?;
                    removed = true;
                }
            }
            if removed {
                report.files.push(f.claude_json.clone());
            }
        } else if json::remove_mcp(&mut cj) && json::write(&f.claude_json, &cj)? {
            report.files.push(f.claude_json.clone());
        }
    }
    Ok(report)
}

// ---------------------------------------------------------------- hooks

/// Our event word (as installed) or Claude's own hook name → session event.
fn parse_event(name: &str) -> Option<&'static str> {
    let n: String = name
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    Some(match n.as_str() {
        "start" | "sessionstart" => "start",
        "prompt" | "userpromptsubmit" => "prompt",
        "stop" => "stop",
        "idle" => "idle",
        "step" | "posttooluse" => "step",
        "blocked" | "permissionrequest" => "blocked",
        "end" | "sessionend" => "end",
        _ => return None,
    })
}

pub fn hook(name: &str, payload: &Value, env: &BTreeMap<String, String>) -> HookParsed {
    // Cursor also runs Claude Code's hooks, with its own payload: only
    // Cursor's own hooks speak there.
    if payload.get("cursor_version").is_some() {
        return HookParsed::default();
    }
    let named = payload.get("hook_event_name").and_then(Value::as_str);
    let Some(kind) = parse_event(name).or_else(|| named.and_then(parse_event)) else {
        return HookParsed::default();
    };
    let var = |k: &str| env.get(k).filter(|v| !v.is_empty()).cloned();
    HookParsed {
        kind: Some(kind.into()),
        session_id: payload
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        key: None,
        cwd: payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string),
        // Claude Code's own cross-session socket: teletapd posts answers there.
        inbox: var("CLAUDE_CODE_MESSAGING_SOCKET").map(|socket| Inbox {
            socket,
            token: var("CLAUDE_CODE_MESSAGING_TOKEN"),
        }),
    }
}

/// What the hook prints: the waiting messages as `additionalContext` on
/// `SessionStart`, `UserPromptSubmit` and `PostToolUse`. A turn end, an
/// idle notification or a permission request prints nothing.
pub fn render(kind: &str, handover: Option<String>, intro: Option<String>) -> String {
    match kind {
        "step" => handover.map_or_else(String::new, |t| additional_context("PostToolUse", &t)),
        "prompt" => {
            handover.map_or_else(String::new, |t| additional_context("UserPromptSubmit", &t))
        }
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

    #[test]
    fn reads_claude_hooks() {
        let env = BTreeMap::from([("CLAUDE_CODE_MESSAGING_SOCKET".to_string(), "/s".to_string())]);
        let p = hook(
            "prompt",
            &json!({"session_id": "abc", "cwd": "/w", "hook_event_name": "UserPromptSubmit"}),
            &env,
        );
        assert_eq!(p.kind.as_deref(), Some("prompt"));
        assert_eq!(p.session_id.as_deref(), Some("abc"));
        assert_eq!(p.cwd.as_deref(), Some("/w"));
        assert_eq!(p.inbox.unwrap().socket, "/s");
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
        assert_eq!(render("stop", Some("m".into()), None), "");
        assert_eq!(render("prompt", None, None), "");
        assert_eq!(
            render("prompt", Some("m".into()), None),
            r#"{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"m"}}"#
        );
        assert_eq!(
            render("start", Some("m".into()), Some("hi".into())),
            r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"hi\n\nm"}}"#
        );
    }

    fn opts() -> InstallRequest {
        InstallRequest {
            teletap_bin: "/usr/local/bin/teletap".into(),
            home: "/Users/p".into(),
            ..InstallRequest::default()
        }
    }

    #[test]
    fn a_launch_brings_the_servers_and_touches_no_config() {
        let req = LaunchRequest {
            dir: "/w/.teletap/claude/".into(),
            teletap_bin: "/usr/local/bin/teletap".into(),
            teletap_home: "/tmp/teletap-x".into(),
        };
        let (path, text) = launch_config(&req);
        assert_eq!(path, "/w/.teletap/claude/mcp.json");
        let cfg: Value = serde_json::from_str(&text).unwrap();
        let env = json!({"TELETAP_HOME": "/tmp/teletap-x"});
        assert_eq!(
            cfg,
            json!({"mcpServers": {
                "teletap": {"type": "stdio", "command": "/usr/local/bin/teletap", "args": ["mcp"], "env": env},
                "teletap-ext": {"type": "stdio", "command": "/usr/local/bin/teletap", "args": ["mcp", "--ext"], "env": env},
            }})
        );
        // One value each: a prompt after them isn't read as a config file.
        assert_eq!(
            launch_args(&path),
            [
                "--mcp-config=/w/.teletap/claude/mcp.json",
                "--allowedTools=mcp__teletap__*"
            ]
        );
        assert!(
            PERMISSIONS.contains(&"mcp__teletap__*"),
            "the install's own"
        );
    }

    #[test]
    fn mcp_servers_in_the_file() {
        let original = json!({"numStartups": 3, "mcpServers": {"gh": {"command": "gh-mcp"}}});
        let mut cj = original.clone();
        add_mcp(&mut cj, &opts()).unwrap();
        assert_eq!(
            cj["mcpServers"]["teletap"],
            json!({"type": "stdio", "command": "/usr/local/bin/teletap", "args": ["mcp"]})
        );
        assert_eq!(
            cj["mcpServers"]["teletap-ext"],
            json!({"type": "stdio", "command": "/usr/local/bin/teletap", "args": ["mcp", "--ext"]})
        );
        let once = cj.clone();
        add_mcp(&mut cj, &opts()).unwrap();
        assert_eq!(cj, once, "idempotent");
        assert!(json::remove_mcp(&mut cj));
        assert_eq!(cj, original);
    }

    #[test]
    fn mcp_servers_through_the_cli() {
        let add = |name: &str, args: &str| {
            argv(&[
                "mcp",
                "add-json",
                "--scope",
                "user",
                name,
                &format!(
                    r#"{{"type":"stdio","command":"/usr/local/bin/teletap","args":[{args}]}}"#
                ),
            ])
        };
        let remove = |name: &str| argv(&["mcp", "remove", "--scope", "user", name]);
        let r = opts();
        assert_eq!(
            cli_installs(None, &r),
            [
                add("teletap", r#""mcp""#),
                add("teletap-ext", r#""mcp","--ext""#)
            ]
        );
        let mut cj = json!({});
        add_mcp(&mut cj, &r).unwrap();
        assert!(cli_installs(Some(&cj), &r).is_empty(), "idempotent");
        // An entry for an old teletap binary is replaced.
        cj["mcpServers"]["teletap-ext"]["command"] = json!("/old/teletap");
        assert_eq!(
            cli_installs(Some(&cj), &r),
            [
                remove("teletap-ext"),
                add("teletap-ext", r#""mcp","--ext""#)
            ]
        );
    }

    #[test]
    fn extension_tools_are_asked_about() {
        assert!(PERMISSIONS.contains(&"mcp__teletap__*"));
        assert!(!PERMISSIONS.iter().any(|p| p.contains("teletap-ext")));
    }

    #[test]
    fn config_files_follow_claude() {
        let req = |dir: Option<&str>, env: Option<&str>| InstallRequest {
            config_dir: dir.map(str::to_string),
            home: "/Users/p".into(),
            env: env
                .map(|e| BTreeMap::from([("CLAUDE_CONFIG_DIR".to_string(), e.to_string())]))
                .unwrap_or_default(),
            ..InstallRequest::default()
        };
        let f = files(&req(None, None));
        assert_eq!(
            (f.settings.as_str(), f.claude_json.as_str()),
            ("/Users/p/.claude/settings.json", "/Users/p/.claude.json")
        );
        let f = files(&req(Some("/tmp/c/"), None));
        assert_eq!(
            (f.settings.as_str(), f.claude_json.as_str()),
            ("/tmp/c/settings.json", "/tmp/c/.claude.json")
        );
        let f = files(&req(None, Some("/x")));
        assert_eq!(f.claude_json, "/x/.claude.json");
    }
}
