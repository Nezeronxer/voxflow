// Defensive wrappers around Tauri commands. Every call is wrapped in try/catch
// so the UI never crashes if a command errors during early boot.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { EventCallback } from "@tauri-apps/api/event";
import type {
  Settings,
  ModelInfo,
  Stats,
  HistoryItem,
  DictionaryEntry,
  SnippetEntry,
  CorrectionEntry,
  ActiveAppContext,
  ProfileOverride,
  TransformResult,
  UpdateInfo,
  UpdateInstallResult,
  SecretStatus,
  SecretKind,
} from "./types";
import { DEFAULT_SETTINGS } from "./types";

/**
 * The production application always runs inside Tauri.  The lightweight web
 * bridge below deliberately exists for visual regression tests and for `vite`
 * previews: previously every screen rendered, but the console was flooded by
 * failed `invoke`/`listen` calls, which made automated UI QA close to useless.
 */
export const IS_TAURI_RUNTIME =
  typeof window !== "undefined" &&
  "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

const mockListeners = new Map<string, Set<EventCallback<unknown>>>();
let mockSettings: Settings = { ...DEFAULT_SETTINGS };
let mockRecording = false;
let mockSeq = 0;
const mockInstalledModels = new Set([
  "gigaam-v3",
  "parakeet-v3",
  "ggml-large-v3-turbo-q5_0.bin",
]);

const MOCK_HISTORY: HistoryItem[] = [
  {
    ts: "2026-07-09 18:42:00",
    text: "Добавь автоматические тесты для Windows и выведи понятное сообщение для пользователя.",
    app: "Codex",
    words: 13,
  },
  {
    ts: "2026-07-09 18:17:00",
    text: "Составь план статьи о лучших практиках локальной разработки с Tauri и Rust.",
    app: "Notion",
    words: 12,
  },
  {
    ts: "2026-07-09 17:53:00",
    text: "Напиши короткое обновление для команды о статусе релиза и следующем шаге.",
    app: "Slack",
    words: 12,
  },
];

function emitMock<T>(event: string, payload: T) {
  const handlers = mockListeners.get(event);
  if (!handlers) return;
  for (const handler of handlers) {
    handler({ event, id: -1, payload } as never);
  }
}

/** Preview-only event injection used by deterministic overlay visual tests. */
export function emitDemoEvent<T>(event: string, payload: T) {
  if (!IS_TAURI_RUNTIME) emitMock(event, payload);
}

// Race-safe обёртка над Tauri listen(). Проблема: listen() асинхронный, а под
// React.StrictMode эффект монтируется → размонтируется → монтируется снова. Если
// промис listen() резолвится УЖЕ ПОСЛЕ cleanup, мы получаем «живой» слушатель без
// способа его снять → утечка и дубли событий. Решение: возвращаем синхронную
// функцию-отписку; пока listen() резолвится, держим локальный флаг cancelled —
// если успели отписаться, тут же зовём настоящий unlisten вместо его хранения.
//
// Использование в useEffect:
//   useEffect(() => {
//     const off = subscribe<Foo>("foo", (e) => { ... });
//     return off;            // или: return () => off();
//   }, []);
export function subscribe<T>(
  event: string,
  handler: EventCallback<T>,
): () => void {
  if (!IS_TAURI_RUNTIME) {
    const handlers = mockListeners.get(event) ?? new Set<EventCallback<unknown>>();
    handlers.add(handler as EventCallback<unknown>);
    mockListeners.set(event, handlers);
    return () => {
      handlers.delete(handler as EventCallback<unknown>);
      if (handlers.size === 0) mockListeners.delete(event);
    };
  }
  let cancelled = false;
  let unlisten: (() => void) | null = null;
  listen<T>(event, handler)
    .then((fn) => {
      if (cancelled) {
        // Эффект уже размонтировался, пока резолвился listen() — снимаем сразу.
        fn();
      } else {
        unlisten = fn;
      }
    })
    .catch((err) => console.warn(`[voxflow] listen(${event}) failed:`, err));
  return () => {
    cancelled = true;
    if (unlisten) {
      unlisten();
      unlisten = null;
    }
  };
}

