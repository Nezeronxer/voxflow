import type { ReactNode } from "react";
import { Icon } from "../ui";
import type { Settings } from "../types";
import Models from "./Models";
import Recognition from "./Recognition";
import Control from "./Control";
import Ai from "./Ai";
import Stt from "./Stt";
import Corrections from "./Corrections";
import Applications from "./Applications";

export type SettingsPageId =
  | "general"
  | "dictation"
  | "models"
  | "personalization"
  | "applications"
  | "privacy"
  | "advanced";

const SETTINGS_NAV: {
  id: SettingsPageId;
  label: string;
  icon: (props: { className?: string }) => ReactNode;
}[] = [
  { id: "general", label: "Основные", icon: Icon.Sliders },
  { id: "dictation", label: "Диктовка", icon: Icon.Mic },
  { id: "models", label: "Модели", icon: Icon.Cube },
  { id: "personalization", label: "Персонализация", icon: Icon.Wand },
  { id: "applications", label: "Приложения", icon: Icon.Code },
  { id: "privacy", label: "Приватность", icon: Icon.Check },
  { id: "advanced", label: "Дополнительно", icon: Icon.Sparkles },
];

export default function SettingsHub({
  page,
  settings,
  update,
  persist,
  onPageChange,
}: {
  page: SettingsPageId;
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
  persist: (settings: Settings) => Promise<boolean>;
  onPageChange: (page: SettingsPageId) => void;
}) {
  return (
    <div className="settings-hub">
      <aside className="settings-rail" aria-label="Разделы настроек">
        <div className="settings-rail-title">Настройки</div>
        <nav className="settings-rail-nav">
          {SETTINGS_NAV.map((item) => {
            const ItemIcon = item.icon;
            const active = page === item.id;
            return (
              <button
                type="button"
                key={item.id}
                className={`settings-rail-item${active ? " active" : ""}`}
                aria-current={active ? "page" : undefined}
                onClick={() => onPageChange(item.id)}
              >
                <ItemIcon className="ico" />
                <span>{item.label}</span>
              </button>
            );
          })}
        </nav>
      </aside>

      <section className="settings-stage">
        {page === "general" && (
          <Control settings={settings} update={update} persist={persist} />
        )}
        {page === "dictation" && (
          <Recognition settings={settings} update={update} />
        )}
        {page === "models" && <Models settings={settings} update={update} />}
        {page === "personalization" && <Corrections />}
        {page === "applications" && (
          <Applications settings={settings} update={update} />
        )}
        {page === "privacy" && <Stt settings={settings} update={update} persist={persist} />}
        {page === "advanced" && <Ai settings={settings} update={update} persist={persist} />}
      </section>

      <footer className="settings-statusbar">
        <span className="settings-private">
          <Icon.Check className="ico" />
          Локальный режим · данные не покидают устройство
        </span>
        <span>VoxFlow 2.0.9</span>
      </footer>
    </div>
  );
}
