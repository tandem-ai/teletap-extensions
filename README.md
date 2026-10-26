# teletap extensions

Extensions for [teletap](https://usetandem.com): what the community and the teletap team
build on top of it, through the host API only. The design is `docs/extensions.md` in
the teletap repo (decisions E1–E27).

| Path | What |
|---|---|
| `sdk/rust` | `teletap-ext`, the Rust SDK: host API 1.3 types and calls, over Extism |
| `providers/claude-code` | `teletap/claude-code`: connects Claude Code to teletap (hooks, the `teletap` and `teletap-ext` MCP servers, permissions) |
| `providers/codex` | `teletap/codex`: Codex (hooks and their trust, the `teletap` and `teletap-ext` MCP servers, in `config.toml`) |
| `providers/gemini` | `teletap/gemini`: Gemini CLI (hooks and the `teletap` and `teletap-ext` MCP servers, in `settings.json`) |
| `providers/cursor` | `teletap/cursor`: Cursor (hooks, the `teletap` and `teletap-ext` MCP servers, CLI permissions) |
| `providers/pi` | `teletap/pi`: Pi (a TypeScript extension) |
| `providers/hermes` | `teletap/hermes`: Hermes (a Python plugin, enabled with `hermes plugins`) |
| `extensions/pulse` | `teletap/pulse`: an example org extension (a server part with cron and a webhook, the store, an agent tool) |
| `scripts/build.sh` | builds an extension to WASM and packs it into a `.teletapext` |
| `scripts/registry.sh` | builds them all and publishes them into `dist/registry`, the folder lookup serves at `/dl/extensions/` |

An extension is a WebAssembly module exporting `handle`, which gets one event (JSON) and
answers JSON. It reaches teletap through one import, `teletap_call`, behind the permissions in
its `manifest.toml`. A provider (`kind = "provider"`) also gets the process door: teletapd
reads and writes the agent config files it declares, and runs the commands it declares,
for it. Claude Code, Codex and Pi also answer `launch` (host API 1.3): a cloud agent's
run (`teletap run --token`) gets teletap's tools as arguments, with files only in the run's
folder (`{run_dir}/` in the door) and nothing in `~`.

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

Getting an extension on teletap's official list: open a PR here. The teletap team reviews it by
hand; once it is on `main`, it runs teletap's Extensions workflow, which builds
what the registry doesn't have yet (an id at a new version), signs it and publishes it. A
published version never changes: bump `version` in `manifest.toml` and `Cargo.toml`.

Licence: Apache-2.0.
