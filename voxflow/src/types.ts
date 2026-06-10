// Shared TypeScript types mirroring the Rust backend contracts.

export interface Settings {
  hotkey: string;
  mode: string;
  input_device: string;
  language: string;
  model: string;
  engine: string; // "gigaam" | "whisper_server" | "whisper_cli"
  theme: string; // "system" | "light" | "dark"
  verbatim: boolean;
  remove_fillers: boolean;
  auto_punct: boolean;
  tone: string;
  paste_method: string;
  play_sounds: boolean;
  autostart: boolean;
  personalize: boolean;
  threads: number;
  ai_backend: string; // "off" | "ollama" | "gemini" | "openai_compat"
  ai_api_key: string;
  ai_model: string;
  ollama_url: string;
  ollama_model: string;
  // Облачный rewrite (OpenAI-совместимый chat): Claude Haiku / OpenAI / Groq.
  rewrite_base_url: string;
  rewrite_model: string;
  rewrite_key: string;
  cloud_asr: boolean;
  tone_by_app: boolean;
  stream_mode: string;
  // Облачный STT (D-022)
  stt_provider: string; // "local" | "openai_compat" | "deepgram"
  stt_fallback_local: boolean;
  cloud_live_draft: boolean;
  oai_stt_base_url: string;
  oai_stt_model: string;
  oai_stt_key: string;
  deepgram_base: string;
  deepgram_model: string;
  deepgram_key: string;
  proxy_url: string;
  app_profile_overrides: ProfileOverride[];
}

export interface ProfileOverride {
  match: string; // подстрока в exe/заголовке (lowercase)
  profile: string; // verbatim|ai|formal|work|casual|doc|neutral
}

export const DEFAULT_SETTINGS: Settings = {
  hotkey: "ControlRight",
  mode: "hold",
  input_device: "",
  language: "ru",
  // Зеркало Rust-дефолтов (settings.rs). Расхождение раньше позволяло UI
  // застампить в БД engine=whisper_cli и навсегда выключить живой ввод (B2).
  model: "ggml-large-v3-turbo-q5_0.bin",
  engine: "gigaam",
  theme: "system",
  verbatim: false,
  remove_fillers: true,
  auto_punct: true,
  tone: "neutral",
  paste_method: "clipboard",
  play_sounds: true,
  autostart: false,
  personalize: true,
  threads: 0,
  ai_backend: "ollama",
  ai_api_key: "",
  ai_model: "gemini-2.5-flash",
  ollama_url: "http://localhost:11434",
  ollama_model: "qwen3:4b",
  rewrite_base_url: "",
  rewrite_model: "",
  rewrite_key: "",
  cloud_asr: false,
  tone_by_app: true,
  stream_mode: "never",
  stt_provider: "local",
  stt_fallback_local: true,
  cloud_live_draft: true,
  oai_stt_base_url: "https://api.groq.com/openai/v1",
  oai_stt_model: "whisper-large-v3",
  oai_stt_key: "",
  deepgram_base: "https://api.deepgram.com",
  deepgram_model: "nova-3",
  deepgram_key: "",
  proxy_url: "",
  app_profile_overrides: [],
};

export interface ModelInfo {
  name: string;
  label: string;
  size_mb: number;
  installed: boolean;
  // "gigaam" — русская ONNX-модель (набор файлов в models/gigaam/),
  // "whisper" — одиночный ggml-*.bin. Отсутствует у старых бэкендов → whisper.
  kind?: string;
}

export interface Stats {
  today_words: number;
  total_words: number;
  total_sessions: number;
  streak_days: number;
  apps_count: number;
}

export interface HistoryItem {
  ts: number;
  text: string;
  app: string;
  words: number;
}

export interface DictionaryEntry {
  id: number;
  term: string;
  replacement: string;
}

export interface SnippetEntry {
  id: number;
  trigger: string;
  content: string;
  is_template: boolean;
}

export interface CorrectionEntry {
  id: number;
  wrong: string;
  right: string;
}

// Event payloads emitted from the backend.
// seq — монотонный счётчик диктовки (растёт на каждую новую запись). Нужен фронту,
// чтобы отбрасывать устаревшие/дублирующиеся события (StrictMode/async-гонки).
export interface TranscriptEvent {
  text: string;
  ms?: number;
  words?: number;
  seq?: number;
}

// Живой (негейченый) частичный текст — стримится в пилюлю во время записи.
// text — полный (committed + volatile), для обратной совместимости со старыми
// слушателями. committed — стабильный префикс, который НЕ переписывается
// (рендерим обычным цветом). volatile — изменчивый «хвост» (рендерим серым).
export interface PartialEvent {
  text: string;
  committed: string;
  volatile: string;
  // seq — монотонный счётчик диктовки; отбрасываем партиалы старее текущей записи.
  seq?: number;
}

// Событие "no_model": модель не выбрана/не установлена (B3). Фронт показывает
// баннер с кнопкой перехода на вкладку «Модель», overlay дублирует кратко.
export interface NoModelEvent {
  message: string;
}

// Общая ошибка движка (микрофон/сервер/прочее) — событие "error".
export interface ErrorEvent {
  message: string;
}

// Гейт уверенности отклонил распознавание — событие "norecog".
export interface NoRecogEvent {
  message: string;
}

export interface ModelProgressEvent {
  name: string;
  received: number;
  total: number;
}

export interface ModelDoneEvent {
  name: string;
}

export interface ModelErrorEvent {
  name: string;
  error?: string;
}

// Какой STT реально отработал последнюю диктовку. offline=true → облако было
// недоступно и сработал авто-fallback на локальный whisper (ненавязчивая
// индикация «оффлайн-режим» в плашке/дашборде).
export interface SttModeEvent {
  engine: string; // "local" | "openai_compat" | "deepgram"
  offline: boolean;
}

// Уровень громкости микрофона для orb-визуализатора — событие "level",
// шлётся ~каждые 33 мс во время записи. rms нормирован в 0..1.
export interface LevelEvent {
  rms: number;
  seq?: number;
}

export type OverlayStatus = "idle" | "recording" | "transcribing";
