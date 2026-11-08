# mblink D-Bus notification-daemon contract (2026-11-08)

The interface between `mblinkd` and the owner's notification daemon.
The daemon owns the standard bus name `org.freedesktop.Notifications` at
object path `/org/freedesktop/Notifications`; `mblinkd` is a client.
Everything mblink needs is listed here — the standard surface (with the
behaviors mblink depends on made explicit) plus one custom extension
interface. If a capability is not listed, mblink does not rely on it.

## A. Standard surface — `org.freedesktop.Notifications`

### Notify

```
Notify(s app_name, u replaces_id, s app_icon, s summary, s body,
       as actions, a{sv} hints, i expire_timeout) → u id
```

`mblinkd` always calls it like this:

- `app_name` = `"mblink"` — every mblink-sourced notification carries this
  stable discriminator (used by the withdrawal extension below). The
  human-readable app name travels in a hint.
- `replaces_id` = the id mblinkd previously received for the same Android
  notification (updates replace, never stack). **Requirement:** a replaced
  notification remains a single history entry; the returned id is the one
  mblinkd uses for subsequent replaces/closures.
- `actions` = `["default", "Open", "a0", "Like", "a1", "Reply", …]` —
  pairs of (key, label). Keys are mblink's stable per-notification action
  keys. **Requirement:** buttons are rendered and clickable from history,
  not only while the popup is up.
- `hints` — required to be carried into history/detail views:
  - `x-mblink.app` (s) — Android app display label ("Gmail") — use for
    display/grouping in the history UI.
  - `x-mblink.package` (s) — Android package name
    (`com.google.android.gm`) — marks the notification as mblink-sourced
    (presence of this hint is what "mblink-sourced" means).
  - `x-mblink.key` (s) — the Android notification key (diagnostics).
  - `x-mblink.ongoing` (b) — true for ongoing/foreground-service
    notifications.
  - standard `category` (s) when Android provides one; icons/image-data
    are optional future work, not relied on in MVP.
- `expire_timeout` = `5000`. **Requirement:** the popup auto-hides after
  the timeout but the entry **stays in history** (expiry retires the
  popup, not the notification).

### CloseNotification

```
CloseNotification(u id) → nothing
```

**Requirement:** removes the history entry (not just the popup) and emits
`NotificationClosed(id, 3)`. mblinkd uses this for phone-side dismissal.

### Signals — must be emitted reliably, always, for mblink's notifications

```
NotificationClosed(u id, u reason)   # 1 = expired, 2 = user dismissed,
                                    # 3 = CloseNotification, 4 = undefined
ActionInvoked(u id, s action_key)
```

- `NotificationClosed` with reason 1 **must** still be emitted on expiry —
  mblinkd distinguishes expiry (ignore) from dismissal (2/3 → forward to
  the phone) by the reason code.
- `ActionInvoked` fires for `default` (body click) and for every button
  key (`a0`, …), including clicks from history.

### Locked (ongoing) notifications

Notifications with `x-mblink.ongoing = true` must be displayed **without a
dismiss affordance**. These cannot be cancelled on Android; an offered
dismissal would trigger a dismiss → re-post loop against apps that keep
re-posting their ongoing notifications.

## B. Custom extension — `org.mblink.Extensions` (same object)

### WithdrawAllMblink

```
WithdrawAllMblink() → nothing
```

Withdraws every notification carrying an `x-mblink.package` hint — popup
and history, unconditionally. Called by mblinkd exactly once at its own
startup (and on notification-daemon reappearance, see below) so it can
re-Notify the current phone snapshot without stale entries or stale
key↔id mappings.

## C. Lifecycle notes for both sides

- mblinkd watches the bus name's owner (`NameOwnerChanged` on
  `org.freedesktop.Notifications`): if the notification daemon goes away
  and comes back (restart), mblinkd reruns the reconciliation:
  `WithdrawAllMblink()` → re-Notify the current snapshot. This makes
  notification-daemon restarts self-healing, and means the daemon's
  history persistence across its own restarts is irrelevant to mblink.
- No other coupling exists: mblinkd never expects mblink-specific
  behavior outside this document.

## Summary checklist

1. Notify: replaces_id replaces-in-place; actions render from history;
   hints preserved in history; expire_timeout = popup-only.
2. CloseNotification: removes history entry + emits NotificationClosed(3).
3. NotificationClosed: always emitted, all reasons, mblink's notifications.
4. ActionInvoked: body click (`default`) + button keys, from history too.
5. x-mblink.ongoing: no dismiss affordance.
6. WithdrawAllMblink: nukes all mblink-sourced entries (popup + history).
