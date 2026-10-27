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

test("a detached settled preview cannot replace the final text for the same seq", () => {
  const lateSettled = previewState.resolveOverlayPreviewEvent("transcribing", 41, 41, {
    text: "устаревший settled-черновик",
    committed: "устаревший settled-черновик",
    volatile: "",
    settled: true,
    seq: 41,
  });

  assert.equal(lateSettled.currentSeq, 41);
  assert.equal(lateSettled.finalSeq, 41);
  assert.equal(lateSettled.preview, null);
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

test("word stream keeps the stable prefix and re-animates only the rewritten tail", () => {
  const before = previewState.wordTokens("добавь авто");
  const after = previewState.wordTokens("добавь автоматические тесты");
  assert.deepEqual(after, ["добавь ", "автоматические ", "тесты"]);
  // «авто» переписано в «автоматические» — анимируется с него, «добавь» стоит.
  assert.equal(previewState.sharedWordPrefix(before, after), 1);
  // Появившийся хвостовой пробел не считается правкой слова.
  const grown = previewState.wordTokens("добавь автоматические тесты для");
  assert.equal(previewState.sharedWordPrefix(after, grown), 3);
  assert.equal(previewState.sharedWordPrefix(grown, []), 0);
  assert.deepEqual(previewState.wordTokens("  "), []);
});

test("meta label shows recognizer and AI model, warns when missing", () => {
  const label = (model, ai) =>
    previewState.overlayMetaLabel(model, ai)?.map((p) => (p.warn ? `!${p.text}` : p.text)).join(" · ") ?? null;
  assert.equal(label("GigaAM", { kind: "local", name: "gemma4:e2b" }), "GigaAM · ИИ gemma4:e2b");
  assert.equal(label("Облако", { kind: "off", name: null }), "Облако · ИИ выкл.");
  assert.equal(label("GigaAM", { kind: "missing", name: null }), "GigaAM · !ИИ не найдена");
  assert.equal(label("Parakeet", { kind: "cloud" }), "Parakeet · ИИ облако");
  assert.equal(label(null, { kind: "local", name: "" }), "!Нет модели · ИИ локально");
  assert.equal(label("  ", undefined), "!Нет модели");
  // Старый бэкенд без полей - метки нет.
  assert.equal(label(undefined, undefined), null);
  assert.equal(label(undefined, { kind: "unknown" }), null);
});

test("raw preview is hidden until final only when AI rewrites the text", () => {
  assert.equal(previewState.hidesRawUntilFinal({ kind: "cloud" }), true);
  assert.equal(previewState.hidesRawUntilFinal({ kind: "local", name: "gemma4:e2b" }), true);
  assert.equal(previewState.hidesRawUntilFinal({ kind: "off" }), false);
  assert.equal(previewState.hidesRawUntilFinal({ kind: "missing" }), false);
  assert.equal(previewState.hidesRawUntilFinal(null), false);
  assert.equal(previewState.hidesRawUntilFinal(undefined), false);
});
