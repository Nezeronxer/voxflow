import { useEffect, useState } from "react";
import { listAudioDevices } from "../api";
import {
  PageHead,
  Field,
  Select,
  Switch,
  HotkeyCapture,
  normalizeTheme,
} from "../ui";
import type { Settings } from "../types";

// Сегменты темы: значения зеркалят Settings.theme ("system"|"light"|"dark").
// Применение мгновенное (эффект в App.tsx), сохранение — штатным debounce-
// saveSettings через update().
const THEME_OPTIONS = [
  { value: "system", label: "Системная" },
  { value: "light", label: "Светлая" },
  { value: "dark", label: "Тёмная" },
] as const;

export default function Control({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  const [devices, setDevices] = useState<string[]>([]);

  useEffect(() => {
    let alive = true;
    listAudioDevices().then((d) => alive && setDevices(d));
    return () => {
      alive = false;
    };
  }, []);

  const deviceOptions = [
    { value: "", label: "По умолчанию" },
    ...devices.map((d) => ({ value: d, label: d })),
  ];

  return (
    <div className="content-inner">
      <PageHead
        title="Управление"
        desc="Устройство ввода, горячая клавиша и поведение приложения."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Аудио и горячая клавиша</div>
        </div>

        <Field label="Устройство ввода" hint="Микрофон для записи речи">
          <Select
            value={settings.input_device}
            onChange={(v) => update({ input_device: v })}
            options={deviceOptions}
          />
        </Field>

        <Field
          label="Горячая клавиша"
          hint="Нажмите на поле и нажмите любую клавишу — она станет горячей. В режиме «Удержание» лучше брать Ctrl / Alt / F-клавишу: печатная клавиша (буква, цифра) будет печататься в активном окне. Клавиша Fn на Windows обычно не перехватывается."
        >
          <HotkeyCapture
            value={settings.hotkey}
            onChange={(code) => update({ hotkey: code })}
          />
        </Field>

        <Field
          label="Режим"
          hint="«Удержание» — пока клавиша зажата; ДВОЙНОЕ нажатие — защёлка (остаётся включённым, выключить одним нажатием). «Переключатель» — нажал/нажал."
        >
          <Select
            value={settings.mode}
            onChange={(v) => update({ mode: v })}
            options={[
              { value: "hold", label: "Удержание" },
              { value: "toggle", label: "Переключатель" },
            ]}
          />
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Вид</div>
        </div>

        <Field
          label="Тема"
          hint="«Системная» следует за темой Windows. Применяется мгновенно и сохраняется автоматически."
        >
          {/* radiogroup: один выбор из трёх; активный сегмент — инверсия. */}
          <div className="seg" role="radiogroup" aria-label="Тема оформления">
            {THEME_OPTIONS.map((t) => {
              const active = normalizeTheme(settings.theme) === t.value;
              return (
                <button
                  key={t.value}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`seg-btn${active ? " active" : ""}`}
                  onClick={() => update({ theme: t.value })}
                >
                  {t.label}
                </button>
              );
            })}
          </div>
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Приложение</div>
        </div>

        <Field label="Звуки" hint="Звуковой сигнал при старте и завершении записи">
          <Switch
            checked={settings.play_sounds}
            onChange={(v) => update({ play_sounds: v })}
          />
        </Field>

        <Field label="Автозапуск" hint="Запускать VoxFlow при входе в систему">
          <Switch
            checked={settings.autostart}
            onChange={(v) => update({ autostart: v })}
          />
        </Field>
      </div>
    </div>
  );
}
