import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { checkForUpdate, getSettings, installUpdate, saveSettings, subscribe } from "./api";
import { Icon, Toast, applyTheme, normalizeTheme } from "./ui";
import FpsMeter from "./components/FpsMeter";
import type {
  Settings,
  NoModelEvent,
  ErrorEvent as VoxErrorEvent,
  NoRecogEvent,
  UpdateInfo,
} from "./types";
import { DEFAULT_SETTINGS } from "./types";
import {
  SECRET_FIELDS,
  mergeRendererSettings,
  settingsFingerprint,
} from "./settingsSync";
import Dashboard from "./sections/Dashboard";
import type { SettingsPageId } from "./sections/SettingsHub";
import { ErrorBoundary } from "./ErrorBoundary";

const History = lazy(() => import("./sections/History"));
const Dictionary = lazy(() => import("./sections/Dictionary"));
const Snippets = lazy(() => import("./sections/Snippets"));
const SettingsHub = lazy(() => import("./sections/SettingsHub"));

type TabId = "dashboard" | "history" | "dictionary" | "snippets" | "settings";

const NAV: {
  id: Exclude<TabId, "settings">;
  label: string;
  icon: (props: { className?: string }) => ReactNode;
}[] = [
  { id: "dashboard", label: "Главная", icon: Icon.Home },
  { id: "history", label: "История", icon: Icon.Clock },
  { id: "dictionary", label: "Словарь", icon: Icon.Book },
  { id: "snippets", label: "Сниппеты", icon: Icon.Code },
];

type Route = { tab: TabId; settingsPage?: SettingsPageId };
type Notice = {
  message: string;
  variant: "warning" | "error";
  actionLabel?: string;
  route?: Route;
  onAction?: () => void;
};

function RouteFallback() {
  return (
    <div className="route-fallback" role="status" aria-label="Загрузка раздела">
      <span />
      <span />
      <span />
    </div>
  );
}