async function safe<T>(fn: () => Promise<T>, fallback: T): Promise<T> {
  try {
    const r = await fn();
    return r ?? fallback;
  } catch (e) {
    console.warn("[voxflow] command failed:", e);
    return fallback;
  }
}

export function getSettings(): Promise<Settings> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve({ ...mockSettings });
  return safe<Settings>(
    async () => {
      const s = await invoke<Partial<Settings>>("get_settings");
      return { ...DEFAULT_SETTINGS, ...(s ?? {}) };
    },
    { ...DEFAULT_SETTINGS },
  );
}

// B4: НЕ глотаем ошибку записи молча. Возвращаем true/false, чтобы вызывающий код
// мог отреагировать (например, показать предупреждение), и логируем провал явно.
export function saveSettings(settings: Settings): Promise<boolean> {
  if (!IS_TAURI_RUNTIME) {
    mockSettings = { ...settings };
    queueMicrotask(() => emitMock("settings_changed", { ...mockSettings }));
    queueMicrotask(() => {
      void getSecretStatus().then((status) => emitMock("secret_status", status));
    });
    return Promise.resolve(true);
  }
  return (async () => {
    try {
      await invoke("save_settings", { settings });
      return true;
    } catch (e) {
      console.error("[voxflow] save_settings failed:", e);
      return false;
    }
  })();
}

export function setHotkeyCaptureActive(active: boolean): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return safe<void>(async () => {
    await invoke("set_hotkey_capture_active", { active });
  }, undefined);
}

export function getSecretStatus(): Promise<SecretStatus> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({
      ai_api_key: Boolean(mockSettings.ai_api_key),
      oai_stt_key: Boolean(mockSettings.oai_stt_key),
      deepgram_key: Boolean(mockSettings.deepgram_key),
      rewrite_key: Boolean(mockSettings.rewrite_key),
    });
  }
  return safe<SecretStatus>(() => invoke<SecretStatus>("get_secret_status"), {
    ai_api_key: false,
    oai_stt_key: false,
    deepgram_key: false,
    rewrite_key: false,
  });
}

export async function clearSecret(secret: SecretKind): Promise<boolean> {
  if (!IS_TAURI_RUNTIME) {
    mockSettings = { ...mockSettings, [secret]: "" };
    emitMock("secret_status", await getSecretStatus());
    return true;
  }
  try {
    await invoke("clear_secret", { secret });
    return true;
  } catch (error) {
    console.error(`[voxflow] clear_secret(${secret}) failed:`, error);
    return false;
  }
}

export function listAudioDevices(): Promise<string[]> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve(["MacBook Pro Microphone", "Studio USB Microphone"]);
  }
  return safe<string[]>(() => invoke<string[]>("list_audio_devices"), []);
}

export function listModels(): Promise<ModelInfo[]> {
  if (!IS_TAURI_RUNTIME) {
    const models: ModelInfo[] = [
      { name: "gigaam-v3", label: "GigaAM-v3 — быстрый русский", size_mb: 217, installed: true, kind: "gigaam" },
      { name: "parakeet-v3", label: "Parakeet TDT v3 — быстрый English (явный EN)", size_mb: 640, installed: true, kind: "parakeet" },
      { name: "ggml-tiny.bin", label: "Tiny — минимальная задержка (78 МБ)", size_mb: 78, installed: false, kind: "whisper" },
      { name: "ggml-base.bin", label: "Base — быстрая, для слабых ПК (148 МБ)", size_mb: 148, installed: false, kind: "whisper" },
      { name: "ggml-small.bin", label: "Small — компромисс качество/скорость (488 МБ)", size_mb: 488, installed: false, kind: "whisper" },
      { name: "ggml-medium.bin", label: "Medium — повышенная точность без Large (1.53 ГБ)", size_mb: 1530, installed: false, kind: "whisper" },
      { name: "ggml-large-v3-turbo-q5_0.bin", label: "Large v3 Turbo Q5 — рекомендуется (все языки, 574 МБ)", size_mb: 574, installed: true, kind: "whisper" },
      { name: "ggml-large-v3-turbo-q8_0.bin", label: "Large v3 Turbo Q8 — точнее Q5, всё ещё быстрый (874 МБ)", size_mb: 874, installed: false, kind: "whisper" },
      { name: "ggml-large-v3-turbo.bin", label: "Large v3 Turbo — мощная, тяжелее (1.6 ГБ)", size_mb: 1620, installed: false, kind: "whisper" },
      { name: "ggml-large-v3.bin", label: "Large v3 — максимальная точность, медленнее (3.1 ГБ)", size_mb: 3100, installed: false, kind: "whisper" },
    ];
    return Promise.resolve(
      models.map((model) => ({
        ...model,
        installed: mockInstalledModels.has(model.name),
      })),
    );
  }
  return safe<ModelInfo[]>(() => invoke<ModelInfo[]>("list_models"), []);
}

