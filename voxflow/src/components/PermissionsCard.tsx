import { useEffect, useState } from "react";
import { permissionsStatus, requestPermission } from "../api";
import {
  permissionRows,
  showPermissionsCard,
  type PermissionsStatus,
} from "../permissionsState";

/** Опрос идёт, пока окно видно: если доступ отозвали, карточка вернётся сама. */
const POLL_MS = 1500;

export default function PermissionsCard() {
  const [status, setStatus] = useState<PermissionsStatus | null>(null);

  useEffect(() => {
    let alive = true;
    let inFlight = false;
    const tick = async () => {
      // Не перекрываем тики и не опрашиваем скрытое окно.
      if (inFlight || document.visibilityState !== "visible") return;
      inFlight = true;
      try {
        const next = await permissionsStatus();
        if (alive) setStatus(next);
      } finally {
        inFlight = false;
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      alive = false;
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", tick);
    };
  }, []);

  if (!status || !showPermissionsCard(status)) return null;

  return (
    <section className="card" aria-label="Доступы">
      <div className="card-head">
        <h2 className="card-title">Доступы</h2>
        <p className="sub">
          Выдайте их один раз - дальше VoxFlow работает сам. Статус обновляется сам.
        </p>
      </div>
      {permissionRows(status).map((row) => (
        <div className="field" key={row.kind}>
          <div>
            <strong>{row.title}</strong>
            <div className="sub">{row.hint}</div>
          </div>
          {row.granted ? (
            <span className="sub" aria-label={`${row.title}: выдано`}>
              Выдано
            </span>
          ) : (
            <button
              type="button"
              className="btn btn-sm btn-primary"
              aria-label={`${row.action}: ${row.title}`}
              onClick={() => {
                void requestPermission(row.kind);
              }}
            >
              {row.action}
            </button>
          )}
        </div>
      ))}
    </section>
  );
}
