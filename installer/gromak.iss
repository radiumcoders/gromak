; Inno Setup script for gromak. Build it with installer\build.ps1, which
; compiles gromak in release mode and passes the version from Cargo.toml.
;
; The installer is per-user (no administrator prompt): it installs into
; %LOCALAPPDATA%\Programs\gromak, adds a Start menu entry, optionally starts
; gromak at sign-in (listed in Task Manager > Startup apps), and registers an
; uninstaller under Settings > Apps.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\gromak.exe"
#endif

[Setup]
; Never change AppId: it is how upgrades and the uninstaller find gromak.
AppId={{BAE55915-B860-400F-9733-5F7D2B688ABF}
AppName=gromak
AppVersion={#AppVersion}
AppVerName=gromak {#AppVersion}
AppPublisher=gromak
AppPublisherURL=https://github.com/radiumcoders/gromak
AppSupportURL=https://github.com/radiumcoders/gromak/issues
AppUpdatesURL=https://github.com/radiumcoders/gromak/releases
DefaultDirName={localappdata}\Programs\gromak
DisableDirPage=auto
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
LicenseFile=..\LICENSE
SetupIconFile=..\assets\gromak.ico
UninstallDisplayIcon={app}\gromak.exe
UninstallDisplayName=gromak
OutputDir=..\target\installer
OutputBaseFilename=gromak-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; gromak is stopped cleanly with --quit (see [Code]); the Restart Manager
; would kill it instead and skip restoring hidden windows.
CloseApplications=no
RestartApplications=no

[Tasks]
Name: startup; Description: "Start gromak when I sign in"; GroupDescription: "Startup:"
Name: desktopicon; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\config\default.toml"; DestDir: "{app}"; DestName: "default-config.toml"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\gromak"; Filename: "{app}\gromak.exe"; Comment: "Tiling window manager"
Name: "{userdesktop}\gromak"; Filename: "{app}\gromak.exe"; Comment: "Tiling window manager"; Tasks: desktopicon

[Registry]
; The Run key is what Task Manager's Startup apps page lists and toggles.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "gromak"; ValueData: """{app}\gromak.exe"""; Flags: uninsdeletevalue; Tasks: startup
; Unticking the task on an upgrade removes the entry.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "gromak"; ValueType: none; Flags: deletevalue; Tasks: not startup

[Run]
Filename: "{app}\gromak.exe"; Description: "Start gromak now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Restores hidden windows and Windows Snap before the files go away.
Filename: "{app}\gromak.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "QuitGromak"

[Code]
// Stop a running gromak (any version: they all honour the quit message)
// before its exe is replaced. The new exe is extracted to {tmp} to send it.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  ExtractTemporaryFile('gromak.exe');
  Exec(ExpandConstant('{tmp}\gromak.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;
