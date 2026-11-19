# Handy Tower

A private fork of [Handy](https://github.com/cjpais/Handy) (branch `tower`, cut
from upstream `v0.10.1`) with a second dictation backend, **tower**.

One USB mic is plugged into the Linux server "tower". The tower service owns
the mic and the single recording session and returns the transcript to
whichever client calls stop. This app, on the Windows PC (Deskflow server) and
the work Mac (Deskflow client), only sends hotkey intents over HTTP and pastes
the text it gets back. The contract is `dictation/docs/API.md` in
desk-tools; research notes are in `dictation/docs/DESIGN-NOTES.md`.

In tower mode the app never opens a local audio device, never asks for
microphone permission, and never downloads or loads a model. Local mode is
stock Handy, except that nothing is ever persisted (see [Privacy](#privacy)).

## What changed and where

New code (all of the logic):

| File                                 | What                                                                                                                          |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| `src-tauri/src/tower/mod.rs`         | Hooks called from upstream code, the runtime (client, server view, overlay/Esc/tray sync, paste), Tauri commands              |
| `src-tauri/src/tower/client.rs`      | HTTP client (`/record/*`, `/state`, `/health`, `/events` request), error → message mapping, opt-in live tests                 |
| `src-tauri/src/tower/events.rs`      | SSE parser, `ServerView` (what this client knows about the session), reconnecting stream task                                 |
| `src-tauri/src/tower/hotkeys.rs`     | Pure key state machine (hold-or-toggle, auto-repeat) and the actor that runs it in order                                      |
| `src-tauri/src/tower/overlay.rs`     | What the overlay shows for a given view/message                                                                               |
| `src-tauri/src/tower/settings.rs`    | Tower settings in their own store, `tower_settings.json`                                                                      |
| `src-tauri/tauri.tower.conf.json`    | Merged over `tauri.conf.json` by CI: identifier `com.boolyy.handytower`, no updater artifacts, no Azure `signCommand`         |
| `src/components/tower/*`             | Settings > Tower, the connection form, the tower first-run screen                                                             |
| `src/overlay/TowerOverlay.{tsx,css}` | The overlay's `tower` state                                                                                                   |
| `src/stores/towerStore.ts`           | Frontend tower settings + connection status                                                                                   |
| `src/i18n/tower.{ts,en.json}`        | The fork's strings, merged into English at startup (other languages fall back to English), so no upstream locale file changes |
| `.github/workflows/tower-build.yml`  | The only workflow that can start on this branch                                                                               |

Hooks in upstream files (each marked `handy-tower`, `grep -rn handy-tower src src-tauri`):

| File                                                                     | Hook                                                                                                 |
| ------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| `lib.rs`                                                                 | `mod tower`, `tower::prepare` / `tower::start` in `initialize_core_logic`, 4 commands                |
| `settings.rs`                                                            | default bindings `tower_dictate` (F14) and `tower_raw` (F15); updater forced off                     |
| `shortcut/handler.rs`                                                    | tower bindings and Esc go to `tower::handle_shortcut`                                                |
| `shortcut/{mod,tauri_impl,handy_keys}.rs`                                | `tower::binding_enabled` filter in the registration loops; tauri backend enforced in tower mode      |
| `signal_handle.rs`, `utils.rs`                                           | CLI/SIGUSR2 toggle and every cancel path go to the tower in tower mode; `redact_text` always redacts |
| `managers/audio.rs`                                                      | `start_microphone_stream` refuses in tower mode (the one place a mic opens)                          |
| `managers/transcription.rs`, `managers/model.rs`                         | model load and download refuse in tower mode                                                         |
| `managers/history.rs`, `managers/transcription_jobs.rs`, `actions.rs`    | no history DB, no recordings, no file jobs                                                           |
| `overlay.rs`                                                             | `show_overlay_state` is `pub(crate)`; the `tower` state gets a 380 px window                         |
| `Info.plist`                                                             | `NSLocalNetworkUsageDescription` for the macOS Local Network prompt                                  |
| `App.tsx`, `Sidebar.tsx`, `AdvancedSettings.tsx`, `RecordingOverlay.tsx` | tower setup step, Tower section, History hidden, file drop removed, `tower` overlay state            |
| `bindings.ts`                                                            | regenerated (new commands and types only)                                                            |

## Settings

Settings > **Tower**:

- **Dictation backend**: `Tower` (default, also on first run) or `Local (Handy)`.
  Switching re-registers the hotkeys at once; no restart.
- **Tower URL**: default `http://192.168.1.182:8790`. Don't use `tower.local`
  (Avahi also advertises Docker bridge addresses).
- **Token**: `DICTATION_TOKEN` from the tower's `.env`. v1 stores it in plain
  text in `tower_settings.json` in the app data directory
  (`%APPDATA%\com.boolyy.handytower\` on Windows,
  `~/Library/Application Support/com.boolyy.handytower/` on macOS). It is
  write-only in the UI, never sent to the webview and never logged. Anyone who
  can read your user profile can read it; the token only gates a LAN-bound
  service, which is acceptable for v1.
- **Hotkeys**: Dictate (clean, default F14), Dictate raw (default F15), the hold
  threshold (shared with Handy's hold-or-toggle setting, default 300 ms), and
  Esc (cancel).
- **Test connection** calls `/health`; the status line follows `/events`.

The client sends `X-Dictation-Client: windows|mac|linux` by OS and never uses a
proxy for the tower (`no_proxy`, so GlobalProtect's proxy can't interfere).

Settings that only tune local transcription (General, Models, Post Process,
VAD/filler words/custom words, model unload) are hidden in tower mode. Paste
method, clipboard handling, trailing space, overlay position/style and
autostart still apply.

## Hotkeys

**Keys: F14 (dictate, cleaned) and F15 (raw), no modifiers.** The Mac screen
has Deskflow's `alt = super` swap, so modifier combos are remapped on the way;
F17 and above only reach the Mac on Deskflow 1.27 or later; avoid
Ctrl+Alt+Win+C (center-coord), Right Alt (AltGr) and Ctrl+Space. Both keys are
always hold-or-toggle, whatever Handy's activation setting says.

**Mapping** (stateless; the session may have been started on the other machine):

1. Key down → `POST /record/toggle?mode=clean|raw`. Auto-repeat downs are ignored.
2. `started` → remember when the key went down. On key up, a hold of at least
   the threshold sends `POST /record/stop?mode=…` (push-to-talk); a shorter tap
   leaves it recording until the next press (on either machine).
3. `stopped` → paste the text and ignore this key's release.

Pressing the other key stops in its mode (the server's "mode at stop wins"):
start with F14, stop with F15 and you get raw text.

A release is skipped when the stream has already shown that its session ended,
so a stale key-up can never stop a newer session.

### Windows: RegisterHotKey, and how key release works

Tower mode forces the **tauri** shortcut backend (`RegisterHotKey`, via
tauri-plugin-global-shortcut and the global-hotkey crate), not handy-keys. While
the cursor is on the Mac, Deskflow's `WH_KEYBOARD_LL` hook swallows the key so
`RegisterHotKey` doesn't fire, but low-level hooks run last-installed-first, so
handy-keys' own hook (installed after Deskflow's) would see the key first and
fire on Windows too. Switching to handy-keys is refused while the tower backend
is active.

**Finding:** global-hotkey 0.8.0 (the version in `Cargo.lock`) does deliver key
release on Windows. It registers with `MOD_NOREPEAT`, sends `Pressed` on
`WM_HOTKEY`, then spawns a thread that polls `GetAsyncKeyState(vk)` every 50 ms
and sends `Released` when it reads 0
(`global-hotkey-0.8.0/src/platform_impl/windows/mod.rs`, `global_hotkey_proc`).
So push-to-talk works with no Windows-specific code in this fork; release
latency is up to 50 ms.

The catch is Deskflow: if you hold F14 on Windows and cross to the Mac before
releasing, Deskflow's hook swallows the key-up, so Windows' async key state
stays "down" and no `Released` comes until F14 next goes up on Windows. The
recording just continues as if tapped (stop it from either machine). That late
release is then ignored because its session has ended. Because Windows still
thinks F14 is down, `MOD_NOREPEAT` may treat your next F14 press on Windows as
a repeat and swallow it; press again.

### macOS: Carbon hot keys

Tower mode uses the **tauri** backend on macOS too (Carbon
`RegisterEventHotKey`), not handy-keys' CGEventTap, because it is the more
robust one for keys Deskflow injects with `CGEventPost(kCGHIDEventTap)`:

- Events posted at the HID tap go through the window server like hardware
  input, and Carbon hot keys are matched there; that is the same path that lets
  Cmd+Space open Spotlight through Deskflow/Synergy.
- Carbon hot keys keep working under Secure Input (password fields, Terminal's
  Secure Keyboard Entry, some password managers), where a CGEventTap stops
  seeing keys; upstream Handy carries a whole Carbon fallback for exactly that.
- It needs no Input Monitoring permission and is never disabled by the system
  for a slow callback, unlike an event tap.
- `kEventHotKeyReleased` gives a real key-up for push-to-talk.

Only **Accessibility** is needed, for pasting.

### Linux

Supported only enough for testing (X11). Upstream disables the dynamic cancel
shortcut on Linux, so Esc doesn't cancel there; use the overlay's ✕.

## Overlay, Esc and the event stream

Both machines keep `GET /events` open (reconnect with backoff 0.5 s → 10 s, a
40 s idle timeout against the 15 s pings, and an immediate retry when you press
a hotkey while disconnected). The stream drives, on **both** screens:

- the overlay whenever the server isn't idle: "Recording on tower" with the
  elapsed time, "Transcribing...", and after auto-stop "Held: press hotkey to
  paste, Esc to discard";
- the tray icon;
- **Esc**: registered only while the server isn't idle and released when it goes
  idle; it calls `POST /record/cancel`. While it's registered, Esc doesn't reach
  other apps on either machine (up to `held_ttl`, 120 s, when held). The overlay's
  ✕, the tray's Cancel and `handy --cancel` do the same.

The machine that pressed the key shows its own transition at once, before the
stream confirms it, and shows a message when something goes wrong. If the
stream is down, the overlay shows nothing for the session, and a hotkey press
that can't reach the tower shows "Tower unreachable".

| Response                                         | What happens                                               |
| ------------------------------------------------ | ---------------------------------------------------------- |
| `stopped` with text                              | paste it                                                   |
| `stopped`, `cleanup:"failed"`                    | paste the raw text, then "Cleanup failed: pasted raw text" |
| `stopped`, empty `text`                          | "No speech", nothing pasted                                |
| `409 debounced`                                  | nothing (the other machine's key fired too)                |
| `409 busy`                                       | "Tower is busy"                                            |
| `409 cancelled`                                  | nothing pasted                                             |
| `409 not_recording` on a release                 | nothing (the session already ended elsewhere)              |
| `401`                                            | "Tower rejected the token"                                 |
| `503 mic_unavailable`                            | "Tower mic unavailable"                                    |
| other 4xx/5xx                                    | "Tower error"                                              |
| network error                                    | "Tower unreachable"                                        |
| no answer to a stop within 30 s                  | "Tower timed out"                                          |
| stream: idle with `reason:error` after a session | "Tower error" on both screens                              |

Pasting uses Handy's own path unchanged: clipboard snapshot and restore, the
Windows exclusion formats (`ExcludeClipboardContentFromMonitorProcessing`,
`CanIncludeInClipboardHistory=0`, `CanUploadToCloudClipboard=0`) and the macOS
`org.nspasteboard.TransientType`/`ConcealedType` markers. Paste failures use
Handy's existing notices.

## Privacy

In both backends:

- No transcript history: `HistoryManager` creates neither `history.db` nor the
  recordings directory; saves are no-ops and reads see an empty in-memory
  database. History is hidden in the sidebar and Advanced settings, and
  file drop/file transcription is removed (its results went to History).
- No audio files: the local pipeline no longer writes a WAV per dictation.
- Transcripts are never logged: `utils::redact_text` always returns
  `[REDACTED]` (upstream logged text in debug builds), the tower code logs only
  lengths and states, and the text-carrying types have hand-written `Debug`.
- The token is never logged (`TowerSettings`'s `Debug` prints `token_set` only).
- The event stream never carries text, by contract.
- Keep **Clipboard handling** on its default ("don't modify"), or the text stays
  on the clipboard.
- The self-updater is forced off: upstream's would replace this fork with stock
  Handy.

Files from an earlier stock Handy install (`com.pais.handy`) are not deleted;
CI builds use a different app data directory.

## Installing

Builds come from the **Tower Build** workflow (see [CI](#ci)), as artifacts of
the workflow run: `handy-tower-x86_64-pc-windows-msvc` (NSIS `.exe` and `.msi`)
and `handy-tower-aarch64-apple-darwin` (`.dmg`; ignore the `.app` folder, the
artifact zip drops its executable bit).

### Windows (Deskflow server)

1. Run the NSIS `Handy_…_x64-setup.exe`. It's unsigned, so SmartScreen says
   "Windows protected your PC": **More info → Run anyway**.
2. On first run the Tower setup screen asks for the URL and token. **Save**
   (it tests the connection), then **Continue**. No model, no microphone.
3. Settings > Tower shows "Connected". Settings > Advanced > Experimental, if
   shown, lists the keyboard implementation as `tauri`.

### macOS (work Mac, Deskflow client)

1. Open the `.dmg` and drag **Handy** to Applications.
2. **Gatekeeper**: the app is only ad-hoc signed. The first open is blocked
   ("Apple could not verify…"). Open **System Settings → Privacy & Security**,
   scroll to the message about Handy and click **Open Anyway**, then confirm.
   (Or run `xattr -dr com.apple.quarantine /Applications/Handy.app` once.)
   Every new build needs this again.
3. **Accessibility** (for pasting): the setup screen's **Allow** button opens the
   prompt; enable Handy under Privacy & Security → Accessibility. Every unsigned
   update changes the app's signature, so the old entry stops matching: the
   setup screen comes back after an update, and **Allow** clears the stale entry
   and asks again (or remove Handy with "−" and add it back).
4. **Input Monitoring** is not needed (Carbon hot keys).
5. **Local Network**: the first connection to `192.168.1.182` shows "Allow
   Handy to find and connect to devices on your local network?" once. Click
   **Allow**. If you clicked Don't Allow, enable Handy under Privacy & Security →
   Local Network. A rebuilt unsigned app may ask again. Without it, the overlay
   says "Tower unreachable" although `curl` works.
6. If F14/F15 change the display brightness instead of dictating, macOS is
   using them; choose other keys in Settings > Tower (F13 or F16; F17+ needs
   Deskflow 1.27).

## CI

`boolyy/handy-tower` is public, so builds on GitHub-hosted runners cost no
minutes. On this branch only `tower-build.yml` can start a run:

- **Triggers**: `workflow_dispatch` (choose `both`, `macos` or `windows`) and
  tags `tower-v*`. Nothing runs on push or pull requests.
- **Targets**: macOS Apple silicon (`macos-26`, `aarch64-apple-darwin`) and
  Windows x64 (`windows-latest`). Unsigned; no secrets needed.
- **How**: it calls upstream's reusable `build.yml` (kept; it is
  `workflow_call` only) with `sign-binaries: false` and
  `--config src-tauri/tauri.tower.conf.json`, which sets the fork's identifier,
  turns off updater artifacts (they need a signing key) and removes the Azure
  `signCommand`. Upstream's Windows package audit still runs.
- **Time**: a release build with LTO takes about 15 minutes per platform; build
  one platform when that's enough.
  The Rust cache helps later runs.

Deleted on this branch so a push triggers nothing (re-delete them if a rebase
brings them back): `build-test.yml`, `code-quality.yml`, `main-build.yml`,
`nix-check.yml`, `playwright.yml`, `pr-test-build.yml`, `release.yml`,
`test.yml`.

**Local engines kept.** The whisper/transcribe-cpp engines (Vulkan on Windows,
Metal on Apple silicon) are still built. Removing them is not trivial: it
touches the per-target `Cargo.toml` tables, `build.rs` library staging, the
bundle resources and the model manager, which would conflict with every
upstream release. The cost is build time (Vulkan SDK, ggml) and size (tens of
MB of DLLs/dylibs), and in exchange local mode keeps working (away from home)
and rebases stay easy. The tower backend never loads them.

## Rebasing on upstream

```sh
cd ~/code/handy-tower
git fetch upstream --tags
git switch tower
git rebase v0.10.2            # or upstream/main
# Conflicts can only be in the hook lines: grep -rn handy-tower src src-tauri
git status .github/workflows  # delete any new upstream workflow except build.yml
```

Then check, from the repo root (Docker build container as below):

```sh
bun install && bun run build && bun run lint && bun run check:translations
cd src-tauri && cargo check && cargo clippy && cargo test --lib tower::
```

Regenerate `src/bindings.ts` when commands or types changed: a debug build
writes it at startup (`bun run tauri dev`; on a headless Linux box,
`cd src-tauri && cargo build && LD_LIBRARY_PATH=transcribe-libs
target/debug/handy --list-devices`, which exports and then fails to open a
display). Keep only the tower hunks if the export also rewrites
platform-specific doc comments (it does for `isLaptop` on Linux).

Things that would break quietly: upstream adding a new place that opens the mic
(check that it goes through `start_microphone_stream`), a new binding
registration loop (needs `tower::binding_enabled`), or a new cancel path (route
it through `utils::cancel_current_operation`).

### Tests on a Linux box

No toolchain on the host: build in a throwaway container (Rust + Tauri's Linux
dev packages + Bun) with the repo bind-mounted and the cargo cache in a named
volume. The opt-in live tests drive the real session (nothing else should be
dictating) and print only lengths:

```sh
set -a; . /home/tower/dictation/.env; set +a
HANDY_TOWER_IT_URL=http://127.0.0.1:8790 HANDY_TOWER_IT_TOKEN="$DICTATION_TOKEN" \
  cargo test --lib tower::client::live -- --nocapture --test-threads=1
```

## 5-minute test through Deskflow

Both apps running, Settings > Tower says "Connected" on both. A text field
focused on each machine. `docker logs` of the dictation API shows
`client=windows|mac` per request, without content.

1. **Only the active screen reacts.** Cursor on Windows: tap F14. The overlay
   shows "Recording on tower" on **both** screens; the log has one toggle from
   `windows`. Say a sentence, tap F14: it pastes on Windows only. Move to the
   Mac and repeat: one toggle from `mac`, paste on the Mac only. No "Tower is
   busy", no second toggle.
2. **Push-to-talk.** Hold F14 for a few seconds while speaking and release: it
   pastes on release. On each machine.
3. **Hold and cross the edge.**
   - Hold F14 on the Mac and move to Windows while holding: Deskflow releases
     every key when the cursor leaves the Mac, so recording stops at the
     crossing and the text pastes on the Mac.
   - Hold F14 on Windows, cross to the Mac, release there: Windows never sees
     the key-up, so recording continues (as if tapped). Tap F14 on the Mac: it
     pastes on the Mac. Back on Windows, the first F14 press may do nothing;
     press again.
4. **Esc on each side.** Start recording, cursor on Windows, press Esc: the
   overlay disappears on both screens and nothing pastes; Esc then works in
   apps again. Repeat with the cursor on the Mac.
5. **Start on one, stop on the other.** Tap F14 on the Mac, move to Windows,
   tap F14: it pastes on **Windows**.
6. **Held** (optional, 45 s): tap F14 and stay silent until both overlays say
   "Held: …", then press F14 on either machine to paste there (or Esc).
7. **Raw and errors** (optional): F15 pastes uncleaned text; with the tower
   service stopped, F14 shows "Tower unreachable".

Also check that the clipboard holds what it held before, and that Windows'
clipboard history (Win+V) doesn't show the dictation.

## Deviations from the contract

- `X-Dictation-Client` is `linux` on Linux (the contract lists
  `windows|mac|curl`); Windows and Mac send `windows` and `mac` as specified.
- The hotkeys never call `POST /record/start`; a press always toggles, because
  the session may belong to the other machine. Push-to-talk release uses
  `POST /record/stop`. `/transcribe` (away mode) is not used yet.
