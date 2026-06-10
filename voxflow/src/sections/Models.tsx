import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { listModels, downloadModel, deleteModel } from "../api";
import { PageHead, Field, Select, Icon } from "../ui";
import type {
  Settings,
  ModelInfo,
  ModelProgressEvent,
  ModelDoneEvent,
  ModelErrorEvent,
} from "../types";

// Коэффициент EMA-сглаживания мгновенной скорости: резкие скачки сети не дёргают ETA.
const SPEED_EMA = 0.3;

type Progress = {
  received: number;
  total: number;
  error?: string;
  speed?: number; // байт/с, сглаженная EMA
  eta?: number; // секунд до конца, по сглаженной скорости
};

// Память для расчёта скорости между событиями прогресса (вне React-состояния).
type SpeedSample = { received: number; t: number; ema: number };

function fmtSize(mb: number): string {
  if (mb >= 1024) return `${(mb / 1024).toFixed(1)} ГБ`;
  return `${mb} МБ`;
}

// «12 МБ/с» — десятичные мегабайты, одна цифра после запятой на малых скоростях.
function fmtSpeed(bps: number): string {
  const mbps = bps / 1_000_000;
  return `${mbps >= 10 ? Math.round(mbps) : mbps.toFixed(1)} МБ/с`;
}

// «осталось 0:18» — минуты:секунды.
function fmtEta(sec: number): string {
  const s = Math.max(0, Math.round(sec));
  const m = Math.floor(s / 60);
  return `осталось ${m}:${String(s % 60).padStart(2, "0")}`;
}

