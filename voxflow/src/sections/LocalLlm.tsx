import { useCallback, useEffect, useRef, useState } from "react";
import {
  localLlmDelete,
  localLlmDownload,
  localLlmState,
  localLlmStop,
  subscribe,
} from "../api";
import { Icon, PageHead, Switch } from "../ui";
import type {
  LocalLlmState,
  ModelDoneEvent,
  ModelErrorEvent,
  ModelProgressEvent,
  Settings,
} from "../types";

const EMPTY: LocalLlmState = {
  runtime_tag: "",
  runtime_installed: false,
  gpu: false,
  downloading: false,
  machine: { ram_gb: 0, cpu_cores: 0, accel: { kind: "cpu_only" } },
  models: [],
  server: { running: false, model_id: null, gpu: false },
};

type Progress = { received: number; total: number; stage: "runtime" | "model" };

function accelLabel(m: LocalLlmState["machine"]): string {
  switch (m.accel.kind) {
    case "apple_silicon":
      return "Apple Silicon · Metal";
    case "nvidia":
      return m.accel.vram_gb > 0 ? `NVIDIA · ${m.accel.vram_gb} ГБ` : "NVIDIA";
    default:
      return "процессор";
  }
}

function fmtGb(v: number): string {
  return `${v.toFixed(1).replace(".0", "")} ГБ`;
}

