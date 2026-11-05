; bb1 for Windows installer. Build with scripts/installer.sh (Mac) or the
; Windows workflow:  makensis -DVERSION=0.1.0 -DBUILD=45 -DEXE=path\to\bb1.exe -DOUT=bb1-setup.exe installer/bb1.nsi
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

; Ask a running copy to quit (it quits at once), then make sure it's gone,
; so bb1.exe can be replaced.
!macro CloseRunning
  nsExec::Exec 'taskkill /im ${APP}.exe'
  Pop $0
  Sleep 1000
  nsExec::Exec 'taskkill /f /im ${APP}.exe'
  Pop $0
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
  File "/oname=${APP}.exe" "${EXE}"
  WriteUninstaller "$INSTDIR\Uninstall ${APP}.exe"

  SetShellVarContext all
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${APP}.exe"

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

  SetShellVarContext all
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  Delete "$INSTDIR\${APP}.exe"
  Delete "$INSTDIR\Uninstall ${APP}.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "${UNINST_KEY}"
  ; Pairings and settings (%APPDATA%\bb1) and logs stay, like on the Mac.
SectionEnd
