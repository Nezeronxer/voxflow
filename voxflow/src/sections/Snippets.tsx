import { useEffect, useState } from "react";
import { snippetList, snippetUpsert, snippetDelete } from "../api";
import { PageHead, Switch, Icon } from "../ui";
import type { SnippetEntry } from "../types";

export default function Snippets() {
  const [entries, setEntries] = useState<SnippetEntry[]>([]);
  const [trigger, setTrigger] = useState("");
  const [content, setContent] = useState("");
  const [isTemplate, setIsTemplate] = useState(false);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<{
    kind: "success" | "error";
    text: string;
  } | null>(null);

  async function refresh(): Promise<boolean> {
    try {
      setEntries(await snippetList());
      return true;
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось загрузить сниппеты"),
      });
      return false;
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function onAdd() {
    const t = trigger.trim();
    if (!t || !content.trim() || busy) return;
    setBusy(true);
    setStatus(null);
    try {
      await snippetUpsert(null, t, content, isTemplate);
      if (!(await refresh())) return;
      setTrigger("");
      setContent("");
      setIsTemplate(false);
      setStatus({ kind: "success", text: "Сниппет сохранён и готов к диктовке" });
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось сохранить сниппет"),
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
      await snippetDelete(id);
      if (!(await refresh())) return;
      setStatus({ kind: "success", text: "Сниппет удалён" });
    } catch (error) {
      setStatus({
        kind: "error",
        text: errorText(error, "Не удалось удалить сниппет"),
      });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="content-inner">
      <PageHead
        title="Сниппеты"
        desc="Короткие триггеры, которые разворачиваются в готовый текст."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Сниппеты</div>
          <div className="sub">
            Триггер /адрес можно сказать как «адрес», «слэш адрес» или «сниппет адрес».
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
          <div className="empty">Пока нет ни одного сниппета</div>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th style={{ width: 160 }}>Триггер</th>
                <th>Содержимое</th>
                <th style={{ width: 100 }}>Шаблон</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {entries.map((s) => (
                <tr key={s.id}>
                  <td className="mono">{s.trigger}</td>
                  <td style={{ whiteSpace: "pre-wrap" }}>{s.content}</td>
                  <td>
                    {s.is_template ? (
                      <span className="badge accent">Да</span>
                    ) : (
                      <span style={{ color: "var(--text-faint)" }}>—</span>
                    )}
                  </td>
                  <td className="table-actions">
                    <button
                      className="btn btn-sm btn-danger btn-ghost"
                      onClick={() => onDelete(s.id)}
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

        <div className="add-row" style={{ flexDirection: "column" }}>
          <div className="row-flex" style={{ width: "100%", gap: 10 }}>
            <input
              type="text"
              placeholder="Триггер (например, /адрес)"
              value={trigger}
              onChange={(e) => setTrigger(e.currentTarget.value)}
              style={{ flex: "0 0 220px" }}
              disabled={busy}
            />
            <label className="row-flex" style={{ gap: 8, fontSize: 13 }}>
              <Switch
                checked={isTemplate}
                onChange={(value) => {
                  if (!busy) setIsTemplate(value);
                }}
              />
              Шаблон
            </label>
          </div>
          <textarea
            placeholder="Содержимое сниппета…"
            value={content}
            onChange={(e) => setContent(e.currentTarget.value)}
            disabled={busy}
          />
          {isTemplate && (
            <div className="sub" style={{ width: "100%" }}>
              Переменные: {"{date}"} / {"{дата}"}, {"{time}"} / {"{время}"},{" "}
              {"{clipboard}"} / {"{буфер}"}.
            </div>
          )}
          <div className="row-flex" style={{ width: "100%", justifyContent: "flex-end" }}>
            <button
              className="btn btn-primary"
              onClick={onAdd}
              disabled={busy || !trigger.trim() || !content.trim()}
            >
              <Icon.Plus className="ico" />
              {busy ? "Сохранение…" : "Добавить сниппет"}
            </button>
          </div>
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