export default function Models({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, Progress>>({});
  const speedRef = useRef<Record<string, SpeedSample>>({});

  async function refresh() {
    setModels(await listModels());
  }

  useEffect(() => {
    refresh();
    const unlisteners: Array<() => void> = [];

    listen<ModelProgressEvent>("model:progress", (e) => {
      const p = e.payload;
      if (!p?.name) return;
      // Скорость: дельта байт / дельта времени между событиями (~400 мс), EMA 0.3.
      const now = performance.now();
      const prev = speedRef.current[p.name];
      let ema = prev?.ema ?? 0;
      if (prev && now > prev.t && p.received >= prev.received) {
        const inst = ((p.received - prev.received) * 1000) / (now - prev.t);
        ema = ema > 0 ? ema * (1 - SPEED_EMA) + inst * SPEED_EMA : inst;
      }
      speedRef.current[p.name] = { received: p.received, t: now, ema };
      setProgress((prevState) => ({
        ...prevState,
        [p.name]: {
          received: p.received,
          total: p.total,
          speed: ema > 0 ? ema : undefined,
          eta:
            ema > 0 && p.total > 0 && p.total >= p.received
              ? (p.total - p.received) / ema
              : undefined,
        },
      }));
    })
      .then((fn) => unlisteners.push(fn))
      .catch(() => {});

    listen<ModelDoneEvent>("model:done", (e) => {
      const name = e.payload?.name;
      if (!name) return;
      delete speedRef.current[name];
      setProgress((prev) => {
        const next = { ...prev };
        delete next[name];
        return next;
      });
      refresh();
    })
      .then((fn) => unlisteners.push(fn))
      .catch(() => {});

    listen<ModelErrorEvent>("model:error", (e) => {
      const name = e.payload?.name;
      if (!name) return;
      delete speedRef.current[name];
      setProgress((prev) => ({
        ...prev,
        [name]: {
          received: prev[name]?.received ?? 0,
          total: prev[name]?.total ?? 0,
          error: e.payload?.error || "Ошибка загрузки",
        },
      }));
    })
      .then((fn) => unlisteners.push(fn))
      .catch(() => {});

    return () => unlisteners.forEach((u) => u());
  }, []);

  async function onDownload(name: string) {
    delete speedRef.current[name];
    setProgress((prev) => ({
      ...prev,
      [name]: { received: 0, total: 0 },
    }));
    await downloadModel(name);
  }

  async function onDelete(name: string) {
    await deleteModel(name);
    refresh();
  }

  // Бэкенд кладёт GigaAM первой строкой (kind:"gigaam"); рисуем её отдельной
  // hero-карточкой, остальное (whisper) — привычным списком ниже.
  const giga = models.find((m) => m.kind === "gigaam");
  const whisperModels = models.filter((m) => m.kind !== "gigaam");

  const gigaProg = giga ? progress[giga.name] : undefined;
  const gigaDownloading = !!gigaProg && !gigaProg.error;
  const gigaPct =
    gigaProg && gigaProg.total > 0
      ? Math.min(100, Math.round((gigaProg.received / gigaProg.total) * 100))
      : 0;

  return (
    <div className="content-inner">
      <PageHead
        title="Модель"
        desc="Модели распознавания речи хранятся локально и работают офлайн."
      />

      {settings.stt_provider !== "local" && (
        <div className="toast" role="status">
          <span className="toast-msg">
            Сейчас активна ОНЛАЙН-модель распознавания:{" "}
            {settings.stt_provider === "deepgram"
              ? settings.deepgram_model
              : settings.oai_stt_model}{" "}
            (настраивается во вкладке «Облако»). Локальная модель ниже — не
            обязательна: скачайте её только для офлайн-режима и более быстрого
            живого черновика.
          </span>
        </div>
      )}

      {models.length > 0 && !models.some((m) => m.installed) && (
        <div className="toast toast-warning" role="alert">
          <span className="toast-msg">
            Скачайте модель, чтобы начать распознавание. Рекомендуем GigaAM-v3 —
            она скачается автоматически при первом запуске.
          </span>
        </div>
      )}

      {/* ── Hero-карточка GigaAM: основная русская модель, ставится/удаляется здесь ── */}
      {giga && (
        <div
          className="card"
          style={{ borderColor: "var(--border-strong)" }}
        >
          <div className="model-row" style={{ borderBottom: "none" }}>
            <div className="model-icon">
              <Icon.Cube />
            </div>
            <div className="model-info">
              <div className="model-name">
                {giga.label}{" "}
                {giga.installed && (
                  <span className="badge ok">✓ Установлена</span>
                )}
              </div>
              <div className="model-size">
                Русская речь, пунктуация, офлайн на CPU ·{" "}
                {fmtSize(giga.size_mb)}
                {gigaProg?.error ? (
                  <span style={{ color: "var(--red)", marginLeft: 8 }}>
                    {gigaProg.error}
                  </span>
                ) : null}
              </div>
            </div>

            {gigaDownloading ? (
              <div className="progress-wrap" style={{ flexDirection: "column", alignItems: "flex-end", gap: 6 }}>
                <div className="progress-wrap">
                  <div className="progress">
                    <div className="bar" style={{ width: `${gigaPct}%` }} />
                  </div>
                  <span className="progress-pct">{gigaPct}%</span>
                </div>
                {/* «12 МБ/с · осталось 0:18» — скорость EMA + ETA по ней */}
                {gigaProg.speed && gigaProg.eta !== undefined ? (
                  <span className="model-size">
                    {fmtSpeed(gigaProg.speed)} · {fmtEta(gigaProg.eta)}
                  </span>
                ) : (
                  <span className="model-size">скачивание…</span>
                )}
              </div>
            ) : giga.installed ? (
              <button
                className="btn btn-sm btn-danger"
                onClick={() => onDelete(giga.name)}
                title="Удалить"
              >
                <Icon.Trash className="ico" />
              </button>
            ) : (
              <button
                className="btn btn-sm btn-primary"
                onClick={() => onDownload(giga.name)}
              >
                <Icon.Download className="ico" />
                Скачать
              </button>
            )}
          </div>
        </div>
      )}

      {/* ── Whisper: запасной движок; выбор активной модели (settings.model) как раньше ── */}
      <div className="card">
        <div className="card-head">
          <div className="card-title">
            Whisper (английский / запасной движок)
          </div>
          <div className="sub">
            Используется, когда движок переключён на Whisper
          </div>
        </div>

        {whisperModels.length === 0 ? (
          <div className="empty">Список моделей пуст или ещё загружается…</div>
        ) : (
          whisperModels.map((m) => {
            const prog = progress[m.name];
            const downloading = !!prog && !prog.error;
            const pct =
              prog && prog.total > 0
                ? Math.min(100, Math.round((prog.received / prog.total) * 100))
                : 0;
            const isSelected = settings.model === m.name;
            return (
              <div
                key={m.name}
                className={`model-row ${isSelected ? "selected" : ""}`}
              >
                <div className="model-icon">
                  <Icon.Cube />
                </div>
                <div className="model-info">
                  <div className="model-name">
                    {m.label || m.name}{" "}
                    {isSelected && <span className="badge accent">Активна</span>}
                  </div>
                  <div className="model-size">
                    {fmtSize(m.size_mb)}
                    {prog?.error ? (
                      <span style={{ color: "var(--red)", marginLeft: 8 }}>
                        {prog.error}
                      </span>
                    ) : null}
                  </div>
                </div>

                {downloading ? (
                  <div className="progress-wrap">
                    <div className="progress">
                      <div className="bar" style={{ width: `${pct}%` }} />
                    </div>
                    <span className="progress-pct">{pct}%</span>
                  </div>
                ) : m.installed ? (
                  <div className="row-flex">
                    {!isSelected && (
                      <button
                        className="btn btn-sm"
                        onClick={() => update({ model: m.name })}
                      >
                        Выбрать
                      </button>
                    )}
                    <span className="badge ok">Установлена</span>
                    <button
                      className="btn btn-sm btn-danger"
                      onClick={() => onDelete(m.name)}
                      title="Удалить"
                    >
                      <Icon.Trash className="ico" />
                    </button>
                  </div>
                ) : (
                  <button
                    className="btn btn-sm btn-primary"
                    onClick={() => onDownload(m.name)}
                  >
                    <Icon.Download className="ico" />
                    Скачать
                  </button>
                )}
              </div>
            );
          })
        )}
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Параметры распознавания</div>
        </div>
        <Field label="Язык" hint="Язык речи для модели распознавания">
          <Select
            value={settings.language}
            onChange={(v) => update({ language: v })}
            options={[
              { value: "ru", label: "Русский" },
              { value: "en", label: "English" },
              { value: "auto", label: "Авто" },
            ]}
          />
        </Field>
        <Field
          label="Движок"
          hint="GigaAM — русская модель, быстро на CPU. Whisper Server — модель в памяти (быстрые повторы). Whisper CLI — грузит модель каждый раз."
        >
          <Select
            value={settings.engine}
            onChange={(v) => update({ engine: v })}
            options={[
              { value: "gigaam", label: "GigaAM (русский, рекомендуется)" },
              { value: "whisper_server", label: "Whisper Server (быстро)" },
              { value: "whisper_cli", label: "Whisper CLI (медленнее)" },
            ]}
          />
        </Field>
        <Field
          label="Потоки"
          hint="Число потоков CPU для распознавания (больше — быстрее, выше нагрузка)"
        >
          <input
            type="number"
            min={1}
            max={32}
            value={settings.threads}
            onChange={(e) => {
              const n = parseInt(e.currentTarget.value, 10);
              update({ threads: Number.isFinite(n) ? n : 1 });
            }}
          />
        </Field>
      </div>
    </div>
  );
}
