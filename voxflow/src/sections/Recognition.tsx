import { PageHead, Field, Select, Switch } from "../ui";
import type { Settings } from "../types";

export default function Recognition({
  settings,
  update,
}: {
  settings: Settings;
  update: (patch: Partial<Settings>) => void;
}) {
  return (
    <div className="content-inner">
      <PageHead
        title="Распознавание"
        desc="Как обрабатывать распознанный текст перед вставкой."
      />

      <div className="card">
        <div className="card-head">
          <div className="card-title">Обработка текста</div>
        </div>

        <Field
          label="Дословно (verbatim)"
          hint="Вставлять текст как есть, без редактирования и переформулирования"
        >
          <Switch
            checked={settings.verbatim}
            onChange={(v) => update({ verbatim: v })}
          />
        </Field>

        <Field
          label="Убирать слова-паразиты"
          hint="Удалять «эээ», «ну», «как бы» и подобные заполнители"
        >
          <Switch
            checked={settings.remove_fillers}
            onChange={(v) => update({ remove_fillers: v })}
          />
        </Field>

        <Field
          label="Автопунктуация"
          hint="Автоматически расставлять знаки препинания и заглавные буквы"
        >
          <Switch
            checked={settings.auto_punct}
            onChange={(v) => update({ auto_punct: v })}
          />
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Стиль</div>
        </div>

        <Field label="Тон" hint="Тональность итогового текста">
          <Select
            value={settings.tone}
            onChange={(v) => update({ tone: v })}
            options={[
              { value: "very_casual", label: "Очень неформальный" },
              { value: "casual", label: "Неформальный" },
              { value: "neutral", label: "Нейтральный" },
              { value: "formal", label: "Формальный" },
            ]}
          />
        </Field>

        <Field
          label="Способ вставки"
          hint="«Вставка» — через буфер обмена, «Печать» — эмуляция нажатий клавиш"
        >
          <Select
            value={settings.paste_method}
            onChange={(v) => update({ paste_method: v })}
            options={[
              { value: "clipboard", label: "Вставка" },
              { value: "type", label: "Печать" },
            ]}
          />
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Живой ввод</div>
          <div className="sub">
            Текст в пилюле всегда обновляется по мере речи (когда доступен
            GPU-движок whisper-server). Настройка ниже управляет вставкой текста
            в активное поле во время речи.
          </div>
        </div>

        <Field
          label="Потоковая вставка"
          hint="Никогда (рекомендуется) — во время речи текст живёт только в пилюле, в поле ничего не печатается; готовый текст вставляется после отпускания клавиши. Авто — вставлять живьём устоявшуюся часть фразы, «хвост» остаётся серым в пилюле. Всегда — печатать каждую частичную версию прямо в поле с дотипыванием/забоем (может выглядеть дёргано)."
        >
          <Select
            value={settings.stream_mode}
            onChange={(v) => update({ stream_mode: v })}
            options={[
              { value: "never", label: "Никогда" },
              { value: "auto", label: "Авто" },
              { value: "always", label: "Всегда" },
            ]}
          />
        </Field>
      </div>

      <div className="card">
        <div className="card-head">
          <div className="card-title">Персонализация</div>
        </div>

        <Field
          label="Учиться на моей речи"
          hint="Сохранять пары (аудио ↔ текст) и адаптировать распознавание под ваш голос и лексику"
        >
          <Switch
            checked={settings.personalize}
            onChange={(v) => update({ personalize: v })}
          />
        </Field>
      </div>
    </div>
  );
}
