import { useEffect, useState } from "react";
import {
  dictionaryList,
  dictionaryUpsert,
  dictionaryDelete,
} from "../api";
import { PageHead, Icon } from "../ui";
import type { DictionaryEntry } from "../types";

export default function Dictionary() {
  const [entries, setEntries] = useState<DictionaryEntry[]>([]);
  const [term, setTerm] = useState("");
  const [replacement, setReplacement] = useState("");
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<{
    kind: "success" | "error";
    text: string;
  } | null>(null);

  async function refresh(): Promise<boolean> {
    try {
      setEntries(await dictionaryList());
      return true;
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось загрузить словарь"),
      });
      return false;
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function onAdd() {
    const t = term.trim();
    if (!t || busy) return;
    setBusy(true);
    setStatus(null);
    try {
      await dictionaryUpsert(null, t, replacement.trim());
      if (!(await refresh())) return;
      setTerm("");
      setReplacement("");
      setStatus({ kind: "success", text: "Термин сохранён и уже применяется" });
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось сохранить термин"),
      });
    } finally {
      setBusy(false);
    }
  }

  async function onDelete(id: number) {
    if (busy) return;
    setBusy(true);
    setStatus(null);
    try {
      await dictionaryDelete(id);
      if (!(await refresh())) return;
      setStatus({ kind: "success", text: "Термин удалён" });
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось удалить термин"),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="content-inner">
      <PageHead
        title="Словарь"
        desc="Замены распознанных слов: имена, термины, бренды, написание."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Замены</div>
          <div className="sub">
            Слева — что распознаётся, справа — на что заменить
          </div>
        </div>

        {status && (
          <div
            className={`toast toast-${status.kind}`}
            role={status.kind === "error" ? "alert" : "status"}
          >
            <span className="toast-msg">{status.text}</span>
          </div>
        )}

        {entries.length === 0 ? (
          <div className="empty">Пока нет ни одной замены</div>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Термин</th>
                <th>Замена</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.id}>
                  <td className="mono">{e.term}</td>
                  <td>{e.replacement || <span style={{ color: "var(--text-faint)" }}>—</span>}</td>
                  <td className="table-actions">
                    <button
                      className="btn btn-sm btn-danger btn-ghost"
                      onClick={() => onDelete(e.id)}
                      title="Удалить"
                      disabled={busy}
                    >
                      <Icon.Trash className="ico" />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        <div className="add-row">
          <input
            type="text"
            placeholder="Термин (как слышится)"
            value={term}
            onChange={(e) => setTerm(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && onAdd()}
            disabled={busy}
          />
          <input
            type="text"
            placeholder="Замена (необязательно)"
            value={replacement}
            onChange={(e) => setReplacement(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && onAdd()}
            disabled={busy}
          />
          <button
            className="btn btn-primary"
            onClick={onAdd}
            disabled={busy || !term.trim()}
          >
            <Icon.Plus className="ico" />
            {busy ? "Сохранение…" : "Добавить"}
          </button>
        </div>
        <div className="sub" style={{ marginTop: 10 }}>
          Если замену не указывать, VoxFlow запомнит точное написание термина.
        </div>
      </div>
    </div>
  );
}

function errorText(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  return fallback;
}