export function downloadModel(name: string): Promise<void> {
  if (!IS_TAURI_RUNTIME) {
    queueMicrotask(() => {
      mockInstalledModels.add(name);
      emitMock("model:done", { name });
    });
    return Promise.resolve();
  }
  return safe<void>(async () => {
    await invoke("download_model", { name });
  }, undefined);
}

export function deleteModel(name: string): Promise<void> {
  if (!IS_TAURI_RUNTIME) {
    mockInstalledModels.delete(name);
    return Promise.resolve();
  }
  return safe<void>(async () => {
    await invoke("delete_model", { name });
  }, undefined);
}

export function toggleDictation(): Promise<void> {
  if (!IS_TAURI_RUNTIME) {
    mockRecording = !mockRecording;
    const seq = ++mockSeq;
    emitMock("status", mockRecording ? "recording" : "transcribing");
    if (!mockRecording) {
      window.setTimeout(() => {
        emitMock("transcript", {
          text: "Версия 2.0 готова к автоматической проверке.",
          ms: 176,
          words: 8,
          seq,
        });
        emitMock("status", "idle");
      }, 420);
    }
    return Promise.resolve();
  }
  return safe<void>(async () => {
    await invoke("toggle_dictation");
  }, undefined);
}

export function isRecording(): Promise<boolean> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve(mockRecording);
  return safe<boolean>(() => invoke<boolean>("is_recording"), false);
}

export function getStats(): Promise<Stats> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({
      today_words: 847,
      total_words: 48_290,
      total_sessions: 312,
      streak_days: 9,
      apps_count: 12,
    });
  }
  return safe<Stats>(() => invoke<Stats>("get_stats"), {
    today_words: 0,
    total_words: 0,
    total_sessions: 0,
    streak_days: 0,
    apps_count: 0,
  });
}

export function getHistory(limit: number): Promise<HistoryItem[]> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve(MOCK_HISTORY.slice(0, limit));
  return safe<HistoryItem[]>(
    () => invoke<HistoryItem[]>("get_history", { limit }),
    [],
  );
}

export function dictionaryList(): Promise<DictionaryEntry[]> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve([]);
  return invoke<DictionaryEntry[]>("dictionary_list");
}

export function dictionaryUpsert(
  id: number | null,
  term: string,
  replacement: string,
): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return invoke<void>("dictionary_upsert", { id, term, replacement });
}

export function dictionaryDelete(id: number): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return invoke<void>("dictionary_delete", { id });
}

export function snippetList(): Promise<SnippetEntry[]> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve([]);
  return invoke<SnippetEntry[]>("snippet_list");
}

export function snippetUpsert(
  id: number | null,
  trigger: string,
  content: string,
  is_template: boolean,
): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return invoke<void>("snippet_upsert", {
    id,
    trigger,
    content,
    isTemplate: is_template,
  });
}

export function snippetDelete(id: number): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return invoke<void>("snippet_delete", { id });
}

export function showMainWindow(): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return safe<void>(async () => {
    await invoke("show_main_window");
  }, undefined);
}

