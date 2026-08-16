// Shared TypeScript types mirroring the Rust backend contracts.

export interface Settings {
  hotkey: string;
  improve_hotkey: string;
  mode: string;
  double_tap_latch: boolean;
  double_tap_behavior_version: number;
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
  // Старое (до 2.0.12) поведение локальных «самоисправлений»: любой маркер
  // («то есть», «нет», «точнее») режет левую часть фразы. По умолчанию выкл.
  aggressive_self_correction: boolean;
  learn_corrections: boolean;
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
  ai_backend_behavior_version: number;
  ai_api_key: string;
  ai_model: string;
  ollama_url: string;
  ollama_model: string;
  // Облачный rewrite (OpenAI-совместимый chat): Claude Haiku / OpenAI / Groq.
  rewrite_base_url: string;
  rewrite_model: string;
  rewrite_key: string;
  /** Имя HTTP-заголовка с ключом; пусто = `Authorization: Bearer`. */
  rewrite_auth_header: string;
  // Верхняя граница токенов ответа рерайта (фактический лимит считается от входа).
  rewrite_max_output_tokens: number;
  // Доля слов диктовки, обязанных остаться в ответе модели (0..1).
  rewrite_min_recall: number;
  // Таймаут запроса к ИИ, секунды (локальная модель получает минимум 60).
  ai_timeout_s: number;
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
  /** Пересобирать диктовку в промпт по правилам целевой нейросети. */
  prompt_rebuild: boolean;
  /** Какая модель выбрана для каждого сервиса. */
  prompt_models: PromptModelChoice[];
  /** Пользователь отказался включать найденный локальный ИИ. */
  local_ai_dismissed: boolean;
}

export interface ProfileOverride {
  match: string; // подстрока в exe/заголовке (lowercase)
  profile: string; // verbatim|code|ai|formal|work|casual|doc|neutral
}

/** Модель из каталога локального ИИ. */
export interface LocalAiModel {
  tag: string;
  label: string;
  size_gb: number;
  /** С какого объёма памяти модель имеет смысл предлагать. */
  min_ram_gb: number;
}

export interface LocalAiEngine {
  engine: string;
  label: string;
  url: string;
  models: string[];
}

/** Что предлагается включить; приходит и событием `local-ai:found`. */
export interface LocalAiSuggestion {
  engine: string;
  label: string;
  /** Значение ai_backend: "ollama" либо "openai_compat". */
  backend: string;
  url: string;
  model: string;
}

/** Чем машина считает модель — главный вопрос, а не объём ОЗУ. */
export type LocalAiAccel =
  | { kind: "apple_silicon" }
  /** vram_gb = 0 — карта есть, объём выяснить не удалось. */
  | { kind: "nvidia"; vram_gb: number }
  | { kind: "cpu_only" };

export interface LocalAiMachine {
  /** Полный объём памяти, ГБ. 0 — определить не удалось. */
  ram_gb: number;
  cpu_cores: number;
  accel: LocalAiAccel;
}

export interface LocalAiState {
  engines: LocalAiEngine[];
  machine: LocalAiMachine;
  /** Ряд моделей, подходящих этой машине. */
  shortlist: LocalAiModel[];
  /** Не влезающие — показываем с пометкой, а не прячем. */
  too_heavy: LocalAiModel[];
  /** Только Ollama умеет ставить модели по кнопке. */
  can_pull: boolean;
  suggestion: LocalAiSuggestion | null;
}

/** Модель из каталога `prompt_rules.json` с действующими правилами. */
export interface PromptModelView {
  id: string;
  service: string;
  label: string;
  /** Страница документации вендора; пусто — обновлять нечего. */
  doc: string;
  rules: string;
  /** Правила пересобраны из документации, а не взяты из сборки. */
  refreshed: boolean;
  /** Когда последний раз проверяли документацию (RFC3339, пусто — ни разу). */
  checked: string;
}

export interface PromptRulesRefresh {
  checked: number;
  updated: string[];
  skipped: string[];
  failed: string[];
}

/** Выбор конкретной модели внутри сервиса: claude → claude-opus-5. */
export interface PromptModelChoice {
  service: string;
  model: string;
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
  auto_install: boolean;
  current_version: string;
  latest_version: string;
  release_name: string;
  release_url: string;
  asset_name: string;
  asset_url: string;
  asset_size: number;
  asset_digest: string;
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
  double_tap_latch: true,
  double_tap_behavior_version: 1,
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
  aggressive_self_correction: false,
  learn_corrections: false,
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
  // Локальный LLM-rewrite тяжёлый и синхронный: только после opt-in.
  ai_backend: "off",
  ai_backend_behavior_version: 1,
  ai_api_key: "",
  ai_model: "gemini-2.5-flash",
  ollama_url: "http://localhost:11434",
  ollama_model: "qwen3:4b",
  rewrite_base_url: "",
  rewrite_model: "",
  rewrite_key: "",
  rewrite_auth_header: "",
  rewrite_max_output_tokens: 4096,
  rewrite_min_recall: 0.9,
  ai_timeout_s: 20,
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
  prompt_rebuild: false,
  prompt_models: [],
  local_ai_dismissed: false,
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

// Событие "status": legacy-строка ЛИБО атомарный объект. seq — поколение
// диктовки для отсечения запоздалых partial/level; latched подтверждает
// double-tap без промежуточной hold-анимации. На
// status=="recording" фронт сначала сбрасывает lang в null (новая диктовка),
// затем применяет lang из этого же события, если оно объект и поле прислано.
export type StatusPayload =
  | string
  | { status?: string; lang?: DetectedLang; seq?: number; latched?: boolean };
