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
VersionInfoVersion={#AppVersionNumeric}
VersionInfoTextVersion={#AppVersion}
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
