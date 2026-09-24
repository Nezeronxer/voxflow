import { useEffect, useRef, useState } from "react";
import {
  listModels,
  downloadModel,
  cancelModelDownload,
  deleteModel,
  subscribe,
  modelsDir,
  revealPath,
  openExternalUrl,
} from "../api";
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

// Прогресс загрузки: полоска, «12 МБ/с · осталось 0:18» и отмена. Общий для
// hero-карточек и строк Whisper.
function DownloadProgress({
  name,
  prog,
}: {
  name: string;
  prog: Progress;
}) {
  const pct =
    prog.total > 0
      ? Math.min(100, Math.round((prog.received / prog.total) * 100))
      : 0;
  return (
    <div
      className="progress-wrap"
      style={{ flexDirection: "column", alignItems: "flex-end", gap: 6 }}
    >
      <div className="progress-wrap">
        <div className="progress">
          <div className="bar" style={{ width: `${pct}%` }} />
        </div>
        <span className="progress-pct">{pct}%</span>
        <button
          className="btn btn-sm"
          onClick={() => void cancelModelDownload(name)}
          title="Остановить. Уже скачанное сохранится — следующая загрузка продолжит с места"
        >
          Отмена
        </button>
      </div>
      {prog.speed && prog.eta !== undefined ? (
        <span className="model-size">
          {fmtSpeed(prog.speed)} · {fmtEta(prog.eta)}
        </span>
      ) : (
        <span className="model-size">скачивание…</span>
      )}
    </div>
  );
}

// Размер уже показан отдельно — из подписи каталога убираем «, 574 МБ».
function cleanLabel(label: string): string {
  return label
    .replace(/,?\s*\d[\d.,]*\s*(МБ|ГБ)\)/, ")")
    .replace(/\s*\(\)/, "")
    .replace(/\s*\(\d[\d.,]*\s*(МБ|ГБ)\)/, "");
}

const KIND_NOTE: Record<string, string> = {
  gigaam: "Русский",
  parakeet: "English",
  whisper: "Все языки",
};

// Строка модели: название, язык и размер, справа — действие по состоянию:
// прогресс с отменой, «Выбрать»/«Активна» для Whisper, удалить, скачать.
function ModelRow({
  model,
  prog,
  selected,
  onSelect,
  onDownload,
  onDelete,
}: {
  model: ModelInfo;
  prog?: Progress;
  selected: boolean;
  onSelect?: () => void;
  onDownload: (name: string) => void;
  onDelete: (name: string) => void;
}) {
  const dl = prog && !prog.error ? prog : undefined;
  return (
    <div className={`model-row ${selected ? "selected" : ""}`}>
      <div className="model-info">
        <div className="model-name">
          {cleanLabel(model.label || model.name)}
          {selected && <span className="badge accent">Активна</span>}
        </div>
        <div className="model-size">
          {KIND_NOTE[model.kind ?? ""] ?? ""} · {fmtSize(model.size_mb)}
          {prog?.error ? <span className="model-error">{prog.error}</span> : null}
        </div>
      </div>
      {dl ? (
        <DownloadProgress name={model.name} prog={dl} />
      ) : model.installed ? (
        <div className="row-flex">
          {onSelect && !selected && (
            <button className="btn btn-sm" onClick={onSelect}>
              Выбрать
            </button>
          )}
          <button
            className="btn btn-sm btn-ghost"
            onClick={() => onDelete(model.name)}
            title="Удалить с диска"
            aria-label={`Удалить ${model.label}`}
          >
            <Icon.Trash className="ico" />
          </button>
        </div>
      ) : (
        <button className="btn btn-sm" onClick={() => onDownload(model.name)}>
          <Icon.Download className="ico" />
          Скачать
        </button>
      )}
    </div>
  );
}

