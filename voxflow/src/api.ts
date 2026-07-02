// Defensive wrappers around Tauri commands. Every call is wrapped in try/catch
// so the UI never crashes if a command errors during early boot.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
} from "./types";
import { DEFAULT_SETTINGS } from "./types";

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

export function listAudioDevices(): Promise<string[]> {
  return safe<string[]>(() => invoke<string[]>("list_audio_devices"), []);
}

export function listModels(): Promise<ModelInfo[]> {
  return safe<ModelInfo[]>(() => invoke<ModelInfo[]>("list_models"), []);
}

export function downloadModel(name: string): Promise<void> {
  return safe<void>(async () => {
    await invoke("download_model", { name });
  }, undefined);
}

export function deleteModel(name: string): Promise<void> {
  return safe<void>(async () => {
    await invoke("delete_model", { name });
  }, undefined);
}

export function toggleDictation(): Promise<void> {
  return safe<void>(async () => {
    await invoke("toggle_dictation");
  }, undefined);
}

export function getStats(): Promise<Stats> {
  return safe<Stats>(() => invoke<Stats>("get_stats"), {
    today_words: 0,
    total_words: 0,
    total_sessions: 0,
    streak_days: 0,
    apps_count: 0,
  });
}

export function getHistory(limit: number): Promise<HistoryItem[]> {
  return safe<HistoryItem[]>(
    () => invoke<HistoryItem[]>("get_history", { limit }),
    [],
  );
}

export function dictionaryList(): Promise<DictionaryEntry[]> {
  return safe<DictionaryEntry[]>(
    () => invoke<DictionaryEntry[]>("dictionary_list"),
    [],
  );
}

export function dictionaryUpsert(
  id: number | null,
  term: string,
  replacement: string,
): Promise<void> {
  return safe<void>(async () => {
    await invoke("dictionary_upsert", { id, term, replacement });
  }, undefined);
}

export function dictionaryDelete(id: number): Promise<void> {
  return safe<void>(async () => {
    await invoke("dictionary_delete", { id });
  }, undefined);
}

export function snippetList(): Promise<SnippetEntry[]> {
  return safe<SnippetEntry[]>(() => invoke<SnippetEntry[]>("snippet_list"), []);
}

export function snippetUpsert(
  id: number | null,
  trigger: string,
  content: string,
  is_template: boolean,
): Promise<void> {
  return safe<void>(async () => {
    await invoke("snippet_upsert", { id, trigger, content, isTemplate: is_template });
  }, undefined);
}

export function snippetDelete(id: number): Promise<void> {
  return safe<void>(async () => {
    await invoke("snippet_delete", { id });
  }, undefined);
}

export function showMainWindow(): Promise<void> {
  return safe<void>(async () => {
    await invoke("show_main_window");
  }, undefined);
}

export function activeAppContext(): Promise<ActiveAppContext> {
  return safe<ActiveAppContext>(() => invoke<ActiveAppContext>("active_app_context"), {
    exe: "",
    title: "",
    profile: "neutral",
    builtin_profile: "neutral",
  });
}

export function defaultAppProfilePresets(): Promise<ProfileOverride[]> {
  return safe<ProfileOverride[]>(
    () => invoke<ProfileOverride[]>("default_app_profile_presets"),
    [],
  );
}

export function transformText(
  text: string,
  transform: string,
): Promise<TransformResult> {
  return safe<TransformResult>(
    () => invoke<TransformResult>("transform_text", { text, transform }),
    { ok: false, text: "", message: "—" },
  );
}

export function rewritePromptWithInstruction(
  originalPrompt: string,
  voiceInstruction: string,
): Promise<TransformResult> {
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
  return safe<string>(() => invoke<string>("stt_test"), "—");
}

export function checkForUpdate(): Promise<UpdateInfo | null> {
  return safe<UpdateInfo | null>(
    () => invoke<UpdateInfo>("check_for_update"),
    null,
  );
}

export function installUpdate(
  assetUrl: string,
  assetName: string,
): Promise<UpdateInstallResult | null> {
  return safe<UpdateInstallResult | null>(
    () =>
      invoke<UpdateInstallResult>("install_update", {
        assetUrl,
        assetName,
      }),
    null,
  );
}

export function correctionsList(): Promise<CorrectionEntry[]> {
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
  return safe<void>(async () => {
    await invoke("corrections_upsert", { id, wrong, right });
  }, undefined);
}

export function correctionsDelete(id: number): Promise<void> {
  return safe<void>(async () => {
    await invoke("corrections_delete", { id });
  }, undefined);
}
