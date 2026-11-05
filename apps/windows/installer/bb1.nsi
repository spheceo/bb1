; bb1 for Windows installer. Build with scripts/installer.sh (Mac) or the
; Windows workflow:  makensis -DVERSION=0.1.0 -DBUILD=45 -DEXE=path\to\bb1.exe -DSDL=path\to\SDL3.dll -DOUT=bb1-setup.exe installer/bb1.nsi
;
; SDL3.dll (game controllers, zlib licence) comes from SDL's official Windows release zip; both
; build scripts download it and check its SHA-256.
;
; Installs to Program Files (only admins can write there, so excluding the
; folder from Defender doesn't give other programs a hiding place), adds the
; Defender exclusion before bb1.exe is written so Defender can't remove it on
; the way in, and registers an uninstaller that undoes both.

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef BUILD
  !define BUILD "0"
!endif
!ifndef EXE
  !error "Pass -DEXE=<path to bb1.exe>"
!endif
!ifndef SDL
  !error "Pass -DSDL=<path to SDL3.dll>"
!endif
!ifndef OUT
  !define OUT "bb1-setup.exe"
!endif

!define APP "bb1"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"
!define PS '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command'

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel admin

Name "${APP}"
OutFile "${OUT}"
InstallDir "$PROGRAMFILES64\${APP}"
BrandingText "${APP} ${VERSION} (build ${BUILD})"

VIProductVersion "${VERSION}.${BUILD}"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} for Windows installer"
VIAddVersionKey "FileVersion" "${VERSION}.${BUILD}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${APP}"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 the ${APP} authors. GPL-3.0-or-later."

!include MUI2.nsh
!include LogicLib.nsh
!include x64.nsh
!include FileFunc.nsh

!define MUI_ICON "..\res\bb1.ico"
!define MUI_UNICON "..\res\bb1.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Open ${APP}"
!define MUI_FINISHPAGE_RUN_FUNCTION RunUnelevated

!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "${APP} needs 64-bit Windows 10 or 11." /SD IDOK
    Abort
  ${EndIf}
  ; 64-bit PowerShell (it has the Defender commands) and 64-bit registry view.
  ${DisableX64FSRedirection}
  SetRegView 64
FunctionEnd

Function un.onInit
  ${DisableX64FSRedirection}
  SetRegView 64
FunctionEnd

; The installer runs as admin; opening bb1 from it directly would too.
; Explorer opens it as the signed-in user instead.
Function RunUnelevated
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\${APP}.exe"'
FunctionEnd

; bb1 updating itself runs this quietly (/S): open the new bb1 again when it's done. Not with
; /NORUN: the update task runs it as SYSTEM, and its helper opens bb1 on the desktop instead.
Function .onInstSuccess
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/NORUN" $R1
  ${If} ${Silent}
  ${AndIf} ${Errors}
    Call RunUnelevated
  ${EndIf}
FunctionEnd

; Ask a running copy to quit (it quits at once), then make sure it's gone,
; so bb1.exe can be replaced.
; Not in session 0: that's the update task's own bb1 (as SYSTEM), which is running this installer.
!macro CloseRunning
  nsExec::Exec 'taskkill /fi "IMAGENAME eq ${APP}.exe" /fi "SESSION ne 0"'
  Pop $0
  Sleep 1000
  nsExec::Exec 'taskkill /f /fi "IMAGENAME eq ${APP}.exe" /fi "SESSION ne 0"'
  Pop $0
!macroend

; The "bb1 Update" task: runs `bb1.exe --apply-update` as SYSTEM when any signed-in user starts it,
; so bb1 updates without an admin prompt (needed when another computer asks it to). It only installs
; a bb1 release from GitHub whose installer matches the release's checksums (win/selfupdate.rs).
!macro RegisterUpdateTask
  InitPluginsDir
  FileOpen $0 "$PLUGINSDIR\update-task.xml" w
  FileWriteWord $0 0xFEFF
  FileWriteUTF16LE $0 '<?xml version="1.0" encoding="UTF-16"?>$\r$\n'
  FileWriteUTF16LE $0 '<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">$\r$\n'
  FileWriteUTF16LE $0 '  <RegistrationInfo><Description>Installs bb1 updates without an admin prompt.</Description>'
  FileWriteUTF16LE $0 '<SecurityDescriptor>D:(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;AU)</SecurityDescriptor></RegistrationInfo>$\r$\n'
  FileWriteUTF16LE $0 '  <Principals><Principal id="Author"><UserId>S-1-5-18</UserId><RunLevel>HighestAvailable</RunLevel></Principal></Principals>$\r$\n'
  FileWriteUTF16LE $0 '  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>'
  FileWriteUTF16LE $0 '<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT15M</ExecutionTimeLimit>'
  FileWriteUTF16LE $0 '<AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled></Settings>$\r$\n'
  FileWriteUTF16LE $0 '  <Actions Context="Author"><Exec><Command>$INSTDIR\${APP}.exe</Command><Arguments>--apply-update</Arguments></Exec></Actions>$\r$\n'
  FileWriteUTF16LE $0 '</Task>$\r$\n'
  FileClose $0
  nsExec::ExecToLog 'schtasks /create /tn "${APP} Update" /xml "$PLUGINSDIR\update-task.xml" /f'
  Pop $0
  ; Where bb1 asks for an update and reads how it went (users write, the task reads).
  ; ($APPDATA is %ProgramData% with the shell context set to all users.)
  SetShellVarContext all
  CreateDirectory "$APPDATA\${APP}"
  nsExec::Exec 'icacls "$APPDATA\${APP}" /grant *S-1-5-32-545:(OI)(CI)M'
  Pop $0
  SetShellVarContext current
