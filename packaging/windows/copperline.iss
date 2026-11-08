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
ChangesAssociations=yes
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked

[Files]
; Inno records the shipped files individually. No wildcard uninstall deletion:
; settings, saves and media added by the user must survive removal.
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Registry]
; Add Open with choices in the selected install mode. Never write extension
; defaults or Explorer UserChoice; other applications keep their associations.
Root: HKA; Subkey: "Software\Classes\Copperline.Media"; ValueType: string; ValueName: ""; ValueData: "Amiga media and Copperline configurations"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Copperline.Media\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\copperline.exe,0"
Root: HKA; Subkey: "Software\Classes\Copperline.Media\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\copperline.exe"" -- ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\.adf\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".adf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.adz\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".adz"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.dms\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".dms"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.ipf\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".ipf"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.scp\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".scp"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.lha\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".lha"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.lzh\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".lzh"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.zip\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".zip"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.slave\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".slave"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.slav\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".slav"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.cue\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".cue"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.iso\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".iso"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.nrg\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".nrg"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.chd\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".chd"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.rom\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".rom"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\.toml\OpenWithProgids"; ValueType: string; ValueName: "Copperline.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Applications\copperline.exe\SupportedTypes"; ValueType: string; ValueName: ".toml"; ValueData: ""

[Icons]
Name: "{autoprograms}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Copperline"; Filename: "{app}\copperline.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\copperline.exe"; Description: "Launch Copperline"; Flags: nowait postinstall skipifsilent
