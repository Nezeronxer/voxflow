import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import * as ts from "typescript";

const source = await readFile(
  new URL("../src/overlayPreviewState.ts", import.meta.url),
  "utf8",
);
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2020,
  },
}).outputText;
const previewState = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`
);

test("idle-before-final keeps the exact inserted text visible and latches the generation", () => {
  const exactInsertedText = "  Точный финал.\nВторая строка  ";
  const resolved = previewState.resolveOverlayPreviewEvent("idle", 41, -1, {
    text: exactInsertedText,
    committed: "другой черновик",
    volatile: "",
    final: true,
    seq: 41,
  });

  assert.equal(resolved.currentSeq, 41);
  assert.equal(resolved.finalSeq, 41);
  assert.equal(resolved.preview.text, exactInsertedText);
  assert.equal(resolved.preview.committedLen, Array.from(exactInsertedText).length);
  assert.equal(resolved.preview.holdFinal, true);
  assert.equal(previewState.previewPillMode("idle", true, true), "stream");
  assert.equal(previewState.previewPillMode("transcribing", true, false), "stream");
  assert.equal(previewState.previewPillMode("transcribing", false, false), "trans");
  assert.equal(previewState.shouldResetFinalPreviewAfterHold("idle"), true);
});

test("a detached live partial cannot replace the final text for the same seq", () => {
  const lateDraft = previewState.resolveOverlayPreviewEvent("idle", 41, 41, {
    text: "устаревший черновик",
    committed: "устаревший черновик",
    volatile: "",
    seq: 41,
  });

  assert.equal(lateDraft.currentSeq, 41);
  assert.equal(lateDraft.finalSeq, 41);
  assert.equal(lateDraft.preview, null);
});

test("stale final and idle settled previews remain rejected", () => {
  const staleFinal = previewState.resolveOverlayPreviewEvent("idle", 42, 42, {
    text: "старый финал",
    final: true,
    seq: 41,
  });
  const idleSettled = previewState.resolveOverlayPreviewEvent("idle", 42, 42, {
    text: "не финал",
    settled: true,
    seq: 42,
  });

  assert.equal(staleFinal.preview, null);
  assert.equal(idleSettled.preview, null);
  assert.equal(previewState.shouldResetFinalPreviewAfterHold("recording"), false);
});
