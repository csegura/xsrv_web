// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// The key replacements and the macros of the settings panel, and the macro buttons of the bar.
import {
  MAX_MACROS, MAX_NAME_LENGTH, MAX_SUBSTITUTIONS, comboFromEvent, formatHotkey, hotkeyProblem,
  keyLabel, parseSteps,
} from "./macros.js";

const hasModifier = (event) => event.ctrlKey || event.altKey || event.shiftKey || event.metaKey;

// A box that records the key pressed in it. `read(event)` gives the value to keep (a
// KeyboardEvent.code, or a hotkey text) and its label, or null when the press is not usable.
function captureBox(box, read, { clearable = false } = {}) {
  const set = (value, label) => {
    box.dataset.value = value;
    box.value = label;
  };
  box.addEventListener("focus", () => box.classList.add("waiting"));
  box.addEventListener("blur", () => box.classList.remove("waiting"));
  box.addEventListener("keydown", (event) => {
    if (event.key === "Tab") return;
    event.preventDefault();
    // Escape stops the capture and must not close the panel.
    event.stopPropagation();
    if (event.key === "Escape" && !hasModifier(event)) return box.blur();
    if (clearable && ["Backspace", "Delete"].includes(event.key) && !hasModifier(event)) {
      return set("", "");
    }
    const result = read(event);
    if (result) set(result.value, result.label);
  });
  return { set, value: () => box.dataset.value ?? "" };
}

const cell = (tag, text, className) => {
  const element = document.createElement(tag);
  element.textContent = text;
  if (className) element.className = className;
  return element;
};

const actionButton = (text, handler, className = "") => {
  const button = cell("button", text, className);
  button.type = "button";
  button.addEventListener("click", handler);
  return button;
};

