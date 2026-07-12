import test from "node:test";
import assert from "node:assert/strict";
import { createSerializedCaptureSetter } from "../src/hotkeyCapture.ts";

test("capture transitions cannot finish out of order", async () => {
  const calls = [];
  let releaseEnable;
  const enableBlocked = new Promise((resolve) => {
    releaseEnable = resolve;
  });
  const setCapture = createSerializedCaptureSetter(async (active) => {
    calls.push(active);
    if (active) await enableBlocked;
  });

  const enabling = setCapture(true);
  const disabling = setCapture(false);
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual(calls, [true]);

  releaseEnable();
  await Promise.all([enabling, disabling]);
  assert.deepEqual(calls, [true, false]);
});

test("a failed capture transition does not block the final disable", async () => {
  const calls = [];
  const setCapture = createSerializedCaptureSetter(async (active) => {
    calls.push(active);
    if (active) throw new Error("renderer closed during enable");
  });

  await assert.rejects(setCapture(true));
  await setCapture(false);
  assert.deepEqual(calls, [true, false]);
});
