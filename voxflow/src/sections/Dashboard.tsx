import { useEffect, useMemo, useRef, useState } from "react";
import {
  activeAppContext,
  getHistory,
  getStats,
  isRecording,
  subscribe,
  toggleDictation,
} from "../api";
import { Icon, prettyHotkey } from "../ui";
import type {
  ActiveAppContext,
  HistoryItem,
  OverlayStatus,
  Settings,
  Stats,
  TranscriptEvent,
} from "../types";
import type { SettingsPageId } from "./SettingsHub";

type StatusPayload = OverlayStatus | { status?: OverlayStatus };

const EMPTY_STATS: Stats = {
  today_words: 0,
  total_words: 0,
  total_sessions: 0,
  streak_days: 0,
  apps_count: 0,
};

const WAVE_BARS = [
  6, 10, 16, 9, 20, 13, 24, 12, 18, 28, 17, 11, 22, 14, 25, 10, 19, 8, 14, 6,
];

function greeting(): string {
  const hour = new Date().getHours();
  if (hour < 5) return "Доброй ночи";
  if (hour < 12) return "Доброе утро";
  if (hour < 18) return "Добрый день";
  return "Добрый вечер";
}

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
    return settings.oai_stt_model || "Cloud STT";
  }
  if (settings.language === "ru" || settings.engine === "gigaam") return "GigaAM v3";
  if (settings.language === "en") return "Parakeet TDT v3";
  return settings.model.includes("turbo") ? "Whisper Turbo" : "Локальная авто";
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

