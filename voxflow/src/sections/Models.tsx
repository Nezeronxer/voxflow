import { useEffect, useRef, useState } from "react";
import { listModels, downloadModel, deleteModel, subscribe } from "../api";
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

// Слабые пресеты создают ложный выбор и заметно уступают Turbo. Новым
// пользователям их не показываем; уже установленную/активную legacy-модель
// оставляем видимой, чтобы её можно было безопасно сменить или удалить.
const WEAK_WHISPER_MODELS = new Set([
  "ggml-tiny.bin",
  "ggml-base.bin",
  "ggml-small.bin",
]);

const PRIMARY_ENGINE_OPTIONS = [
  { value: "whisper_server", label: "Whisper Server (все языки)" },
  { value: "gigaam", label: "GigaAM RU / Parakeet EN" },
];
const LEGACY_CLI_OPTION = {
  value: "whisper_cli",
  label: "Whisper CLI (устаревший, медленнее)",
};

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

// Hero-карточка каталожной ONNX-модели (GigaAM/Parakeet): статус, суммарный
// прогресс со скоростью/ETA, скачать/удалить. Логика и классы общие — карточки
// различаются только данными.
function HeroModelCard({
  model,
  prog,
  subtitle,
  onDownload,
  onDelete,
}: {
  model: ModelInfo;
  prog?: Progress;
  subtitle: string;
  onDownload: (name: string) => void;
  onDelete: (name: string) => void;
}) {
  const dl = prog && !prog.error ? prog : undefined;
  const pct =
    dl && dl.total > 0
      ? Math.min(100, Math.round((dl.received / dl.total) * 100))
      : 0;
  return (
    <div className="card" style={{ borderColor: "var(--border-strong)" }}>
      <div className="model-row" style={{ borderBottom: "none" }}>
        <div className="model-icon">
          <Icon.Cube />
        </div>
        <div className="model-info">
          <div className="model-name">
            {model.label}{" "}
            {model.installed && <span className="badge ok">✓ Установлена</span>}
          </div>
          <div className="model-size">
            {subtitle} · {fmtSize(model.size_mb)}
            {prog?.error ? (
              <span style={{ color: "var(--red)", marginLeft: 8 }}>
                {prog.error}
              </span>
            ) : null}
          </div>
        </div>

        {dl ? (
          <div
            className="progress-wrap"
            style={{ flexDirection: "column", alignItems: "flex-end", gap: 6 }}
          >
            <div className="progress-wrap">
              <div className="progress">
                <div className="bar" style={{ width: `${pct}%` }} />
              </div>
              <span className="progress-pct">{pct}%</span>
            </div>
            {/* «12 МБ/с · осталось 0:18» — скорость EMA + ETA по ней */}
            {dl.speed && dl.eta !== undefined ? (
              <span className="model-size">
                {fmtSpeed(dl.speed)} · {fmtEta(dl.eta)}
              </span>
            ) : (
              <span className="model-size">скачивание…</span>
            )}
          </div>
        ) : model.installed ? (
          <button
            className="btn btn-sm btn-danger"
            onClick={() => onDelete(model.name)}
            title="Удалить"
          >
            <Icon.Trash className="ico" />
          </button>
        ) : (
          <button
            className="btn btn-sm btn-primary"
            onClick={() => onDownload(model.name)}
          >
            <Icon.Download className="ico" />
            Скачать
          </button>
        )}
      </div>
    </div>
  );
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
    // subscribe снимает listener, даже если async listen() резолвится
    // уже после cleanup (важно для StrictMode и быстрой смены вкладки).
    const offs = [
      subscribe<ModelProgressEvent>("model:progress", (e) => {
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
      }),
      subscribe<ModelDoneEvent>("model:done", (e) => {
        const name = e.payload?.name;
        if (!name) return;
        delete speedRef.current[name];
        setProgress((prev) => {
          const next = { ...prev };
          delete next[name];
          return next;
        });
        refresh();
      }),
      subscribe<ModelErrorEvent>("model:error", (e) => {
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
      }),
    ];

    return () => offs.forEach((off) => off());
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

  // Бэкенд кладёт каталожные ONNX-модели первыми строками (kind:"gigaam"/"parakeet");
  // рисуем их hero-карточками, остальное (whisper) — привычным списком ниже.
  const giga = models.find((m) => m.kind === "gigaam");
  const para = models.find((m) => m.kind === "parakeet");
  const whisperModels = models.filter((m) => {
    if (m.kind === "gigaam" || m.kind === "parakeet") return false;
    if (!WEAK_WHISPER_MODELS.has(m.name)) return true;
    return m.installed || settings.model === m.name;
  });
  // Новому выбору CLI не предлагаем. Если он уже сохранён, option остаётся
  // видимым, чтобы select не получил неизвестный value и переход был явным.
  const engineOptions =
    settings.engine === "whisper_cli"
      ? [...PRIMARY_ENGINE_OPTIONS, LEGACY_CLI_OPTION]
      : PRIMARY_ENGINE_OPTIONS;

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
            Скачайте модель, чтобы начать распознавание. На свежей установке
            VoxFlow автоматически готовит Whisper Large v3 Turbo для всех языков.
          </span>
        </div>
      )}

      {/* Parakeet — специализированный маршрут только для explicit EN. */}
      {settings.language === "en" &&
        para &&
        !para.installed && (
          <div className="toast" role="status">
            <span className="toast-msg">
              Для быстрого английского распознавания скачайте Parakeet TDT v3
              ниже. Без неё English распознаётся запасным Whisper.
            </span>
          </div>
        )}

      {/* ── Hero-карточка GigaAM: основная русская модель ── */}
      {giga && (
        <HeroModelCard
          model={giga}
          prog={progress[giga.name]}
          subtitle="Русская речь, пунктуация, офлайн на CPU"
          onDownload={onDownload}
          onDelete={onDelete}
        />
      )}

      {/* ── Hero-карточка Parakeet: специализированный explicit EN маршрут ── */}
      {para && (
        <HeroModelCard
          model={para}
          prog={progress[para.name]}
          subtitle="Английская речь, офлайн на CPU"
          onDownload={onDownload}
          onDelete={onDelete}
        />
      )}

      {/* ── Whisper: универсальный локальный движок; выбор активной модели как раньше ── */}
      <div className="card">
        <div className="card-head">
          <div className="card-title">
            Whisper (все языки)
          </div>
          <div className="sub">
            Основная модель для авто/смешанной речи и универсальный запасной маршрут
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
        <Field
          label="Язык"
          hint="Авто и смешанная речь — Whisper Large v3 Turbo; Русский — GigaAM; English — Parakeet. Если специализированная модель не установлена, используется запасной Whisper."
        >
          <Select
            value={settings.language}
            onChange={(v) => update({ language: v })}
            options={[
              { value: "auto", label: "Все языки (авто)" },
              { value: "ru", label: "Русский" },
              { value: "en", label: "English" },
              { value: "uk", label: "Українська" },
              { value: "de", label: "Deutsch" },
              { value: "fr", label: "Français" },
              { value: "es", label: "Español" },
              { value: "it", label: "Italiano" },
              { value: "pt", label: "Português" },
              { value: "pl", label: "Polski" },
              { value: "tr", label: "Türkçe" },
              { value: "zh", label: "中文" },
              { value: "ja", label: "日本語" },
              { value: "ko", label: "한국어" },
              { value: "ar", label: "العربية" },
              { value: "hi", label: "हिन्दी" },
            ]}
          />
        </Field>
        <Field
          label="Движок"
          hint="Whisper Server обслуживает авто и смешанную речь. GigaAM/Parakeet — быстрые специализированные маршруты для явно выбранных RU/EN."
        >
          <Select
            value={settings.engine}
            onChange={(v) => update({ engine: v })}
            options={engineOptions}
          />
        </Field>
        <Field
          label="Потоки"
          hint="0 — автоматически; 1–32 — явное число потоков CPU (больше — быстрее, но выше нагрузка)"
        >
          <input
            type="number"
            min={0}
            max={32}
            value={settings.threads}
            onChange={(e) => {
              const n = parseInt(e.currentTarget.value, 10);
              update({
                threads: Number.isFinite(n) ? Math.min(32, Math.max(0, n)) : 0,
              });
            }}
          />
        </Field>
      </div>
    </div>
  );
}
