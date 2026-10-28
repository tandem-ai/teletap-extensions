//! The Rust SDK for teletap providers, the extensions that connect one agent
//! to teletap: the events a provider's `handle` export gets, and the host API
//! calls it makes through `teletap_call` (host API 1.3, `docs/extensions.md`
//! in the mesh repo). `providers/*` are the examples.
//!
//! ```ignore
//! use extism_pdk::*;
//!
//! #[plugin_fn]
//! pub fn handle(input: String) -> FnResult<String> {
//!     match teletap_ext::Event::parse(&input)? {
//!         teletap_ext::Event::Install(req) => { /* … */ }
//!         _ => {}
//!     }
//!     Ok(String::new())
//! }
//! ```

pub mod config;

use std::collections::BTreeMap;

use extism_pdk::host_fn;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[host_fn]
extern "ExtismHost" {
    fn teletap_call(input: String) -> String;
}

/// The host API version this SDK speaks.
pub const API: &str = "1.3";

/// What the host hands `handle`. Events this SDK doesn't know are
/// [`Event::Other`]: ignore them.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Install(InstallRequest),
    Uninstall(InstallRequest),
    /// 1.3: the agent runs once, in a cloud agent's run (`teletap
    /// run --token`), with teletap's tools and nothing written outside `dir`
    /// (the door's `{run_dir}/`, its only paths then; no commands). Answer
    /// a [`LaunchResult`].
    Launch(LaunchRequest),
    Hook {
        name: String,
        payload: Value,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    HookRender {
        kind: String,
        handover: Option<String>,
        intro: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl Event {
    pub fn parse(input: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(input)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct InstallRequest {
    pub config_dir: Option<String>,
    pub teletap_bin: String,
    pub teletap_home: Option<String>,
    pub home: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LaunchRequest {
    /// The run's folder for this agent, kept between runs of the same
    /// conversation.
    pub dir: String,
    pub teletap_bin: String,
    /// The run's device folder (`TELETAP_HOME`).
    pub teletap_home: String,
}

impl LaunchRequest {
    /// As an install into `dir`, for the config helpers.
    pub fn as_install(&self) -> InstallRequest {
        InstallRequest {
            config_dir: Some(self.dir.clone()),
            teletap_bin: self.teletap_bin.clone(),
            teletap_home: Some(self.teletap_home.clone()),
            home: String::new(),
            env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LaunchResult {
    /// Put before the person's arguments.
    pub args: Vec<String>,
    /// Why the agent can't have teletap's tools this run: it runs without.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct InstallResult {
    pub files: Vec<String>,
    pub notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HookParsed {
    /// `start`, `prompt`, `step`, `stop`, `idle`, `blocked`, `end`; None:
    /// not a hook teletap acts on.
    pub kind: Option<String>,
    pub session_id: Option<String>,
    pub key: Option<String>,
    pub cwd: Option<String>,
    pub inbox: Option<Inbox>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inbox {
    pub socket: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HookRendered {
    pub stdout: String,
}

/// A host API call that failed: `code` is teletap's error code (`refused`, …).
#[derive(Debug, Clone)]
pub struct HostError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for HostError {}

/// Calls a host API method.
pub fn call<R: DeserializeOwned>(method: &str, params: Value) -> Result<R, HostError> {
    let fail = |message: String| HostError {
        code: "internal".into(),
        message,
    };
    let input = json!({ "method": method, "params": params }).to_string();
    let out = unsafe { teletap_call(input) }.map_err(|e| fail(e.to_string()))?;
    let reply: Value = serde_json::from_str(&out).map_err(|e| fail(e.to_string()))?;
    if let Some(e) = reply.get("error") {
        return Err(HostError {
            code: e["code"].as_str().unwrap_or("internal").into(),
            message: e["message"].as_str().unwrap_or("").into(),
        });
    }
    serde_json::from_value(reply.get("result").cloned().unwrap_or(Value::Null))
        .map_err(|e| fail(e.to_string()))
}

/// A line in teletapd's log.
pub fn log(text: &str) {
    let _: Result<Value, _> = call("log", json!({ "text": text }));
}

/// The process door (providers, during an install): only the files and
/// commands in the manifest's `[permissions.door]`.
pub mod door {
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::{HostError, call};

    /// A file's text, None when it doesn't exist.
    pub fn read(path: &str) -> Result<Option<String>, HostError> {
        let r: Value = call("door_read", json!({ "path": path }))?;
        Ok(r["text"].as_str().map(str::to_string))
    }

    /// Writes `text` (atomically, through links, keeping the mode, after a
    /// backup) when it differs. True when it wrote.
    pub fn write(path: &str, text: &str) -> Result<bool, HostError> {
        let r: Value = call("door_write", json!({ "path": path, "text": text }))?;
        Ok(r["changed"].as_bool().unwrap_or(false))
    }

    /// The command on the installing shell's `PATH`, if there.
    pub fn which(name: &str) -> Result<Option<String>, HostError> {
        let r: Value = call("door_which", json!({ "name": name }))?;
        Ok(r["path"].as_str().map(str::to_string))
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct Output {
        pub code: Option<i32>,
        pub stdout: String,
        pub stderr: String,
    }

    /// The real path of a declared file, its folder made first.
    pub fn realpath(path: &str) -> Result<String, HostError> {
        let r: Value = call("door_realpath", json!({ "path": path }))?;
        Ok(r["path"].as_str().unwrap_or(path).to_string())
    }

    /// Removes a declared file or folder. True when there was one.
    pub fn remove(path: &str) -> Result<bool, HostError> {
        let r: Value = call("door_remove", json!({ "path": path }))?;
        Ok(r["removed"].as_bool().unwrap_or(false))
    }

    /// Runs a declared command (`argv[0]` is its name, found on `PATH`).
    pub fn exec(argv: &[&str]) -> Result<Output, HostError> {
        call("door_exec", json!({ "argv": argv }))
    }
}
