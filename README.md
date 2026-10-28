# teletap extensions

Extensions for [teletap](https://usetandem.com). An extension is a provider: the adapter
that connects one agent to teletap, through the host API only. The design is
`docs/extensions.md` in the mesh repo (decisions E1–E27).

| Path | What |
|---|---|
| `sdk/rust` | `teletap-ext`, the Rust SDK for providers: host API 1.3 types and calls, over Extism |
| `providers/claude-code` | `teletap/claude-code`: connects Claude Code to teletap (hooks, the `teletap` MCP server, permissions) |
| `providers/codex` | `teletap/codex`: Codex (hooks and their trust, the `teletap` MCP server, in `config.toml`) |
| `providers/gemini` | `teletap/gemini`: Gemini CLI (hooks and the `teletap` MCP server, in `settings.json`) |
| `providers/cursor` | `teletap/cursor`: Cursor (hooks, the `teletap` MCP server, CLI permissions) |
| `providers/pi` | `teletap/pi`: Pi (a TypeScript extension) |
| `providers/hermes` | `teletap/hermes`: Hermes (a Python plugin, enabled with `hermes plugins`) |
| `scripts/build.sh` | builds a provider to WASM and packs it into a `.teletapext` |
| `scripts/registry.sh` | builds them all and publishes them into `dist/registry`, the folder lookup serves at `/dl/extensions/` |

An extension is a WebAssembly module exporting `handle`, which gets one event (JSON) and
answers JSON. It reaches teletap through one import, `teletap_call`, behind the permissions
in its `manifest.toml`, and gets the process door: teletapd reads and writes the agent
config files it declares, and runs the commands it declares, for it. Claude Code, Codex and
Pi also answer `launch` (host API 1.3): a cloud agent's run (`teletap run --token`) gets
teletap's tools as arguments, with files only in the run's folder (`{run_dir}/` in the
door) and nothing in `~`.

```sh
scripts/build.sh claude-code                  # dist/claude-code-<version>.teletapext
teletap ext install dist/claude-code-*.teletapext
teletap install claude                           # through the extension

scripts/registry.sh                           # dist/registry: index.json + packages
TELETAP_EXT_INDEX=$PWD/dist/registry/index.json teletap up   # or the lookup server's registry:
teletap ext install teletap/codex                   #   by id, the newest version
teletap install                                  #   every agent found, providers on demand
```

`teletap/` extensions only load signed by teletap (`scripts/build.sh` signs with
`TELETAP_SIGN_KEY`; debug builds of teletap also trust `TELETAP_EXT_DEV_KEY`).

## Support a new agent

Write a provider for it with the SDK; `providers/*` are the examples. If the agent loads
Agent Skills, list where in `[provider]` `skill` (first path that expands wins, each ending
`/skills/teletap/SKILL.md`): teletapd writes teletap's skill there and removes it on
uninstall. Build and try it:

```sh
scripts/build.sh <name>                       # dist/<name>-<version>.teletapext
teletap ext install ./dist/<name>-<version>.teletapext
teletap install <agent>                       # shows as not reviewed
```

Then open a PR here. The teletap team reviews it by hand; once it is merged, they run
teletap's Extensions workflow, which builds what the registry doesn't have yet (an id at
a new version), signs it and publishes it, and `teletap install` finds the new agent. A
published version never changes: bump `version` in `manifest.toml` and `Cargo.toml`.

Licence: Apache-2.0.
