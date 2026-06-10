import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { getSettings, saveSettings, subscribe } from "./api";
import { Icon, Toast, applyTheme, normalizeTheme } from "./ui";
import FpsMeter from "./components/FpsMeter";
import type {
  Settings,
  NoModelEvent,
  ErrorEvent as VoxErrorEvent,
  NoRecogEvent,
} from "./types";
import { DEFAULT_SETTINGS } from "./types";

import Dashboard from "./sections/Dashboard";
import Models from "./sections/Models";
import Recognition from "./sections/Recognition";
import Control from "./sections/Control";
import Dictionary from "./sections/Dictionary";
import Snippets from "./sections/Snippets";
import History from "./sections/History";
import Ai from "./sections/Ai";
import Stt from "./sections/Stt";
import Corrections from "./sections/Corrections";
import { ErrorBoundary } from "./ErrorBoundary";

// Иконка «Облако» для вкладки STT. Inline-компонент (а не Icon.* из ui.tsx),
// так как ui.tsx не входит в зону правок этой ветки. Сигнатура совместима с Icon.*.
function CloudIcon(p: { className?: string }) {
  return (
    <svg
      className={p.className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M17.5 19a4.5 4.5 0 0 0 .5-8.97A6 6 0 0 0 6.2 9.3 4 4 0 0 0 7 19h10.5Z" />
    </svg>
  );
}

type TabId =
  | "dashboard"
  | "models"
  | "recognition"
  | "control"
  | "dictionary"
  | "snippets"
  | "corrections"
  | "ai"
  | "stt"
  | "history";

const NAV: { id: TabId; label: string; icon: (p: { className?: string }) => ReactNode }[] = [
  { id: "dashboard", label: "Главная", icon: Icon.Home },
  { id: "models", label: "Модель", icon: Icon.Cube },
  { id: "recognition", label: "Распознавание", icon: Icon.Wand },
  { id: "control", label: "Управление", icon: Icon.Sliders },
  { id: "dictionary", label: "Словарь", icon: Icon.Book },
  { id: "snippets", label: "Сниппеты", icon: Icon.Code },
  { id: "corrections", label: "Исправления", icon: Icon.Check },
  { id: "ai", label: "ИИ", icon: Icon.Sparkles },
  { id: "stt", label: "Облако", icon: CloudIcon },
  { id: "history", label: "История", icon: Icon.Clock },
];

type Notice = {
  message: string;
  variant: "warning" | "error";
  actionLabel?: string;
  action?: TabId;
};

export default function App() {
  const [tab, setTab] = useState<TabId>("dashboard");
  const [settings, setSettings] = useState<Settings>({ ...DEFAULT_SETTINGS });
  const [loaded, setLoaded] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);

  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const loadedRef = useRef(false);
  // Держим последние настройки в ref, чтобы flush'ить их синхронно при сокрытии окна.
  const settingsRef = useRef(settings);
  settingsRef.current = settings;

  // Initial load.
  useEffect(() => {
    let alive = true;
    getSettings().then((s) => {
      if (!alive) return;
      setSettings(s);
      loadedRef.current = true;
      setLoaded(true);
    });
    return () => {
      alive = false;
    };
  }, []);

  // Тема: применяем после загрузки настроек и мгновенно при каждой смене.
  // applyTheme заодно обновляет localStorage-кэш "vf-theme" — его читает
  // main.tsx до рендера (без вспышки) и ловит overlay через storage-событие.
  // До loaded не трогаем: иначе дефолт "system" затёр бы кэш реального выбора.
  useEffect(() => {
    if (!loaded) return;
    applyTheme(normalizeTheme(settings.theme));
  }, [loaded, settings.theme]);

  // B3: предупреждения от движка. no_model — баннер с кнопкой на вкладку «Модель»;
  // error/norecog раньше никто не слушал (тихие провалы) — теперь показываем тост.
  useEffect(() => {
    // Race-safe подписки (subscribe): под StrictMode эффект монтируется дважды;
    // обёртка гарантирует, что слушатель, чей listen() резолвится после cleanup,
    // тут же снимается — без утечек и дублей.
    const offs = [
      subscribe<NoModelEvent>("no_model", (e) => {
        setNotice({
          message: e.payload?.message || "Выберите модель во вкладке «Модель»",
          variant: "warning",
          actionLabel: "Открыть вкладку «Модель»",
          action: "models",
        });
      }),
      subscribe<VoxErrorEvent>("error", (e) => {
        const msg = e.payload?.message;
        if (msg) setNotice({ message: msg, variant: "error" });
      }),
      subscribe<NoRecogEvent>("norecog", (e) => {
        const msg = e.payload?.message;
        if (msg) setNotice({ message: msg, variant: "warning" });
      }),
    ];
    return () => offs.forEach((off) => off());
  }, []);

  // Debounced persistence whenever settings change (after initial load).
  useEffect(() => {
    if (!loadedRef.current) return;
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => {
      saveSettings(settings);
    }, 400);
    return () => {
      if (saveTimer.current) clearTimeout(saveTimer.current);
    };
  }, [settings]);

  // B4: окно настроек прячется в трей (а не закрывается), и debounce-save в пределах
  // 400 мс перед сокрытием раньше отменялся. При visibilitychange/pagehide flush'им
  // последнее значение немедленно, чтобы правка не потерялась.
  useEffect(() => {
    const flush = () => {
      if (!loadedRef.current) return;
      if (saveTimer.current) {
        clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      saveSettings(settingsRef.current);
    };
    const onVis = () => {
      if (document.visibilityState === "hidden") flush();
    };
    document.addEventListener("visibilitychange", onVis);
    window.addEventListener("pagehide", flush);
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      window.removeEventListener("pagehide", flush);
    };
  }, []);

  function update(patch: Partial<Settings>) {
    setSettings((prev) => ({ ...prev, ...patch }));
  }

  return (
    <div className="app">
      <FpsMeter />
      {notice && (
        <div className="toast-stack">
          <Toast
            message={notice.message}
            variant={notice.variant}
            actionLabel={notice.actionLabel}
            onAction={
              notice.action
                ? () => {
                    setTab(notice.action!);
                    setNotice(null);
                  }
                : undefined
            }
            onClose={() => setNotice(null)}
          />
        </div>
      )}
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <Icon.Mic className="" />
          </div>
          <div>
            <div className="brand-name">VoxFlow</div>
            <div className="brand-sub">Локальная диктовка</div>
          </div>
        </div>

        <nav className="nav">
          {NAV.map((n) => {
            const ActiveIcon = n.icon;
            return (
              <div
                key={n.id}
                className={`nav-item ${tab === n.id ? "active" : ""}`}
                // div — не кнопка: даём роль/таб-фокус и Enter/Space, чтобы по
                // вкладкам можно было ходить с клавиатуры (focus-visible-кольцо
                // в styles.css).
                role="button"
                tabIndex={0}
                onClick={() => setTab(n.id)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    setTab(n.id);
                  }
                }}
              >
                <ActiveIcon className="ico" />
                {n.label}
              </div>
            );
          })}
        </nav>

        <div className="sidebar-foot">
          <span className="dot-ok" />
          {loaded ? "Офлайн · всё локально" : "Загрузка…"}
        </div>
      </aside>

      <main className="content">
        {/* key={tab} — свежая граница на каждую вкладку: ошибка в одной секции
            не «залипает» при переходе на другую. */}
        <ErrorBoundary key={tab}>
          {/* .tab-fade: мягкий вход контента (fade+rise 160мс) при смене
              вкладки — ремоунт по key={tab} выше переигрывает CSS-анимацию. */}
          <div className="tab-fade">
            {tab === "dashboard" && <Dashboard settings={settings} />}
            {tab === "models" && <Models settings={settings} update={update} />}
            {tab === "recognition" && (
              <Recognition settings={settings} update={update} />
            )}
            {tab === "control" && (
              <Control settings={settings} update={update} />
            )}
            {tab === "dictionary" && <Dictionary />}
            {tab === "snippets" && <Snippets />}
            {tab === "corrections" && <Corrections />}
            {tab === "ai" && <Ai settings={settings} update={update} />}
            {tab === "stt" && <Stt settings={settings} update={update} />}
            {tab === "history" && <History />}
          </div>
        </ErrorBoundary>
      </main>
    </div>
  );
}