// Страница меню «Модели распознавания»: список моделей и свёрнутое
// «Дополнительно» (движок, потоки).
export default function Models({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [progress, setProgress] = useState<Record<string, Progress>>({});
  const [dir, setDir] = useState("");
  const speedRef = useRef<Record<string, SpeedSample>>({});

  async function refresh() {
    setModels(await listModels());
  }

  useEffect(() => {
    void modelsDir().then(setDir);
  }, []);

  function dropProgress(name?: string): boolean {
    if (!name) return false;
    delete speedRef.current[name];
    setProgress((prev) => {
      const next = { ...prev };
      delete next[name];
      return next;
    });
    return true;
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
        if (dropProgress(e.payload?.name)) refresh();
      }),
      // Отмена: прогресс убираем, кнопка снова «Скачать» (продолжит с места).
      subscribe<ModelDoneEvent>("model:cancelled", (e) => {
        dropProgress(e.payload?.name);
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

  // Слабые Whisper-пресеты показываем, только если уже стоят или выбраны.
  const visible = models.filter(
    (m) =>
      !WEAK_WHISPER_MODELS.has(m.name) || m.installed || settings.model === m.name,
  );
  // Наверху — то, что уже на диске или качается; остальное свёрнуто.
  const inUse = visible.filter((m) => m.installed || progress[m.name]);
  const others = visible.filter((m) => !m.installed && !progress[m.name]);
  // Новому выбору CLI не предлагаем. Если он уже сохранён, option остаётся
  // видимым, чтобы select не получил неизвестный value и переход был явным.
  const engineOptions =
    settings.engine === "whisper_cli"
      ? [...PRIMARY_ENGINE_OPTIONS, LEGACY_CLI_OPTION]
      : PRIMARY_ENGINE_OPTIONS;

  const row = (m: ModelInfo) => (
    <ModelRow
      key={m.name}
      model={m}
      prog={progress[m.name]}
      selected={m.kind === "whisper" && settings.model === m.name}
      onSelect={m.kind === "whisper" ? () => update({ model: m.name }) : undefined}
      onDownload={onDownload}
      onDelete={onDelete}
    />
  );

  const main = (
    <div className="card">
      <div className="card-head">
        <div className="card-title">Модели на устройстве</div>
        <div className="sub">
          {settings.stt_provider !== "local"
            ? "Сейчас распознаёт облако — модели нужны для офлайна и живого текста в плашке."
            : "Работают без интернета. Русский — GigaAM, English — Parakeet, остальные языки — Whisper."}
        </div>
      </div>

      {models.length > 0 && inUse.length === 0 && (
        <div className="toast toast-warning" role="alert">
          <span className="toast-msg">Скачайте хотя бы одну модель, чтобы начать диктовку.</span>
        </div>
      )}

      {models.length === 0 ? (
        <div className="empty">Список моделей загружается…</div>
      ) : (
        inUse.map(row)
      )}

      {others.length > 0 && (
        <details className="model-more">
          <summary>Другие модели ({others.length})</summary>
          {others.map(row)}
        </details>
      )}

      {/* Где лежат файлы: без этого путь к моделям выясняется только из логов. */}
      <div className="model-footer">
        <code>{dir || "…"}</code>
        {dir && (
          <button type="button" className="link-btn" onClick={() => void revealPath(dir)}>
            Открыть папку
          </button>
        )}
        <button
          type="button"
          className="link-btn"
          onClick={() => void openExternalUrl("https://huggingface.co")}
        >
          Источник: huggingface.co
        </button>
      </div>
    </div>
  );

  const advanced = (
    <>
      <Field
        label="Движок на устройстве"
        hint="Whisper — все языки; GigaAM/Parakeet — быстрее для русского и английского"
      >
        <Select
          value={settings.engine}
          onChange={(v) => update({ engine: v })}
          options={engineOptions}
        />
      </Field>
      <Field label="Потоки процессора" hint="0 — автоматически">
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
    </>
  );

  return (
    <div className="content-inner settings-flat">
      <PageHead
        title="Модели распознавания"
        desc="Переводят голос в текст прямо на компьютере, без интернета."
      />
      {main}
      <details className="card settings-advanced">
        <summary>Дополнительно</summary>
        {advanced}
      </details>
    </div>
  );
}
