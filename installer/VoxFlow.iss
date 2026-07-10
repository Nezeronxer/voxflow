; ============================================================================
;  VoxFlow — per-user neon-dark installer
;  Inno Setup 6.7 script. IMPORTANT: build the packaged Tauri exe first.
;    cd voxflow
;    npm run tauri -- build --no-bundle
;  Then compile with:
;    "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\VoxFlow.iss
;  Output: installer\Output\VoxFlow-Setup-2.0.0.exe
;
;  Do NOT compile this installer after a plain `cargo build --release`: that
;  Tauri binary stays in dev mode and opens http://localhost:1420 in WebView2.
; ----------------------------------------------------------------------------
;  PER-USER install (PrivilegesRequired=lowest, HKCU, {localappdata}\VoxFlow).
;  Wizard sequence: Welcome -> Destination -> Select Additional Tasks ->
;                   Ready -> Installing -> Finished.  NO License page.
;  DATA-SAFETY: install dir holds ~2.3GB user models + voxflow.db. We NEVER
;  add an [UninstallDelete] rule for {app} or recursively delete it. Inno
;  removes only the files it logged at install time.
; ============================================================================

#define AppName    "VoxFlow"
#define AppVersion "2.0.0"
#define Publisher  "Крылов Анатолий Евгеньевич"
#define AppExe     "voxflow.exe"
#define SrcDir     "..\voxflow\src-tauri\target\release"

