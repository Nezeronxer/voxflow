import { useEffect, useState } from "react";
import { getHistory } from "../api";
import { PageHead } from "../ui";
import type { HistoryItem } from "../types";

function fmtTime(ts: number): string {
  // Accept seconds or milliseconds epoch.
  const ms = ts < 1e12 ? ts * 1000 : ts;
  const d = new Date(ms);
  if (isNaN(d.getTime())) return "";
  return d.toLocaleString("ru-RU", {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function History() {
  const [items, setItems] = useState<HistoryItem[]>([]);

  useEffect(() => {
    let alive = true;
    getHistory(50).then((h) => alive && setItems(h));
    return () => {
      alive = false;
    };
  }, []);

  return (
    <div className="content-inner">
      <PageHead
        title="История"
        desc="Последние 50 диктовок. Хранится локально на вашем устройстве."
      />

      <div className="card">
        {items.length === 0 ? (
          <div className="empty">История пуста</div>
        ) : (
          items.map((it, i) => (
            <div className="hist-item" key={i}>
              <div className="hist-meta">
                <span>{fmtTime(it.ts)}</span>
                {it.app && <span className="hist-app">{it.app}</span>}
                <span>· {it.words} сл.</span>
              </div>
              <div className="hist-text">{it.text}</div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
