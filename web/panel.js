// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

// The settings panel: it opens next to the button bar, has tabs, and closes with Escape.

const GAP = 6;
const EDGE = 8;

// Where the panel goes: below the bar or above it (the side with more room), its edge in line with
// the edge of the bar that is nearer to the side of the window, and always inside the window.
export function anchorPosition(bar, panel, view) {
  const below = view.height - bar.bottom;
  const above = bar.top;
  const fitsBelow = below >= panel.height + GAP;
  const fitsAbove = above >= panel.height + GAP;
  const useBelow = fitsBelow || (!fitsAbove && below >= above);
  const top = useBelow ? bar.bottom + GAP : bar.top - panel.height - GAP;
  const onRight = (bar.left + bar.right) / 2 > view.width / 2;
  const left = onRight ? bar.right - panel.width : bar.left;
  const fit = (value, size, room) => Math.max(EDGE, Math.min(value, room - size - EDGE));
  return { left: fit(left, panel.width, view.width), top: fit(top, panel.height, view.height) };
}

// `tabs` is the element with the tab buttons (`data-pane` names the pane of each).
export function createPanel({ panel, toggle, close, bar, tabs, canvas, firstTab }) {
  const buttons = [...tabs.querySelectorAll("[role=tab]")];
  const panes = new Map(buttons.map((button) => [button.dataset.pane, document.getElementById(button.getAttribute("aria-controls"))]));

  const select = (name) => {
    for (const button of buttons) {
      const on = button.dataset.pane === name;
      button.setAttribute("aria-selected", String(on));
      button.tabIndex = on ? 0 : -1;
      panes.get(button.dataset.pane).hidden = !on;
    }
    anchor();
  };
  tabs.addEventListener("click", (event) => {
    const button = event.target.closest("[role=tab]");
    if (button) select(button.dataset.pane);
  });
  tabs.addEventListener("keydown", (event) => {
    const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
    if (!step) return;
    const next = buttons[(buttons.indexOf(document.activeElement) + step + buttons.length) % buttons.length];
    next.focus();
    select(next.dataset.pane);
    event.preventDefault();
  });

  function anchor() {
    if (panel.hidden) return;
    const rect = bar.getBoundingClientRect();
    const size = { width: panel.offsetWidth, height: panel.offsetHeight };
    const view = { width: window.innerWidth, height: window.innerHeight };
    const { left, top } = anchorPosition(rect, size, view);
    panel.style.left = `${Math.round(left)}px`;
    panel.style.top = `${Math.round(top)}px`;
  }
  const open = () => {
    panel.hidden = false;
    toggle.classList.add("on");
    toggle.setAttribute("aria-expanded", "true");
    anchor();
  };
  const shut = () => {
    panel.hidden = true;
    toggle.classList.remove("on");
    toggle.setAttribute("aria-expanded", "false");
    canvas.focus();
  };
  toggle.addEventListener("click", () => (panel.hidden ? open() : shut()));
  close.addEventListener("click", shut);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !panel.hidden && panel.contains(document.activeElement)) shut();
  });
  window.addEventListener("resize", anchor);
  select(firstTab);
  return { open, close: shut, anchor, select };
}
