; ScreenBridge for Windows installer. Build with scripts\build-windows.ps1 (passes /DAppVersion and /DSourceExe).
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\windows\target\release\screenbridge.exe"
#endif

[Setup]
AppId={{7E1C5A04-3B1F-4E8A-9C52-5B7C0D9A2F61}
AppName=ScreenBridge
AppVersion={#AppVersion}
AppPublisher=Matías Vivanco
AppPublisherURL=https://github.com/matiasvivancob/screenbridge
DefaultDirName={autopf}\ScreenBridge
DefaultGroupName=ScreenBridge
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\screenbridge.exe
UninstallDisplayName=ScreenBridge
OutputDir=..\dist
OutputBaseFilename=ScreenBridge-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Admin only to add the firewall rule; the app itself runs as the normal user.
; /CURRENTUSER on the command line installs without admin (no firewall rule); used for automated testing.
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline
; The autostart entry is written for the user who ran setup, which is the intent.
UsedUserAreasWarning=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force
MinVersion=10.0.19041

[Tasks]
Name: "autostart"; Description: "Start ScreenBridge when I sign in to Windows"; GroupDescription: "Options:"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{group}\ScreenBridge"; Filename: "{app}\screenbridge.exe"
Name: "{group}\ScreenBridge Settings"; Filename: "{app}\screenbridge.exe"; Parameters: "--settings"
Name: "{group}\Uninstall ScreenBridge"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ScreenBridge"; ValueData: """{app}\screenbridge.exe"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
; Mac discovery (UDP 47801) and the video streams (UDP 47810+) arrive unsolicited, so Windows Firewall must allow them.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""ScreenBridge"" dir=in action=allow program=""{app}\screenbridge.exe"" enable=yes profile=private,domain protocol=UDP"; Flags: runhidden; StatusMsg: "Allowing ScreenBridge through Windows Firewall (private networks)..."
Filename: "{app}\screenbridge.exe"; Description: "Start ScreenBridge now"; Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""ScreenBridge"""; Flags: runhidden; RunOnceId: "RemoveFirewallRule"

[Code]
procedure StopRunningApp;
var
  code: Integer;
begin
  // The tray app has no window the installer can close politely; ending it makes the Mac drop its virtual displays.
  // Only the installed copy: a portable copy running elsewhere is left alone.
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -Command "Get-Process -Name screenbridge -ErrorAction SilentlyContinue | Where-Object Path -eq ''' +
      ExpandConstant('{app}\screenbridge.exe') + ''' | Stop-Process -Force"',
    '', SW_HIDE, ewWaitUntilTerminated, code);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  code: Integer;
begin
  StopRunningApp;
  // A previous version may have left a stale rule behind; remove it so reinstalling never duplicates it.
  Exec(ExpandConstant('{sys}\netsh.exe'), 'advfirewall firewall delete rule name="ScreenBridge"', '', SW_HIDE, ewWaitUntilTerminated, code);
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopRunningApp;
  Result := True;
end;
