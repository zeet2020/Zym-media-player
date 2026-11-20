; Inno Setup script. Built by .github/workflows/release.yml:
;   ISCC /DAppVersion=0.1.0 /DSrcDir=<dir with zym-media-player.exe + libmpv-2.dll> /DOutDir=<out> zym-media-player.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6C1E7A52-9B0D-4C5E-8E43-3F2B7D9A1C40}
AppName=Zym Media Player
AppVersion={#AppVersion}
AppPublisher=Zym Media Player
DefaultDirName={autopf}\Zym Media Player
DefaultGroupName=Zym Media Player
SetupIconFile=zym-media-player.ico
UninstallDisplayIcon={app}\zym-media-player.ico
OutputDir={#OutDir}
OutputBaseFilename=zym-media-player-{#AppVersion}-windows-x64-setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\LICENSE
; Refresh Explorer's file-type cache after (un)registering associations.
ChangesAssociations=yes

[Files]
Source: "{#SrcDir}\zym-media-player.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "zym-media-player.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\sources\*"; DestDir: "{app}\sources"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#SrcDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\THIRD-PARTY-LICENSES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\LIBMPV-NOTICE.txt"; DestDir: "{app}"; Flags: ignoreversion

; Upgrading from 0.1.0, which shipped as zym-player.exe: remove the old binary.
[InstallDelete]
Type: files; Name: "{app}\zym-player.exe"
Type: files; Name: "{app}\zym-player.ico"

[Icons]
Name: "{group}\Zym Media Player"; Filename: "{app}\zym-media-player.exe"; IconFilename: "{app}\zym-media-player.ico"
Name: "{group}\Uninstall Zym Media Player"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Zym Media Player"; Filename: "{app}\zym-media-player.exe"; IconFilename: "{app}\zym-media-player.ico"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Run]
Filename: "{app}\zym-media-player.exe"; Description: "{cm:LaunchProgram,Zym Media Player}"; Flags: nowait postinstall skipifsilent

; Register as a media player so Windows lists Zym under Settings > Default apps / "Open with".
; (Windows 10/11 don't let installers silently take over defaults; the user picks Zym there.)
[Registry]
; Leftovers of 0.1.0's names (zym-player.exe, "Zym Player").
Root: HKA; Subkey: "Software\Classes\Applications\zym-player.exe"; Flags: deletekey dontcreatekey
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: none; ValueName: "Zym Player"; Flags: deletevalue dontcreatekey
Root: HKA; Subkey: "Software\Classes\ZymPlayer.Media"; ValueType: string; ValueName: ""; ValueData: "Media file"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\ZymPlayer.Media\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\zym-media-player.ico"
Root: HKA; Subkey: "Software\Classes\ZymPlayer.Media\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\zym-media-player.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Applications\zym-media-player.exe\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\zym-media-player.exe"" ""%1"""; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Zym Media Player"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Lightweight media player built on mpv"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Zym Media Player"; ValueData: "Software\ZymPlayer\Capabilities"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mp4"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mp4\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".m4v"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.m4v\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mkv"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mkv\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".webm"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.webm\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".avi"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.avi\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mov"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mov\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".wmv"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.wmv\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".flv"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.flv\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mpg"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mpg\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mpeg"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mpeg\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ts"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.ts\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".m2ts"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.m2ts\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".3gp"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.3gp\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ogv"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.ogv\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".mp3"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.mp3\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".flac"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.flac\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".m4a"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.m4a\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".aac"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.aac\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".ogg"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.ogg\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".opus"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.opus\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".wav"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.wav\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".wma"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.wma\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".m3u"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.m3u\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\ZymPlayer\Capabilities\FileAssociations"; ValueType: string; ValueName: ".m3u8"; ValueData: "ZymPlayer.Media"
Root: HKA; Subkey: "Software\Classes\.m3u8\OpenWithProgids"; ValueType: string; ValueName: "ZymPlayer.Media"; ValueData: ""; Flags: uninsdeletevalue
