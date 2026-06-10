import { useEffect, useState } from "react";
import { snippetList, snippetUpsert, snippetDelete } from "../api";
import { PageHead, Switch, Icon } from "../ui";
import type { SnippetEntry } from "../types";

export default function Snippets() {
  const [entries, setEntries] = useState<SnippetEntry[]>([]);
  const [trigger, setTrigger] = useState("");
  const [content, setContent] = useState("");
  const [isTemplate, setIsTemplate] = useState(false);

  async function refresh() {
    setEntries(await snippetList());
  }

  useEffect(() => {
    refresh();
  }, []);

  async function onAdd() {
    const t = trigger.trim();
    if (!t) return;
    await snippetUpsert(null, t, content, isTemplate);
    setTrigger("");
    setContent("");
    setIsTemplate(false);
    refresh();
  }

  async function onDelete(id: number) {
    await snippetDelete(id);
    refresh();
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
            Скажите триггер — вставится содержимое. Шаблоны поддерживают переменные.
          </div>
        </div>

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
            />
            <label className="row-flex" style={{ gap: 8, fontSize: 13 }}>
              <Switch checked={isTemplate} onChange={setIsTemplate} />
              Шаблон
            </label>
          </div>
          <textarea
            placeholder="Содержимое сниппета…"
            value={content}
            onChange={(e) => setContent(e.currentTarget.value)}
          />
          <div className="row-flex" style={{ width: "100%", justifyContent: "flex-end" }}>
            <button
              className="btn btn-primary"
              onClick={onAdd}
              disabled={!trigger.trim()}
            >
              <Icon.Plus className="ico" />
              Добавить сниппет
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
