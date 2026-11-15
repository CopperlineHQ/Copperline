; The payload is the portable ZIP, extracted by build-installer.ps1. Keep the
; AppId stable across versions so reinstalling updates the existing install.
#ifndef AppVersion
  #error AppVersion must be supplied by build-installer.ps1
#endif
#ifndef AppVersionNumeric
  #error AppVersionNumeric must be supplied by build-installer.ps1
#endif
#ifndef AppArch
  #error AppArch must be supplied by build-installer.ps1
#endif
#ifndef PayloadDir
  #error PayloadDir must be supplied by build-installer.ps1
#endif

[Setup]
AppId=CopperlineHQ.Copperline
AppName=Copperline
AppVersion={#AppVersion}
AppPublisher=CopperlineHQ
AppPublisherURL=https://copperline.dev/
AppSupportURL=https://github.com/CopperlineHQ/Copperline/issues
AppUpdatesURL=https://github.com/CopperlineHQ/Copperline/releases
; Setup opens by asking "for me only" or "for all users". Me only is the
; recommended default: no administrator rights, {autopf} is
; %LOCALAPPDATA%\Programs. All users elevates and {autopf} is Program Files;
; the {auto*} shortcut folders follow the same choice. /CURRENTUSER and
; /ALLUSERS answer it on the command line, and win over everything else.
; Without one, an update finds the earlier install's mode
; (UsePreviousPrivileges, default yes) and does not ask.
; A read-only Program Files folder is fine: host data defaults to each
; user's Documents\Copperline, and the crash log falls back to a writable
; directory. Only portable.txt mode needs the install folder writable.
DefaultDirName={autopf}\Copperline
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
MinVersion=10.0
#if AppArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#elif AppArch == "x64"
ArchitecturesAllowed=x64compatible and not arm64
ArchitecturesInstallIn64BitMode=x64compatible
#else
  #error Unsupported AppArch
#endif
OutputBaseFilename=Copperline-{#AppVersion}-win-{#AppArch}-setup
SetupIconFile=..\..\assets\brand\copperline.ico
UninstallDisplayIcon={app}\copperline.exe
InfoBeforeFile={#PayloadDir}\LICENSE.txt
; Spelled out rather than left to Inno's defaults: the SignPath artifact
; configuration (signpath/windows-installer.xml) only signs a Setup whose
; product name, publisher and product version match these, and the Windows
; CI installer smoke checks them. Inno pads each value with spaces to a fixed
; width; build-installer.ps1 removes that padding after compiling.
VersionInfoVersion={#AppVersionNumeric}
VersionInfoTextVersion={#AppVersion}
VersionInfoProductName=Copperline
VersionInfoCompany=CopperlineHQ
VersionInfoProductTextVersion={#AppVersion}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked

[Files]
; Inno records the shipped files individually. No wildcard uninstall deletion:
; settings, saves and media added by the user must survive removal.
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\copperline.exe"; Description: "Launch Copperline"; Flags: nowait postinstall skipifsilent

[Code]
var
  DesktopShortcutInstalled: Boolean;

// Something else can have a shortcut open at the moment the uninstaller
// reaches it (Explorer, or a scanner reading the desktop), and an uninstall
// leaves a file it cannot delete. Give the shortcut further attempts, for up
// to ten seconds, once everything else has been removed.
procedure DeleteShortcutWhenFree(const Path: String);
var
  Attempt: Integer;
begin
  for Attempt := 1 to 100 do
  begin
    if not FileExists(Path) then
      Exit;
    if DeleteFile(Path) then
    begin
      Log('Deleted on a later attempt: ' + Path);
      Exit;
    end;
    Sleep(100);
  end;
  Log('Still in use, left in place: ' + Path);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Tasks: String;
begin
  if CurUninstallStep = usUninstall then
  begin
    // The desktop shortcut is optional, and a shortcut of the same name that
    // Setup did not create is not this uninstaller's to remove. The record
    // of the chosen tasks goes with the uninstall entry, so read it first.
    DesktopShortcutInstalled :=
      RegQueryStringValue(HKA,
        'Software\Microsoft\Windows\CurrentVersion\Uninstall\CopperlineHQ.Copperline_is1',
        'Inno Setup: Selected Tasks', Tasks) and (Pos('desktopicon', Tasks) > 0);
  end
  else if CurUninstallStep = usPostUninstall then
  begin
    DeleteShortcutWhenFree(ExpandConstant('{autoprograms}\Copperline.lnk'));
    if DesktopShortcutInstalled then
      DeleteShortcutWhenFree(ExpandConstant('{autodesktop}\Copperline.lnk'));
  end;
end;
