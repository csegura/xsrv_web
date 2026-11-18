# Input: keyboard layouts and touch

## Keyboard layouts

The page sends the physical key (the evdev code from `KeyboardEvent.code`, plus 8). The server
decides which symbol the key types. Set the layout with `XSRV_KEYMAP`:

| Value | Layout |
| --- | --- |
| `us` (default) | US. Two symbols per key. Right Alt is Alt (Mod1). |
| `es` | Spanish (ISO). Four symbols per key. Right Alt is AltGr on Mod5: `Mode_switch` in the core keymap, `ISO_Level3_Shift` in the XKEYBOARD keymap (the third and fourth level). |
| `gb` (or `uk`) | British (ISO). Same AltGr. |
| `de` | German (ISO). Same AltGr. |

XKEYBOARD clients (Xlib, GTK, Chromium, i3) read the same layout through the extension, see
[extensions.md](extensions.md). A layout switch from the settings panel sends the core
MappingNotify and the XKB NewKeyboardNotify and MapNotify, so both kinds of client reload it.

The layout of the browser keyboard does not matter: a physical key gives the same X keycode on
every keyboard, so the server layout must match the keyboard that people use.

Spanish dead keys (accent, grave, circumflex, diaeresis) are sent as dead keysyms. The X client
joins them with the next letter, as it does on any X server.

- Tested with a real `xterm` (UTF-8): with `es`, the `ñ` key, Shift+`ñ`, and AltGr+2 (`@`) type
  `ñÑ@`; with `de`, the keys at the US `y` and `[` positions type `zü`; with `gb`, Shift+3, Shift+apostrophe,
  the hash key and Shift+the backtick key type `£@#¬`.
- Headless Chromium: Right Alt then the `2` key gave key 108, then key 11 with the Mod5 bit set.
- Switching layouts at run time (settings panel): checked with xterm, urxvt, GTK 3 and Chromium; after `es`, AltGr+2 types `@` and the `ñ` key types `ñ`.
- Not tested: dead-key composition, other applications. The tables for `gb` and `de` were
  written from memory of the layouts and should be checked by a person who uses them.
- On Windows, a browser sends a fake Control before AltGr. That was not tested.

## The settings panel

The buttons of the page sit in one bar in a corner: Win, the gear and full screen (⛶). The gear
opens the settings panel (`web/settings.js`), next to the bar. Escape, or the gear again, closes it,
and the keyboard focus goes back to the screen.

- **Keyboard layout**: us, es, gb or de, for the whole session (every page and every X client). The
  server lets go of every key that is down, changes the keymap, and sends MappingNotify (and the XKB
  NewKeyboardNotify, NamesNotify and MapNotify), so running clients read the new keymap without a
  restart. Every open page shows the new layout. `XSRV_KEYMAP` is only the layout at start.
- **Selecting text also copies it** (Mouse tab, on by default): a mouse selection in an X program goes
  to the browser clipboard, as in PuTTY. Off, only a copy command (Ctrl+C, Ctrl+Shift+C) does.
  See `docs/clipboard.md`.
- **Wheel speed**: 0.25x to 4x. The page adds up the wheel distance (`web/wheel.js`): 100 pixels (or
  3 lines) is one wheel step at 1x, so a mouse notch is one step and a touchpad scrolls by distance
  instead of one step per event. The first event after a pause of 200 ms always gives one step, so
  a small notch (macOS) still scrolls. At most 10 steps go out for one browser event.
- **Keys**: buttons that send the combinations that the browser or the system keep: Ctrl+Alt+Del,
  Ctrl+W, Ctrl+T, Ctrl+N, Ctrl+Tab, Alt+Tab and Alt+F4. The keys go down in order and up in reverse.
  What they do is up to the X session.
- **Win / Cmd key is**: the key that acts as the Windows (Cmd) key, for a system or browser that
  keeps that key: the Windows key itself, Right Ctrl, the Menu key, or Right Alt (then there is no
  AltGr). Caps Lock is not offered, because the system would still switch the caps lock. It
  replaces `?super=` once a choice is saved.
- **Win / Cmd** button and the **Win** button in the bar: the sticky Super key (below), one click
  away in the bar. A check box in the panel hides the bar button.
- **Place of the buttons**: the bar has a handle (⠇) at its left. Drag it anywhere, with a mouse, a
  finger or a pen; with the keyboard, focus the handle and use the arrow keys (Shift for bigger
  steps). The place is kept as a fraction of the free space, so the bar stays inside the window
  when the window changes size. The Buttons tab also has the four corners, "Where I put it" and
  "Back to top right". The settings panel opens next to the bar, on the side with room, and is a
  sheet at the bottom of the window under 520 pixels.
- **Screenshot button**: the camera button in the bar saves the screen as a PNG file
  (`xsrv-web-YYYYMMDD-HHMMSS.png`, the local time of the browser); Shift+click copies the image to
  the clipboard (Chromium and Safari; Firefox refuses image clipboard writes without a setting, and
  the message line then says so). The page takes the pixels of its canvas, so the screenshot has the
  size of the screen, not of the browser window. A check box in the Buttons tab hides the button.
- **Replace a key** (Keyboard tab): "when I press the first key, the screen gets the second one",
  for example Right Ctrl as Win. Click each box and press the key. The Win / Cmd choice above wins
  when it names the same key. Only single keys are replaced; use a macro for a combination.
