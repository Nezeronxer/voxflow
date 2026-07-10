// Shared TypeScript types mirroring the Rust backend contracts.

export interface Settings {
  hotkey: string;
  improve_hotkey: string;
  mode: string;
  input_device: string;
  language: string;
  model: string;
  engine: string; // "gigaam" | "whisper_server" | "whisper_cli"
  theme: string; // "system" | "light" | "dark"
  // Масштаб плавающей плашки: 0.75..1.5 (75..150%).
  overlay_scale: number;
  verbatim: boolean;
  remove_fillers: boolean;
  auto_punct: boolean;
  // "very_casual" | "casual" | "neutral" | "work" | "formal" | "doc" | "ai"
  tone: string;
  smart_prompt_enabled: boolean;
  smart_prompt_source: string;
  smart_prompt_instruction: string;
  paste_method: string;
  play_sounds: boolean;
  auto_mute: boolean;
  autostart: boolean;
  auto_update_check: boolean;
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
  ai_prompt_rules: AiPromptRule[];
}

export interface ProfileOverride {
  match: string; // подстрока в exe/заголовке (lowercase)
  profile: string; // verbatim|code|ai|formal|work|casual|doc|neutral
}

export interface AiPromptRule {
  match: string; // подстрока в exe/заголовке нейросети
  prompt: string; // пользовательские правила переписывания диктовки под эту нейросеть
}

export interface ActiveAppContext {
  exe: string;
  title: string;
  profile: string;
  builtin_profile: string;
}

export interface TransformResult {
  ok: boolean;
  text: string;
  message: string;
}

export interface UpdateInfo {
  available: boolean;
  current_version: string;
  latest_version: string;
  release_name: string;
  release_url: string;
  asset_name: string;
  asset_url: string;
  asset_size: number;
  published_at: string;
  notes: string;
}

export interface UpdateInstallResult {
  launched: boolean;
  path: string;
  message: string;
}

export interface SecretStatus {
  ai_api_key: boolean;
  oai_stt_key: boolean;
  deepgram_key: boolean;
  rewrite_key: boolean;
}

export type SecretKind = keyof SecretStatus;

export const DEFAULT_HOTKEY =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.platform)
    ? "AltRight"
    : "ControlRight";

export const OVERLAY_SCALE_MIN = 0.75;
export const OVERLAY_SCALE_MAX = 1.5;
export const OVERLAY_SCALE_STEP = 0.05;

export function normalizeOverlayScale(value: number | null | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 1;
  return Math.min(OVERLAY_SCALE_MAX, Math.max(OVERLAY_SCALE_MIN, value));
}

export const DEFAULT_SETTINGS: Settings = {
  hotkey: DEFAULT_HOTKEY,
  improve_hotkey: "F8",
  mode: "hold",
  input_device: "",
  language: "auto",
  // Зеркало Rust-дефолтов (settings.rs). Расхождение раньше позволяло UI
  // застампить в БД engine=whisper_cli и навсегда выключить живой ввод (B2).
  model: "ggml-large-v3-turbo-q5_0.bin",
  engine: "whisper_server",
  theme: "system",
  overlay_scale: 1,
  verbatim: false,
  remove_fillers: true,
  auto_punct: true,
  tone: "neutral",
  smart_prompt_enabled: true,
  smart_prompt_source: "",
  smart_prompt_instruction: "",
  paste_method: "clipboard",
  play_sounds: true,
  auto_mute: true,
  autostart: false,
  auto_update_check: true,
  personalize: false,
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
  ai_prompt_rules: [],
};

export interface ModelInfo {
  name: string;
  label: string;
  size_mb: number;
  installed: boolean;
  // "gigaam" — русская ONNX-модель (набор файлов в models/gigaam/),
  // "parakeet" — EN/auto ONNX-модель, "whisper" — одиночный ggml-*.bin.
  // Отсутствует у старых бэкендов → whisper.
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
  /** Бэкенд отдаёт строку "YYYY-MM-DD HH:MM:SS" (commands.rs get_history). */
  ts: string;
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

// Язык текущей диктовки, определённый STT (бейдж в пилюле): поле отсутствует →
// старый бэкенд, ничего не менять; null → язык не определён, бейдж скрыт;
// "ru"/"en" → бейдж RU/EN. Незнакомое значение трактуется как null.
export type DetectedLang = "ru" | "en" | null;

// Живой (негейченый) частичный текст — стримится в пилюлю во время записи.
// text — полный (committed + volatile), для обратной совместимости со старыми
// слушателями. committed — стабильный префикс, который НЕ переписывается
// (рендерим обычным цветом). volatile — изменчивый «хвост» (рендерим серым).
export interface PartialEvent {
  text: string;
  committed: string;
  volatile: string;
  // true — text/committed/volatile уже прошли live postprocess и пригодны
  // для показа в синей пилюле во время записи.
  processed?: boolean;
  // true — это уже финальный исправленный preview после postprocess/LLM,
  // который overlay показывает во время status=="transcribing" вместо raw live draft.
  final?: boolean;
  // seq — монотонный счётчик диктовки; отбрасываем партиалы старее текущей записи.
  seq?: number;
  // Язык диктовки для бейджа (контракт overlay). Партиалы, отброшенные
  // seq-дедупом, lang тоже НЕ применяют.
  lang?: DetectedLang;
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

export interface HotkeyLatchEvent {
  message?: string;
  detail?: string;
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
// недоступно и сработал авто-fallback на локальное распознавание (ненавязчивая
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

// Событие "status": legacy-строка (как раньше) ЛИБО объект { status, lang } —
// бэкенд шлёт объект, когда знает язык диктовки (бейдж в пилюле). На
// status=="recording" фронт сначала сбрасывает lang в null (новая диктовка),
// затем применяет lang из этого же события, если оно объект и поле прислано.
export type StatusPayload =
  | string
  | { status?: string; lang?: DetectedLang };
