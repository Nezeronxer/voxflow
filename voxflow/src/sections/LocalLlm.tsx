import { useCallback, useEffect, useRef, useState } from "react";
import {
  localLlmDelete,
  localLlmDownload,
  localLlmState,
  localLlmStop,
  subscribe,
} from "../api";
import { Icon } from "../ui";
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

  const machine = [
    state.machine.ram_gb > 0 ? `${state.machine.ram_gb} ГБ памяти` : "",
    state.machine.cpu_cores ? `${state.machine.cpu_cores} ядер` : "",
    state.gpu ? accelLabel(state.machine) : accelLabel({ ...state.machine, accel: { kind: "cpu_only" } }),
  ]
    .filter(Boolean)
    .join(" · ");

  // Секция страницы «Обработка текста»: те же строки, что у моделей
  // распознавания, — одна визуальная система для обоих видов моделей.
  return (
    <div className="card">
      <div className="card-head">
        <div className="card-title">Модели обработки на устройстве</div>
        <div className="sub">
          Правят текст без интернета и ключей. Этот компьютер: {machine}.
        </div>
      </div>

      {note && (
        <div className={`toast ${note.kind === "ok" ? "toast-success" : "toast-error"}`} role="status">
          <span className="toast-msg">{note.text}</span>
        </div>
      )}

      {state.models.map((m) => {
        const prog = progress[m.id];
        const pct = prog && prog.total > 0 ? Math.min(100, Math.round((prog.received / prog.total) * 100)) : 0;
        const isActive = enabled && settings.builtin_llm_model === m.id;
        const busy = pendingRef.current !== null;
        return (
          <div key={m.id} className={`model-row${isActive ? " selected" : ""}`}>
            <div className="model-info">
              <div className="model-name">
                {m.label}
                {isActive && <span className="badge accent">Активна</span>}
                {m.recommended && !m.installed && <span className="badge">Подходит</span>}
                {!m.fits && <span className="badge warn">Тяжёлая</span>}
              </div>
              <div className="model-size">
                {fmtGb(m.size_gb)} · от {m.min_ram_gb} ГБ памяти
              </div>
              {m.blurb && <div className="model-blurb">{m.blurb}</div>}
            </div>
            {prog ? (
              <div className="progress-wrap">
                <div className="progress"><div className="bar" style={{ width: `${pct}%` }} /></div>
                <span className="progress-pct">{prog.stage === "runtime" ? "движок" : `${pct}%`}</span>
              </div>
            ) : m.installed ? (
              <div className="row-flex">
                {!isActive && (
                  <button type="button" className="btn btn-sm" onClick={() => onUse(m.id)}>
                    Использовать
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-sm btn-ghost"
                  onClick={() => void onDelete(m.id)}
                  title="Удалить с диска"
                  aria-label={`Удалить ${m.label}`}
                >
                  <Icon.Trash className="ico" />
                </button>
              </div>
            ) : (
              <button
                type="button"
                className="btn btn-sm"
                disabled={busy}
                onClick={() => void onDownload(m.id)}
              >
                <Icon.Download className="ico" />
                Скачать
              </button>
            )}
          </div>
        );
      })}

      {(installedCount > 0 || state.server.running) && (
        <div className="model-footer">
          <span>
            {state.server.running
              ? `В памяти: ${state.models.find((m) => m.id === state.server.model_id)?.label ?? "модель"}`
              : state.server.starting
                ? "Модель загружается…"
                : "Модель выгружена из памяти"}
          </span>
          {state.server.running && (
            <button type="button" className="link-btn" onClick={() => void onUnload()}>
              Выгрузить
            </button>
          )}
        </div>
      )}
    </div>
  );
}
