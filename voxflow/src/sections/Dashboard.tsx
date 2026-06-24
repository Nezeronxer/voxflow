import { useEffect, useRef, useState } from "react";
import { getStats, toggleDictation, subscribe } from "../api";
import { PageHead, Icon, prettyHotkey } from "../ui";
import type { Settings, Stats, OverlayStatus, TranscriptEvent } from "../types";

export default function Dashboard({ settings }: { settings: Settings }) {
  const [stats, setStats] = useState<Stats>({
    today_words: 0,
    total_words: 0,
    total_sessions: 0,
    streak_days: 0,
    apps_count: 0,
  });
  const [status, setStatus] = useState<OverlayStatus>("idle");
  const [lastTranscript, setLastTranscript] = useState<string>("");
  const [busy, setBusy] = useState(false);
  // Дедуп по seq: транскрипт-событие может прийти дважды (StrictMode/async-гонки),
  // что давало двойной getStats() и затирание свежего текста устаревшим. Игнорируем
  // событие, чей seq не новее уже обработанного.
  const lastSeqRef = useRef(-1);

  useEffect(() => {
    let alive = true;
    getStats().then((s) => alive && setStats(s));

    // Race-safe подписки (см. subscribe в api.ts) против StrictMode-двойного маунта.
    const offs = [
      subscribe<TranscriptEvent>("transcript", (e) => {
        const seq = e.payload?.seq;
        if (seq != null && seq <= lastSeqRef.current) return; // устаревший/дубль
        if (seq != null) lastSeqRef.current = seq;
        if (e.payload?.text) {
          setLastTranscript(e.payload.text);
          getStats().then((s) => setStats(s));
        }
      }),
      subscribe<string>("status", (e) => {
        const v = e.payload;
        if (v === "recording" || v === "transcribing" || v === "idle") {
          setStatus(v);
        }
      }),
    ];

    return () => {
      alive = false;
      offs.forEach((off) => off());
    };
  }, []);

  async function onToggle() {
    setBusy(true);
    await toggleDictation();
    setTimeout(() => setBusy(false), 350);
  }

  const orbClass =
    status === "recording" ? "live" : status === "transcribing" ? "busy" : "";
  const statusText =
    status === "recording"
      ? "Идёт запись"
      : status === "transcribing"
        ? "Распознавание"
        : "Готов к работе";

  return (
    <div className="content-inner">
      <PageHead
        title="Главная"
        desc="Бесплатная локальная диктовка: работает на вашем устройстве и готова сразу после запуска."
      />

      {/* Hero */}
      <div className="hero">
        <div className={`mic-orb ${orbClass}`}>
          <Icon.Mic />
        </div>
        <div className="hero-body">
          <div className="hero-status">{statusText}</div>
          <h2 className="hero-title">VoxFlow готов слушать</h2>
          <div className="hero-meta">
            <span>
              Горячая клавиша: <span className="kbd">{prettyHotkey(settings.hotkey)}</span>
            </span>
            <span>
              Режим: <b>{settings.mode === "toggle" ? "Переключатель" : "Удержание"}</b>
            </span>
          </div>
        </div>
        <button
          className="btn btn-primary"
          onClick={onToggle}
          disabled={busy}
          style={{ padding: "12px 22px", fontSize: 14 }}
        >
          {status === "recording" ? "Остановить" : "Начать диктовку"}
        </button>
      </div>

      {/* Stats */}
      <div className="grid-3" style={{ marginTop: 18 }}>
        <div className="stat-card">
          <div className="stat-val accent">{stats.today_words.toLocaleString("ru-RU")}</div>
          <div className="stat-label">Слов сегодня</div>
        </div>
        <div className="stat-card">
          <div className="stat-val">{stats.total_words.toLocaleString("ru-RU")}</div>
          <div className="stat-label">Слов всего</div>
        </div>
        <div className="stat-card">
          <div className="stat-val">{stats.total_sessions.toLocaleString("ru-RU")}</div>
          <div className="stat-label">Сессий · серия {stats.streak_days} дн.</div>
        </div>
      </div>

      {/* Last transcript */}
      <div className="card" style={{ marginTop: 18 }}>
        <div className="card-head">
          <div className="row-flex" style={{ justifyContent: "space-between" }}>
            <div className="card-title">Последняя диктовка</div>
            {status === "recording" && (
              <span className="eq" aria-hidden>
                <span /><span /><span /><span /><span />
              </span>
            )}
          </div>
        </div>
        {lastTranscript ? (
          <div className="transcript-box">{lastTranscript}</div>
        ) : (
          <div className="transcript-box placeholder">
            Здесь появится распознанный текст последней диктовки
          </div>
        )}
      </div>
    </div>
  );
}
