# mblink — Design Document (2026-11-08)

## Intent

mblink is a personal phone ↔ Linux link: two applications — a Rust daemon
(`mblinkd`) with a control CLI/TUI (`mblink`) on Linux, and a Kotlin/Compose
Android app — that stay paired over the LAN and mirror useful phone state on
the Linux desktop. It is deliberately built as a **modular platform** meant to
grow new capabilities over time; the MVP ships exactly one module:
**notification state synchronization**.

The owner's deeper motivation was not stated in the brainstorming sessions;
this design assumes a personal daily-driver tool for the owner's own machines
(owner's call to correct), not a published product.

## Goals

- Phone notifications appear on Linux through the owner's custom D-Bus
  notification daemon; the notification set is **state-synchronized**, not
  replayed or queued: whatever is active on the phone at (re)connect is what
  Linux sees.
- **Dismissal works from either side**: dismiss in the Linux notification
  daemon's history → cancelled on the phone; cleared on the phone → withdrawn
  from the daemon's history.
- **One synchronized, module-scoped config** (notisync: a map of Android
  package → tick). A ticked app both *sends* from the phone and *is shown*
  on Linux — one fact, not two policies. Editable from the `mblink` TUI
  and from the Android app; changes propagate with connectivity,
  latest-write-wins per entry. No manual sync controls anywhere — the
  daemons reconcile by themselves.
- One-time **pairing** with a short code; keys pinned thereafter.
- **mDNS discovery** on the LAN.
- **Modular architecture**: modules compiled in/out via build flags that come
  from a *single source of truth* and affect both the daemon and the Android
  app.
- Daemon runs as a **user service** under systemd or OpenRC, selected at
  build time.

## Non-goals (MVP)

- Replying to notifications from Linux (feasible via RemoteInput; deferred).
- Per-notification / pattern rules — per-app ticks only.
- File transfer or any binary-streaming module.
- Replay or store-and-forward of notifications missed while disconnected.
- Bluetooth transport and off-LAN connectivity (hotspot / Wi-Fi Direct) —
  deferred; see Future directions.
- Clipboard, media control, and other modules — future work the module API
  must leave room for.

## Anti-goals

- Anything that implies granting Android's notification-listener consent
  remotely — that one-time grant can only happen in Android's system
  settings, and no UI may pretend otherwise. The ecosystem instead presents
  **one symmetric permission model**: every mblink setting, including
  module on/off, is synced config, editable from either side with the same
  effect. A missing consent grant surfaces identically in both apps as a
  setup state ("waiting for notification access") — never as a
  Linux-vs-phone permission split.
- Divergent phone-side vs Linux-side configs. The config is one shared state.
- Hand-writing wire message types per side (the reason gRPC was chosen).

## Constraints (external facts the design must respect)

- Android `NotificationListenerService`: reading notifications and
  `cancelNotification(key)` share one consent; **ongoing/foreground-service
  notifications cannot be dismissed programmatically** (read-only state on
  Linux, shown locked); some apps re-post cancelled notifications.
- Android background execution: holding a LAN session needs a foreground
  service (declared type, e.g. `connectedDevice`); Doze may suspend
  connectivity; reconnection is event-driven.
- freedesktop notifications: `Notify()` returns an id; `expire_timeout = 5000`
  for the popup; `NotificationClosed(id, reason)` distinguishes expiry
  (reason 1) from user dismissal (reason 2/3); `ActionInvoked(id, key)`
  reports clicks/actions. The owner **owns the notification daemon** and
  implements this side themselves — that work is **outside mblink's build
  scope**, happening in parallel. The full required contract — standard
  surface behaviors (replace-in-place, actions from history, hints
  preserved, popup-only expiry, locked ongoing entries) plus one custom
  extension (`WithdrawAllMblink`) — is specified in
  `docs/superpowers/specs/2026-11-08-mblink-dbus-contract.md`.
  mblinkd is built and tested against a mock of exactly that contract, so
  notisync's Linux side is verifiable before the daemon side lands.
  GNOME's stock daemon is spotty on the required signals; irrelevant to
  the owner, but worth a compat note if this is ever distributed.
- mDNS is plaintext; the LAN is not trusted. Pairing + pinned keys are
  mandatory before any session traffic.
- **Topology is strictly 1 phone ↔ 1 Linux machine** for the MVP (owner's
  decision). One pinned peer each side. If multi-machine ever comes, config
  LWW-by-wall-clock must be redesigned (per-peer versions), and the phone
  app grows a daemon picker — noted as a known future pressure, not MVP
  work.
- **Version skew is handled properly, not by a same-commit rule**
  (owner's decision): see Protocol versioning below.

## Approaches considered

Three wire-protocol designs were laid out; the owner picked the third.

1. **TCP + rustls mTLS + JSON envelopes** — boring, debuggable, proven; but
   every message type hand-written twice (serde + kotlinx). Rejected: cost
   grows with each new module, and the owner intends to add many.
2. **TCP + Noise + postcard** — lean pairing-friendly crypto; bespoke
   framing, opaque debugging, Kotlin hand-rolls everything. Rejected: buys
   little at this scale.
3. **gRPC (tonic on Rust, grpc-kotlin on Android) + protobuf, shared `.proto`
   files** — ✅ chosen. Codegen for both sides from one schema, streaming
   built in for future push-style modules. Cost accepted: protoc/HTTP/2
   toolchain complexity.

Repo structure: **monorepo** with shared proto files — owner-confirmed.

## Architecture

### Repo layout

```
mblink/
├── modules.toml                # single source of truth for module + service flags
├── proto/                      # shared schemas
│   ├── mblink/link/v1/link.proto        # session, pairing, config
│   └── mblink/notisync/v1/notisync.proto
├── daemon/                     # Rust workspace
│   ├── crates/mblink-core/     # transport, discovery, pairing, config store, module API
│   ├── crates/mblink-module-notisync/
│   ├── crates/mblinkd/         # daemon binary (core + enabled modules)
│   ├── crates/mblink/          # CLI/TUI binary (ratatui)
│   └── xtask/                  # reads modules.toml; feature/service generation, checks
└── android/                    # Gradle, Kotlin + Compose
    └── app/                    # + Gradle task reading modules.toml
```

### modules.toml — one switch, two build systems

```toml
[modules]
notisync = true               # each key ⇒ cargo feature `module-<key>`
                              #           AND Gradle flag of the same name

[service]
systemd = true                # ⇒ cargo feature `service-systemd`
openrc = false                # ⇒ cargo feature `service-openrc`
```

- `xtask` validates that daemon `Cargo.toml` features match `modules.toml`
  and sets default features; a Gradle task reads the same file into
  BuildConfig/source-set gating so module screens exist in the Android app
  only when the module is enabled. The two builds can never disagree
  silently.
- Adding a module = new proto package + new Rust crate + Android module class
  + one `modules.toml` line.

### Discovery, transport, pairing

- `mblinkd` is the **gRPC server**, listening with TLS; advertised via
  `_mblink._tcp` mDNS (TXT: instance name, protocol version). Rust uses
  `mdns-sd` (pure Rust, no Avahi dependency); Android uses `NsDManager`.
- **Pairing**: `mblink pair` generates a 6-digit code shown in the terminal;
  the phone app has a Pair screen where the code is entered. Both sides
  exchange and pin long-term public keys, then refuse any peer whose key
  isn't pinned. Unpaired connections are rejected before any session
  traffic; the pairing RPC surface is only open while an explicit
  `mblink pair` session is running — everything else is auth-gated at all
  times.
- **Session**: the phone (client) opens one bidirectional stream
  `Link(stream Upstream) returns (stream Downstream)`:
  - Upstream: `NotificationSnapshot` (full set on connect),
    `NotificationEvent` (posted/updated/removed, keyed by Android
    notification key), `ConfigUpdate`, keepalives.
  - Downstream: `ConfigUpdate`, `DismissRequest` (Android key),
    `InvokeAction` (action pass-through), keepalives.
  - Each module's message types live in its own proto package and are
    imported into the stream's oneof; the core stream stays stable as
    modules come and go.
- **Protocol versioning & graceful degradation**: the mDNS TXT record
  carries a protocol version. At session start, before any snapshot, both
  sides exchange capabilities (proto versions + enabled module list per
  side). Unknown oneof arms are ignored and logged, never fatal. A module
  present on one side but not the other (or proto-incompatible) goes idle
  for that session, with the mismatch surfaced in the TUI status screen and
  the Android app. Proto discipline: field numbers are never reused or
  changed; changes are additive only; a module's incompatible change means
  a new proto package version. This is deliberate overkill for 1:1 — it
  keeps the owner free to update the APK and daemon independently.
- Auth after pairing: TLS with the daemon's pinned cert, plus a token/key
  authenticating the phone. Exact mechanism (client-cert mTLS vs signed
  metadata token) is left to the builder; grpc-java's Android client makes
  token-over-pinned-TLS the pragmatic favorite. **Crypto of the pairing
  exchange itself** (how the 6-digit code protects the key exchange — a PAKE
  such as SPAKE2 vs a simpler transcript-encrypted handshake) needs a
  dedicated review during planning; a wrong choice here is the single
  biggest security risk in the project.
- **Unpair / re-pair lifecycle**: unpair clears **pinned keys only** —
  config is data, keys are the relationship, so re-pairing the same two
  machines merges the old ticks back via normal LWW with no special
  casing. `mblink unpair` mirrors the Android Unpair action. Pairing a
  different peer while one is pinned prompts "replace paired device?" and
  replaces the pinning.

### notisync — notification state sync (MVP module)

The phone is the **source of truth** for the active notification set; Linux
mirrors it.

- On (re)connect: phone sends a full snapshot; then incremental events.
- Daemon maintains the Android-key ↔ D-Bus-id mapping **in memory** (no
  persistence — restart reconciliation below makes it unnecessary).
- Posted/updated on phone → `Notify()` with `expire_timeout = 5000`, app hint
  = package name; updates **replace** the existing notification
  (`existing_id`), never stack duplicates.
- Popup expiry (reason 1) does nothing on the phone — history still holds it.
- Dismissal in the daemon's history (reason 2/3) → `DismissRequest` → phone
  `cancelNotification(key)`.
- Cleared on the phone (`onNotificationRemoved`) → daemon withdraws the
  history entry by id (custom-daemon capability the owner will add).
- Ongoing/foreground-service notifications: shown **locked** (read-only);
  dismiss attempts are surfaced as such, not silently dropped.
- Click on a notification (`ActionInvoked`, typically `default`) → handler
  dispatch (see Handlers).
- **Action buttons pass through to Android.** Notifications carry their
  action list over the wire — each entry is a wire-defined key (position-
  based, stable within the notification's lifetime; Android has no stable
  public action identifier) plus the button's display label — and mblinkd
  exposes them as D-Bus action buttons. Clicking one sends
  `InvokeAction(notif_key, action_key)` → the phone calls that action's
  `actionIntent.send()`, executing it as the posting app — "Like" on the
  laptop likes it in Teams on the phone, with zero handler config. Caveats: activity-launching actions may be
  blocked by Android's background-activity-start rules (service/receiver
  actions are expected to work); RemoteInput replies are future work.
  This mechanism (same one KDE Connect uses for notification replies)
  needs an **early verification spike** on the target device.

### File layout (Linux)

mblinkd is the main actor; `mblink` only controls it over the UDS channel.
All files are plain text, **daemon-owned but hand-editable** — the daemon
watches them and merges hand-edits as LWW writes timestamped by mtime:

```
~/.config/mblink/
├── config.toml                    # core (editor.mode, …)
└── notisync/
    ├── config.toml                # module config — the synced per-app map
    └── handlers/com.google.gmail.lua

~/.local/state/mblinkd/            # runtime state: pinned keys, peer info
```

### Config

- **Config is module-scoped**: each module's proto defines its own config
  schema; core keeps only pairing/peer state. notisync's config (persisted
  as `notisync/config.toml`, see File layout): map of
  `package → {enabled, updated_at, updated_by}`, LWW per entry, replicated
  both directions over the session stream. On connect both sides exchange
  full maps and merge.