[Setup]
; Stable AppId — never change (used for upgrade/uninstall matching).
AppId={{B2F1A9E0-7C4D-4E8A-9F3B-1A2C5D6E7F80}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#Publisher}
VersionInfoVersion=2.0.0.0

; --- Per-user install: no elevation, current user only. ---
PrivilegesRequired=lowest
DefaultDirName={localappdata}\VoxFlow
DefaultGroupName=VoxFlow
DisableProgramGroupPage=yes
DisableWelcomePage=no
ShowLanguageDialog=no

; --- Uninstall presentation ---
UninstallDisplayIcon={app}\voxflow.exe
UninstallDisplayName=VoxFlow

; --- Architecture: x64-compatible only ---
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

; --- Output ---
OutputDir=Output
OutputBaseFilename=VoxFlow-Setup-{#AppVersion}
Compression=lzma2/ultra64
SolidCompression=yes

; --- Branding / wizard look ---
; Иконка setup.exe = отдельная яркая installer-иконка:
; installer\assets\build_setup_icon.py -> assets\voxflow-setup.ico.
SetupIconFile=assets\voxflow-setup.ico
WizardStyle=modern
WizardImageFile=assets\wizard-banner-164.bmp,assets\wizard-banner-246.bmp,assets\wizard-banner-328.bmp,assets\wizard-banner-459.bmp
WizardSmallImageFile=assets\wizard-small-55.bmp,assets\wizard-small-83.bmp,assets\wizard-small-110.bmp,assets\wizard-small-138.bmp

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Tasks]
; Desktop shortcut is OPT-IN (unchecked). Renders on the Additional Tasks page
; and is summarized on the Ready page. No autostart task — the app manages its
; own autostart at runtime.
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
; Keep prerequisite payloads first: with SolidCompression, ExtractTemporaryFile
; otherwise has to decompress every preceding (including CUDA) file before the
; prerequisite can start. noencryption permits early extraction if installer
; encryption is enabled in a future release; nocompression avoids useless work
; on the already-compressed signed bootstrapper.
Source: "..\voxflow\src-tauri\resources\windows-prerequisites\MicrosoftEdgeWebview2Setup.exe"; Flags: dontcopy noencryption nocompression
; The wizard extracts this bitmap during InitializeWizard, so it must also stay
; ahead of the large solid-compressed application payload.
Source: "assets\progress-grad.bmp"; Flags: dontcopy noencryption
; Main executable.
Source: "{#SrcDir}\voxflow.exe"; DestDir: "{app}"; Flags: ignoreversion
; App-local Microsoft VC++ CRT/OpenMP. This keeps the per-user install free of a
; VC_redist elevation/UAC step. The same DLLs are already copied into both
; whisper Release folders by fetch_windows_runtime.ps1 and travel with the
; wildcard resource entries below.
Source: "..\voxflow\src-tauri\resources\windows-redist\*"; DestDir: "{app}"; Flags: ignoreversion
; Whisper runtime — MUST land at {app}\resources\whisper\Release\* (paths.rs
; resolves resource_dir() == exe directory).
Source: "{#SrcDir}\resources\whisper\Release\*"; DestDir: "{app}\resources\whisper\Release"; Flags: ignoreversion recursesubdirs createallsubdirs
; GPU (CUDA) whisper runtime (~698 MB). paths.rs prefers this over the CPU build
; when NVIDIA nvcuda.dll is present. Lands at {app}\resources\whisper-cuda\Release\*.
Source: "{#SrcDir}\resources\whisper-cuda\Release\*"; DestDir: "{app}\resources\whisper-cuda\Release"; Flags: ignoreversion recursesubdirs createallsubdirs
; Silero VAD (~2.3 МБ) — берём напрямую из проверенного runtime-каталога.
Source: "..\voxflow\src-tauri\resources\vad\*"; DestDir: "{app}\resources\vad"; Flags: ignoreversion
[Icons]
Name: "{autoprograms}\VoxFlow"; Filename: "{app}\voxflow.exe"
Name: "{autodesktop}\VoxFlow"; Filename: "{app}\voxflow.exe"; Tasks: desktopicon

[Registry]
; Per-user (HKCU). InstallPath key removed on uninstall via uninsdeletekey.
Root: HKCU; Subkey: "Software\VoxFlow"; ValueType: string; ValueName: "InstallPath"; ValueData: "{app}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\VoxFlow"; ValueType: string; ValueName: "Version"; ValueData: "{#AppVersion}"
Root: HKCU; Subkey: "Software\VoxFlow"; ValueType: string; ValueName: "Publisher"; ValueData: "{#Publisher}"

[Run]
Filename: "{app}\voxflow.exe"; Description: "{cm:LaunchProgram,VoxFlow}"; Flags: nowait postinstall skipifsilent

[CustomMessages]
english.CreateDesktopIcon=Create a desktop shortcut
russian.CreateDesktopIcon=Создать ярлык на рабочем столе
english.WebView2LaunchFailed=Could not start the Microsoft WebView2 Runtime installer (error %1). VoxFlow was not installed.
russian.WebView2LaunchFailed=Не удалось запустить установщик Microsoft WebView2 Runtime (ошибка %1). VoxFlow не установлен.
english.WebView2InstallFailed=Microsoft WebView2 Runtime installation failed (exit code %1). Check the internet connection and try again.
russian.WebView2InstallFailed=Не удалось установить Microsoft WebView2 Runtime (код %1). Проверьте подключение к интернету и повторите попытку.
english.WebView2StillMissing=Microsoft WebView2 Runtime was not detected after installation. VoxFlow was not installed.
russian.WebView2StillMissing=Microsoft WebView2 Runtime не обнаружен после установки. VoxFlow не установлен.

; ============================================================================
;  [Code] — dark-neon recolor + custom cyan->magenta gradient progress bar.
;  Pure Inno; no third-party DLLs / .vsf. Optional WizardForm members are
;  guarded so a missing one cannot crash Pascal Script at runtime.
; ============================================================================
[Code]
const
  clBase      = $000F0A0A;   { #0A0A0F  base background        }
  clPanel     = $00181111;   { #111118  inset panel/field      }
  clText      = $00F7F5F5;   { #F5F5F7  primary text           }
  clSecondary = $00998A8A;   { #8A8A99  secondary text         }
  clDivider   = $00281E1E;   { #1E1E28  divider                }
  clCyan      = $00FFE500;   { #00E5FF  neon cyan accent        }
  clMagenta   = $00D62BFF;   { #FF2BD6  neon magenta            }
  WebView2ClientKey = 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

var
  ProgTrack: TPanel;       { fixed-size dark track            }
  ProgClip:  TPanel;       { grows L->R, clips the gradient   }
  ProgGrad:  TBitmapImage; { full-width cyan->magenta bitmap  }

{ VISIBILITY GUARD — per-user app must NOT be installed elevated.
  This is a PER-USER install (PrivilegesRequired=lowest): the uninstall entry is
  written to the running user's HKCU. If the user launches the setup "Run as
  administrator", it runs in the ADMINISTRATOR's security context and the entry
  lands in the administrator's profile/HKCU — invisible in the logged-in user's
  "Apps & features" (Параметры -> Приложения). This is the documented cause of
  "installed but not visible". We detect that case and warn (interactive only;
  silent installs are unaffected). IsAdminInstallMode is True only when actually
  running with admin rights — a normal double-click leaves it False (no warning). }
function InitializeSetup(): Boolean;
begin
  Result := True;
  if IsAdminInstallMode and (not WizardSilent) then
    MsgBox(
      'VoxFlow устанавливается ДЛЯ ТЕКУЩЕГО ПОЛЬЗОВАТЕЛЯ (без прав администратора).' + #13#10 + #13#10 +
      'Похоже, установщик запущен «от имени администратора». В этом случае запись об установке попадёт в список приложений АДМИНИСТРАТОРСКОГО профиля и НЕ будет видна в «Приложениях и компонентах» вашего профиля.' + #13#10 + #13#10 +
      'Рекомендуется: закройте установщик и запустите его ОБЫЧНЫМ двойным кликом (без «от имени администратора»).',
      mbInformation, MB_OK);
end;

{ The Evergreen runtime can be installed per-user or per-machine, and EdgeUpdate
  is a 32-bit component on many x64 systems. Check both registry views and both
  scopes before extracting or running the embedded online bootstrapper. }
function WebView2At(RootKey: Integer): Boolean;
var
  Version: String;
begin
  Result := RegQueryStringValue(RootKey, WebView2ClientKey, 'pv', Version);
  if Result then
  begin
    Version := Trim(Version);
    Result := (Version <> '') and (Version <> '0.0.0.0');
  end;
end;

function WebView2RuntimeInstalled(): Boolean;
begin
  Result :=
    WebView2At(HKCU32) or WebView2At(HKCU64) or
    WebView2At(HKLM32) or WebView2At(HKLM64);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Bootstrapper: String;
  ResultCode: Integer;
  Attempt: Integer;
begin
  Result := '';
  if WebView2RuntimeInstalled() then
  begin
    Log('Microsoft WebView2 Runtime detected; bootstrapper skipped.');
    Exit;
  end;

  Log('Microsoft WebView2 Runtime missing; running embedded Evergreen bootstrapper.');
  ExtractTemporaryFile('MicrosoftEdgeWebview2Setup.exe');
  Bootstrapper := ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe');
  if not Exec(
    Bootstrapper, '/silent /install', ExpandConstant('{tmp}'),
    SW_HIDE, ewWaitUntilTerminated, ResultCode) then
  begin
    Result := FmtMessage(CustomMessage('WebView2LaunchFailed'), [IntToStr(ResultCode)]);
    Exit;
  end;
  if ResultCode <> 0 then
  begin
    Result := FmtMessage(CustomMessage('WebView2InstallFailed'), [IntToStr(ResultCode)]);
    Exit;
  end;

  { EdgeUpdate can publish the registry value a moment after the bootstrapper
    process exits. Wait briefly, then fail closed instead of installing an app
    whose only UI cannot start. }
  for Attempt := 1 to 20 do
  begin
    if WebView2RuntimeInstalled() then
    begin
      Log('Microsoft WebView2 Runtime installed successfully.');
      Exit;
    end;
    Sleep(500);
  end;
  Result := CustomMessage('WebView2StillMissing');
end;

{ Recolor a single notebook page to the neon base (guarded). }
procedure ColorPage(Page: TNewNotebookPage);
begin
  if Page <> nil then
    Page.Color := clBase;
end;

{ Recolor every wizard page background to the neon base.
  NOTE: TNewNotebook (Outer/InnerNotebook) does NOT expose Color in Pascal
  Script — only the individual TNewNotebookPage pages do, so we color those. }
procedure RecolorPages;
begin
  ColorPage(WizardForm.WelcomePage);
  ColorPage(WizardForm.InnerPage);
  ColorPage(WizardForm.FinishedPage);
  ColorPage(WizardForm.LicensePage);
  ColorPage(WizardForm.PasswordPage);
  ColorPage(WizardForm.InfoBeforePage);
  ColorPage(WizardForm.SelectDirPage);
  ColorPage(WizardForm.SelectComponentsPage);
  ColorPage(WizardForm.SelectProgramGroupPage);
  ColorPage(WizardForm.SelectTasksPage);
  ColorPage(WizardForm.ReadyPage);
  ColorPage(WizardForm.PreparingPage);
  ColorPage(WizardForm.InstallingPage);
  ColorPage(WizardForm.InfoAfterPage);
end;

{ Build the custom neon gradient progress bar over the native gauge. }
procedure BuildProgressBar;
var
  GaugeParent: TWinControl;
begin
  if WizardForm.ProgressGauge = nil then
    Exit;

  { Hide the native gauge — we draw our own. }
  WizardForm.ProgressGauge.Visible := False;
  GaugeParent := WizardForm.ProgressGauge.Parent;
  if GaugeParent = nil then
    Exit;

  { Dark track at the gauge's exact bounds. }
  ProgTrack := TPanel.Create(WizardForm);
  ProgTrack.Parent := GaugeParent;
  ProgTrack.Left := WizardForm.ProgressGauge.Left;
  ProgTrack.Top := WizardForm.ProgressGauge.Top;
  ProgTrack.Width := WizardForm.ProgressGauge.Width;
  ProgTrack.Height := WizardForm.ProgressGauge.Height;
  ProgTrack.BevelOuter := bvNone;
  ProgTrack.BevelInner := bvNone;
  ProgTrack.ParentBackground := False;
  ProgTrack.Color := clPanel;

  { Clipping panel — width = current progress; children clip to it. }
  ProgClip := TPanel.Create(WizardForm);
  ProgClip.Parent := ProgTrack;
  ProgClip.Left := 0;
  ProgClip.Top := 0;
  ProgClip.Width := 0;
  ProgClip.Height := ProgTrack.Height;
  ProgClip.BevelOuter := bvNone;
  ProgClip.BevelInner := bvNone;
  ProgClip.ParentBackground := False;
  ProgClip.Color := clBase;

  { Full-width gradient bitmap inside the clip. Revealing more of the clip
    reveals more of the cyan->magenta gradient. }
  ProgGrad := TBitmapImage.Create(WizardForm);
  ProgGrad.Parent := ProgClip;
  ProgGrad.Left := 0;
  ProgGrad.Top := 0;
  ProgGrad.Width := ProgTrack.Width;
  ProgGrad.Height := ProgTrack.Height;
  ProgGrad.Stretch := True;
  try
    ExtractTemporaryFile('progress-grad.bmp');
    ProgGrad.Bitmap.LoadFromFile(ExpandConstant('{tmp}\progress-grad.bmp'));
  except
    { If extraction/loading fails, fall back to a flat cyan fill so the bar
      still animates. }
    ProgClip.Color := clCyan;
  end;
end;

procedure InitializeWizard;
begin
  { --- Window + top panel --- }
  WizardForm.Color := clBase;
  if WizardForm.MainPanel <> nil then
  begin
    WizardForm.MainPanel.Color := clBase;
    WizardForm.MainPanel.Font.Color := clText;
  end;

  { --- Page header labels --- }
  if WizardForm.PageNameLabel <> nil then
  begin
    WizardForm.PageNameLabel.Font.Color := clText;
    WizardForm.PageNameLabel.Color := clBase;
  end;
  if WizardForm.PageDescriptionLabel <> nil then
  begin
    WizardForm.PageDescriptionLabel.Font.Color := clSecondary;
    WizardForm.PageDescriptionLabel.Color := clBase;
  end;

  { --- Hide the bevels / divider lines (guarded) --- }
  if WizardForm.Bevel <> nil then
    WizardForm.Bevel.Visible := False;
  if WizardForm.BeveledLabel <> nil then
    WizardForm.BeveledLabel.Visible := False;

  { --- All page backgrounds --- }
  RecolorPages;

  { --- Welcome page text --- }
  if WizardForm.WelcomeLabel1 <> nil then
    WizardForm.WelcomeLabel1.Font.Color := clText;
  if WizardForm.WelcomeLabel2 <> nil then
    WizardForm.WelcomeLabel2.Font.Color := clSecondary;

  { --- Finished page text --- }
  if WizardForm.FinishedHeadingLabel <> nil then
    WizardForm.FinishedHeadingLabel.Font.Color := clText;
  if WizardForm.FinishedLabel <> nil then
    WizardForm.FinishedLabel.Font.Color := clText;
  if WizardForm.RunList <> nil then
  begin
    WizardForm.RunList.Color := clPanel;
    WizardForm.RunList.Font.Color := clText;
  end;

  { --- Destination page --- }
  if WizardForm.SelectDirLabel <> nil then
    WizardForm.SelectDirLabel.Font.Color := clText;
  if WizardForm.SelectDirBrowseLabel <> nil then
    WizardForm.SelectDirBrowseLabel.Font.Color := clText;
  if WizardForm.DiskSpaceLabel <> nil then
    WizardForm.DiskSpaceLabel.Font.Color := clSecondary;
  if WizardForm.DirEdit <> nil then
  begin
    WizardForm.DirEdit.Color := clPanel;
    WizardForm.DirEdit.Font.Color := clText;
  end;

  { --- Tasks page --- }
  if WizardForm.SelectTasksLabel <> nil then
    WizardForm.SelectTasksLabel.Font.Color := clText;
  if WizardForm.TasksList <> nil then
  begin
    WizardForm.TasksList.Color := clPanel;
    WizardForm.TasksList.Font.Color := clText;
  end;

  { --- Ready page --- }
  if WizardForm.ReadyLabel <> nil then
    WizardForm.ReadyLabel.Font.Color := clText;
  if WizardForm.ReadyMemo <> nil then
  begin
    WizardForm.ReadyMemo.Color := clPanel;
    WizardForm.ReadyMemo.Font.Color := clText;
  end;

  { --- Installing page status labels --- }
  if WizardForm.StatusLabel <> nil then
    WizardForm.StatusLabel.Font.Color := clSecondary;
  if WizardForm.FilenameLabel <> nil then
    WizardForm.FilenameLabel.Font.Color := clSecondary;

  { --- Preparing page / close-running-apps prompt --- }
  if WizardForm.PreparingLabel <> nil then
  begin
    WizardForm.PreparingLabel.Color := clBase;
    WizardForm.PreparingLabel.Font.Color := clSecondary;
  end;
  if WizardForm.PreparingMemo <> nil then
  begin
    WizardForm.PreparingMemo.Color := clPanel;
    WizardForm.PreparingMemo.Font.Color := clText;
  end;
  if WizardForm.PreparingYesRadio <> nil then
  begin
    WizardForm.PreparingYesRadio.Color := clBase;
    WizardForm.PreparingYesRadio.Font.Color := clText;
  end;
  if WizardForm.PreparingNoRadio <> nil then
  begin
    WizardForm.PreparingNoRadio.Color := clBase;
    WizardForm.PreparingNoRadio.Font.Color := clText;
  end;

  { --- Buttons: neon-cyan accent on the primary action --- }
  if WizardForm.NextButton <> nil then
    WizardForm.NextButton.Font.Color := clCyan;
  if WizardForm.BackButton <> nil then
    WizardForm.BackButton.Font.Color := clText;
  if WizardForm.CancelButton <> nil then
    WizardForm.CancelButton.Font.Color := clText;

  { --- Custom neon progress bar --- }
  BuildProgressBar;
end;

{ Drive the custom progress bar during file copy. }
procedure CurInstallProgressChanged(CurProgress, MaxProgress: Integer);
begin
  if (ProgTrack <> nil) and (ProgClip <> nil) and (MaxProgress > 0) then
  begin
    ProgClip.Width := Round(ProgTrack.Width * CurProgress / MaxProgress);
    WizardForm.Refresh;
  end;
end;
