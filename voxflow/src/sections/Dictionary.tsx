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

  async function refresh() {
    setEntries(await dictionaryList());
  }

  useEffect(() => {
    refresh();
  }, []);

  async function onAdd() {
    const t = term.trim();
    if (!t) return;
    await dictionaryUpsert(null, t, replacement.trim());
    setTerm("");
    setReplacement("");
    refresh();
  }

  async function onDelete(id: number) {
    await dictionaryDelete(id);
    refresh();
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
          />
          <input
            type="text"
            placeholder="Замена"
            value={replacement}
            onChange={(e) => setReplacement(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && onAdd()}
          />
          <button className="btn btn-primary" onClick={onAdd} disabled={!term.trim()}>
            <Icon.Plus className="ico" />
            Добавить
          </button>
        </div>
      </div>
    </div>
  );
}
