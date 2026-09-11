import type { UpdateProgressEvent } from "../types";

/** Состояние идущей установки обновления (App.tsx). */
export type UpdateRun = {
  version: string;
  phase: UpdateProgressEvent["phase"];
  received: number;
  total: number;
  /** Установщик запущен: приложение закроется через секунду. */
  done: string | null;
  /** Установка сорвалась — приложение остаётся открытым, текст причины. */
  error: string | null;
};

function formatMb(bytes: number): string {
  return `${(bytes / 1_048_576).toFixed(1)} МБ`;
}

function phaseLabel(run: UpdateRun): string {
  if (run.done) return run.done;
  switch (run.phase) {
    case "download":
      return "Скачиваю пакет обновления…";
    case "verify":
      return "Проверяю целостность пакета…";
    case "launch":
      return "Запускаю установщик…";
  }
}

/**
 * Полноэкранный прогресс установки. Пока идёт работа, закрыть нельзя: через
 * секунду после запуска установщика приложение закроется само, а после
 * установки откроется уже новым.
 */
export default function UpdateOverlay({
  run,
  onDismiss,
}: {
  run: UpdateRun;
  onDismiss: () => void;
}) {
  const percent =
    run.done || run.phase !== "download"
      ? 100
      : run.total > 0
        ? Math.min(100, Math.round((run.received / run.total) * 100))
        : 0;

  return (
    <div className="update-overlay" role="dialog" aria-modal="true" aria-live="polite">
      <div className="update-card">
        <h2>Обновление до VoxFlow {run.version}</h2>
        {run.error ? (
          <>
            <p className="update-error">{run.error}</p>
            <div className="update-actions">
              <button className="btn" type="button" onClick={onDismiss}>
                Закрыть
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="update-phase">{phaseLabel(run)}</p>
            <div className="progress" aria-hidden="true">
              <div className="bar" style={{ width: `${percent}%` }} />
            </div>
            <div className="update-meta">
              <span>
                {run.phase === "download" && !run.done
                  ? `${formatMb(run.received)} из ${formatMb(run.total)}`
                  : "Приложение закроется и откроется обновлённым"}
              </span>
              <span>{percent}%</span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
