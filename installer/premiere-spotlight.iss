#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName "Premiere Spotlight"
#define MyAppExeName "premiere-spotlight.exe"
#define MyAppMutex "PremiereSpotlightInstanceMutex"
#define MyTrayWndClass "PremiereSpotlightTrayWndClass"

[Setup]
AppId={{7E8E9E4C-4B2A-4D6D-9B2B-6C5F9A4E1A23}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=Premiere Spotlight
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=PremiereSpotlightSetup-{#MyAppVersion}
SetupIconFile=..\helper-app\assets\spotlight.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
AppMutex={#MyAppMutex}
CloseApplications=yes
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &Desktop shortcut"; GroupDescription: "Additional options:"; Flags: unchecked

[Files]
Source: "..\helper-app\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
; Premiere's UXP plugin launches Spotlight on demand, so no Windows startup entry is registered.
Filename: "{app}\{#MyAppExeName}"; Parameters: "--install"; Flags: runhidden waituntilterminated; StatusMsg: "Registering Premiere Pro UXP plugin..."
Filename: "{app}\{#MyAppExeName}"; Parameters: "--watch"; Description: "Launch Premiere Spotlight in the system tray"; Flags: nowait postinstall skipifsilent

[Code]
procedure CloseRunningSpotlight();
var
  Hwnd: HWND;
  Attempts: Integer;
  ResultCode: Integer;
begin
  Hwnd := FindWindowByClassName('{#MyTrayWndClass}');
  if Hwnd <> 0 then
  begin
    PostMessage(Hwnd, $0010, 0, 0);
    Attempts := 0;
    while CheckForMutexes('{#MyAppMutex}') and (Attempts < 20) do
    begin
      Sleep(100);
      Attempts := Attempts + 1;
    end;
  end;

  if CheckForMutexes('{#MyAppMutex}') then
  begin
    Exec('taskkill.exe', '/IM {#MyAppExeName} /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(300);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseRunningSpotlight();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  CloseRunningSpotlight();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Params: String;
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    Params := '--uninstall';
    if not UninstallSilent then
    begin
      if MsgBox(
        'Do you also want to remove your saved Premiere Spotlight settings and custom hotkeys?'#13#10#13#10 +
        'Choose "No" to keep them in case you reinstall later.',
        mbConfirmation,
        MB_YESNO or MB_DEFBUTTON2
      ) = IDYES then
      begin
        Params := Params + ' --purge-config';
      end;
    end;

    Exec(ExpandConstant('{app}\{#MyAppExeName}'), Params, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpFinished then
  begin
    WizardForm.FinishedLabel.Caption :=
      WizardForm.FinishedLabel.Caption + #13#10#13#10 +
      'If Adobe Premiere Pro is currently open, please restart it once so the Premiere Spotlight Bridge UXP plugin loads.';
  end;
end;
