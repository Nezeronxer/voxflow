import { useEffect, useMemo, useRef, useState } from "react";
import {
  activeAppContext,
  getHistory,
  getStats,
  isRecording,
  localLlmState,
  subscribe,
  toggleDictation,
} from "../api";
import { Icon, prettyHotkey } from "../ui";
import type {
  ActiveAppContext,
  HistoryItem,
  LocalLlmState,
  OverlayStatus,
  Settings,
  Stats,
  TranscriptEvent,
} from "../types";
import type { SettingsPageId } from "./SettingsHub";
import type { TabId } from "../App";

type StatusPayload = OverlayStatus | { status?: OverlayStatus };
type LevelPayload = { rms?: number; seq?: number };

const EMPTY_STATS: Stats = {
  today_words: 0,
  total_words: 0,
  total_sessions: 0,
  streak_days: 0,
  apps_count: 0,
};

/** Сегментов в индикаторе уровня. */
const METER_SEGMENTS = 28;

function languageLabel(language: string): string {
  if (language === "ru") return "Русский";
  if (language === "en") return "English";
  return "Авто · RU/EN";
}

function profileLabel(profile: string): string {
  const labels: Record<string, string> = {
    ai: "Промпты",
    code: "Код",
    formal: "Формальный",
    work: "Рабочий",
    casual: "Общение",
    doc: "Документы",
    verbatim: "Дословно",
    neutral: "Нейтральный",
  };
  return labels[profile] ?? "Нейтральный";
}

function modelLabel(settings: Settings): string {
  if (settings.stt_provider === "deepgram") return settings.deepgram_model || "Deepgram";
  if (settings.stt_provider === "openai_compat") {
    return settings.oai_stt_model || "Облачное STT";
  }
  if (settings.language === "ru" || settings.engine === "gigaam") return "GigaAM v3";
  if (settings.language === "en") return "Parakeet TDT v3";
  return settings.model.includes("turbo") ? "Whisper Turbo" : "Whisper";
}

function aiLabel(settings: Settings, llm: LocalLlmState | null): string {
  switch (settings.ai_backend) {
    case "builtin": {
      const model = llm?.models.find((m) => m.id === settings.builtin_llm_model);
      return model ? model.label : "Встроенный";
    }
    case "ollama":
      return `Ollama · ${settings.ollama_model || "модель не выбрана"}`;
    case "gemini":
      return "Google Gemini";
    case "openai_compat":
      return settings.rewrite_model || "Свой ключ";
    default:
      return "Выключен";
  }
}

function fmtTime(ts: string): string {
  const d = new Date(ts.replace(" ", "T"));
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleTimeString("ru-RU", { hour: "2-digit", minute: "2-digit" });
}

async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** Индикатор уровня: сегменты зажигаются по RMS, верх — жёлтый и красный. */
function Meter({ level, active }: { level: number; active: boolean }) {
  const lit = active ? Math.round(Math.min(1, level) * METER_SEGMENTS) : 0;
  return (
    <div className="meter" aria-hidden="true">
      {Array.from({ length: METER_SEGMENTS }, (_, i) => {
        const on = i < lit;
        const zone = i >= METER_SEGMENTS - 2 ? " clip" : i >= METER_SEGMENTS - 7 ? " hot" : "";
        return <span key={i} className={on ? `on${zone}` : ""} />;
      })}
    </div>
  );
}

