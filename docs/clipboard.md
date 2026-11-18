# Clipboard

Text, HTML and a PNG image go both ways between the browser and the X clients.

| Format | Limit | Notes |
|---|---|---|
| Text (UTF-8) | 4 MiB | `UTF8_STRING`; `STRING` (Latin-1) when that is all an X program has |
| HTML | 4 MiB | `text/html` |
| Image | 16 MiB | PNG (`image/png`); the page turns a JPEG or another image into PNG |

One copy or one paste is one message (`wire::KIND_CLIPBOARD_SET` from X to the page,
`KIND_CLIPBOARD_SET_IN` from the page; a text-only paste still uses the short `KIND_CLIPBOARD_IN`).
The formats are in `crates/wire/src/clipboard.rs`.

## Browser to X client

The user presses Ctrl+V, Ctrl+Shift+V or Shift+Insert on the page. The page reads what the browser
clipboard holds and sends it before the key. The server makes a hidden window the owner of CLIPBOARD
and PRIMARY and answers `TARGETS`, `UTF8_STRING`, `STRING`, and `text/html` and `image/png` when the
clipboard has them. The X program asks for the format it likes.

- The page gets the content from the `paste` event, which needs no permission in any browser. Chromium
  gives plain text only for Ctrl+Shift+V (that key means "paste as plain text"); use Ctrl+V or
  Shift+Insert to paste HTML. When the event is empty (some browsers leave an image out of it), the
  page asks the Clipboard API, which may ask the user once for permission.
- Content that is too big, or that has no text, HTML or image (files, for example), makes X paste
  **nothing**, and the message line says why. Before, the previous text was pasted without a word.
- A clipboard with nothing at all is not sent: the X clipboard stays as it is.
- The same content again does not take the selections from an X program that holds it (a repeated
  Ctrl+V must not clear the selection of a terminal). Different content always does.

`navigator.clipboard` works on `localhost` and on HTTPS pages only. On a page with plain HTTP the message line and Settings > Clipboard say so; Ctrl+V still pastes into X programs.

## X client to browser

When an X program becomes the owner of CLIPBOARD or PRIMARY, the server asks it what it offers
(`TARGETS`, then the text, and for CLIPBOARD also HTML and an image) and sends one message to the
pages. Long content comes by the `INCR` protocol (the owner sends chunks); the server receives it up
to the limits above and leaves out a format that is too big. An owner that never answers is dropped
after five seconds. Empty text is a copy of nothing and changes nothing.

**The browser lets a page write the clipboard only after the user acted on the page in the last five
seconds, unless the user allowed clipboard access once.** A copy in an X program often comes later
(a long selection, a program that copies by itself), and then the browser refuses with
"Write permission denied". Measured in Chromium: with the default permissions a copy made 7 seconds
after the last click was refused, and with the clipboard permission granted it always went through.
The page therefore asks for it: Settings > Clipboard shows the state and a button *Allow clipboard
access* (it calls `navigator.clipboard.readText()` inside the click, which makes the browser ask once;
the answer is remembered by the browser). When the access is granted, a copy that was waiting goes
out at once (with a few retries, because the browser applies the change a moment after it tells).
Firefox and Safari cannot say; there the copy goes out at the next click or key.

The page writes the set with `navigator.clipboard.write` (one `ClipboardItem` with every format). A
browser that refuses a type gets the text alone. A browser that refuses for lack of a user action
(Firefox, Safari) makes the set wait for the next click or key, and the message line says so.

- **Selection = copy.** Text selected with the mouse (PRIMARY) arrives marked as such. The setting
  "Selecting text in a program also copies it to the clipboard" (Mouse tab, on by default) decides
  whether it replaces the browser clipboard. With it off, a selection never overwrites a Ctrl+C.
  A middle click in a program pastes the selection, as on any X server; the browser clipboard
  cannot be read on a middle click (a browser allows that only with a key or a permission).

## Tested

- Integration tests with x11rb (`tests/x11_core.rs`): text, HTML and a 2 MiB image in one set; a 300 KB
  text by `INCR`; a `STRING`-only owner; an owner that refuses `TARGETS`; a text over the limit left
  out; empty text ignored; PRIMARY marked as such; a set from the page served with HTML and image;
  an empty set that pastes nothing.
- Page logic with fake clipboards (`make test-web`).
- Chromium (Playwright) against the Debian package on a VM with `xfce4-terminal` and `xclip`: an image
  copied in X reaches the browser as `image/png`; HTML copied in X reaches the browser; an image in
  the browser is served to X as `image/png`; text and HTML together reach X; an image-only clipboard
  leaves no old text; a 5 MB text gives the "too large" message and X pastes nothing; a 1 MiB text
  reaches a terminal whole; the selection setting on and off.
- Not tested: the real permission prompt and the user-gesture rules of a normal Firefox or Safari
  window; a real Windows clipboard with Office content.

## Limits

- The hidden window (id `0x30`) is a child of the root and is never mapped.
- A page that connects later does not get what was copied earlier.
- No files, no images other than PNG on the X side (BMP and JPEG targets), no `text/uri-list`.
- `INCR` from the page to X is not used: the server writes the property itself, which has no request
  size limit.

## Which keys paste and copy

- **Paste into a terminal.** Shift+Insert and Ctrl+Shift+V both paste: the page sends the browser
  content to the server, and for Ctrl+Shift+V it sends Shift+Insert to the program (released Control,
  Insert down and up), because `xterm` and many X programs know only Shift+Insert. Plain Ctrl+V is
  sent as it is: `xterm` takes it as a literal-next character, as on any X server, and GUI programs
  paste with it.
- **Copy from a terminal.** A copy command (Ctrl+Shift+C in most terminals) sets CLIPBOARD. Selecting
  text sets PRIMARY, which reaches the browser clipboard only with the setting above.
- **Permission prompt.** The browser asks once for the clipboard access (Settings > Clipboard, or when
  the page has to read the clipboard because the paste event was empty). The keys do not wait more
  than 1.5 seconds for the answer, which is used when it comes.

## How the page reads the browser clipboard (paste)

- The paste keys keep their default action, and a hidden text box takes the focus while the keys are
  down (Firefox fires `paste` only at an editable element). The canvas gets the focus back when the
  content is in.
- If no `paste` event comes in 150 ms, or it is empty, the page reads the clipboard with the
  Clipboard API (`read()`, or `readText()` in a browser without it).

## Copied content and the browser (copy)

A browser may refuse the write when the user did not just act. The page then keeps the latest set,
shows the message line, and writes it at the next click, key or touch of the user, inside that event.
