# mblink

mblink is a personal phone ↔ Linux link: a Rust daemon (`mblinkd`) with a control CLI (`mblink`) on Linux and (planned) a Kotlin/Compose Android app, designed as a modular platform that stays paired over the LAN and mirrors useful phone state on the desktop. The design is described in [`docs/superpowers/specs/2026-11-08-mblink-design.md`](docs/superpowers/specs/2026-11-08-mblink-design.md).

This repository currently contains the foundation (control plane, config, module registry). LAN link/pairing, a TUI, the notification-sync module behavior, and the Android app are not implemented yet and arrive in later plans.

## Build

```sh
cargo build --workspace
cargo xtask check-modules
```

`cargo xtask check-modules` verifies that [`modules.toml`](modules.toml) stays consistent with the daemon's Cargo features and default features.

## Run

`mblinkd` is a headless daemon that never self-daemonizes; whichever process spawns it owns supervision. In the intended setup it is started from a compositor (for example niri) with an explicit log file, since compositor child output is otherwise lost:

```kdl
# niri config.kdl
spawn-at-startup "sh" "-c" "exec mblinkd --log-file \"$HOME/.local/state/mblinkd/mblinkd.log\""
```

`--listen` is accepted but the LAN link is not implemented yet (the daemon logs that and continues serving the local control socket).

### `mblinkd` flags

| Flag | Default | Description |
|---|---|---|
| `--config-dir <DIR>` | `~/.config/mblink` | Configuration root (`<module>/config.toml`). |
| `--state-dir <DIR>` | `~/.local/state/mblinkd` | Runtime state root (e.g. pinned keys/peer info, when implemented). |
| `--socket <PATH>` | `$XDG_RUNTIME_DIR/mblink.sock` | Unix domain socket for the local gRPC control channel. |
| `--listen <ADDR>` | *(unset)* | gRPC LAN bind address (accepted; LAN link not implemented yet). |
| `--log-file <PATH>` | *(unset → stderr)* | Write logs to a file instead of stderr. |
| `--log-format <FORMAT>` | `text` | Log format: `text` or `json`. |
| `-v` | *(repeatable)* | Increase log verbosity (info → debug → trace). `RUST_LOG` is respected for per-crate targeting. |
| `--check` | off | Dry run: load configs under the config root and report parse errors, then exit. |
| `--version` | off | Print the daemon version and the list of **compiled** modules, then exit. |
| `-h`, `--help` | | Print help. |

With `modules.toml`'s `notisync = false`, `--version` does not list `notisync`, while the control-plane module list still names it as `not compiled`.

## CLI

`mblink` talks to `mblinkd` over the Unix domain socket control channel. All commands accept `--socket <SOCKET>` to override the socket path.

```sh
mblink status
mblink modules
mblink config get notisync
mblink config set notisync com.google.android.gm on
mblink --socket /run/user/1000/mblink.sock status
```

`config set` takes a state of `on` or `off`.

## Layout

Mirroring the repo layout described in the spec (items arrive across plans; `android/` is a later plan):

```text
mblink/
├── modules.toml                    # single source of truth for module + service flags
├── Cargo.toml                      # Rust workspace
├── proto/                          # shared protobuf schemas
│   └── mblink/link/v1/link.proto   # session/control/config (notisync.proto arrives with its module)
├── daemon/                         # Rust workspace members
│   ├── crates/mblink-core/         # config, control service, dirs, watchers, shared core
│   ├── crates/mblink-proto/        # generated protobuf/gRPC bindings
│   ├── crates/mblinkd/             # daemon binary
│   ├── crates/mblink/              # CLI binary
│   └── xtask/                      # reads modules.toml; feature/service generation, checks
└── android/                        # (later plan) Kotlin + Compose app
```

Runtime/config paths (defaults): `~/.config/mblink/` for config and `$XDG_RUNTIME_DIR/mblink.sock` / `~/.local/state/mblinkd/` for runtime state.
