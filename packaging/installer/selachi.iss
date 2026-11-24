; Selachi installer (Inno Setup 6.3+/7). Per-user, no administrator.
; Build it with packaging\installer\build.ps1 (stages the files and calls ISCC with the defines below).
;
;   /DAppVersion=0.1.0            version shown in Apps & Features (required)
;   /DAppVersionNumeric=0.1.0.0   a.b.c.d for the exe version resource (required)
;   /DStageDir=<abs path>         folder produced by build.ps1 (required)
;   /DOutDir=<abs path>           where Selachi-Setup-<version>.exe is written
;   /DNoContextMenu               omit the "Abrir en Selachi" Explorer task
;   /DSign                        sign setup + uninstaller with the SignTool named "selachi" (/Sselachi=...)
;
; NEVER change AppId: it is what makes a newer installer an in-place UPDATE (6b.2 auto-update).
;
; Silent install (winget / auto-update):
;   Selachi-Setup-x.y.z.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- [/TASKS="addtopath,desktopicon,contextmenu"]
;   Update over a running app: add /CLOSEAPPLICATIONS /RESTARTAPPLICATIONS
; Silent uninstall (keeps %APPDATA%\Selachi):
;   "%LOCALAPPDATA%\Programs\Selachi\unins000.exe" /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/DELETEDATA]

#ifndef AppVersion
  #error AppVersion is required (build.ps1 passes it)
#endif
#ifndef AppVersionNumeric
  #define AppVersionNumeric "0.0.0.0"
#endif
#ifndef StageDir
  #error StageDir is required (build.ps1 passes it)
#endif
#ifndef OutDir
  #define OutDir "dist"
#endif

#define AppName "Selachi"
#define AppExe "selachi.exe"

[Setup]
AppId={{54B88A1C-4587-45C6-917D-CFE29C4B595A}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Selachi
AppPublisherURL=https://github.com/YewdielMartinez/Selachi
AppSupportURL=https://github.com/YewdielMartinez/Selachi/issues
AppUpdatesURL=https://github.com/YewdielMartinez/Selachi/releases
VersionInfoVersion={#AppVersionNumeric}
VersionInfoProductName={#AppName}
VersionInfoDescription={#AppName} Setup
; Per-user: {autopf} resolves to %LOCALAPPDATA%\Programs when running without admin.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; ConPTY needs Windows 10 1809 (build 17763).
MinVersion=10.0.17763
OutputDir={#OutDir}
OutputBaseFilename=Selachi-Setup-{#AppVersion}
SetupIconFile={#StageDir}\selachi.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; Restart Manager closes a running Selachi / selachi-agent before replacing the files.
CloseApplications=yes
RestartApplications=no
; HKCU\Environment\Path change (opt-in task) is broadcast to new processes.
ChangesEnvironment=yes
#ifdef Sign
SignTool=selachi
SignedUninstaller=yes
#endif

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
spanish.TaskAddToPath=Añadir la carpeta de Selachi al PATH (para usar selachi-agent desde cualquier terminal)
spanish.TaskContextMenu=Añadir "Abrir en Selachi" al menú contextual del Explorador
spanish.AskDeleteData=¿Borrar también tus datos de Selachi (configuración, historial, plugins en %APPDATA%\Selachi)?%n%nElige No para conservarlos y poder reinstalar sin perder nada.
spanish.RunSelachi=Abrir Selachi
english.TaskAddToPath=Add the Selachi folder to PATH (to use selachi-agent from any terminal)
english.TaskContextMenu=Add "Open in Selachi" to the Explorer context menu
english.AskDeleteData=Also delete your Selachi data (settings, history, plugins in %APPDATA%\Selachi)?%n%nChoose No to keep it so a reinstall loses nothing.
english.RunSelachi=Open Selachi

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "{cm:TaskAddToPath}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
#ifndef NoContextMenu
Name: "contextmenu"; Description: "{cm:TaskContextMenu}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
#endif

[Files]
Source: "{#StageDir}\selachi.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\selachi-agent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\selachi.ico"; DestDir: "{app}"; Flags: ignoreversion
; The exe resolves the onboarding GIF at <exe dir>\assets\onboarding\shark.gif (ui_onboarding::gif).
Source: "{#StageDir}\assets\onboarding\shark.gif"; DestDir: "{app}\assets\onboarding"; Flags: ignoreversion
Source: "{#StageDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\THIRD-PARTY-LICENSES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\selachi.ico"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\selachi.ico"; Tasks: desktopicon

[Registry]
; PATH (HKCU\Environment, opt-in). Removed again in CurUninstallStepChanged.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Check: NeedsAddPath(ExpandConstant('{app}')); Tasks: addtopath
#ifndef NoContextMenu
; Explorer context menu (opt-in, per user). On Windows 11 it appears under "Mostrar más opciones".
; The command passes the folder as --cwd (the app must support it; see the 6b.1 report).
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Selachi"; ValueType: string; ValueName: ""; ValueData: "Abrir en Selachi"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Selachi"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Selachi\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --cwd ""%V"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Selachi"; ValueType: string; ValueName: ""; ValueData: "Abrir en Selachi"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Selachi"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#AppExe}"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Selachi\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" --cwd ""%V"""; Tasks: contextmenu
#endif

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:RunSelachi}"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

var
  DeleteUserData: Boolean;

function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
    exit;
  P := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
  if P = 0 then
    exit;
  { P is the 1-based position of Dir inside Paths. Remove it with one neighbouring ';'. }
  if P > 1 then
    Delete(Paths, P - 1, Length(Dir) + 1)
  else
    Delete(Paths, 1, Length(Dir) + 1);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths);
end;

function HasParam(const Name: string): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), Name) = 0 then
    begin
      Result := True;
      exit;
    end;
end;

function InitializeUninstall(): Boolean;
begin
  { User data is kept unless the user asks for it (prompt) or passes /DELETEDATA (silent). }
  DeleteUserData := HasParam('/DELETEDATA');
  if (not DeleteUserData) and (not UninstallSilent()) then
    DeleteUserData := MsgBox(CustomMessage('AskDeleteData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}'))
  else if CurUninstallStep = usPostUninstall then
  begin
    { Always remove what the app regenerates by itself (shell integration script). }
    DelTree(ExpandConstant('{localappdata}\Selachi\shell'), True, True, True);
    if DeleteUserData then
    begin
      DelTree(ExpandConstant('{userappdata}\Selachi'), True, True, True);
      DelTree(ExpandConstant('{localappdata}\Selachi'), True, True, True);
    end;
  end;
end;
