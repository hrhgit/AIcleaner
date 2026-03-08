; ==============================================================
;  AIcleaner Inno Setup Script
;  Installer for the portable Node runtime and Rust scanner sidecar
; ==============================================================

#define MyAppName "AIcleaner"
#define MyAppVersion "1.0"
#define MyAppPublisher "AIcleaner Team"
#define MyAppURL "http://localhost:3001"
#define MyAppExeName "start.bat"

[Setup]
AppId={{A3F1B2C4-D5E6-4F78-9A0B-C1D2E3F4A5B6}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableDirPage=no
DisableProgramGroupPage=yes
OutputDir=..
OutputBaseFilename=AIcleaner_Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
WizardResizable=no
ShowLanguageDialog=no
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
Uninstallable=yes
UninstallDisplayName={#MyAppName}
UninstallDisplayIcon={app}\bin\node.exe

[Run]
Filename: "{app}\start.bat"; Description: "Launch {#MyAppName}"; \
  Flags: postinstall nowait skipifsilent shellexec

[Languages]
Name: "ChineseSimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\release\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "settings.json"

[Icons]
Name: "{group}\AIcleaner"; Filename: "{app}\start.bat"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\AIcleaner"; Filename: "{app}\start.bat"; WorkingDir: "{app}"; \
  Tasks: desktopicon

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
