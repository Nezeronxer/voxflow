; VoxFlow — кастомные NSIS-хуки.
;
; Tauri сам пишет ветку «Установка и удаление программ»
; (HKCU\...\Uninstall\{com.voxflow.app}) и создаёт ярлыки. Здесь добавляем
; ТОЛЬКО App Paths, чтобы приложение запускалось по имени из «Выполнить» (Win+R)
; и поиска. Установка per-user → пишем в HKCU (Windows читает App Paths и из HKCU).
; Не плодим лишних веток (см. бриф, раздел 4).

!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\VoxFlow.exe" "" "$INSTDIR\VoxFlow.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\VoxFlow.exe" "Path" "$INSTDIR"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\VoxFlow.exe"
!macroend
