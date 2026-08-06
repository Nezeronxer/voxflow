import { useCallback, useEffect, useRef, useState } from "react";
import { localAiDetect, localAiPull, subscribe } from "../api";
import type {
  LocalAiState,
  LocalAiSuggestion,
  ModelDoneEvent,
  ModelErrorEvent,
  ModelProgressEvent,
  Settings,
} from "../types";
import { Icon } from "../ui";

type Props = {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
};

const EMPTY: LocalAiState = {
  engines: [],
  machine: { ram_gb: 0, cpu_cores: 0, accel: { kind: "cpu_only" } },
  shortlist: [],
  too_heavy: [],
  can_pull: false,
  suggestion: null,
};

/// Человеческое описание того, чем машина будет считать модель. Показываем его
/// рядом с рядом моделей, чтобы было видно, ПОЧЕМУ предложено именно это.
function machineSummary(m: LocalAiState["machine"]): string {
  const parts: string[] = [];
  if (m.ram_gb > 0) parts.push(`${m.ram_gb} ГБ памяти`);
  if (m.cpu_cores > 0) parts.push(`${m.cpu_cores} ядер`);
  switch (m.accel.kind) {
    case "apple_silicon":
      parts.push("Apple Silicon, память общая с GPU");
      break;
    case "nvidia":
      parts.push(
        m.accel.vram_gb > 0
          ? `видеокарта NVIDIA, ${m.accel.vram_gb} ГБ видеопамяти`
          : "видеокарта NVIDIA, объём памяти не определился",
      );
      break;
    case "cpu_only":
      parts.push("без ускорителя — считать будет процессор");
      break;
  }
  return parts.join(", ");
}

/// Настройки, которыми включается найденный движок. LM Studio и llama.cpp идут
/// существующим OpenAI-совместимым маршрутом, поэтому различие только в полях.
function applyPatch(s: LocalAiSuggestion): Partial<Settings> {
  return s.backend === "ollama"
    ? {
        ai_backend: "ollama",
        ai_backend_behavior_version: 1,
        ollama_url: s.url,
        ollama_model: s.model,
      }
    : {
        ai_backend: "openai_compat",
        ai_backend_behavior_version: 1,
        rewrite_base_url: s.url,
        rewrite_model: s.model,
      };
}