export function activeAppContext(): Promise<ActiveAppContext> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({
      exe: "Codex",
      title: "VoxFlow 2.0 implementation",
      profile: "ai",
      builtin_profile: "ai",
    });
  }
  return safe<ActiveAppContext>(() => invoke<ActiveAppContext>("active_app_context"), {
    exe: "",
    title: "",
    profile: "neutral",
    builtin_profile: "neutral",
  });
}

export function defaultAppProfilePresets(): Promise<ProfileOverride[]> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve([]);
  return safe<ProfileOverride[]>(
    () => invoke<ProfileOverride[]>("default_app_profile_presets"),
    [],
  );
}

export function transformText(
  text: string,
  transform: string,
): Promise<TransformResult> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({ ok: true, text, message: `Demo: ${transform}` });
  }
  return safe<TransformResult>(
    () => invoke<TransformResult>("transform_text", { text, transform }),
    { ok: false, text: "", message: "—" },
  );
}

export function rewritePromptWithInstruction(
  originalPrompt: string,
  voiceInstruction: string,
): Promise<TransformResult> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({
      ok: true,
      text: `${originalPrompt}\n\n${voiceInstruction}`.trim(),
      message: "Demo preview",
    });
  }
  return safe<TransformResult>(
    () =>
      invoke<TransformResult>("rewrite_prompt_with_instruction", {
        originalPrompt,
        voiceInstruction,
      }),
    {
      ok: false,
      text: "",
      message: "Переработка prompt доступна внутри приложения VoxFlow.",
    },
  );
}

export type AiModelOption = { value: string; label: string };
export type AiTestResult = {
  ok: boolean;
  message: string;
  models?: AiModelOption[];
};

export function aiTest(): Promise<AiTestResult> {
  if (!IS_TAURI_RUNTIME) {
    return Promise.resolve({ ok: true, message: "Demo-проверка пройдена" });
  }
  return safe<AiTestResult>(
    () => invoke<AiTestResult>("ai_test"),
    { ok: false, message: "—" },
  );
}

// Облачный STT (D): проверка соединения с выбранным провайдером. Бэкенд берёт
// настройки из БД (поэтому секцию надо предварительно сохранить), делает короткий
// тестовый запрос и возвращает человекочитаемую строку результата. Ключи остаются
// на бэкенде — сюда приходит только итог (без секретов).
export function sttTest(): Promise<string> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve("Demo-проверка пройдена");
  return safe<string>(() => invoke<string>("stt_test"), "—");
}

export function checkForUpdate(): Promise<UpdateInfo | null> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve(null);
  return safe<UpdateInfo | null>(
    () => invoke<UpdateInfo>("check_for_update"),
    null,
  );
}

export function installUpdate(
  assetUrl: string,
  assetName: string,
  assetSize: number,
  assetDigest: string,
): Promise<UpdateInstallResult | null> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve(null);
  return safe<UpdateInstallResult | null>(
    () =>
      invoke<UpdateInstallResult>("install_update", {
        assetUrl,
        assetName,
        assetSize,
        assetDigest,
      }),
    null,
  );
}

export async function openReleaseUrl(url: string): Promise<boolean> {
  if (!url.startsWith("https://github.com/Nezeronxer/voxflow/releases/")) return false;
  if (!IS_TAURI_RUNTIME) {
    window.open(url, "_blank", "noopener,noreferrer");
    return true;
  }
  try {
    await openUrl(url);
    return true;
  } catch {
    return false;
  }
}

export function correctionsList(): Promise<CorrectionEntry[]> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve([]);
  return safe<CorrectionEntry[]>(
    () => invoke<CorrectionEntry[]>("corrections_list"),
    [],
  );
}

export function correctionsUpsert(
  id: number | null,
  wrong: string,
  right: string,
): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return safe<void>(async () => {
    await invoke("corrections_upsert", { id, wrong, right });
  }, undefined);
}

export function correctionsDelete(id: number): Promise<void> {
  if (!IS_TAURI_RUNTIME) return Promise.resolve();
  return safe<void>(async () => {
    await invoke("corrections_delete", { id });
  }, undefined);
}