export default function App() {
  const [tab, setTab] = useState<TabId>("dashboard");
  const [settingsPage, setSettingsPage] = useState<SettingsPageId>("general");
  const [settings, setSettings] = useState<Settings>({ ...DEFAULT_SETTINGS });
  const [loaded, setLoaded] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);

  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const loadedRef = useRef(false);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const backendSnapshotRef = useRef<Settings>({ ...DEFAULT_SETTINGS });
  const lastFromBackendRef = useRef("");
  const localEchoesRef = useRef<string[]>([]);
  const saveChainRef = useRef<Promise<unknown>>(Promise.resolve());

  const enqueueSave = useCallback((snapshot: Settings): Promise<boolean> => {
    const fingerprint = settingsFingerprint(snapshot);
    localEchoesRef.current.push(fingerprint);
    if (localEchoesRef.current.length > 12) localEchoesRef.current.shift();

    const task = saveChainRef.current.then(async () => {
      const ok = await saveSettings(snapshot);
      if (ok) {
        lastFromBackendRef.current = fingerprint;
        // Do not keep successfully persisted API keys in React state. The
        // backend reports only `configured: boolean`; empty renderer fields
        // subsequently mean "leave the stored secret unchanged".
        setSettings((current) => {
          let changed = false;
          const next = { ...current };
          for (const field of SECRET_FIELDS) {
            if (snapshot[field] && current[field] === snapshot[field]) {
              next[field] = "";
              changed = true;
            }
          }
          if (changed) settingsRef.current = next;
          return changed ? next : current;
        });
      } else {
        const index = localEchoesRef.current.indexOf(fingerprint);
        if (index >= 0) localEchoesRef.current.splice(index, 1);
        setNotice({
          message: "Не удалось сохранить настройки. Изменения оставлены на экране — повторите попытку.",
          variant: "error",
        });
      }
      return ok;
    });
    saveChainRef.current = task.catch(() => undefined);
    return task;
  }, []);

  const goTo = useCallback((route: Route) => {
    if (route.settingsPage) setSettingsPage(route.settingsPage);
    setTab(route.tab);
  }, []);

  async function handleInstallUpdate(info: UpdateInfo) {
    setNotice({ message: `Скачиваю VoxFlow ${info.latest_version}…`, variant: "warning" });
    const result = await installUpdate(info.asset_url, info.asset_name);
    setNotice({
      message: result?.launched
        ? "Установщик обновления запущен. VoxFlow сейчас закроется."
        : "Не удалось скачать или запустить обновление.",
      variant: result?.launched ? "warning" : "error",
    });
  }

  useEffect(() => {
    let alive = true;
    void getSettings().then((initial) => {
      if (!alive) return;
      backendSnapshotRef.current = initial;
      lastFromBackendRef.current = settingsFingerprint(initial);
      settingsRef.current = initial;
      setSettings(initial);
      loadedRef.current = true;
      setLoaded(true);
    });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    return subscribe<Settings>("settings_changed", (event) => {
      if (!event.payload) return;
      const incoming = { ...DEFAULT_SETTINGS, ...event.payload };
      const fingerprint = settingsFingerprint(incoming);
      const echoIndex = localEchoesRef.current.indexOf(fingerprint);

      if (echoIndex >= 0) {
        localEchoesRef.current.splice(echoIndex, 1);
        backendSnapshotRef.current = incoming;
        lastFromBackendRef.current = fingerprint;
        return;
      }

      const merged = mergeRendererSettings(
        backendSnapshotRef.current,
        settingsRef.current,
        incoming,
        Object.keys(DEFAULT_SETTINGS),
      );
      backendSnapshotRef.current = incoming;
      lastFromBackendRef.current = fingerprint;
      settingsRef.current = merged;
      setSettings(merged);
    });
  }, []);

  useEffect(() => {
    if (!loaded) return;
    applyTheme(normalizeTheme(settings.theme));
  }, [loaded, settings.theme]);

  useEffect(() => {
    if (!loaded || !settings.auto_update_check) return;
    let alive = true;
    const timer = window.setTimeout(async () => {
      const info = await checkForUpdate();
      if (!alive || !info?.available) return;
      setNotice({
        message: `Доступно обновление VoxFlow ${info.latest_version}.`,
        variant: "warning",
        actionLabel: "Установить",
        onAction: () => void handleInstallUpdate(info),
      });
    }, 1600);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [loaded, settings.auto_update_check]);

  useEffect(() => {
    const offs = [
      subscribe<NoModelEvent>("no_model", (event) => {
        setNotice({
          message: event.payload?.message || "Выберите локальную модель распознавания.",
          variant: "warning",
          actionLabel: "Открыть модели",
          route: { tab: "settings", settingsPage: "models" },
        });
      }),
      subscribe<VoxErrorEvent>("error", (event) => {
        if (event.payload?.message) {
          setNotice({ message: event.payload.message, variant: "error" });
        }
      }),
      subscribe<NoRecogEvent>("norecog", (event) => {
        if (event.payload?.message) {
          setNotice({ message: event.payload.message, variant: "warning" });
        }
      }),
    ];
    return () => offs.forEach((off) => off());
  }, []);

  useEffect(() => {
    if (!loadedRef.current) return;
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => {
      const snapshot = settingsRef.current;
      if (settingsFingerprint(snapshot) === lastFromBackendRef.current) return;
      void enqueueSave(snapshot);
    }, 300);
    return () => {
      if (saveTimer.current) clearTimeout(saveTimer.current);
    };
  }, [settings, enqueueSave]);

  useEffect(() => {
    const flush = () => {
      if (!loadedRef.current) return;
      if (saveTimer.current) {
        clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      const snapshot = settingsRef.current;
      if (settingsFingerprint(snapshot) !== lastFromBackendRef.current) {
        void enqueueSave(snapshot);
      }
    };
    const onVisibility = () => {
      if (document.visibilityState === "hidden") flush();
    };
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("pagehide", flush);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("pagehide", flush);
    };
  }, [enqueueSave]);

  function update(patch: Partial<Settings>) {
    setSettings((previous) => ({ ...previous, ...patch }));
  }

  function openSettings(page: SettingsPageId) {
    setSettingsPage(page);
    setTab("settings");
  }

  return (
    <div className={`app app-v2${loaded ? " is-ready" : " is-loading"}`}>
      <FpsMeter />
      {notice && (
        <div className="toast-stack">
          <Toast
            message={notice.message}
            variant={notice.variant}
            actionLabel={notice.actionLabel}
            onAction={
              notice.onAction
                ? notice.onAction
                : notice.route
                  ? () => {
                      goTo(notice.route!);
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
          <div className="brand-wave" aria-hidden="true">
            {[8, 18, 28, 18, 10].map((height, index) => (
              <span key={index} style={{ height }} />
            ))}
          </div>
          <div>
            <div className="brand-name">VoxFlow <span>2.0</span></div>
            <div className="brand-sub">Локальная диктовка</div>
          </div>
        </div>

        <nav className="nav" aria-label="Основная навигация">
          {NAV.map((item) => {
            const NavIcon = item.icon;
            const active = tab === item.id;
            return (
              <button
                type="button"
                key={item.id}
                className={`nav-item${active ? " active" : ""}`}
                aria-current={active ? "page" : undefined}
                onClick={() => setTab(item.id)}
              >
                <NavIcon className="ico" />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>

        <div className="sidebar-bottom">
          <button
            type="button"
            className={`nav-item settings-entry${tab === "settings" ? " active" : ""}`}
            aria-current={tab === "settings" ? "page" : undefined}
            onClick={() => setTab("settings")}
          >
            <Icon.Sliders className="ico" />
            <span>Настройки</span>
          </button>
          <div className="sidebar-foot">
            <span className="dot-ok" />
            <span>{loaded ? "Готово к диктовке" : "Загрузка…"}</span>
            <span className="sidebar-hotkey">{loaded ? settings.hotkey.replace("Meta", "⌘ ").replace("Control", "Ctrl ") : ""}</span>
          </div>
        </div>
      </aside>

      <main className="content">
        <ErrorBoundary key={`${tab}-${tab === "settings" ? settingsPage : ""}`}>
          <Suspense fallback={<RouteFallback />}>
            <div className="tab-fade">
              {tab === "dashboard" && (
                <Dashboard settings={settings} onOpenSettings={openSettings} />
              )}
              {tab === "history" && <History />}
              {tab === "dictionary" && <Dictionary />}
              {tab === "snippets" && <Snippets />}
              {tab === "settings" && (
                <SettingsHub
                  page={settingsPage}
                  settings={settings}
                  update={update}
                  persist={enqueueSave}
                  onPageChange={setSettingsPage}
                />
              )}
            </div>
          </Suspense>
        </ErrorBoundary>
      </main>
    </div>
  );
}
