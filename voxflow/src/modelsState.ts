/**
 * Экран «Модели»: размер на диске, метка «Не используется» и текст
 * подтверждения удаления. Что нужно при текущих настройках, решает Rust
 * (`list_models` -> `in_use`, D-053); здесь только показ, без JSX.
 */
import type { ModelInfo } from "./types";

export function fmtSize(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} ГБ`;
  return `${mb} МБ`;
}

/** Скачанная - настоящий размер файлов, иначе размер из каталога. */
export function modelSizeText(m: ModelInfo): string {
  if (m.installed && typeof m.disk_mb === "number") {
    return `${fmtSize(m.disk_mb)} на диске`;
  }
  return fmtSize(m.size_mb);
}

/** Скачана, но при текущих настройках не нужна - её можно удалить. */
export function isUnused(m: ModelInfo): boolean {
  return m.installed && m.in_use === false;
}

export function deleteConfirmText(m: ModelInfo): string {
  const size = typeof m.disk_mb === "number" ? m.disk_mb : m.size_mb;
  const parts = [`Удалить «${m.label || m.name}» и освободить ${fmtSize(size)}?`];
  if (m.in_use) {
    parts.push("Модель сейчас используется - распознавание перейдёт на другую или попросит скачать модель.");
  }
  if (m.name === "gigaam-v3") {
    parts.push("Это запасная быстрая GigaAM на случай сбоя полной.");
  }
  if (m.name === "gigaam-v3-fp32") {
    parts.push("Сама полная модель больше не скачается - вернуть её можно кнопкой «Скачать».");
  }
  return parts.join(" ");
}
