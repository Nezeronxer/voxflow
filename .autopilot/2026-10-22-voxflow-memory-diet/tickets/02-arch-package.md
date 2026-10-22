# 02 — Пакет для Arch в релизе и обновление на Arch

**Требования:** R05, R06, R08i
**Зависит от:** —
**Зона:** `.github/workflows/release-linux.yml` · `.github/workflows/release.yml` · `voxflow/src-tauri/src/updater.rs` · `voxflow/src-tauri/src/linux_env.rs` · `script/` (новый PKGBUILD или скрипт сборки пакета, `install` при нужде) · `README.md` (раздел установки на Linux)
**Волна:** 1
**Ревью:** нет
**Модель:** сильная

## Что должно заработать

В релизе Linux рядом с AppImage, deb и rpm появляется `VoxFlow-X.Y.Z-linux-x64.pkg.tar.zst`, который ставится
`sudo pacman -U` на Arch и производных (Manjaro, EndeavourOS). Встроенное обновление на Arch, где VoxFlow стоит
этим пакетом, находит новый `.pkg.tar.zst`, проверяет SHA-256 и ставит через `pkexec pacman -U --noconfirm`,
затем перезапускает приложение; если pkexec нет или пользователь отказался - показывает команду для терминала.
На AppImage, macOS и Windows обновление работает ровно как раньше.

## Из брифа, дословно

> «выпусти обновления для всех операционных систем Linux, MC и Windows, в основном под все дистрибутивы. под арч»

Ответ пользователя: «Пакет для pacman в релизе» (без AUR).

## Разделы спецификации

§5, истории 11-14, «Решения по реализации», «Вне рамок». Карта кода - `notes.md` пункты 6-8.

## Критерии приёмки

- [ ] `release-linux.yml` собирает `.pkg.tar.zst` из готового deb в контейнере `archlinux:latest` (`makepkg` не от root), с depends по библиотекам, которые реально нужны бинарнику (webkit2gtk-4.1, gtk3, alsa-lib, libayatana-appindicator или что показывает deb/ldd)
- [ ] В CI пакет проверяется: `pacman -Qip` (имя, версия), список файлов (бинарник, .desktop, иконки), установка `pacman -U --noconfirm` в чистом контейнере archlinux проходит
- [ ] Ассет попадает в `SHA256SUMS-linux.txt` и в публикацию релиза; имя `VoxFlow-X.Y.Z-linux-x64.pkg.tar.zst`
- [ ] updater.rs: на Linux, если приложение установлено pacman (не AppImage), выбирается `.pkg.tar.zst`; AppImage-установка по-прежнему выбирает AppImage; тесты выбора ассета для обоих случаев (функции выбора - чистые, с инъекцией признака, чтобы тест шёл на любой ОС)
- [ ] Установка на Arch: `pkexec pacman -U --noconfirm <файл>`, перезапуск; без pkexec/отказ - понятное сообщение с командой `sudo pacman -U <путь>`; имя скачанного файла оканчивается на `.pkg.tar.zst` (pacman требует)
- [ ] Тесты updater.rs для macOS/Windows/AppImage не изменились по смыслу и зелёные
- [ ] README: как поставить на Arch одной командой
- [ ] В ответе - текст решения D-054 (что / почему / отвергнуто) для DECISIONS.md; сам DECISIONS.md не трогать (его пишет таск 01)
- [ ] `bash -n script/*.sh`, `cargo test --locked --lib`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --check` зелёные; Linux-код помечен так, чтобы clippy на macOS/Windows не падал на dead_code
