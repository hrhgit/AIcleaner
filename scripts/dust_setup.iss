; ==============================================================
;  DustCleaner Inno Setup Script
;  生成专业 GUI 安装向导，支持自定义安装路径、卸载
; ==============================================================

#define MyAppName "AIcleaner"
#define MyAppVersion "1.0"
#define MyAppPublisher "AIcleaner Team"
#define MyAppURL "http://localhost:3001"
#define MyAppExeName "start.bat"

[Setup]
; 应用基本信息
AppId={{A3F1B2C4-D5E6-4F78-9A0B-C1D2E3F4A5B6}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}

; 默认安装目录（用户可在向导中修改）
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}

; 是否允许用户修改安装目录（关键设置）
DisableDirPage=no
DisableProgramGroupPage=yes

; 输出文件设置
OutputDir=..
OutputBaseFilename=AIcleaner_Setup
Compression=lzma2
SolidCompression=yes

; 安装程序外观
WizardStyle=modern
WizardResizable=no
ShowLanguageDialog=no

; 权限（不强制要求管理员，普通用户也可装到自己的目录）
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

; 卸载程序设置
Uninstallable=yes
UninstallDisplayName={#MyAppName}
UninstallDisplayIcon={app}\bin\node.exe

; 安装完成后自动启动
[Run]
Filename: "{app}\start.bat"; Description: "立即启动 {#MyAppName}"; \
  Flags: postinstall nowait skipifsilent shellexec

[Languages]
Name: "ChineseSimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
; 将 release 目录下的全部内容打包进安装程序
Source: "..\release\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; Excludes: "settings.json"

[Icons]
; 开始菜单快捷方式
Name: "{group}\AIcleaner"; Filename: "{app}\start.bat"; WorkingDir: "{app}"
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"

; 桌面快捷方式（仅当用户勾选时创建）
Name: "{autodesktop}\AIcleaner"; Filename: "{app}\start.bat"; WorkingDir: "{app}"; \
  Tasks: desktopicon

[UninstallDelete]
; 卸载时额外清理应用数据目录
Type: filesandordirs; Name: "{app}"
