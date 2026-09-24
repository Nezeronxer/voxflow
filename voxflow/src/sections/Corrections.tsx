import { useEffect, useState } from "react";
import {
  correctionsList,
  correctionsUpsert,
  correctionsDelete,
} from "../api";
import { PageHead, SectionShell, Icon, Field, Switch } from "../ui";
import type { CorrectionEntry, Settings } from "../types";

export default function Corrections({
  settings,
  update,
  embedded,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  embedded?: boolean;
}) {
  const [entries, setEntries] = useState<CorrectionEntry[]>([]);
  const [wrong, setWrong] = useState("");
  const [right, setRight] = useState("");

  async function refresh() {
    setEntries(await correctionsList());
  }

  useEffect(() => {
    refresh();
  }, []);

  async function onAdd() {
    const w = wrong.trim();
    if (!w) return;
    await correctionsUpsert(null, w, right.trim());
    setWrong("");
    setRight("");
    refresh();
  }

  async function onDelete(id: number) {
    await correctionsDelete(id);
    refresh();
  }

  return (
    <SectionShell embedded={embedded}>
      {!embedded && (
        <PageHead
          title="Исправления"
          desc="Пары распознано → правильно. Применяются автоматически к каждой диктовке."
        />
      )}

      <div className="card">
        <div className="card-head">
          <div className="card-title">Исправления</div>
          <div className="sub">
            Слово, которое распознаётся неправильно, и чем его заменять
          </div>
        </div>

        <Field
          label="Учиться на моих правках"
          hint="Запоминать, когда вы правите вставленный текст. Иногда ошибается"
        >
          <Switch
            checked={settings.learn_corrections}
            onChange={(v) => update({ learn_corrections: v })}
          />
        </Field>

        {entries.length === 0 ? (
          <div className="empty">Пока нет ни одного исправления</div>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Распознано</th>
                <th>Правильно</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {entries.map((e) => (
                <tr key={e.id}>
                  <td className="mono">{e.wrong}</td>
                  <td>
                    {e.right || (
                      <span style={{ color: "var(--text-faint)" }}>—</span>
                    )}
                  </td>
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
            placeholder="Распознано (как слышится)"
            value={wrong}
            onChange={(e) => setWrong(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && onAdd()}
          />
          <input
            type="text"
            placeholder="Правильно"
            value={right}
            onChange={(e) => setRight(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && onAdd()}
          />
          <button
            className="btn btn-primary"
            onClick={onAdd}
            disabled={!wrong.trim()}
          >
            <Icon.Plus className="ico" />
            Добавить
          </button>
        </div>
      </div>
    </SectionShell>
  );
}
