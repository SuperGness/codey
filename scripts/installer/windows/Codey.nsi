Unicode true

!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.1.0"
!endif

!ifndef PROJECT_ROOT
  !define PROJECT_ROOT "..\..\.."
!endif
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Codey"

Name "Codey"
OutFile "${PROJECT_ROOT}\dist\windows\Codey-${VERSION}-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Codey"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
RequestExecutionLevel user
SetCompressor /SOLID lzma
Icon "${PROJECT_ROOT}\backend\icons\Codey.ico"

!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Section "Codey" SEC_CODEY
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "/oname=Codey.exe" "${PROJECT_ROOT}\target\release\codey.exe"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Codey"
  CreateShortcut "$SMPROGRAMS\Codey\Codey.lnk" "$INSTDIR\Codey.exe" "" "$INSTDIR\Codey.exe" 0
  CreateShortcut "$SMPROGRAMS\Codey\Uninstall Codey.lnk" "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "Codey"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\Codey.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Desktop shortcut" SEC_DESKTOP
  CreateShortcut "$DESKTOP\Codey.lnk" "$INSTDIR\Codey.exe" "" "$INSTDIR\Codey.exe" 0
SectionEnd

Section "Uninstall"
  Delete "$DESKTOP\Codey.lnk"
  Delete "$SMPROGRAMS\Codey\Codey.lnk"
  Delete "$SMPROGRAMS\Codey\Uninstall Codey.lnk"
  RMDir "$SMPROGRAMS\Codey"
  Delete "$INSTDIR\Codey.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
