import { useEffect, useRef, useState } from "react";
import { getHistory } from "../api";
import { PageHead, Icon } from "../ui";
import type { HistoryItem } from "../types";

function fmtTime(ts: string): string {
  // Бэкенд шлёт "YYYY-MM-DD HH:MM:SS" (локальное время) — приводим к ISO-виду,
  // иначе парсинг даты с пробелом не специфицирован.
  const d = new Date(ts.replace(" ", "T"));
  if (isNaN(d.getTime())) return ts;
  return d.toLocaleString("ru-RU", {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

// Копирование в буфер. Backend-команды для clipboard нет (вставка идёт через
// inject внутри Rust и наружу не экспортируется), поэтому navigator.clipboard;
// если он недоступен (нет разрешения/старый webview) — фолбэк через скрытую
// textarea + execCommand. Возвращаем успех, чтобы не показывать ложное «Скопировано».
async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // падаем в фолбэк ниже
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    return ok;
  } catch {
    return false;
  }
}

export default function History() {
  const [items, setItems] = useState<HistoryItem[]>([]);
  const [query, setQuery] = useState("");
  // Помечаем скопированную запись ссылкой на объект, а не индексом: при смене
  // фильтра индексы видимого списка съезжают, а ссылка остаётся той же.
  const [copied, setCopied] = useState<HistoryItem | null>(null);
  const copyTimer = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;
    getHistory(50).then((h) => alive && setItems(h));
    return () => {
      alive = false;
      if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    };
  }, []);

  // Фильтр client-side по уже загруженному списку: подстрока без учёта
  // регистра в тексте диктовки и имени приложения.
  const q = query.trim().toLowerCase();
  const visible = q
    ? items.filter(
        (it) =>
          it.text.toLowerCase().includes(q) ||
          (it.app || "").toLowerCase().includes(q),
      )
    : items;

  async function onCopy(it: HistoryItem) {
    const ok = await copyToClipboard(it.text);
    if (!ok) return;
    if (copyTimer.current !== null) window.clearTimeout(copyTimer.current);
    setCopied(it);
    copyTimer.current = window.setTimeout(() => {
      setCopied(null);
      copyTimer.current = null;
    }, 1500);
  }

  return (
    <div className="content-inner">
      <PageHead
        title="История"
        desc="Последние 50 диктовок. Хранится локально на вашем устройстве."
      />

      <div className="hist-toolbar">
        <input
          type="text"
          className="hist-search"
          placeholder="Поиск по тексту и приложению"
          value={query}
          onChange={(e) => setQuery(e.currentTarget.value)}
        />
        {q && (
          <span className="hist-count">
            {visible.length} из {items.length}
          </span>
        )}
      </div>

      <div className="card">
        {items.length === 0 ? (
          <div className="empty">История пуста</div>
        ) : visible.length === 0 ? (
          <div className="empty">Ничего не найдено</div>
        ) : (
          visible.map((it, i) => (
            <div className="hist-item" key={i}>
              <div className="hist-meta">
                <span>{fmtTime(it.ts)}</span>
                {it.app && <span className="hist-app">{it.app}</span>}
                <span>· {it.words} сл.</span>
                <button
                  className={"hist-copy" + (copied === it ? " copied" : "")}
                  onClick={() => onCopy(it)}
                  title="Скопировать текст в буфер"
                >
                  {copied === it ? (
                    <>
                      <Icon.Check className="hist-copy-ico" />
                      Скопировано
                    </>
                  ) : (
                    "Копировать"
                  )}
                </button>
              </div>
              <div className="hist-text">{it.text}</div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