export default function Dashboard({
  settings,
  onOpenSettings,
  onOpenTab,
}: {
  settings: Settings;
  onOpenSettings: (page: SettingsPageId) => void;
  onOpenTab: (tab: TabId) => void;
}) {
  const [stats, setStats] = useState<Stats>(EMPTY_STATS);
  const [status, setStatus] = useState<OverlayStatus>("idle");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [llm, setLlm] = useState<LocalLlmState | null>(null);
  const [activeApp, setActiveApp] = useState<ActiveAppContext>({
    exe: "",
    title: "",
    profile: "neutral",
    builtin_profile: "neutral",
  });
  const [busy, setBusy] = useState(false);
  const [latencyMs, setLatencyMs] = useState<number | null>(null);
  const [copied, setCopied] = useState<HistoryItem | null>(null);
  const [level, setLevel] = useState(0);
  const lastSeqRef = useRef(-1);
  const levelSeqRef = useRef(-1);
  const activeAppRef = useRef(activeApp);
  const copyTimerRef = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    Promise.all([getStats(), getHistory(6), activeAppContext(), isRecording(), localLlmState()]).then(
      ([nextStats, nextHistory, context, recording, llmState]) => {
        if (!alive) return;
        setStats(nextStats);
        setHistory(nextHistory);
        activeAppRef.current = context;
        setActiveApp(context);
        setLlm(llmState);
        if (recording) setStatus("recording");
      },
    ).catch(() => undefined);

    const offs = [
      subscribe<TranscriptEvent>("transcript", (event) => {
        const transcript = event.payload;
        const seq = transcript?.seq;
        if (seq != null && seq <= lastSeqRef.current) return;
        if (seq != null) lastSeqRef.current = seq;
        if (transcript?.ms != null) setLatencyMs(transcript.ms);
        if (transcript?.text) {
          const now = new Date();
          const stamp = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")} ${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}:00`;
          const item: HistoryItem = {
            ts: stamp,
            text: transcript.text,
            app: activeAppRef.current.exe || activeAppRef.current.title || "",
            words: transcript.words ?? transcript.text.trim().split(/\s+/u).length,
          };
          setHistory((previous) => [item, ...previous].slice(0, 6));
          void getStats().then(setStats);
        }
      }),
      subscribe<StatusPayload>("status", (event) => {
        const payload = event.payload;
        const value = typeof payload === "string" ? payload : payload?.status;
        if (value === "idle" || value === "recording" || value === "transcribing") {
          setStatus(value);
          setBusy(false);
          if (value !== "recording") setLevel(0);
        }
      }),
      subscribe<LevelPayload>("level", (event) => {
        const payload = event.payload;
        const seq = payload?.seq;
        if (seq != null && seq <= levelSeqRef.current) return;
        if (seq != null) levelSeqRef.current = seq;
        const rms = typeof payload?.rms === "number" ? payload.rms : 0;
        // лог-кривая: тихая речь тоже видна, пик не упирается в потолок
        setLevel(Math.min(1, Math.log10(1 + 9 * Math.max(0, rms))));
      }),
    ];

    return () => {
      alive = false;
      offs.forEach((off) => off());
      if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
    };
  }, []);

  const appName = useMemo(() => {
    const raw = activeApp.exe || activeApp.title || "Активное окно";
    return raw.replace(/\.exe$/i, "").split(/[\\/]/).pop() || raw;
  }, [activeApp.exe, activeApp.title]);

  async function onToggle() {
    setBusy(true);
    try {
      await toggleDictation();
    } finally {
      setBusy(false);
    }
  }

  async function onCopy(item: HistoryItem) {
    if (!(await copyText(item.text))) return;
    setCopied(item);
    if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
    copyTimerRef.current = window.setTimeout(() => setCopied(null), 1400);
  }

  const local = settings.stt_provider === "local";
  const statusText =
    status === "recording" ? "Запись" : status === "transcribing" ? "Обработка" : "Готов";
  const title =
    status === "recording"
      ? "Слушаю"
      : status === "transcribing"
        ? "Собираю текст"
        : "Готов к диктовке";
  const aiOn = settings.ai_backend !== "off";
  const llmReady = llm?.models.some((m) => m.installed) ?? false;

  return (
    <div className="hub">
      <header className="hub-head">
        <div>
          <h1>Диктовка</h1>
          <p>Курсор в нужном поле, клавиша зажата, текст появляется после отпускания.</p>
        </div>
        <div className="hub-usage" aria-label="Статистика использования">
          <strong>{stats.today_words.toLocaleString("ru-RU")}</strong>
          слов сегодня{stats.streak_days > 1 ? ` · ${stats.streak_days} дн. подряд` : ""}
        </div>
      </header>

      <section className={`deck state-${status}`} aria-live="polite">
        <div>
          <div className="deck-status">
            <span className="deck-led" />
            {statusText}
            {latencyMs != null && status === "idle" ? ` · ${latencyMs} мс` : ""}
          </div>
          <h2 className="deck-title">{title}</h2>
          <p className="deck-copy">
            Удерживайте <span className="kbd">{prettyHotkey(settings.hotkey)}</span>
            {settings.mode === "toggle" ? " — нажатие начинает и заканчивает запись." : " и говорите."}
            {" "}Кнопка справа делает то же самое.
          </p>
        </div>

        <div className="deck-rec">
          <button
            type="button"
            className="voice-orb"
            data-testid="dictation-orb"
            onClick={onToggle}
            disabled={busy}
            aria-label={status === "recording" ? "Остановить диктовку" : "Начать диктовку"}
          >
            {status === "transcribing" ? <Icon.Refresh /> : <Icon.Mic />}
          </button>
          <div className="voice-status">
            <span />
            {status === "recording" ? "стоп" : "запись"}
          </div>
        </div>

        <Meter level={level} active={status === "recording"} />

        <div className="deck-readouts">
          <div className="context-cell">
            <Icon.Code className="ico" />
            <span>
              <strong>{appName}</strong>
              <small>Окно</small>
            </span>
          </div>
          <div className="context-cell">
            <Icon.Wand className="ico" />
            <span>
              <strong>{profileLabel(activeApp.profile)}</strong>
              <small>Стиль</small>
            </span>
          </div>
          <div className="context-cell">
            <Icon.Cube className="ico" />
            <span>
              <strong>{modelLabel(settings)}</strong>
              <small>{languageLabel(settings.language)}</small>
            </span>
          </div>
          <div className={`context-cell privacy-cell${local ? "" : " cloud"}`}>
            <Icon.Check className="ico" />
            <span>
              <strong>{local ? "На устройстве" : "Облако"}</strong>
              <small>Аудио</small>
            </span>
          </div>
        </div>
      </section>

      <div className="hub-layout">
        <div className="hub-primary">
          <section className="today-section">
            <div className="section-line-head">
              <h2>Последние диктовки</h2>
              <button type="button" className="text-action" onClick={() => onOpenTab("history")}>
                Вся история
              </button>
            </div>
            <div className="transcript-list">
              {history.length === 0 ? (
                <div className="hub-empty">
                  Здесь появятся ваши диктовки. Они хранятся только на этом компьютере.
                </div>
              ) : (
                history.slice(0, 4).map((item, index) => (
                  <article className="transcript-row" key={`${item.ts}-${index}`}>
                    <time>{fmtTime(item.ts)}</time>
                    <span className="transcript-app">{item.app || "Приложение"}</span>
                    <p>{item.text}</p>
                    <span className="transcript-speed">{item.words} сл.</span>
                    <button type="button" onClick={() => void onCopy(item)}>
                      <Icon.Check className="ico" />
                      {copied === item ? "Скопировано" : "Копировать"}
                    </button>
                  </article>
                ))
              )}
            </div>
          </section>
        </div>

        <aside className="quick-rail" aria-label="Быстрые настройки">
          <div className="ai-card">
            <div className="ai-card-top">
              <Icon.Cube className="ico" />
              <strong>Локальный ИИ</strong>
              <span className={`badge${aiOn ? " ok" : ""}`}>
                {aiOn ? "вкл" : "выкл"}
              </span>
            </div>
            <p>
              {aiOn
                ? `Текст после распознавания правит ${aiLabel(settings, llm)}.`
                : llmReady
                  ? "Модель скачана, но обработка выключена. Включите её, чтобы текст вставлялся по смыслу."
                  : "Скачайте модель — она уберёт паразиты, расставит знаки и исправит ослышки без облака."}
            </p>
            <button type="button" className="btn btn-sm" onClick={() => onOpenTab("ai")}>
              {aiOn ? "Настроить" : llmReady ? "Включить" : "Скачать модель"}
            </button>
          </div>

          <h2>Быстрые настройки</h2>
          <button type="button" onClick={() => onOpenSettings("dictation")}>
            <Icon.Cube className="ico" />
            <span>Распознавание<small>{modelLabel(settings)}</small></span>
            <b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("dictation")}>
            <Icon.Mic className="ico" />
            <span>Микрофон<small>{settings.input_device || "Системный"}</small></span>
            <b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("text")}>
            <Icon.Wand className="ico" />
            <span>Обработка текста<small>{settings.verbatim ? "Дословно" : "Умная чистка"}</small></span>
            <b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("applications")}>
            <Icon.Code className="ico" />
            <span>Приложения<small>Стиль под каждое окно</small></span>
            <b>›</b>
          </button>
        </aside>
      </div>
    </div>
  );
}
