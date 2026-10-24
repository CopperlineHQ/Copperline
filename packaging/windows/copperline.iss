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
DefaultDirName={localappdata}\Programs\Copperline
DefaultGroupName=Copperline
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
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
LicenseFile={#PayloadDir}\LICENSE.txt
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
Name: "{group}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\copperline.exe"; Description: "Launch Copperline"; Flags: nowait postinstall skipifsilent