export default function Dashboard({
  settings,
  onOpenSettings,
}: {
  settings: Settings;
  onOpenSettings: (page: SettingsPageId) => void;
}) {
  const [stats, setStats] = useState<Stats>(EMPTY_STATS);
  const [status, setStatus] = useState<OverlayStatus>("idle");
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [activeApp, setActiveApp] = useState<ActiveAppContext>({
    exe: "",
    title: "",
    profile: "neutral",
    builtin_profile: "neutral",
  });
  const [busy, setBusy] = useState(false);
  const [latencyMs, setLatencyMs] = useState<number | null>(null);
  const [copied, setCopied] = useState<HistoryItem | null>(null);
  const lastSeqRef = useRef(-1);
  const activeAppRef = useRef(activeApp);
  const copyTimerRef = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    Promise.all([getStats(), getHistory(6), activeAppContext(), isRecording()]).then(
      ([nextStats, nextHistory, context, recording]) => {
        if (!alive) return;
        setStats(nextStats);
        setHistory(nextHistory);
        activeAppRef.current = context;
        setActiveApp(context);
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
        }
      }),
    ];

    return () => {
      alive = false;
      offs.forEach((off) => off());
      if (copyTimerRef.current !== null) window.clearTimeout(copyTimerRef.current);
    };
  }, []);

  const appName = useMemo(() => {
    const raw = activeApp.exe || activeApp.title || "Текущее приложение";
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

  const statusText =
    status === "recording"
      ? "Слушаю"
      : status === "transcribing"
        ? "Улучшаю текст"
        : "Готов слушать";

  return (
    <div className="hub">
      <header className="hub-head">
        <div>
          <h1>{greeting()}</h1>
          <p>
            Удерживайте <span className="kbd kbd-inline">{prettyHotkey(settings.hotkey)}</span>,
            чтобы говорить
          </p>
        </div>
        <div className="hub-usage" aria-label="Статистика использования">
          <strong>{stats.today_words.toLocaleString("ru-RU")}</strong> слов сегодня
          {stats.streak_days > 0 ? ` · ${stats.streak_days} дней подряд` : ""}
        </div>
      </header>

      <div className="hub-layout">
        <div className="hub-primary">
          <section className={`dictation-surface state-${status}`} aria-live="polite">
            <div className="dictation-context">
              <div className="context-cell">
                <Icon.Code className="ico" />
                <span><strong>{appName}</strong><small>Активное приложение</small></span>
              </div>
              <div className="context-cell">
                <Icon.Wand className="ico" />
                <span><strong>{profileLabel(activeApp.profile)}</strong><small>Стиль</small></span>
              </div>
              <div className="context-cell">
                <Icon.Clock className="ico" />
                <span><strong>{languageLabel(settings.language)}</strong><small>Язык</small></span>
              </div>
              <div className="context-cell privacy-cell">
                <Icon.Check className="ico" />
                <span>
                  <strong>{settings.stt_provider === "local" ? "Локально" : "BYOK"}</strong>
                  <small>Приватность</small>
                </span>
              </div>
            </div>

            <div className="voice-stage">
              <div className="waveform" aria-hidden="true">
                {WAVE_BARS.map((height, index) => (
                  <span key={index} style={{ height }} />
                ))}
              </div>
              <button
                type="button"
                className="voice-orb"
                data-testid="dictation-orb"
                onClick={onToggle}
                disabled={busy}
                aria-label={status === "recording" ? "Остановить диктовку" : "Начать диктовку"}
              >
                <Icon.Mic />
              </button>
              <div className="waveform waveform-right" aria-hidden="true">
                {WAVE_BARS.map((_, index) => (
                  <span key={index} style={{ height: WAVE_BARS[WAVE_BARS.length - index - 1] }} />
                ))}
              </div>
            </div>
            <div className="voice-status"><span />{statusText}</div>
          </section>

          <section className="today-section">
            <div className="section-line-head">
              <h2>Сегодня</h2>
              <button type="button" className="text-action" onClick={() => onOpenSettings("personalization")}>Исправления</button>
            </div>
            <div className="transcript-list">
              {history.length === 0 ? (
                <div className="hub-empty">Первая диктовка появится здесь и останется только на устройстве.</div>
              ) : (
                history.slice(0, 3).map((item, index) => (
                  <article className="transcript-row" key={`${item.ts}-${index}`}>
                    <time>{fmtTime(item.ts)}</time>
                    <span className="transcript-app">{item.app || "Приложение"}</span>
                    <p>{item.text}</p>
                    <span className="transcript-speed">{item.words} сл.</span>
                    <button type="button" onClick={() => void onCopy(item)}>
                      {copied === item ? <Icon.Check className="ico" /> : <Icon.Code className="ico" />}
                      {copied === item ? "Готово" : "Копировать"}
                    </button>
                  </article>
                ))
              )}
            </div>
          </section>
        </div>

        <aside className="quick-rail" aria-label="Быстрые настройки">
          <h2>Быстрые настройки</h2>
          <button type="button" onClick={() => onOpenSettings("models")}>
            <Icon.Cube className="ico" /><span>Модель<small>{modelLabel(settings)}</small></span><b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("general")}>
            <Icon.Mic className="ico" /><span>Микрофон<small>{settings.input_device || "Системный"}</small></span><b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("dictation")}>
            <Icon.Sparkles className="ico" /><span>Очистка<small>{settings.verbatim ? "Дословно" : "Умная"}</small></span><b>›</b>
          </button>
          <button type="button" onClick={() => onOpenSettings("models")}>
            <Icon.Clock className="ico" /><span>Задержка<small>{latencyMs == null ? "После первой фразы" : `≈ ${latencyMs} мс`}</small></span><b>›</b>
          </button>
          <div className="privacy-note">
            <Icon.Check className="ico" />
            <div>
              <strong>{settings.stt_provider === "local" ? "Локально и приватно" : "Облачный BYOK"}</strong>
              <p>
                {settings.stt_provider === "local"
                  ? "Аудио обрабатывается на вашем устройстве."
                  : "Аудио отправляется только выбранному вами провайдеру."}
              </p>
            </div>
          </div>
        </aside>
      </div>
    </div>
  );
}
