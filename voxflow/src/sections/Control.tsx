import { useEffect, useState } from "react";
import { checkForUpdate, installUpdate, listAudioDevices } from "../api";
import {
  PageHead,
  Field,
  Select,
  Switch,
  HotkeyCapture,
  normalizeTheme,
  HOTKEY_FIELD_HINT,
} from "../ui";
import type { Settings } from "../types";
import type { UpdateInfo } from "../types";

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
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState("");
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);

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

  async function onCheckUpdate() {
    setCheckingUpdate(true);
    setUpdateStatus("Проверяю…");
    const info = await checkForUpdate();
    setCheckingUpdate(false);
    setUpdateInfo(info);
    if (!info) {
      setUpdateStatus("Проверка не удалась");
    } else if (info.available) {
      setUpdateStatus(`Доступна версия ${info.latest_version}`);
    } else {
      setUpdateStatus(`Актуальная версия ${info.current_version}`);
    }
  }

  async function onInstallUpdate() {
    if (!updateInfo?.available) return;
    setInstallingUpdate(true);
    setUpdateStatus("Скачиваю установщик…");
    const result = await installUpdate(updateInfo.asset_url, updateInfo.asset_name);
    setInstallingUpdate(false);
    setUpdateStatus(
      result?.launched
        ? "Установщик запущен. VoxFlow закроется."
        : "Не удалось запустить установщик",
    );
  }

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
          hint={HOTKEY_FIELD_HINT}
        >
          <HotkeyCapture
            value={settings.hotkey}
            onChange={(code) => update({ hotkey: code })}
          />
        </Field>

        <Field
          label="Улучшить выделенное"
          hint="Одиночное нажатие берёт выделенный текст, чистит его и заменяет в активном поле. Esc отменяет незавершённую обработку."
        >
          <HotkeyCapture
            value={settings.improve_hotkey}
            onChange={(code) => update({ improve_hotkey: code })}
          />
        </Field>

        <Field
          label="Режим"
          hint="«Удержание» пишет, пока клавиша зажата. «Переключатель» — нажал/нажал."
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

        <Field
          label="Автомьют"
          hint="Глушить системный звук на время диктовки и возвращать после остановки или Esc"
        >
          <Switch
            checked={settings.auto_mute}
            onChange={(v) => update({ auto_mute: v })}
          />
        </Field>

        <Field label="Автозапуск" hint="Запускать VoxFlow при входе в систему">
          <Switch
            checked={settings.autostart}
            onChange={(v) => update({ autostart: v })}
          />
        </Field>

        <Field
          label="Автообновления"
          hint="Проверять новые версии в GitHub Releases при запуске"
        >
          <Switch
            checked={settings.auto_update_check}
            onChange={(v) => update({ auto_update_check: v })}
          />
        </Field>

        <Field label="Версия" hint={updateStatus || "GitHub Releases"}>
          <div className="row-flex">
            <button
              className="btn btn-sm"
              onClick={onCheckUpdate}
              disabled={checkingUpdate || installingUpdate}
            >
              {checkingUpdate ? "Проверяю…" : "Проверить"}
            </button>
            {updateInfo?.available && (
              <button
                className="btn btn-sm btn-primary"
                onClick={onInstallUpdate}
                disabled={checkingUpdate || installingUpdate}
              >
                {installingUpdate ? "Скачиваю…" : "Установить"}
              </button>
            )}
          </div>
        </Field>
      </div>
    </div>
  );
}
