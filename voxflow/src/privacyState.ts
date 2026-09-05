/**
 * Что при текущих настройках реально уходит с устройства.
 *
 * Подвал настроек до 2.0.19 писал «Локальный режим · данные не покидают
 * устройство» статическим текстом — независимо от того, включён ли облачный
 * ключ. При настроенном BYOK это была прямая неправда, и PRODUCT.md называет
 * такие обещания недопустимыми.
 *
 * Условия зеркалят бэкенд: `potentially_remote_rewrite` в engine.rs считает
 * рерайт удалённым по тем же трём веткам (gemini / openai_compat вне петли /
 * ollama вне петли). Если правило меняется там — менять и здесь, иначе подвал
 * снова начнёт врать.
 *
 * Отдельный .ts (не .tsx), как aiProviders.ts и settingsSync.ts: чистая логика
 * без JSX импортируется напрямую в `node --test`.
 */
// Явное расширение: тот же модуль грузит и Vite, и `node --test` (ESM-загрузчик
// Node расширения не достраивает). В tsconfig для этого включён
// allowImportingTsExtensions.
import { providerFromBaseUrl } from "./aiProviders.ts";
import type { Settings } from "./types.ts";

export type EgressState = {
  /** Ни аудио, ни текст не покидают устройство. */
  local: boolean;
  /** Сервисы, получающие звук диктовки. */
  audioServices: string[];
  /** Сервисы, получающие распознанный текст. */
  textServices: string[];
  /** Готовая строка для подвала. */
  summary: string;
};

/** Петля — это «никуда не уходит»: локальный сервер живёт на этой же машине. */
export function isLoopbackUrl(url: string): boolean {
  const host = hostOf(url).toLowerCase();
  return host === "localhost" || host === "127.0.0.1" || host === "[::1]" || host === "::1";
}

/** Хост из URL. Мусор и пустая строка дают пустую строку, а не исключение. */
export function hostOf(url: string): string {
  const value = url.trim();
  if (!value) return "";
  try {
    return new URL(value).hostname;
  } catch {
    return "";
  }
}

/** Человекочитаемое имя сервиса по адресу: пресет — по названию, чужой — по хосту. */
function serviceLabel(baseUrl: string): string {
  const preset = providerFromBaseUrl(baseUrl);
  if (preset.value !== "custom" && preset.baseUrl) return preset.label;
  return hostOf(baseUrl) || "внешний сервис";
}

function pushUnique(list: string[], value: string) {
  if (value && !list.includes(value)) list.push(value);
}

export function egressState(s: Settings): EgressState {
  const audioServices: string[] = [];
  const textServices: string[] = [];

  // ── Аудио ──
  if (s.stt_provider === "openai_compat") {
    pushUnique(audioServices, serviceLabel(s.oai_stt_base_url));
  } else if (s.stt_provider === "deepgram") {
    pushUnique(audioServices, "Deepgram");
  }
  // Облачное распознавание Gemini включается отдельным флагом.
  if (s.cloud_asr && s.ai_backend === "gemini") {
    pushUnique(audioServices, "Google Gemini");
  }

  // ── Текст (постобработка) ──
  if (s.ai_backend === "gemini") {
    pushUnique(textServices, "Google Gemini");
  } else if (s.ai_backend === "openai_compat") {
    // Пустой адрес — бэкенд ещё не настроен, никуда не ходит.
    if (s.rewrite_base_url.trim() && !isLoopbackUrl(s.rewrite_base_url)) {
      pushUnique(textServices, serviceLabel(s.rewrite_base_url));
    }
  } else if (s.ai_backend === "ollama") {
    if (s.ollama_url.trim() && !isLoopbackUrl(s.ollama_url)) {
      pushUnique(textServices, hostOf(s.ollama_url) || "внешний сервис");
    }
  }

  const local = audioServices.length === 0 && textServices.length === 0;
  return {
    local,
    audioServices,
    textServices,
    summary: summarize(local, audioServices, textServices),
  };
}

function summarize(local: boolean, audio: string[], text: string[]): string {
  if (local) return "Локально · ничего не уходит с устройства";
  const all = [...new Set([...audio, ...text])];
  // Один и тот же сервис на оба канала — не перечисляем его дважды.
  if (audio.length > 0 && text.length > 0) {
    return all.length === 1
      ? `Аудио и текст уходят в ${all[0]}`
      : `Аудио → ${audio.join(", ")} · текст → ${text.join(", ")}`;
  }
  if (audio.length > 0) return `Аудио диктовки уходит в ${audio.join(", ")}`;
  return `Текст диктовки уходит в ${text.join(", ")}`;
}
