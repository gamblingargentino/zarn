#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef BinDir
  #define BinDir "..\target\dist\release"
#endif
#ifndef OutDir
  #define OutDir "..\target\dist\installer"
#endif

[Setup]
AppId={{6B0C2E3A-4D1F-4C5B-9E57-5A2D7F1B8C41}
AppName=zarn
AppVersion={#AppVersion}
AppPublisher=zarn
DefaultDirName={autopf}\zarn
DefaultGroupName=zarn
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\assets\icon\zarn.ico
UninstallDisplayIcon={app}\zarn.ico
OutputDir={#OutDir}
OutputBaseFilename=zarn-setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes

[Tasks]
Name: "addtopath"; Description: "Add zarn to PATH so you can type zarn in any terminal"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "{#BinDir}\zarn.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\icon\zarn.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\zarn"; Filename: "{app}\zarn.exe"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\zarn.ico"
Name: "{autodesktop}\zarn"; Filename: "{app}\zarn.exe"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\zarn.ico"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "{code:EnvironmentKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\zarn.exe"; Parameters: "setup"; Description: "Set up your API keys in the browser now"; WorkingDir: "{%USERPROFILE}"; Flags: postinstall nowait skipifsilent unchecked

[Code]
function EnvironmentKey(Param: String): String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function NeedsPath(Dir: String): Boolean;
var
  Paths: String;
  Root: Integer;
begin
  if IsAdminInstallMode then
    Root := HKEY_LOCAL_MACHINE
  else
    Root := HKEY_CURRENT_USER;
  if not RegQueryStringValue(Root, EnvironmentKey(''), 'Path', Paths) then
  begin
    Result := True;
    Exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Dir: String;
  Root, At: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  if IsAdminInstallMode then
    Root := HKEY_LOCAL_MACHINE
  else
    Root := HKEY_CURRENT_USER;
  if not RegQueryStringValue(Root, EnvironmentKey(''), 'Path', Paths) then
    Exit;
  Dir := ExpandConstant('{app}');
  At := Pos(';' + Uppercase(Dir), Uppercase(Paths));
  if At > 0 then
  begin
    Delete(Paths, At, Length(Dir) + 1);
    RegWriteExpandStringValue(Root, EnvironmentKey(''), 'Path', Paths);
  end;
end;
