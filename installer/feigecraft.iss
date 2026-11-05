; FeigeCraft 安装包脚本（Inno Setup 6）
;
; 编译：scripts\build-installer.ps1（会先 cargo build --release，再调 ISCC）
; 也可以手动：ISCC.exe /DMyAppVersion=1.0.0 installer\feigecraft.iss

#define MyAppName "FeigeCraft"
#define MyAppVerName "FeigeCraft"
#define MyAppPublisher "FeigeCraft contributors"
#define MyAppExeName "feigecraft.exe"

; 版本号由构建脚本从 Cargo.toml 读出来传进来，这里留个兜底。
#ifndef MyAppVersion
  #define MyAppVersion "1.0.0"
#endif

[Setup]
; AppId 一旦发布就不能改，否则新版本会被当成另一个软件、旧版本卸不掉。
AppId={{8F3A5C21-7B4D-4E9A-9C6E-2D1F5A8B3C47}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppVerName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
VersionInfoVersion={#MyAppVersion}
VersionInfoDescription=FeigeCraft 安装程序
VersionInfoProductName={#MyAppName}
VersionInfoProductVersion={#MyAppVersion}

DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes

; 主程序是 64 位（egui + glow），只允许在 64 位 Windows 上安装。
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

; 局域网发现要在防火墙里放行，这一步需要管理员；
; 允许用户用命令行 /CURRENTUSER 覆盖成「只为我安装」，此时自动跳过防火墙任务。
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=dialog commandline

OutputDir=..\dist
OutputBaseFilename={#MyAppName}-{#MyAppVersion}-setup
SetupIconFile=..\crates\gui\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppVerName} {#MyAppVersion}

LicenseFile=..\LICENSE
; 开机自启带 --background：开机只在后台收文件，不弹窗；双击图标才进前台。
; 写在 HKCU。绝大多数人是「自己的账户（本身是管理员）登录并安装」，
; 这时 HKCU 就是正确的当前用户；只有「标准用户 + 输入管理员密码提权」才会落到
; 管理员账户下，属于少见情况，这里明确接受，不为此改写成机器级 HKLM。
UsedUserAreasWarning=no
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern

; 装包时有旧版本在跑：提示关闭而不是让文件被占用。
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
chinesesimplified.CreateDesktopIcon=创建桌面快捷方式
chinesesimplified.StartWithWindows=开机时自动启动 FeigeCraft
chinesesimplified.FirewallTask=在 Windows 防火墙中放行 FeigeCraft（局域网发现与传输需要）
chinesesimplified.GroupShortcuts=快捷方式
chinesesimplified.GroupStartup=启动选项
chinesesimplified.GroupNetwork=网络
chinesesimplified.LaunchApp=立即运行 FeigeCraft
chinesesimplified.RequiresAdminForFirewall=当前为「仅为我安装」，不会修改防火墙。若其他设备发现不到本机，请手动放行 53317 端口。

english.CreateDesktopIcon=Create a desktop shortcut
english.StartWithWindows=Start FeigeCraft when Windows starts
english.FirewallTask=Allow FeigeCraft through Windows Firewall (needed for LAN discovery and transfers)
english.GroupShortcuts=Shortcuts
english.GroupStartup=Startup
english.GroupNetwork=Network
english.LaunchApp=Launch FeigeCraft
english.RequiresAdminForFirewall=Installing for the current user only, so the firewall was not modified. Allow port 53317 manually if other devices cannot find this computer.

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:GroupShortcuts}"; Flags: unchecked
Name: "autostart"; Description: "{cm:StartWithWindows}"; GroupDescription: "{cm:GroupStartup}"
Name: "firewall"; Description: "{cm:FirewallTask}"; GroupDescription: "{cm:GroupNetwork}"; Check: IsAdminInstallMode

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppVerName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppVerName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppVerName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
    ValueName: "FeigeCraft"; ValueData: """{app}\{#MyAppExeName}"" --background"; Tasks: autostart; \
    Flags: uninsdeletevalue

[Run]
; 按程序放行最直观：该程序的 TCP/UDP 入站全部放行，用户以后改了端口也不用再动防火墙。
; 再补端口规则，覆盖「程序被移到别处」的情况。
Filename: "{sys}\netsh.exe"; \
    Parameters: "advfirewall firewall add rule name=""FeigeCraft"" dir=in action=allow program=""{app}\{#MyAppExeName}"" enable=yes profile=private,domain"; \
    Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; \
    Parameters: "advfirewall firewall add rule name=""FeigeCraft (53317)"" dir=in action=allow protocol=TCP localport=53317 profile=private,domain"; \
    Flags: runhidden; Tasks: firewall
Filename: "{sys}\netsh.exe"; \
    Parameters: "advfirewall firewall add rule name=""FeigeCraft (53317 UDP)"" dir=in action=allow protocol=UDP localport=53317 profile=private,domain"; \
    Flags: runhidden; Tasks: firewall
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchApp}"; \
    Flags: nowait postinstall skipifsilent

[UninstallRun]
; 规则不存在时 netsh 返回非零，无害，隐藏输出即可。
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""FeigeCraft"""; \
    Flags: runhidden; RunOnceId: "DelFwProgram"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""FeigeCraft (53317)"""; \
    Flags: runhidden; RunOnceId: "DelFwTcp"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""FeigeCraft (53317 UDP)"""; \
    Flags: runhidden; RunOnceId: "DelFwUdp"

; 不写 [UninstallDelete] 强删整个 {app}：万一用户把程序装在自己的目录里、顺手放了别的东西，
; 卸载时整目录删掉就成事故了。Inno 默认只删自己安装的文件，空目录会自动清理，这就够了。
