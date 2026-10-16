import test from "node:test";
import assert from "node:assert/strict";
import { permissionRows, showPermissionsCard } from "../src/permissionsState.ts";

const all = { microphone: "granted", accessibility: true, inputMonitoring: true };

test("карточки нет, пока статус не получен и когда всё выдано", () => {
  assert.equal(showPermissionsCard(null), false);
  assert.equal(showPermissionsCard(all), false);
});

test("карточка видна, если не выдан любой один доступ, и скрыта при остальных выданных", () => {
  assert.equal(showPermissionsCard({ ...all, accessibility: false }), true);
  assert.equal(showPermissionsCard({ ...all, inputMonitoring: false }), true);
  assert.equal(showPermissionsCard({ ...all, microphone: "notDetermined" }), true);
  assert.equal(showPermissionsCard({ ...all, microphone: "denied" }), true);
});

test("три строки в порядке Микрофон, Универсальный доступ, Мониторинг ввода", () => {
  const rows = permissionRows({
    microphone: "denied",
    accessibility: false,
    inputMonitoring: true
  });
  assert.deepEqual(
    rows.map((r) => [r.kind, r.granted]),
    [
      ["microphone", false],
      ["accessibility", false],
      ["input_monitoring", true],
    ],
  );
  assert.match(rows[0].title, /Микрофон/);
  assert.match(rows[1].title, /Универсальный доступ/);
  assert.match(rows[2].title, /Мониторинг ввода/);
});

test("микрофон: до вопроса - «Разрешить», после отказа - в настройки", () => {
  const pick = (microphone) =>
    permissionRows({ ...all, microphone })[0].action;
  assert.equal(pick("notDetermined"), "Разрешить");
  assert.equal(pick("denied"), "Открыть настройки");
});