export default function LocalLlm({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [state, setState] = useState<LocalLlmState>(EMPTY);
  const [progress, setProgress] = useState<Record<string, Progress>>({});
  const [note, setNote] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const pendingRef = useRef<string | null>(null);

  const refresh = useCallback(async () => {
    setState(await localLlmState());
  }, []);

  useEffect(() => {
    void refresh();
    const offs = [
      subscribe<ModelProgressEvent>("model:progress", (e) => {
        const id = pendingRef.current;
        if (!id) return;
        const name = e.payload.name;
        if (name !== id && name !== "llama-runtime") return;
        setProgress((p) => ({
          ...p,
          [id]: {
            received: e.payload.received,
            total: e.payload.total,
            stage: name === "llama-runtime" ? "runtime" : "model",
          },
        }));
      }),
      subscribe<ModelDoneEvent>("model:done", (e) => {
        if (e.payload.name !== pendingRef.current) return;
        const id = pendingRef.current;
        pendingRef.current = null;
        setProgress((p) => {
          const next = { ...p };
          delete next[id];
          return next;
        });
        setNote({ kind: "ok", text: "Модель установлена и готова к работе." });
        void refresh();
      }),
      subscribe<ModelErrorEvent>("model:error", (e) => {
        if (e.payload.name !== pendingRef.current) return;
        const id = pendingRef.current;
        pendingRef.current = null;
        setProgress((p) => {
          const next = { ...p };
          delete next[id];
          return next;
        });
        setNote({ kind: "error", text: e.payload.error || "Не удалось скачать модель." });
        void refresh();
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [refresh]);

  async function onDownload(id: string) {
    if (pendingRef.current) return;
    setNote(null);
    pendingRef.current = id;
    setProgress((p) => ({ ...p, [id]: { received: 0, total: 0, stage: "runtime" } }));
    try {
      await localLlmDownload(id);
    } catch (e) {
      pendingRef.current = null;
      setProgress((p) => {
        const next = { ...p };
        delete next[id];
        return next;
      });
      setNote({ kind: "error", text: e instanceof Error ? e.message : String(e) });
    }
  }

  async function onDelete(id: string) {
    setNote(null);
    try {
      await localLlmDelete(id);
      setNote({ kind: "ok", text: "Модель удалена." });
    } catch (e) {
      setNote({ kind: "error", text: e instanceof Error ? e.message : String(e) });
    }
    void refresh();
  }

  function onUse(id: string) {
    update({ ai_backend: "builtin", ai_backend_behavior_version: 1, builtin_llm_model: id, cloud_asr: false });
    setNote({ kind: "ok", text: "Включено: текст после распознавания правит эта модель." });
  }

  async function onUnload() {
    await localLlmStop();
    void refresh();
  }

  const enabled = settings.ai_backend === "builtin";
  const installedCount = state.models.filter((m) => m.installed).length;
  const active = state.models.find((m) => m.id === settings.builtin_llm_model);

  return (
    <div className="content-inner">
      <PageHead
        title="Локальный ИИ"
        desc="Нейросеть на вашем компьютере убирает паразиты, расставляет знаки и исправляет ослышки. Аудио и текст никуда не уходят."
      />

      <section className="llm-hero">
        <div>
          <div className="kicker">
            {enabled && active ? `работает · ${active.label}` : installedCount > 0 ? "выключен" : "не установлен"}
          </div>
          <h2>
            {enabled
              ? "Обработка текста включена"
              : installedCount > 0
                ? "Модель скачана, обработка выключена"
                : "Скачайте модель — и диктовка станет чище"}
          </h2>
          <p>
            Движок llama.cpp {state.runtime_tag ? `(${state.runtime_tag})` : ""} и модели
            скачиваются один раз и проверяются по контрольной сумме. Работает без интернета и без ключей.
          </p>
          <div className="llm-machine">
            <div>
              <small>Память</small>
              <strong>{state.machine.ram_gb > 0 ? `${state.machine.ram_gb} ГБ` : "—"}</strong>
            </div>
            <div>
              <small>Ядер</small>
              <strong>{state.machine.cpu_cores || "—"}</strong>
            </div>
            <div>
              <small>Считает</small>
              <strong>{state.gpu ? accelLabel(state.machine) : accelLabel({ ...state.machine, accel: { kind: "cpu_only" } })}</strong>
            </div>
            <div>
              <small>В памяти</small>
              <strong>
                {state.server.running
                  ? state.models.find((m) => m.id === state.server.model_id)?.label ?? "модель"
                  : state.server.starting
                    ? "загружается…"
                    : "ничего"}
              </strong>
            </div>
          </div>
        </div>
        <div className="llm-hero-side">
          <label className="row-flex" style={{ gap: 10 }}>
            <span className="hint" style={{ margin: 0 }}>Обрабатывать текст</span>
            <Switch
              checked={enabled}
              onChange={(v) => {
                if (v) {
                  const pick = active?.installed ? active.id : state.models.find((m) => m.installed)?.id;
                  if (!pick) {
                    setNote({ kind: "error", text: "Сначала скачайте хотя бы одну модель." });
                    return;
                  }
                  onUse(pick);
                } else {
                  update({ ai_backend: "off" });
                }
              }}
            />
          </label>
          {state.server.running && (
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => void onUnload()}>
              Выгрузить из памяти
            </button>
          )}
        </div>
      </section>

      {note && (
        <div className={`toast ${note.kind === "ok" ? "toast-success" : "toast-error"}`} role="status" style={{ marginBottom: 14 }}>
          <span className="toast-msg">{note.text}</span>
        </div>
      )}

      <div className="llm-grid">
        {state.models.map((m) => {
          const prog = progress[m.id];
          const pct = prog && prog.total > 0 ? Math.min(100, Math.round((prog.received / prog.total) * 100)) : 0;
          const isActive = enabled && settings.builtin_llm_model === m.id;
          const busy = pendingRef.current !== null;
          return (
            <div
              key={m.id}
              className={`llm-tile${isActive ? " is-active" : ""}${!m.fits ? " is-heavy" : ""}`}
            >
              <div className="llm-tile-top">
                <strong>{m.label}</strong>
                {isActive && <span className="badge ok">работает</span>}
                {!isActive && m.installed && <span className="badge">скачана</span>}
                {m.recommended && !m.installed && <span className="badge accent">под ваш компьютер</span>}
                {!m.fits && <span className="badge warn">тяжёлая для этой машины</span>}
              </div>
              <div className="llm-tile-meta">{fmtGb(m.size_gb)} · от {m.min_ram_gb} ГБ памяти</div>
              <p>{m.blurb}</p>
              <div className="llm-tile-actions">
                {prog ? (
                  <>
                    <div className="progress-wrap">
                      <div className="progress"><div className="bar" style={{ width: `${pct}%` }} /></div>
                      <span className="progress-pct">{pct}%</span>
                    </div>
                    <span className="hint" style={{ margin: 0 }}>
                      {prog.stage === "runtime" ? "движок" : "модель"}
                    </span>
                  </>
                ) : m.installed ? (
                  <>
                    {!isActive && (
                      <button type="button" className="btn btn-sm btn-primary" onClick={() => onUse(m.id)}>
                        Использовать
                      </button>
                    )}
                    <button
                      type="button"
                      className="btn btn-sm btn-ghost btn-danger"
                      onClick={() => void onDelete(m.id)}
                      title="Удалить файл модели"
                    >
                      <Icon.Trash className="ico" />
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="btn btn-sm btn-primary"
                    disabled={busy}
                    onClick={() => void onDownload(m.id)}
                  >
                    <Icon.Download className="ico" />
                    Скачать
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>

      <p className="hint" style={{ marginTop: 16 }}>
        Ollama, LM Studio или облачные сервисы по-прежнему можно выбрать в
        «Настройки → Обработка текста». Ускорение видеокартой включается в «Настройки → Диктовка».
      </p>
    </div>
  );
}
