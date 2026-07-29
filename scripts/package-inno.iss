; Glean Windows installer (Inno Setup 6).
;
; Build:
;   iscc scripts\package-inno.iss
;
; Expects the release exe at ..\target\release\glean-spike.exe (run
; `cargo build -p glean-app --release` first).
;
; Output: ..\target\installer\glean-spike-setup.exe
;
; Portable mode: after install, create a `data\` folder next to
; glean-spike.exe (e.g. "C:\Program Files\Glean\data\") and the app will
; store glean.db + config.json there instead of %APPDATA%\Glean\.

#define MyAppName "Glean"
#define MyAppNameCN "拾光"
#define MyAppVersion "0.0.1"
#define MyAppPublisher "madlaxcb"
#define MyAppURL "https://github.com/madlaxcb/Glean"
#define MyAppExeName "glean-spike.exe"

[Setup]
AppId={{B8F3E4A2-1C5D-4E6F-9A0B-1D2E3F4A5B6C}
AppName={#MyAppName}
AppVerName={#MyAppName} {#MyAppVersion}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName} ({#MyAppNameCN})
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
OutputDir=..\target\installer
OutputBaseFilename=glean-spike-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
DisableProgramGroupPage=yes
MinVersion=10.0.17763

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\glean-spike.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\scripts\portable-mode.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\卸载 {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{commondesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[Code]
// WebView2 Evergreen Runtime client GUID.
const
  WebView2ClientKey = 'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2ClientKey6432 = 'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2DownloadUrl = 'https://developer.microsoft.com/microsoft-edge/webview2/';

function WebView2Installed: Boolean;
begin
  Result := RegKeyExists(HKLM, WebView2ClientKey)
    or RegKeyExists(HKLM, WebView2ClientKey6432)
    or RegKeyExists(HKCU, WebView2ClientKey)
    or RegKeyExists(HKCU, WebView2ClientKey6432);
end;

function InitializeSetup(): Boolean;
begin
  Result := True;
  if not WebView2Installed then begin
    if MsgBox(
        '未检测到 WebView2 Runtime。' #13#10 #13#10
        'Glean 需要它来显示阅读区。' #13#10
        '是否继续安装？(安装后请自行下载 WebView2 Runtime)' #13#10 #13#10
        '下载地址: ' + WebView2DownloadUrl,
        mbConfirmation, MB_YESNO) = IDNO then
      Result := False;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  if not WebView2Installed then begin
    MsgBox(
      '安装完成。请先下载并安装 WebView2 Runtime 后再运行 Glean:' #13#10 #13#10
      + WebView2DownloadUrl,
      mbInformation, MB_OK);
  end;
end;
