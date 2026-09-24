import type { ReactNode } from "react";
import { Icon, PageHead } from "../ui";
import type { Settings } from "../types";
import { egressState } from "../privacyState";
import Recognition from "./Recognition";
import Control from "./Control";
import Ai from "./Ai";
import Stt from "./Stt";
import Corrections from "./Corrections";
import LocalLlm from "./LocalLlm";
import Applications from "./Applications";

/**
 * Разделы названы тем, что человек ищет, а не тем, как устроен код.
 *
 * До 2.0.19 их было семь, и названия расходились с содержимым: ключ ИИ лежал в
 * «Дополнительно», облачные сервисы — в «Приватности» (то есть ровно наоборот),
 * язык распознавания редактировался и в «Основных», и в «Моделях». Пользователь
 * не нашёл поле своего ключа, хотя оно было на месте с 2.0.17.
 */
export type SettingsPageId = "dictation" | "text" | "applications" | "app";

const SETTINGS_NAV: {
  id: SettingsPageId;
  label: string;
  title: string;
  desc: string;
  icon: (props: { className?: string }) => ReactNode;
}[] = [
  {
    id: "dictation",
    label: "Диктовка",
    title: "Диктовка",
    desc: "Микрофон, клавиша, язык и где распознавать речь.",
    icon: Icon.Mic,
  },
  {
    id: "text",
    label: "Обработка текста",
    title: "Обработка текста",
    desc: "Что делать с текстом перед вставкой: чистка, нейросеть и её модели, исправления.",
    icon: Icon.Wand,
  },
  {
    id: "applications",
    label: "Приложения",
    title: "Приложения",
    desc: "Стиль текста для каждой программы.",
    icon: Icon.Code,
  },
  {
    id: "app",
    label: "Программа",
    title: "Программа",
    desc: "Оформление, звуки, автозапуск и обновления.",
    icon: Icon.Sliders,
  },
];

// Редко нужные настройки страницы — одним свёрнутым блоком внизу, чтобы первый
// экран показывал только то, что меняют чаще всего.
function Advanced({ children }: { children: ReactNode }) {
  return (
    <details className="card settings-advanced">
      <summary>Дополнительно</summary>
      {children}
    </details>
  );
}

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
  const current = SETTINGS_NAV.find((item) => item.id === page) ?? SETTINGS_NAV[0];
  // Подвал говорит про ФАКТИЧЕСКИЕ настройки, а не обещание из вёрстки.
  const egress = egressState(settings);

  return (
    <div className="settings-hub">
      <aside className="settings-rail" aria-label="Разделы настроек">
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
        <div className="content-inner settings-flat">
          <PageHead title={current.title} desc={current.desc} />

          {page === "dictation" && (
            <>
              <Control
                settings={settings}
                update={update}
                persist={persist}
                scope="dictation"
                embedded
              />
              <Stt settings={settings} update={update} persist={persist} part="main" />
              <Advanced>
                <Control
                  settings={settings}
                  update={update}
                  persist={persist}
                  scope="dictation-advanced"
                  embedded
                />
                <Stt settings={settings} update={update} persist={persist} part="advanced" />
              </Advanced>
            </>
          )}

          {page === "text" && (
            <>
              <Recognition settings={settings} update={update} part="main" />
              <Ai settings={settings} update={update} persist={persist} part="main" />
              <LocalLlm settings={settings} update={update} />
              <Corrections settings={settings} update={update} embedded />
              <Advanced>
                <Recognition settings={settings} update={update} part="advanced" />
                <Ai settings={settings} update={update} persist={persist} part="advanced" />
              </Advanced>
            </>
          )}

          {page === "applications" && (
            <Applications settings={settings} update={update} embedded />
          )}

          {page === "app" && (
            <Control
              settings={settings}
              update={update}
              persist={persist}
              scope="app"
              embedded
            />
          )}
        </div>
      </section>

      <footer className="settings-statusbar">
        <span className={`settings-private${egress.local ? "" : " cloud"}`}>
          {egress.local ? (
            <Icon.Check className="ico" />
          ) : (
            <Icon.CloudUp className="ico" />
          )}
          {egress.summary}
        </span>
        <span>VoxFlow 2.1.1</span>
      </footer>
    </div>
  );
}