- **Macros** (Macros tab): a list of named step lists that send keys to the screen, for what the
  browser or the system keep (see below) or for a routine. A macro starts from a hotkey, from a
  button in the bar, or from its Run button. Steps are written one per line or separated by commas:
  - a key combination: `Super+e`, `Ctrl+Alt+t`, `Return`. The names are `ctrl`, `alt`, `shift`,
    `super` (also `win`, `cmd`), `altgr`, letters, digits, `f1` to `f24`, `enter`, `esc`, `tab`,
    `space`, `backspace`, `delete`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`,
    `pagedown`, `menu` and the `KeyboardEvent.code` names (`KeyA`, `ArrowUp`). The keys go down in
    order and up in reverse.
  - `wait 300`: a pause in milliseconds (at most 5000; at most 30 s in all).
  - `paste: some text`: puts the text on the X clipboard and sends Shift+Insert. It takes the rest
    of the line. It works with every layout and with accents, but only where Shift+Insert pastes.

  A macro has at most 50 steps and a name of at most 40 letters; there are at most 32 macros. The
  hotkey needs Ctrl, Alt, Shift or Win with the key (a function key may stand alone), because a
  bare key would never reach the screen. When it is pressed, the keys that were down go up first
  and their key-ups are dropped, then the steps run in order with the other keys. A second press
  of a hotkey (or a click on its button) while the macro runs, and a loss of focus, stop it. A
  browser or system combination such as Ctrl+W or the Windows key cannot be a hotkey: the page
  never sees it.
  A button of the bar sends its keys while the pointer is over the bar, outside the screen. With a
  window manager that sets the focus (every one tried), the keys go to the focused window. Without
  one, X sends keys to the window under the pointer, and nothing is under it: use the hotkey.

When a page connects, a kept layout that differs from the session is sent once. Without storage
(private window, blocked site data) the panel still works for that page.

Checked in headless Chromium on the VM: the panel opens, the layout changes to Spanish for the
session, the focus goes back to the screen, and after a reload Spanish and 2x come back from
storage. Not checked with a real X client: the wheel speed and the key buttons (only the frames that
the page sends are tested, in `make test-web`).

## Touch

On a touch screen, one finger is the left button: touching presses it, moving drags, and lifting
releases it. Two fingers that move up or down scroll (one wheel step for each 40 pixels). A
second finger that lands during a drag releases the button first. There is no right button or
long press.

A tap (touch and lift) is a click. The button goes down at the first move, so a second finger
that lands first makes a scroll and no click.

Tested with fake touch events in `make test-web`, and with touch events in headless Chromium
(a tap gave one click, and a two-finger move up gave three wheel steps and no click). Not tested
on a real touch screen.

## Keys that the browser keeps (Escape, the Windows key, AltGr on Windows)

- A browser and the operating system keep some keys for themselves. Windows takes the combinations
  of the Windows key (`Win+Enter`) before the page sees them, and a browser uses Escape to leave full
  screen. The page cannot get them in a normal window.
- **Full screen with a keyboard lock** (Chromium, Edge): the ⛶ button in the bar (a second click leaves
  full screen; a double click does nothing, so a double click on a window under the bar does not
  switch the page to full screen) makes the page
  full screen and locks `Escape`, `MetaLeft`,
  `MetaRight`, `AltLeft`, `AltRight`, `Tab` and `ContextMenu`, so that they go to the screen. To leave
  full screen, hold Escape. Firefox and Safari have no keyboard lock: there those keys stay with the
  browser. Chromium grants the lock only for full screen that the page started: F11 or a maximized
  window is not enough, and Windows then opens its Start menu on the Windows key. The setting
  "Full screen with the keyboard locked at my first click" (Keyboard tab) starts it at the first
  click on the screen. When the Windows key reaches the page without the lock, the message line
  says what to do, once.
- **AltGr on Windows**: the browser sends ControlLeft and then AltRight for AltGr. The page sees that
  pair (AltRight with the AltGraph state, and a ControlLeft within 30 ms) and lets go of the fake
  Control, so the server sees AltGr alone. Tested with simulated events (`make test-web`); not with a
  real Windows keyboard.
- With the default layout (`us`) the right Alt key is Alt and there is no AltGr. Set `XSRV_KEYMAP=es`
  (or `gb`, `de`) on the server for AltGr symbols.
- Checked in headless Chromium: Escape reaches X as keycode 9, and `Meta` with a letter reaches X as
  the Super key (keycode 133) and the letter with the Mod4 bit set.
- **Another key as the Super key**: add `?super=<KeyboardEvent.code>` to the page address, for example
  `http://localhost:18088/?token=...&super=ControlRight`. That key is then sent as the Super
  (Windows) key, so `ControlRight+Enter` does what `Win+Enter` would in a window manager. Use a key
  that the browser and the system do not keep for themselves. The other way is to set the manager to
  another modifier (in i3: `set $mod Mod1` for Alt).
- **The Win button in the bar** (or Win / Cmd in the settings panel): click it and then press the key. The
  page sends Super_L down, then your key with Super held, then Super_L up after that key. A second click
  cancels. It works in every browser, and on a touch screen, which has no keyboard. A modifier key
  (Shift, Ctrl, Alt) does not end it, so Win / Cmd then `Shift+Q` gives `Super+Shift+Q`.
  Checked on a VM with a personal i3 configuration (`$mod` is Mod4), with the old ⌘ button: it then `Shift+Q` closed the
  window as `$mod+Shift+q kill` says. (`$mod+Return` there runs `urxvt`, which is not installed.)

