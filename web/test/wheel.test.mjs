// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

import { wheelAxis } from "../wheel.js";

const check = (label, actual, expected) => {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  console.log(ok ? "ok  " : "FAIL", label, ok ? "" : JSON.stringify(actual) + " != " + JSON.stringify(expected));
  if (!ok) process.exitCode = 1;
};
const PIXEL = 0;
const LINE = 1;

{
  const axis = wheelAxis();
  check("a mouse notch (100 px) is one click", axis.clicks(100, PIXEL, 1, 0), 1);
  check("another notch soon after is one more", axis.clicks(100, PIXEL, 1, 50), 1);
  check("three lines (Firefox notch) after a pause are one click", axis.clicks(3, LINE, 1, 1000), 1);
}
{
  const axis = wheelAxis();
  check("a small first delta after a pause still scrolls once", axis.clicks(4, PIXEL, 1, 0), 1);
  const steps = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100].map((at) => axis.clicks(10, PIXEL, 1, at));
  check("then a touchpad scrolls by distance: 100 px is one click", steps.reduce((a, b) => a + b), 1);
}
{
  const axis = wheelAxis();
  check("speed 2 doubles the clicks", axis.clicks(200, PIXEL, 2, 0), 4);
  check("speed 0.5 halves them", wheelAxis().clicks(400, PIXEL, 0.5, 0), 2);
}
{
  const axis = wheelAxis();
  axis.clicks(50, PIXEL, 1, 0);
  check("up is negative and a change of direction starts again", axis.clicks(-150, PIXEL, 1, 10), -1);
  check("no delta is no click", axis.clicks(0, PIXEL, 1, 20), 0);
}
