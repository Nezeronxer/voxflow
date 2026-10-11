# 03 — Релизная обвязка 2.2.1

**Требования:** R02i, R02i.1–R02i.3
**Blocked by:** 01, 02
**Зона:** `script/`, `.github/workflows/`, файлы версий (package.json, package-lock.json, tauri.conf.json, Cargo.toml/Cargo.lock — только версия, installer/VoxFlow.iss, SettingsHub.tsx — только строка версии, README — строка check_versions), `RELEASE_NOTES.md`, `DECISIONS.md`
**Волна:** 2
**Status:** ready

## Что должно заработать

Релиз 2.2.0 не вышел: шаг «Build verified Linux runtime resources» упал с exit 126, потому что
`script/build_linux_runtime.sh` лежит в git с правом 100644. Нужно: право 100755 в git, вызов через
`bash script/build_linux_runtime.sh` в workflow, и CI на PR выполняет тот же шаг (или эквивалентную
проверку), чтобы такая ошибка ловилась до тега. Версия 2.2.1 во всех файлах, `check_versions.py
--tag v2.2.1` зелёный. RELEASE_NOTES: заголовок `# VoxFlow v2.2.1`, текст для людей, которые 2.2.0 не
получали (всё из 2.2.0 + фиксы 01 и 02).

## Из брифа, дословно

> «доделай мне voxflow»
> (ответ пользователя: «2.2.1 в конце»)

## Разделы спецификации

Истории 1–4, Решения §1, §5.

## Критерии приёмки

- [ ] `git ls-files -s script/build_linux_runtime.sh` → 100755; workflow вызывает через `bash`
- [ ] ci.yml на Linux-джобе выполняет сборку рантайма (или проверку исполняемости вызываемых скриптов)
- [ ] `python3 script/check_versions.py --tag v2.2.1` зелёный
- [ ] RELEASE_NOTES описывает 2.2.0+2.2.1 вместе; DECISIONS — строка про причину неудачного 2.2.0
- [ ] `npm audit --audit-level=high` чистый