// `saved` is the settings object and `persist()` writes it. `onKeysChanged()` runs after a
// replacement is added or removed, `onBarChanged()` after the macro buttons change, and
// `afterRun()` after a macro was started from the panel.
export function createMacroUi({ saved, persist, input, canvas, onKeysChanged, onBarChanged, afterRun }) {
  const subsList = document.getElementById("subs");
  const subError = document.getElementById("sub-error");
  const subFrom = captureBox(document.getElementById("sub-from"), (event) => ({
    value: event.code,
    label: keyLabel(event.code),
  }));
  const subTo = captureBox(document.getElementById("sub-to"), (event) => ({
    value: event.code,
    label: keyLabel(event.code),
  }));

  const renderSubs = () => {
    subsList.replaceChildren(
      ...saved.substitutions.map((sub, index) => {
        const item = document.createElement("li");
        item.append(
          cell("span", `${keyLabel(sub.from)}  →  ${keyLabel(sub.to)}`, "grow"),
          actionButton("Remove", () => {
            saved.substitutions.splice(index, 1);
            persist();
            renderSubs();
            onKeysChanged();
          }),
        );
        return item;
      }),
    );
  };
  document.getElementById("sub-add").addEventListener("click", () => {
    const from = subFrom.value();
    const to = subTo.value();
    subError.textContent = "";
    subError.classList.remove("notice");
    const earlier = saved.substitutions.find((sub) => sub.from === from);
    if (!from || !to) {
      subError.textContent = "Click each box and press a key.";
    } else if (from === to) {
      subError.textContent = "A key cannot replace itself.";
    } else if (!saved.substitutions.some((sub) => sub.from === from) && saved.substitutions.length >= MAX_SUBSTITUTIONS) {
      subError.textContent = `At most ${MAX_SUBSTITUTIONS} replacements.`;
    } else {
      saved.substitutions = saved.substitutions.filter((sub) => sub.from !== from);
      saved.substitutions.push({ from, to });
      if (earlier && earlier.to !== to) {
        subError.classList.add("notice");
        subError.textContent = `${keyLabel(from)} sent ${keyLabel(earlier.to)} before; now it sends ${keyLabel(to)}.`;
      }
      persist();
      subFrom.set("", "");
      subTo.set("", "");
      renderSubs();
      onKeysChanged();
    }
  });

  // Macros
  const list = document.getElementById("macro-list");
  const form = document.getElementById("macro-form");
  const title = document.getElementById("macro-form-title");
  const nameBox = document.getElementById("macro-name");
  const stepsBox = document.getElementById("macro-steps");
  const barBox = document.getElementById("macro-bar");
  const cancel = document.getElementById("macro-cancel");
  const error = document.getElementById("macro-error");
  const barButtons = document.getElementById("macro-buttons");
  const hotkeyBox = captureBox(
    document.getElementById("macro-hotkey"),
    (event) => {
      const combo = comboFromEvent(event);
      if (combo === null) return null;
      // A hotkey that cannot be used is refused now, not when the form is saved.
      error.textContent = hotkeyProblem(combo);
      return error.textContent ? null : { value: combo, label: formatHotkey(combo) };
    },
    { clearable: true },
  );
  let editing = null;

  const run = (macro) => {
    const { steps } = parseSteps(macro.steps);
    if (steps) input.runMacro(steps);
  };
  const activeMacros = () =>
    saved.macros.map((macro) => ({ ...macro, steps: parseSteps(macro.steps).steps }));

  const renderBar = () => {
    barButtons.replaceChildren(
      ...saved.macros
        .filter((macro) => macro.bar)
        .map((macro) => {
          const button = actionButton(macro.name, () => {
            run(macro);
            canvas.focus();
          });
          button.title = macro.hotkey ? `${macro.name} (${formatHotkey(macro.hotkey)})` : macro.name;
          return button;
        }),
    );
    onBarChanged();
  };
  const renderMacros = () => {
    list.replaceChildren(
      ...saved.macros.map((macro, index) => {
        const item = document.createElement("li");
        item.append(
          cell("span", macro.name, "grow"),
          cell("span", macro.hotkey ? formatHotkey(macro.hotkey) : "", "muted"),
          actionButton("Run", () => {
            run(macro);
            afterRun();
          }),
          actionButton("Edit", () => edit(index)),
          actionButton("Delete", () => {
            saved.macros.splice(index, 1);
            if (editing === index) reset();
            persist();
            changed();
          }, "danger"),
        );
        return item;
      }),
    );
  };
  const changed = () => {
    input.setMacros(activeMacros());
    renderMacros();
    renderBar();
  };
  const reset = () => {
    editing = null;
    form.reset();
    hotkeyBox.set("", "");
    title.textContent = "New macro";
    cancel.hidden = true;
    error.textContent = "";
  };
  const edit = (index) => {
    const macro = saved.macros[index];
    editing = index;
    nameBox.value = macro.name;
    stepsBox.value = macro.steps;
    barBox.checked = macro.bar;
    hotkeyBox.set(macro.hotkey, macro.hotkey ? formatHotkey(macro.hotkey) : "");
    title.textContent = `Edit macro: ${macro.name}`;
    cancel.hidden = false;
    error.textContent = "";
    nameBox.focus();
  };
  cancel.addEventListener("click", reset);

  // The first problem in the form, or an empty text.
  const problem = (macro) => {
    if (macro.name === "") return "Give the macro a name.";
    if (macro.name.length > MAX_NAME_LENGTH) return `The name has at most ${MAX_NAME_LENGTH} letters.`;
    const parsed = parseSteps(macro.steps);
    if (parsed.error) return parsed.error;
    if (macro.hotkey) {
      const bad = hotkeyProblem(macro.hotkey);
      if (bad) return bad;
      const other = saved.macros.findIndex((item, index) => index !== editing && item.hotkey === macro.hotkey);
      if (other >= 0) return `${formatHotkey(macro.hotkey)} is the hotkey of "${saved.macros[other].name}".`;
    }
    if (editing === null && saved.macros.length >= MAX_MACROS) return `At most ${MAX_MACROS} macros.`;
    return "";
  };
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const macro = {
      name: nameBox.value.trim(),
      hotkey: hotkeyBox.value(),
      steps: stepsBox.value,
      bar: barBox.checked,
    };
    error.textContent = problem(macro);
    if (error.textContent) return;
    if (editing === null) saved.macros.push(macro);
    else saved.macros[editing] = macro;
    persist();
    reset();
    changed();
  });

  renderSubs();
  changed();
  return {
    // The bar shows that a macro is running.
    setRunning: (running) => {
      for (const button of barButtons.children) button.classList.toggle("running", running);
    },
  };
}