export default function LocalAiCard({ settings, update }: Props) {
  const [state, setState] = useState<LocalAiState>(EMPTY);
  const [busy, setBusy] = useState(false);
  const [pulling, setPulling] = useState<string | null>(null);
  const [percent, setPercent] = useState(0);
  const [note, setNote] = useState<string | null>(null);
  // События model:* общие с загрузкой моделей распознавания. Без сверки имени
  // скачивание whisper показывало бы прогресс здесь, в чужой карточке.
  const pullingRef = useRef<string | null>(null);
  const mine = (name: string) => pullingRef.current !== null && name === pullingRef.current;

  const refresh = useCallback(async () => {
    setState(await localAiDetect());
  }, []);

  useEffect(() => {
    void refresh();
    // Приложение ищет движок и на старте — подхватываем его находку.
    const offFound = subscribe<LocalAiSuggestion>("local-ai:found", () => {
      void refresh();
    });
    const offProgress = subscribe<ModelProgressEvent>("model:progress", (e) => {
      if (!mine(e.payload.name) || !e.payload.total) return;
      setPercent(Math.round((e.payload.received / e.payload.total) * 100));
    });
    const offDone = subscribe<ModelDoneEvent>("model:done", (e) => {
      if (!mine(e.payload.name)) return;
      pullingRef.current = null;
      setPulling(null);
      setPercent(0);
      setNote("Модель установлена.");
      void refresh();
    });
    const offError = subscribe<ModelErrorEvent>("model:error", (e) => {
      if (!mine(e.payload.name)) return;
      pullingRef.current = null;
      setPulling(null);
      setPercent(0);
      setNote(e.payload.error || "Не удалось скачать модель");
    });
    return () => {
      offFound();
      offProgress();
      offDone();
      offError();
    };
  }, [refresh]);

  const installed = new Set(state.engines.flatMap((e) => e.models));
  const isInstalled = (tag: string) =>
    installed.has(tag) || [...installed].some((m) => m.startsWith(`${tag}-`));

  async function onPull(tag: string) {
    if (pulling) return;
    setNote(null);
    pullingRef.current = tag;
    setPulling(tag);
    setPercent(0);
    try {
      await localAiPull(tag);
    } catch (e) {
      pullingRef.current = null;
      setPulling(null);
      setNote(e instanceof Error ? e.message : String(e));
    }
  }

  function onKeep() {
    if (!state.suggestion || busy) return;
    setBusy(true);
    update(applyPatch(state.suggestion));
    setNote(`Включён ${state.suggestion.label}, модель ${state.suggestion.model}.`);
    setBusy(false);
    void refresh();
  }

  function onDismiss() {
    if (busy) return;
    setBusy(true);
    update({ local_ai_dismissed: true, ai_backend: "off" });
    setNote("Локальный ИИ выключен. Предлагать больше не буду.");
    setBusy(false);
    void refresh();
  }

  const found = state.engines.length > 0;

  return (
    <div className="card">
      <div className="card-head">
        <div className="card-title">Локальный ИИ</div>
        <div className="sub">
          {found
            ? `Найден на этом компьютере: ${state.engines.map((e) => e.label).join(", ")}.`
            : "На этом компьютере локальный ИИ не найден."}
          {machineSummary(state.machine) &&
            ` Ваш компьютер: ${machineSummary(state.machine)}. Ряд моделей подобран под него.`}
        </div>
      </div>

      {note && <p className="hint">{note}</p>}

      {state.suggestion && !settings.local_ai_dismissed && (
        <div className="app-group">
          <div className="app-group-title">
            Нашёл {state.suggestion.label}, модель {state.suggestion.model}
          </div>
          <p className="hint">
            Обработка пойдёт на вашем компьютере, без облака. Включить?
          </p>
          <div className="app-prompt-add">
            <button className="btn" type="button" onClick={onKeep} disabled={busy}>
              Оставить
            </button>
            <button className="btn" type="button" onClick={onDismiss} disabled={busy}>
              Выключить
            </button>
          </div>
        </div>
      )}

      {!found && (
        <p className="hint">
          <Icon.Sparkles className="ico" /> Поставьте{" "}
          <a href="https://ollama.com/download" target="_blank" rel="noreferrer">
            Ollama
          </a>{" "}
          или LM Studio — после запуска приложение найдёт их само. Чужое ПО за вас
          я не устанавливаю.
        </p>
      )}

      {state.shortlist.length > 0 && (
        <div className="app-group">
          <div className="app-group-title">Модели для этого компьютера</div>
          {state.shortlist.map((m) => (
            <div className="app-tile" key={m.tag}>
              <span className="app-tile-copy">
                <strong>{m.label}</strong>
                <small>
                  {m.tag} · {m.size_gb} ГБ
                </small>
              </span>
              {isInstalled(m.tag) ? (
                <small>установлена</small>
              ) : pulling === m.tag ? (
                <small>скачиваю… {percent}%</small>
              ) : state.can_pull ? (
                <button
                  className="btn"
                  type="button"
                  onClick={() => void onPull(m.tag)}
                  disabled={pulling !== null}
                >
                  Скачать
                </button>
              ) : (
                <small>ставится в самом приложении</small>
              )}
            </div>
          ))}
        </div>
      )}

      {state.too_heavy.length > 0 && (
        <p className="hint">
          Этот компьютер не потянет:{" "}
          {state.too_heavy
            .map((m) =>
              state.machine.accel.kind === "cpu_only"
                ? `${m.label} (нужен ускоритель)`
                : `${m.label} (нужно ${m.min_ram_gb} ГБ)`,
            )
            .join(", ")}
          .
        </p>
      )}
    </div>
  );
}
