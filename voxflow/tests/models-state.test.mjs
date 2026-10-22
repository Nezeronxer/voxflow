import test from "node:test";
import assert from "node:assert/strict";
import {
  fmtSize,
  modelSizeText,
  isUnused,
  deleteConfirmText,
} from "../src/modelsState.ts";

const fp32 = {
  name: "gigaam-v3-fp32",
  label: "GigaAM-v3 полная - точнее (885 МБ)",
  size_mb: 885,
  installed: true,
  kind: "gigaam",
  disk_mb: 851,
  in_use: true,
};

test("скачанная модель показывает настоящий размер на диске", () => {
  assert.equal(modelSizeText(fp32), "851 МБ на диске");
  assert.equal(modelSizeText({ ...fp32, disk_mb: 3100 }), "3.0 ГБ на диске");
  // Не скачана или старый бэкенд без поля - размер из каталога.
  assert.equal(modelSizeText({ ...fp32, installed: false, disk_mb: null }), "885 МБ");
  assert.equal(modelSizeText({ ...fp32, disk_mb: undefined }), "885 МБ");
  assert.equal(fmtSize(1024), "1.0 ГБ");
});

test("метка «Не используется» только у скачанной и ненужной модели", () => {
  assert.equal(isUnused({ ...fp32, in_use: false }), true);
  assert.equal(isUnused(fp32), false);
  assert.equal(isUnused({ ...fp32, installed: false, in_use: false }), false);
  // Старый бэкенд не знает поля - ничего не утверждаем.
  assert.equal(isUnused({ ...fp32, in_use: undefined }), false);
});

test("подтверждение удаления называет модель, размер и последствия", () => {
  const unused = deleteConfirmText({ ...fp32, in_use: false });
  assert.match(unused, /GigaAM-v3 полная/);
  assert.match(unused, /851 МБ/);
  assert.doesNotMatch(unused, /сейчас используется/);

  const used = deleteConfirmText(fp32);
  assert.match(used, /сейчас используется/);

  const reserve = deleteConfirmText({
    ...fp32,
    name: "gigaam-v3",
    label: "GigaAM-v3 — быстрый русский",
    disk_mb: 217,
    in_use: false,
  });
  assert.match(reserve, /запасн/);
});
