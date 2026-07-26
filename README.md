# bubbleTranslate

Select text anywhere on macOS — a PDF, a terminal, a browser, an editor — and a
small bubble appears at the cursor with the translation.

## Install

### From the DMG

[**Download bubbleTranslate.dmg**](https://github.com/o0pelamx/bubbleTranslate/raw/main/bubbleTranslate.dmg)
— no Rust toolchain needed. Open it and drag **bubbleTranslate.app** onto the
Applications folder.

The app is ad-hoc signed rather than notarized, so macOS blocks the first
launch with *"Apple could not verify bubbleTranslate is free of malware."*
That is Gatekeeper reacting to the missing Apple Developer signature, not to
anything the app does. To get past it:

1. Double-click the app and dismiss the warning.
2. Open **System Settings › Privacy & Security**, scroll to Security, and click
   **Open Anyway** next to the message about bubbleTranslate.
3. Confirm once more when the app launches.

On macOS 15 and later, Control-clicking the app and choosing *Open* no longer
bypasses this — Open Anyway is the route. If you would rather not go through
Settings, stripping the quarantine flag has the same effect:

```sh
xattr -dr com.apple.quarantine /Applications/bubbleTranslate.app
```

This step is only needed once, and only for a build downloaded from the
internet. Building from source skips it entirely.

### From source

```sh
./bundle.sh
open bubbleTranslate.app
```

### Granting Accessibility

Either route, the first launch asks for **Accessibility** permission. Grant it
in System Settings › Privacy & Security › Accessibility, then **quit and
relaunch** — the event tap is installed at startup, so it needs a restart to
take effect.

macOS ties this permission to the app's exact signature, and an ad-hoc
signature changes with every rebuild. So after `./bundle.sh` or `./release.sh`,
the grant silently stops applying even though the switch still looks on: turn
it off and on again, or remove the entry with **−** and re-grant.

The main window opens on launch, centred and sized to fit your display. Reopen
it any time from the **menu bar** (🌐) or by launching the app again — either
brings it to the front.

Closing the main window does not stop the translator; it keeps watching for
selections in the background. Quit from the menu bar icon.

The app shows a Dock icon only while the main window is open. That is not
cosmetic: macOS will not bring a background-only app to the front, so the app
becomes a regular one for as long as it has a window, and drops back to
background when you close it — which is what keeps the bubble from stealing
focus from whatever you are reading.

## The main window

- **Translate** — a scratch box for text you paste in, using the same chain
- **Languages** — target and source language
- **Providers** — reorder the chain with ↑↓, set the DeepL key and MyMemory
  email, and **Test providers** to see which backends answer right now
- **Behaviour** — bubble text size, auto-hide delay, settle delay, length cap
- **Recent** — what the bubble has translated this session, with copy buttons

Every change saves immediately to the config file.

## Building an installer

```sh
./release.sh          # -> bubbleTranslate.dmg
```

Drag-to-Applications layout. Without an Apple developer account the app is
ad-hoc signed, so the DMG installs fine but Gatekeeper blocks the first launch
and the user has to clear it once — see [From the DMG](#from-the-dmg).

With a `Developer ID Application` certificate ($99/year Apple Developer
Program) the same script produces a release that opens with no warning:

```sh
xcrun notarytool store-credentials bubbleTranslate-notary \
  --apple-id you@example.com --team-id TEAMID --password <app-specific-password>

SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_PROFILE=bubbleTranslate-notary \
./release.sh
```

**This app cannot ship on the Mac App Store.** Store apps must run in the App
Sandbox, which forbids all three of its capture mechanisms — Accessibility
reads of other apps, system-wide event taps, and synthesized keystrokes. A
sandboxed build would launch and capture nothing. Direct distribution is the
route for a tool that reads other applications.

## How it reads the selection

Two strategies, in order:

1. **Accessibility API** — asks the focused element for `AXSelectedText`.
   Instant, and it never touches your clipboard. Works in native apps, Safari,
   and most text fields.
2. **Synthetic Cmd+C** — posts a command-C and watches the pasteboard's change
   count. Terminal.app, PDF viewers and Electron apps expose nothing over AX
   but copy fine, so this is what makes "anywhere" literal. Your previous
   clipboard **text** is restored afterwards; a copied image or file reference
   is not.

Set `clipboard_fallback = false` in the config to disable strategy 2.

Only gestures that actually finish a selection trigger a capture: a drag longer
than a few points, a double/triple click, shift+arrow navigation, or Cmd+A. A
plain click never does — otherwise strategy 2 would fire a copy on every click
in the OS.

## Translation backends

Three, tried in order until one answers:

| # | Provider | Key needed | Notes |
|---|----------|-----------|-------|
| 1 | Google   | no  | Unofficial `translate_a` endpoint. Fast and free; rate-limits per IP and can answer a burst with an HTML captcha page. Falls back to a mirror host and one retry before giving up. |
| 2 | MyMemory | no  | Detects the source itself (`Autodetect`). ~5k chars/day anonymous, ~50k with `mymemory_email` set. Rejects text over 500 bytes, so long selections skip straight past it. |
| 3 | DeepL    | yes | Best quality where it has the language. Skipped entirely unless `deepl_api_key` is set. Free keys end in `:fx` and are routed to `api-free.deepl.com` automatically. |

When every provider refuses, the bubble lists each one's reason rather than a
generic failure.

## Checking the backends

```sh
./target/release/bubbleTranslate --check                # probe all three
./target/release/bubbleTranslate --translate "merhaba"  # run the chain once
```

Neither opens a window or needs any permission, which is how you tell a backend
problem apart from a capture problem. The same probe is available in the main
window under **Providers › Test providers**.

To trace the whole pipeline stage by stage:

```sh
pkill bubbleTranslate
BUBBLETRANSLATE_DEBUG=1 ./bubbleTranslate.app/Contents/MacOS/bubbleTranslate
```

Each stage logs one line (`mouse-up`, `capture`, `translate`), so a selection
that produces no bubble can be traced to the gesture filter, the capture, or
the provider chain.

## Config

`~/Library/Application Support/bubbleTranslate/config.toml`, written on first run.

```toml
target_lang = "en"          # what to translate into
source_lang = "auto"        # or a fixed code
providers = ["google", "mymemory", "deepl"]
deepl_api_key = ""
mymemory_email = ""         # raises the MyMemory quota
auto_translate = true       # bubble on selection
min_chars = 2
max_chars = 4000            # keeps a stray Cmd+A out of the queue
debounce_ms = 180           # settle time before reading the selection
clipboard_fallback = true
auto_hide_secs = 12         # 0 = stay until closed; pauses while hovered
font_size = 15.0
```

Target language, auto-translate and the DeepL key are also editable from the
bubble's ⚙ menu. Changing the language re-translates the text already captured.

## Known limits

- Restoring the clipboard after a synthetic copy only preserves text.
- The bubble never takes keyboard focus (by design — otherwise the source app
  would drop its selection), so it cannot be dismissed with Esc. Close it with
  ✕, let it auto-hide, or just select something else.
- Keyboard-initiated selections anchor the bubble at the mouse pointer, not at
  the caret.
