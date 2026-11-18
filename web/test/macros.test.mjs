// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import {
  applyRemap, cleanMacros, cleanSubstitutions, codeFor, comboFromEvent, formatHotkey,
  hotkeyProblem, keyLabel, parseCombo, parseSteps,
} from "../macros.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};

check("a letter name", codeFor("e"), "KeyE");
check("a digit name", codeFor("7"), "Digit7");
check("a function key", codeFor("f13"), "F13");
check("Win has several names", [codeFor("super"), codeFor("Win"), codeFor("cmd")], ["MetaLeft", "MetaLeft", "MetaLeft"]);
check("a raw code is accepted", codeFor("ArrowUp"), "ArrowUp");
check("an unknown name", codeFor("ctrll"), undefined);
check("labels", [keyLabel("KeyM"), keyLabel("Digit3"), keyLabel("MetaLeft"), keyLabel("F5")], ["M", "3", "Win / Cmd", "F5"]);

check("a combo", parseCombo("Ctrl+Alt+t"), { codes: ["ControlLeft", "AltLeft", "KeyT"] });
check("a bad key in a combo", parseCombo("Ctrl+Foo").error, '"Foo" is not a key name.');
check("an empty key in a combo", parseCombo("Ctrl+").error, '"" is not a key name.');
check("too many keys", parseCombo("a+b+c+d+e+f+g").error, "A combo has at most 6 keys.");

check(
  "steps on one line",
  parseSteps("Super+e, wait 200, Return").steps,
  [
    { type: "combo", codes: ["MetaLeft", "KeyE"] },
    { type: "wait", ms: 200 },
    { type: "combo", codes: ["Enter"] },
  ],
);
check(
  "steps on lines, and paste takes the rest of its line",
  parseSteps("Ctrl+l\npaste: héllo, wörld; ok\nReturn").steps,
  [
    { type: "combo", codes: ["ControlLeft", "KeyL"] },
    { type: "paste", text: "héllo, wörld; ok" },
    { type: "combo", codes: ["Enter"] },
  ],
);
check("a wait with ms", parseSteps("wait 50 ms").steps, [{ type: "wait", ms: 50 }]);
check("an empty macro", parseSteps("  \n ,, ").error, "A macro needs at least one step.");
check("the step number in an error", parseSteps("a, b, nope").error, 'Step 3: "nope" is not a key name.');
check("a long wait", parseSteps("wait 6000").error, "Step 1: a wait is at most 5000 ms.");
check(
  "waits that add up",
  parseSteps(Array(7).fill("wait 5000").join(",")).error,
  "The waits add up to more than 30 seconds.",
);
check("paste needs a text", parseSteps("paste:").error, "Step 1: paste: needs a text.");
check("a big paste", parseSteps("paste: " + "é".repeat(2100)).error, "Step 1: the text is longer than 4096 bytes.");
check("too many steps", parseSteps(Array(51).fill("a").join(",")).error, "A macro has at most 50 steps.");

const ev = (extra) => ({ code: "KeyM", ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...extra });
check("hotkey of an event", comboFromEvent(ev({ ctrlKey: true, altKey: true })), "Ctrl+Alt+KeyM");
check("modifiers go in a fixed order", comboFromEvent(ev({ metaKey: true, shiftKey: true, ctrlKey: true })), "Ctrl+Shift+Meta+KeyM");
check("a modifier alone is not a hotkey", comboFromEvent(ev({ code: "ControlLeft", ctrlKey: true })), null);
check("a plain letter is refused", hotkeyProblem("KeyM"), "Use at least one of Ctrl, Alt, Shift or Win with this key.");
check("a function key may stand alone", hotkeyProblem("F9"), "");
check("a modified letter is fine", hotkeyProblem("Ctrl+KeyM"), "");
check("an unknown key", hotkeyProblem("Ctrl+Nope"), "That is not a usable key.");
check("hotkey label", formatHotkey("Ctrl+Meta+KeyM"), "Ctrl+Win+M");

{
  const remap = { Old: "Gone" };
  const same = applyRemap(remap, "ControlRight", [{ from: "KeyQ", to: "KeyA" }]);
  check("remap is rebuilt in place", [same === remap, remap], [true, { KeyQ: "KeyA", ControlRight: "MetaLeft" }]);
  applyRemap(remap, "KeyQ", [{ from: "KeyQ", to: "KeyA" }]);
  check("the Win key choice wins a conflict", remap, { KeyQ: "MetaLeft" });
  applyRemap(remap, "", []);
  check("nothing is left when nothing is set", remap, {});
}
check(
  "substitutions from storage are cleaned",
  cleanSubstitutions([
    { from: "KeyQ", to: "KeyA" }, { from: "KeyQ", to: "KeyB" }, { from: "KeyZ", to: "KeyZ" },
    { from: "Nope", to: "KeyA" }, null,
  ]),
  { items: [{ from: "KeyQ", to: "KeyB" }], dropped: 3 },
);
check("not a list", cleanSubstitutions("x"), { items: [], dropped: 0 });
check(
  "macros from storage are cleaned",
  cleanMacros([
    { name: "Files", hotkey: "Ctrl+Alt+KeyE", steps: "Super+e", bar: true },
    { name: "Same key", hotkey: "Ctrl+Alt+KeyE", steps: "a" },
    { name: "Bad key", hotkey: "KeyM", steps: "a" },
    { name: "", steps: "a" },
    { name: "Broken", steps: "nope" },
  ]),
  {
    items: [
      { name: "Files", hotkey: "Ctrl+Alt+KeyE", steps: "Super+e", bar: true },
      { name: "Same key", hotkey: "", steps: "a", bar: false },
      { name: "Bad key", hotkey: "", steps: "a", bar: false },
    ],
    dropped: 2,
  },
);