!macroend

Section "Tell Microsoft Defender to leave ${APP} alone" SecDefender
  DetailPrint "Adding a Microsoft Defender exclusion for $INSTDIR"
  nsExec::ExecToLog `${PS} "Add-MpPreference -ExclusionPath '$INSTDIR' -ErrorAction Stop"`
  Pop $0
  ${If} $0 == 0
    WriteRegDWORD HKLM "${UNINST_KEY}" "DefenderExclusion" 1
  ${Else}
    DetailPrint "Couldn't add the Defender exclusion (result $0)"
    ; (continued lines start at column 0: NSIS keeps their leading spaces)
    MessageBox MB_ICONEXCLAMATION|MB_OK "Windows didn't let the installer add the Microsoft Defender exclusion. \
This happens when another antivirus is in charge, or the PC is managed by work or school.$\r$\n$\r$\n\
If ${APP} goes missing, add it yourself: Windows Security > Virus & threat protection > \
Manage settings > Exclusions > Add an exclusion > Folder, and pick:$\r$\n$\r$\n$INSTDIR" /SD IDOK
  ${EndIf}
SectionEnd

Section "-${APP}" SecApp
  SectionIn RO
  !insertmacro CloseRunning
  SetOutPath "$INSTDIR"
  ; A bb1 that's still closing (or a virus scan reading the file) would make writing bb1.exe fail
  ; ("Abort, Retry, Ignore"). Windows lets a file in use be renamed, so move it aside first.
  Delete "$INSTDIR\${APP}.old.exe"
  IfFileExists "$INSTDIR\${APP}.exe" 0 +3
    Rename "$INSTDIR\${APP}.exe" "$INSTDIR\${APP}.old.exe"
    Delete /REBOOTOK "$INSTDIR\${APP}.old.exe"
  File "/oname=${APP}.exe" "${EXE}"
  ; bb1 loads SDL3.dll only from its own folder, for game controllers. Moved aside like bb1.exe.
  Delete "$INSTDIR\SDL3.old.dll"
  IfFileExists "$INSTDIR\SDL3.dll" 0 +3
    Rename "$INSTDIR\SDL3.dll" "$INSTDIR\SDL3.old.dll"
    Delete /REBOOTOK "$INSTDIR\SDL3.old.dll"
  File "/oname=SDL3.dll" "${SDL}"
  WriteUninstaller "$INSTDIR\Uninstall ${APP}.exe"

  !insertmacro RegisterUpdateTask

  ; Let bb1 hear Macs on the home network (Bonjour answers) and their video, on any network type:
  ; Windows often calls a home network "Public". bb1 itself doesn't accept connections.
  nsExec::Exec 'netsh advfirewall firewall delete rule name="${APP}"'
  Pop $0
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="${APP}" dir=in action=allow program="$INSTDIR\${APP}.exe" enable=yes profile=any'
  Pop $0

  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APP}.exe"
  ; (An early nightly had a separate "bb1 Host" shortcut; sharing is in bb1's menu by the clock now.)
  Delete "$SMPROGRAMS\${APP} Host.lnk"

  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION} (build ${BUILD})"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP}.exe"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall ${APP}.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall ${APP}.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" 8192
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  SetShellVarContext all
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${APP}.exe"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDefender} "Adds this folder to Microsoft Defender's exclusions so it doesn't delete ${APP}. Removed again when you uninstall."
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "Puts a ${APP} shortcut on the desktop for everyone on this PC."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

Section "Uninstall"
  !insertmacro CloseRunning
  ReadRegDWORD $1 HKLM "${UNINST_KEY}" "DefenderExclusion"
  ${If} $1 == 1
    nsExec::ExecToLog `${PS} "Remove-MpPreference -ExclusionPath '$INSTDIR' -ErrorAction SilentlyContinue"`
    Pop $0
  ${EndIf}

  nsExec::Exec 'netsh advfirewall firewall delete rule name="${APP}"'
  Pop $0
  nsExec::Exec 'schtasks /delete /tn "${APP} Update" /f'
  Pop $0

  SetShellVarContext all
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$SMPROGRAMS\${APP} Host.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$INSTDIR\${APP}.exe"
  Delete "$INSTDIR\${APP}.old.exe"
  Delete "$INSTDIR\SDL3.dll"
  Delete "$INSTDIR\SDL3.old.dll"
  Delete "$INSTDIR\Uninstall ${APP}.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINST_KEY}"
  ; Pairings and settings (%APPDATA%\bb1) and logs stay, like on the Mac.
SectionEnd