- **App catalog** (owner's decision): the synced list covers apps holding
  the `POST_NOTIFICATIONS` permission **∪ apps observed posting** (the
  union catches framework/system packages like `android` that post without
  a conventional grant). The phone sends the full catalog in the connect
  snapshot, then incremental updates on install/uninstall/permission
  change and when an unknown package is first observed posting. Display
  labels ride along ("Gmail — com.google.gmail"); packages unknown to the
  config (newly installed apps) default to unticked.
- Phone filters what it *sends* by the config; daemon filters what it *shows*
  by the same config. Two enforcers, one fact.
- Changes made while disconnected take effect on the other side after the
  next sync.
- **No manual sync controls in any UI** — no force-sync/refresh buttons.
  Reconciliation is daemon-driven: full exchange on (re)connect, push on
  change, file-watch merge for hand-edits. UIs only ever *display* sync
  state (↻ in flight / · queued), never trigger it.

### Handlers (click actions)

- Keyed by full package name, inside the module's directory:
  `~/.config/mblink/notisync/handlers/com.google.gmail.lua` — **Lua only**
  (no TOML shorthand). Embedded via `mlua`, hot-reloaded via directory
  watch.
- **Contract:** the file returns one function. On a click the daemon calls
  it with the notification `n`; it returns an action, a list of actions,
  or `nil` (nothing). 1-second timeout. Load/runtime errors are logged,
  the click no-ops, and the last error surfaces in `mblink status`.
- **Sandbox rule:** no `io`/`os`/`require` — the sandbox never executes
  anything. Handlers *return* effects; the daemon executes a whitelisted
  verb set. This keeps handlers unit-testable without a daemon.
- The notification `n` (inert data, no functions): `package`, `app`
  (label), `title`, `body`, `when` (epoch seconds), `category` (Android
  category or nil), `ongoing` (bool), `action` (`"default"` for a body
  click, or the key of a clicked action button), `buttons` (map of
  key → `{title}` for the notification's action buttons), `extras`
  (reserved, `{}` for MVP — the safe subset is a later, leak-sensitive
  design). **Action keys are wire-defined** (position-based, stable within
  a notification's lifetime) because Android exposes no stable public
  action identifier — labels are display text: locale-dependent and
  fragile to match against.
- `ml` helpers: `ml.open(target)` → `xdg-open` a URL/path;
  `ml.run(prog, …)` → spawn argv directly, no shell; `ml.log(fmt, …)` →
  daemon log line. A returned table executes all actions in order.
- Interaction with action pass-through (notisync section, above): a
  handler returning non-nil for a named action **overrides** the
  pass-through; returning nil (or having no handler) lets the button
  forward to Android.
- Future helper, not MVP: `ml.push(action_key)` — explicitly forward an
  action to the phone from Lua.

### CLI/TUI (`mblink`)

- `mblink` → TUI, two screens: **home** (persistent connection header —
  phone link + `mblinkd` up + top-level config sync state: ✓ synced /
  ↻ syncing / · queued while offline — then the module list: toggle,
  name, and an optional module-provided short status line; no per-module
  sync state) and the **module screen** (module status line, then its
  config sections as collapsible lists on one page; `⏎` folds). Footer is
  hotkeys only. Breadcrumb: `mblink › notisync`.
- Module toggles and section settings are synced config — same permission
  model as the Android app (see Anti-goals).
- Apps section (notisync's only section): per-app rows; `space` toggles
  a tick (a row marker shows only while unacknowledged: ↻ in flight,
  · queued offline — no other sync UI); `⏎` (enter) opens the handler
  editor on that app's Lua file (creates a commented template if none
  exists). Editor handling: always use `$EDITOR`; terminal editors get the
  yazi-style flow (TUI suspends, editor attached to the same tty, TUI
  restores on exit); GUI editors (`code`, `subl`, …) are spawned
  **detached — they never receive the tty and mblink never waits on
  them**, so launcher behavior is irrelevant — detected by a small
  known-GUI table or by the child exiting immediately after handoff; an
  unknown editor that turns out to be GUI degrades safely: slow hand-off
  = brief blank then restore; blocking launcher = suspended until its
  window closes (Ctrl-C kills the launcher, not mblink, and the TUI
  restores); a config override (`editor.mode = auto|gui|tty`) forces the
  behavior for known-odd launchers. Either way the
  daemon hot-reloads handlers by watching the directory, so saves apply
  without any exit-sync.
- `mblink pair` → guided CLI pairing wizard (gh-login style): waits for
  the phone, shows the code, confirms, pins keys.
- `mblink status`, `mblink service install|uninstall|status`.
- Notification list screen was cut: the notification daemon's history is the
  viewer; the TUI is a control surface.
- `mblink` talks to `mblinkd` over **gRPC on a Unix domain socket**
  (`$XDG_RUNTIME_DIR/mblink.sock`, 0600, user-only) — same tonic + proto
  stack as the phone link, different service. Control API: `Pair`,
  `Status`, `GetConfig`/`SetConfig`, `ModuleList`, and `WatchStatus`
  (server streaming) for the TUI's live updates. `mblink pair` calls the
  UDS endpoint, and that is what opens the LAN pairing surface. No
  concurrent-writer problems between the two binaries: the daemon owns all
  state files (config, pairing keys, peer info).

### systemd / OpenRC

- **mblinkd must run in the user session** — it needs the session bus for
  notifications and `$XDG_RUNTIME_DIR` for the control socket. Two run
  modes, both first-class:
  - **Spawned by the compositor** — the owner's setup: niri
    `spawn-at-startup` launches `mblinkd` directly, inheriting the session
    environment. No service manager involved; the spawn line passes
    `--log-file` explicitly (compositor child output is otherwise lost).
  - **User service** — for other setups and use cases. Cargo features
    `service-systemd` / `service-openrc` select which unit template is
    compiled in; `mblink service install` writes the matching **user**
    service (`~/.config/systemd/user/` or the OpenRC user-service
    equivalent) and enables it. The exact OpenRC user-service mechanism
    (supervise-daemon in a user runlevel) is a builder research item —
    it's the murkier of the two. Either way the unit must arrange for a
    session-bus address / user session context.

### mblinkd surface (non-UI)

- Headless: no interactive mode; inspection and control are `mblink`'s
  job over the UDS channel. **Never self-daemonizes** — whoever spawned
  it (niri, systemd, OpenRC, a terminal) owns supervision. Compositor-
  spawned therefore means no supervision (crash → down until restart):
  the accepted tradeoff of the niri path.
- Single instance by construction: binding the UDS socket is the lock; a
  second instance exits immediately with "already running".
- Graceful SIGTERM/SIGINT (close session + socket, exit 0). **No SIGHUP
  reload** — the file watches make it redundant.
- D-Bus connects lazily with backoff: a missing session bus at startup
  (compositor startup race) degrades notisync instead of
  killing the daemon.
- Flags: `--config-dir` (default `~/.config/mblink`), `--state-dir`
  (default `~/.local/state/mblinkd`), `--socket` (default
  `$XDG_RUNTIME_DIR/mblink.sock`), `--listen` (gRPC LAN bind; mDNS
  advertises whatever this is), `--log-file <path>` (unset → stderr),
  `--log-format text|json`, repeatable `-v` (info → debug → trace;
  `RUST_LOG` respected for per-crate targeting), `--check` (dry run:
  load configs, compile every Lua handler, verify bus reachability,
  print report, exit), `--version` (version + compiled module list).
- Logging via `tracing`, **explicit, no detection magic**: stderr by
  default; the niri spawn line passes `--log-file` explicitly (systemd /
  OpenRC get stderr for free via journal / supervise-daemon).

### Android app

- Kotlin + Compose, single activity. **Three-tab bottom navigation: Home,
  Modules, Settings** — all fixed destinations, so dynamic modules never
  touch the tab bar. Titles and statuses own the top of every screen;
  actionable cards anchor to the bottom edge and grow upward (thumb
  reach; empty space stays in the middle). Home = connection status +
  paired-device card (which becomes the Pair action when unpaired;
  tapping it pushes a link-details screen: session stats + Unpair).
  Modules = module cards (toggle + module status line); tapping a card
  pushes the module screen (status top, config sections bottom-anchored,
  apps as switches with the same ↻/· in-flight marks) **without** the
  bottom nav. Settings = *Keep link alive* (foreground service on/off —
  the only behavioral setting), theme, and About, which doubles as a
  build manifest (modules compiled into this APK). The ⚠
  "waiting for notification access" setup state shows on the module
  screen with the Grant-access button.
- Target device: **Pixel 8 on Android 17** (owner's phone). `minSdk` =
  that API level; no back-compat workarounds. Modern foreground-service
  rules apply (`connectedDevice` type, runtime type checks).
- `NotificationListenerService` for the module; the one-time consent grant
  is a button that opens the system settings screen — never simulated.
  It appears only in the ⚠ setup state, worded identically to the TUI.
- Foreground service (`connectedDevice` type) owns the gRPC session and
  reconnect logic (on network/mDNS reappearance, backoff). Its required
  persistent Android notification is excluded from notisync's own sync by
  package name — no self-feedback loops.
- Config + pinned keys in DataStore; secret material in AndroidKeyStore.
  Module code isolated behind the same module interface the Gradle flags gate.

## Testing strategy

- Daemon crates: unit tests per crate; module logic tested against fake
  event streams; D-Bus interaction tested over a private session bus.
- Protocol: generated code on both sides means schema drift is caught at
  build time — golden-file tests for wire compatibility.
- Android: module logic unit-tested; listener/service behaviors verified
  manually against the daemon (instrumented tests where practical).
- **On-device verification**: the Pixel 8 is attached to the build machine
  via ADB. The action pass-through spike (`actionIntent.send()` from a
  listener under background-activity-start rules) runs on real hardware in
  the first week of implementation — emulator behavior diverges exactly
  where that spike matters.

## Future directions (post-MVP, sketched now)

Two related capabilities the owner wants eventually; the analysis is
recorded so the MVP doesn't preclude them.

### Off-LAN connectivity ("hotspot")

Use case: laptop and phone share no network; the link should still come up.

- **A third-party Android app cannot enable the real tethered hotspot** —
  that API is system-only since Android 8. The feasible variant is
  **Wi-Fi Direct**: the phone creates a P2P group (`WifiP2pManager`), the
  laptop joins, and the P2P interface is an ordinary IP network — the
  **same gRPC + mDNS stack runs over it unchanged**. This is why gRPC was
  the right wire choice: any future transport that yields IP reuses
  everything.
- **Laptop side runs on iwd** (owner's system — no NetworkManager).
  iwd has maintained P2P support since v1.8 (both group-owner and client
  roles, GO-side IP allocation), **exposed only through its D-Bus API**
  (`net.connman.iwd.p2p.*`) — `iwctl` has no P2P commands. That suits
  mblinkd fine: it already speaks D-Bus (zbus), so the connectivity
  module drives iwd's P2P API directly. The phone is the group owner
  (Android's GO assigns IPs), so the laptop only needs the client role.
  Needs a hands-on verification pass when built: iwd's P2P was
  historically display-focused, and D-Bus-only means no quick iwctl
  smoke test.
- Wi-Fi Direct needs `NEARBY_WIFI_DEVICES` / location permissions on modern
  Android, and mDNS-over-P2P-interface behavior must be verified when
  built (phone may need to discover on the P2P interface specifically;
  fallback: dial a known P2P group-owner address directly).
- The trigger itself is module-shaped: a **connectivity module** whose
  phone-side action creates/joins the P2P group. Not MVP.

### Bluetooth

- Realistic role: **presence/wake channel** — BLE advertising says "phone
  nearby, no shared network," prompting the Wi-Fi Direct bring-up.
  A Bluetooth alternative to the LAN pairing ceremony is also possible,
  with the same key-pinning afterwards.
- Running gRPC over RFCOMM would need a custom tonic transport — heavy;
  treated as a last resort only.

### MVP-relevant implication (built now, not later)

Link establishment is abstracted from day one: the daemon's
listen/discovery and the phone's dial/reconnect sit behind small traits,
with mDNS-over-LAN as the first implementation. Wi-Fi Direct or Bluetooth
then slot in without touching session or module code.

## Decisions log (who decided what)

Owner: notification state sync as MVP; one shared config (tick = send +
show); per-app only; state sync not replay; D-Bus surfacing with 5s timeout,
history as the persistent view; two-way dismissal; Lua handlers keyed by
package name (TOML shorthand rejected later — Lua only); gRPC + protobuf +
monorepo + shared protos (chosen for future expansion); pairing option A
(short code, pinned keys); systemd/OpenRC both supported via build flags;
Kotlin+Compose; Rust; notification-daemon contract implemented by owner
outside mblink scope; 1:1 topology; proper graceful version-skew handling;
target device Pixel 8 / Android 17; Bluetooth and off-LAN (Wi-Fi Direct)
connectivity deferred to future modules, with the transport abstraction
built into the MVP; UDS (gRPC) for the mblink ↔ mblinkd control channel;
mblinkd runs in the user session (owner's setup: spawned by niri at
startup, `--log-file` passed explicitly), with systemd/OpenRC user services
kept via build flags for other use cases; module-scoped config with
`mblink/{config.toml, notisync/{config.toml, handlers/*.lua}}` layout;
`mblinkd` is the main actor and owns all files; TUI: two screens
(home module list / module screen with collapsible sections), persistent
header carrying link + mblinkd + top-level sync state, hotkeys-only
footer, notisync has a single `apps` section, Space toggles / Enter opens
$EDITOR (yazi-style for TUI editors, detached spawn for GUI editors,
$EDITOR always), no force-sync UI; Android: three bottom tabs
(Home / Modules / Settings), titles/statuses on top, actionable cards
anchored to the bottom, pushed module screens without the nav bar, no
settings at top-right; symmetric permission model (every setting editable
everywhere; consent grant is a one-time setup state worded identically in
both apps); Lua API settled (file returns one function, sandboxed,
effects returned not executed, `ml.open`/`ml.run`/`ml.log`, `n.action` +
`n.buttons`, `extras` empty for MVP); notification **action buttons pass
through to Android** (`actionIntent.send()`), Lua as override layer;
mblinkd headless with the flag set in its section; D-Bus contract
specified as its own doc (owner implements in parallel; mblinkd built
against a mock of it); app catalog = POST_NOTIFICATIONS holders ∪
observed posters; Pixel 8 attached via ADB for on-device verification;
unpair clears keys only, `mblink unpair`, replace-on-confirm for a new
peer.

Design calls made by the assistant (owner may override at review): monorepo
layout as drawn; `modules.toml` as the flag mechanism with xtask + Gradle
task; phone-as-gRPC-client / daemon-as-server; config LWW with mtime for
hand-edits; new apps default unticked; `ratatui`; state in plain files
under `~/.local/state/mblinkd/`; AndroidKeyStore/DataStore; daemon restart
reconciliation = withdraw-by-app-hint + re-Notify from snapshot; pair QR
optional-not-MVP; `mblinkd --check` dry-run flag; niri-spawn accepts no
supervision (explicit choice over auto-logfile detection).

## Left open to the builder

- Crate/module internal APIs, error types, reconnect/backoff details.
- Pairing crypto details (PAKE choice) — flagged for careful review.
- Post-pairing auth mechanism (mTLS vs token metadata).
- Which Android `extras` cross the wire (deferred until a concrete need;
  `n.extras` is `{}` in MVP).
- OpenRC user-service mechanics.
- QR-in-terminal or not (optional polish).
- Daemon runtime-state file format (pinned keys, peer info) under
  `~/.local/state/mblinkd/`.
- UI wireframes: final mockups from the brainstorming sessions are
  archived in `docs/superpowers/specs/ui/` — treat them as the reference
  for both UIs' structure (not pixel-exact designs).
