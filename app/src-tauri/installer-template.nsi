Unicode true
ManifestDPIAware true
; Add in `dpiAwareness` `PerMonitorV2` to manifest for Windows 10 1607+ (note this should not affect lower versions since they should be able to ignore this and pick up `dpiAware` `true` set by `ManifestDPIAware true`)
; Currently undocumented on NSIS's website but is in the Docs folder of source tree, see
; https://github.com/kichik/nsis/blob/5fc0b87b819a9eec006df4967d08e522ddd651c9/Docs/src/attributes.but#L286-L300
; https://github.com/tauri-apps/tauri/pull/10106
ManifestDPIAwareness PerMonitorV2

!if "{{compression}}" == "none"
  SetCompress off
!else
  ; Set the compression algorithm. We default to LZMA.
  SetCompressor /SOLID "{{compression}}"
!endif

; Keep above !include to stay ahead of any plugin command
; see https://github.com/tauri-apps/tauri/pull/15422#discussion_r3289239624
{{#if signed_plugins_path}}
!addplugindir "{{signed_plugins_path}}"
{{/if}}

!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh
!include "utils.nsh"
!include "FileAssociation.nsh"
!include "Win\COM.nsh"
!include "Win\Propkey.nsh"
!include "StrFunc.nsh"
${StrCase}
${StrLoc}

{{#if installer_hooks}}
!include "{{installer_hooks}}"
{{/if}}

!define WEBVIEW2APPGUID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"

!define MANUFACTURER "{{manufacturer}}"
!define PRODUCTNAME "{{product_name}}"
!define VERSION "{{version}}"
!define VERSIONWITHBUILD "{{version_with_build}}"
!define HOMEPAGE "{{homepage}}"
!define INSTALLMODE "{{install_mode}}"
!define LICENSE "{{license}}"
!define INSTALLERICON "{{installer_icon}}"
!define SIDEBARIMAGE "{{sidebar_image}}"
!define HEADERIMAGE "{{header_image}}"
!define UNINSTALLERICON "{{uninstaller_icon}}"
!define UNINSTALLERHEADERIMAGE "{{uninstaller_header_image}}"
!define MAINBINARYNAME "{{main_binary_name}}"
!define MAINBINARYSRCPATH "{{main_binary_path}}"
!define BUNDLEID "{{bundle_id}}"
!define COPYRIGHT "{{copyright}}"
!define OUTFILE "{{out_file}}"
!define ARCH "{{arch}}"
!define ADDITIONALPLUGINSPATH "{{additional_plugins_path}}"
!define ALLOWDOWNGRADES "{{allow_downgrades}}"
!define DISPLAYLANGUAGESELECTOR "{{display_language_selector}}"
!define INSTALLWEBVIEW2MODE "{{install_webview2_mode}}"
!define WEBVIEW2INSTALLERARGS "{{webview2_installer_args}}"
!define WEBVIEW2BOOTSTRAPPERPATH "{{webview2_bootstrapper_path}}"
!define WEBVIEW2INSTALLERPATH "{{webview2_installer_path}}"
!define MINIMUMWEBVIEW2VERSION "{{minimum_webview2_version}}"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCTNAME}"
!define MANUKEY "Software\${MANUFACTURER}"
!define MANUPRODUCTKEY "${MANUKEY}\${PRODUCTNAME}"
!define UNINSTALLERSIGNCOMMAND "{{uninstaller_sign_cmd}}"
!define ESTIMATEDSIZE "{{estimated_size}}"
!define STARTMENUFOLDER "{{start_menu_folder}}"

Var PassiveMode
Var UpdateMode
Var NoShortcutMode
Var WixMode
Var OldMainBinaryName

; Mirryl: moves a file that a running process may have mapped out of the way,
; so the `File` that follows can write a fresh one. Windows lets an executable
; or DLL in use be RENAMED, never overwritten. A `.old` left by an earlier
; update is deleted first (free by now: whatever held it died with that
; update); if it is somehow still held, Rename fails quietly and File gets the
; same chance it always had. The `.old` files are removed at the end of the
; install and by the uninstaller.
!macro MirrylMoveAside path
  ClearErrors
  Delete "${path}.old"
  IfFileExists "${path}" 0 +2
    Rename "${path}" "${path}.old"
  ClearErrors
!macroend

Name "${PRODUCTNAME}"
BrandingText "${COPYRIGHT}"
OutFile "${OUTFILE}"

; We don't actually use this value as default install path,
; it's just for nsis to append the product name folder in the directory selector
; https://nsis.sourceforge.io/Reference/InstallDir
!define PLACEHOLDER_INSTALL_DIR "placeholder\${PRODUCTNAME}"
InstallDir "${PLACEHOLDER_INSTALL_DIR}"

VIProductVersion "${VERSIONWITHBUILD}"
VIAddVersionKey "ProductName" "${PRODUCTNAME}"
VIAddVersionKey "FileDescription" "${PRODUCTNAME}"
VIAddVersionKey "LegalCopyright" "${COPYRIGHT}"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

# additional plugins
!addplugindir "${ADDITIONALPLUGINSPATH}"

; Uninstaller signing command
!if "${UNINSTALLERSIGNCOMMAND}" != ""
  !uninstfinalize '${UNINSTALLERSIGNCOMMAND}'
!endif

; Handle install mode, `perUser`, `perMachine` or `both`
!if "${INSTALLMODE}" == "perMachine"
  RequestExecutionLevel admin
!endif

!if "${INSTALLMODE}" == "currentUser"
  RequestExecutionLevel user
!endif

!if "${INSTALLMODE}" == "both"
  !define MULTIUSER_MUI
  !define MULTIUSER_INSTALLMODE_INSTDIR "${PRODUCTNAME}"
  !define MULTIUSER_INSTALLMODE_COMMANDLINE
  !if "${ARCH}" == "x64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !else if "${ARCH}" == "arm64"
    !define MULTIUSER_USE_PROGRAMFILES64
  !endif
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_KEY "${UNINSTKEY}"
  !define MULTIUSER_INSTALLMODE_DEFAULT_REGISTRY_VALUENAME "CurrentUser"
  !define MULTIUSER_INSTALLMODEPAGE_SHOWUSERNAME
  !define MULTIUSER_INSTALLMODE_FUNCTION RestorePreviousInstallLocation
  !define MULTIUSER_EXECUTIONLEVEL Highest
  !include MultiUser.nsh
!endif

; Installer icon
!if "${INSTALLERICON}" != ""
  !define MUI_ICON "${INSTALLERICON}"
!endif

; Installer sidebar image
!if "${SIDEBARIMAGE}" != ""
  !define MUI_WELCOMEFINISHPAGE_BITMAP "${SIDEBARIMAGE}"
!endif

; Enable header images for installer and uninstaller pages when either image is configured.
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!else if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE
!endif

; Installer header image
!if "${HEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_BITMAP "${HEADERIMAGE}"
!endif

; Uninstaller header image
!if "${UNINSTALLERHEADERIMAGE}" != ""
  !define MUI_HEADERIMAGE_UNBITMAP "${UNINSTALLERHEADERIMAGE}"
!endif

; Uninstaller icon
!if "${UNINSTALLERICON}" != ""
  !define MUI_UNICON "${UNINSTALLERICON}"
!endif

; Define registry key to store installer language
!define MUI_LANGDLL_REGISTRY_ROOT "HKCU"
!define MUI_LANGDLL_REGISTRY_KEY "${MANUPRODUCTKEY}"
!define MUI_LANGDLL_REGISTRY_VALUENAME "Installer Language"

; ===================================================================
; DARK THEME. Everything in this block comes from reading
; Contrib\Modern UI 2\ of this same NSIS, not from assuming a define
; exists.
;
; MUI_BGCOLOR reaches three areas and only three: the header strip of
; the inner pages, the welcome page and the finish page. The body of the
; directory page is native Windows controls and NSIS exposes no colour
; for them - it stays light on purpose, it is not an oversight. Same for
; the Back/Next/Cancel buttons.
;
; 070914 is the app's dark background colour, and it matches the top edge
; of installer-sidebar.bmp, so the side panel and the page background join
; without a visible seam.
!define MUI_BGCOLOR "070914"
!define MUI_TEXTCOLOR "DBE7F5"

; Install log: cyan on the same background. This is InstallColors,
; which takes "foreground background" as TWO tokens - that is why the
; value carries both, separated by a space (see Pages\InstallFiles.nsh).
!define MUI_INSTFILESPAGE_COLORS "8CE0F2 070914"

; MUI_LICENSEPAGE_BGCOLOR is NOT set on purpose: LicenseBkColor paints the
; control's background, but License.nsh never calls SetCtlColors, so
; the text stays black. Dark background + black text = unreadable.
; Darkening that box would mean converting the EULA to RTF with its own
; text colour.
; ===================================================================

; Installer pages, must be ordered as they appear
; 1. Welcome Page
; No promise about how long the install takes: on a slow PC that promise
; breaks itself and is the first thing the user remembers. There is no
; obligation to state it; disk space, which is what people do expect,
; NSIS already shows on the directory page.
!define MUI_WELCOMEPAGE_TITLE "Your Android, right here"
!define MUI_WELCOMEPAGE_TEXT "Mirryl mirrors and controls your Android phone from this computer, over your own WiFi. Pairing is done by scanning a QR code - no cable required, then or ever. USB stays available if you ever want it.$\r$\n$\r$\nMirryl installs into your own user folder, so Windows will not ask you for an administrator password.$\r$\n$\r$\nClick Next to begin."
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_WELCOME

; 2. License Page (if defined)
!if "${LICENSE}" != ""
  !define MUI_PAGE_HEADER_TEXT "The license"
  !define MUI_PAGE_HEADER_SUBTEXT "Short version: it is yours to use on the computers you own."
  !define MUI_LICENSEPAGE_TEXT_TOP "Scroll to the end to read it all, then tick the box below."
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  ; Mirryl customization: stock MUI_PAGE_LICENSE with no style define shows
  ; a single "I Agree" button that skips straight past Next - nothing forces
  ; the user to actually notice they're agreeing to anything. This switches
  ; it to the standard checkbox variant: Next stays disabled until the user
  ; ticks "I accept the terms of the License Agreement".
  !define MUI_LICENSEPAGE_CHECKBOX
  !insertmacro MUI_PAGE_LICENSE "${LICENSE}"
!endif

; 3. Install mode (if it is set to `both`)
!if "${INSTALLMODE}" == "both"
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !insertmacro MULTIUSER_PAGE_INSTALLMODE
!endif

; 4. Custom page to ask user if he wants to reinstall/uninstall
;    only if a previous installation was detected
Var ReinstallPageCheck
Page custom PageReinstall PageLeaveReinstall
Function PageReinstall
  ; Uninstall previous WiX installation if exists.
  ;
  ; A WiX installer stores the installation info in registry
  ; using a UUID and so we have to loop through all keys under
  ; `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall`
  ; and check if `DisplayName` and `Publisher` keys match ${PRODUCTNAME} and ${MANUFACTURER}
  ;
  ; This has a potential issue that there maybe another installation that matches
  ; our ${PRODUCTNAME} and ${MANUFACTURER} but wasn't installed by our WiX installer,
  ; however, this should be fine since the user will have to confirm the uninstallation
  ; and they can chose to abort it if doesn't make sense.
  StrCpy $0 0
  wix_loop:
    EnumRegKey $1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" $0
    StrCmp $1 "" wix_loop_done ; Exit loop if there is no more keys to loop on
    IntOp $0 $0 + 1
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "DisplayName"
    ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "Publisher"
    StrCmp "$R0$R1" "${PRODUCTNAME}${MANUFACTURER}" 0 wix_loop
    ReadRegStr $R0 HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1" "UninstallString"
    ${StrCase} $R1 $R0 "L"
    ${StrLoc} $R0 $R1 "msiexec" ">"
    StrCmp $R0 0 0 wix_loop_done
    StrCpy $WixMode 1
    StrCpy $R6 "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\$1"
    Goto compare_version
  wix_loop_done:

  ; Check if there is an existing installation, if not, abort the reinstall page
  ReadRegStr $R0 SHCTX "${UNINSTKEY}" ""
  ReadRegStr $R1 SHCTX "${UNINSTKEY}" "UninstallString"
  ${IfThen} "$R0$R1" == "" ${|} Abort ${|}

  ; Compare this installar version with the existing installation
  ; and modify the messages presented to the user accordingly
  compare_version:
  StrCpy $R4 "$(older)"
  ${If} $WixMode = 1
    ReadRegStr $R0 HKLM "$R6" "DisplayVersion"
  ${Else}
    ReadRegStr $R0 SHCTX "${UNINSTKEY}" "DisplayVersion"
  ${EndIf}
  ${IfThen} $R0 == "" ${|} StrCpy $R4 "$(unknown)" ${|}

  nsis_tauri_utils::SemverCompare "${VERSION}" $R0
  Pop $R0
  ; Reinstalling the same version
  ${If} $R0 = 0
    StrCpy $R1 "$(alreadyInstalledLong)"
    StrCpy $R2 "$(addOrReinstall)"
    StrCpy $R3 "$(uninstallApp)"
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(chooseMaintenanceOption)"
  ; Upgrading
  ${ElseIf} $R0 = 1
    StrCpy $R1 "$(olderOrUnknownVersionInstalled)"
    StrCpy $R2 "$(uninstallBeforeInstalling)"
    StrCpy $R3 "$(dontUninstall)"
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(choowHowToInstall)"
  ; Downgrading
  ${ElseIf} $R0 = -1
    StrCpy $R1 "$(newerVersionInstalled)"
    StrCpy $R2 "$(uninstallBeforeInstalling)"
    !if "${ALLOWDOWNGRADES}" == "true"
      StrCpy $R3 "$(dontUninstall)"
    !else
      StrCpy $R3 "$(dontUninstallDowngrade)"
    !endif
    !insertmacro MUI_HEADER_TEXT "$(alreadyInstalled)" "$(choowHowToInstall)"
  ${Else}
    Abort
  ${EndIf}

  ; Skip showing the page if passive
  ;
  ; Note that we don't call this earlier at the begining
  ; of this function because we need to populate some variables
  ; related to current installed version if detected and whether
  ; we are downgrading or not.
  ${If} $PassiveMode = 1
    Call PageLeaveReinstall
  ${Else}
    nsDialogs::Create 1018
    Pop $R4
    ${IfThen} $(^RTL) = 1 ${|} nsDialogs::SetRTL $(^RTL) ${|}

    ${NSD_CreateLabel} 0 0 100% 24u $R1
    Pop $R1

    ${NSD_CreateRadioButton} 30u 50u -30u 8u $R2
    Pop $R2
    ${NSD_OnClick} $R2 PageReinstallUpdateSelection

    ${NSD_CreateRadioButton} 30u 70u -30u 8u $R3
    Pop $R3
    ; Disable this radio button if downgrading and downgrades are disabled
    !if "${ALLOWDOWNGRADES}" == "false"
      ${IfThen} $R0 = -1 ${|} EnableWindow $R3 0 ${|}
    !endif
    ${NSD_OnClick} $R3 PageReinstallUpdateSelection

    ; Check the first radio button if this the first time
    ; we enter this page or if the second button wasn't
    ; selected the last time we were on this page
    ${If} $ReinstallPageCheck <> 2
      SendMessage $R2 ${BM_SETCHECK} ${BST_CHECKED} 0
    ${Else}
      SendMessage $R3 ${BM_SETCHECK} ${BST_CHECKED} 0
    ${EndIf}

    ${NSD_SetFocus} $R2
    nsDialogs::Show
  ${EndIf}
FunctionEnd
Function PageReinstallUpdateSelection
  ${NSD_GetState} $R2 $R1
  ${If} $R1 == ${BST_CHECKED}
    StrCpy $ReinstallPageCheck 1
  ${Else}
    StrCpy $ReinstallPageCheck 2
  ${EndIf}
FunctionEnd
Function PageLeaveReinstall
  ${NSD_GetState} $R2 $R1

  ; If migrating from Wix, always uninstall
  ${If} $WixMode = 1
    Goto reinst_uninstall
  ${EndIf}

  ; In update mode, always proceeds without uninstalling
  ${If} $UpdateMode = 1
    Goto reinst_done
  ${EndIf}

  ; $R0 holds whether same(0)/upgrading(1)/downgrading(-1) version
  ; $R1 holds the radio buttons state:
  ;   1 => first choice was selected
  ;   0 => second choice was selected
  ${If} $R0 = 0 ; Same version, proceed
    ${If} $R1 = 1              ; User chose to add/reinstall
      Goto reinst_done
    ${Else}                    ; User chose to uninstall
      Goto reinst_uninstall
    ${EndIf}
  ${ElseIf} $R0 = 1 ; Upgrading
    ${If} $R1 = 1              ; User chose to uninstall
      Goto reinst_uninstall
    ${Else}
      Goto reinst_done         ; User chose NOT to uninstall
    ${EndIf}
  ${ElseIf} $R0 = -1 ; Downgrading
    ${If} $R1 = 1              ; User chose to uninstall
      Goto reinst_uninstall
    ${Else}
      Goto reinst_done         ; User chose NOT to uninstall
    ${EndIf}
  ${EndIf}

  reinst_uninstall:
    HideWindow
    ClearErrors

    ${If} $WixMode = 1
      ReadRegStr $R1 HKLM "$R6" "UninstallString"
      ExecWait '$R1' $0
    ${Else}
      ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
      ReadRegStr $R1 SHCTX "${UNINSTKEY}" "UninstallString"
      ${IfThen} $UpdateMode = 1 ${|} StrCpy $R1 "$R1 /UPDATE" ${|} ; append /UPDATE
      ${IfThen} $PassiveMode = 1 ${|} StrCpy $R1 "$R1 /P" ${|} ; append /P
      StrCpy $R1 "$R1 _?=$4" ; append uninstall directory
      ExecWait '$R1' $0
    ${EndIf}

    BringToFront

    ${IfThen} ${Errors} ${|} StrCpy $0 2 ${|} ; ExecWait failed, set fake exit code

    ${If} $0 <> 0
    ${OrIf} ${FileExists} "$INSTDIR\${MAINBINARYNAME}.exe"
      ; User cancelled wix uninstaller? return to select un/reinstall page
      ${If} $WixMode = 1
      ${AndIf} $0 = 1602
        Abort
      ${EndIf}

      ; User cancelled NSIS uninstaller? return to select un/reinstall page
      ${If} $0 = 1
        Abort
      ${EndIf}

      ; Other erros? show generic error message and return to select un/reinstall page
      MessageBox MB_ICONEXCLAMATION "$(unableToUninstall)"
      Abort
    ${EndIf}
  reinst_done:
FunctionEnd

; 5. Choose install directory page
!define MUI_PAGE_HEADER_TEXT "Where to install"
!define MUI_PAGE_HEADER_SUBTEXT "Mirryl installs into your own user folder."
!define MUI_DIRECTORYPAGE_TEXT_TOP "Mirryl installs into your user folder, so no administrator password is needed. Pick a different folder below if you prefer."
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
; Mirryl customization: clicking Next here is what actually starts copying
; files (there's no separate "are you sure" step after it) - relabel the
; button so that's obvious, then put it back to "Next >" once we're past
; this page (see MirrylResetNextButtonText below, hooked on the installation
; page) so Back/Cancel etc. on later pages aren't mislabeled.
!define MUI_PAGE_CUSTOMFUNCTION_SHOW MirrylShowInstallButtonText
!insertmacro MUI_PAGE_DIRECTORY

; 6. Start menu shortcut page
Var AppStartMenuFolder
!if "${STARTMENUFOLDER}" != ""
  !define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
  !define MUI_STARTMENUPAGE_DEFAULTFOLDER "${STARTMENUFOLDER}"
!else
  !define MUI_PAGE_CUSTOMFUNCTION_PRE Skip
!endif
!insertmacro MUI_PAGE_STARTMENU Application $AppStartMenuFolder

; 7. Installation page
!define MUI_PAGE_CUSTOMFUNCTION_SHOW MirrylResetNextButtonText
!insertmacro MUI_PAGE_INSTFILES

; 8. Finish page
;
; Don't auto jump to finish page after installation page,
; because the installation page has useful info that can be used debug any issues with the installer.
; The two checkboxes on this page came out with BLACK text on the dark
; background, practically invisible. It is not that MUI forgets to paint
; them: Finish.nsh DOES call SetCtlColors on them (lines 378 and 387), but
; that is not enough - it is a known NSIS bug (#443, cited in the comment at
; Finish.nsh:415): SetCtlColors does not change the text colour of a
; checkbox that has a visual style applied. MUI itself ships the remedy
; right there (SetWindowTheme with an empty theme, which puts the checkbox
; back to classic drawing, where SetCtlColors does rule) but runs it ONLY
; when Windows is in high-contrast mode. MUI_FORCECLASSICCONTROLS is its
; switch to run it always. Checked that it affects nothing else: in all of
; NSIS it is read only in Finish.nsh and in the old MUI's System.nsh, which
; this template does not use.
; Accepted side effect: the checkbox is drawn in the classic style (a flat
; little square) instead of the Windows 11 blue.
!define MUI_FORCECLASSICCONTROLS

!define MUI_FINISHPAGE_NOAUTOCLOSE
!define MUI_FINISHPAGE_TITLE "Mirryl is ready"
; The app's first screen is "Pair with the phone" and both of its paths (QR
; and 6-digit code) are wireless: no cable is needed, not even the first time
; (src\index.html).
!define MUI_FINISHPAGE_TEXT "Open Mirryl and it will show you a pairing code. On the phone, open Wireless debugging, choose Pair device with QR code, and point the camera at the screen."
; Use show readme button in the finish page as a button create a desktop shortcut
!define MUI_FINISHPAGE_SHOWREADME
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Put a shortcut on my desktop"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateOrUpdateDesktopShortcut
; Show run app after installation.
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Open Mirryl now"
!define MUI_FINISHPAGE_RUN_FUNCTION RunMainBinary
; A way on from the last page: the repository.
!define MUI_FINISHPAGE_LINK "Mirryl on GitHub"
!define MUI_FINISHPAGE_LINK_LOCATION "https://github.com/johan-socarras/Mirryl"
!define MUI_FINISHPAGE_LINK_COLOR "3FD8F0"
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipIfPassive
!insertmacro MUI_PAGE_FINISH

Function RunMainBinary
  nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" ""
FunctionEnd

; Mirryl customization: relabel the directory page's Next button to
; "Install" (control ID 1 in the wizard frame is Next/Back/Cancel's shared
; parent - GetDlgItem off $HWNDPARENT is the standard NSIS way to reach it),
; using MUI's own localized ^InstallBtn string rather than a hardcoded
; literal so it stays consistent with everything else MUI draws.
Function MirrylShowInstallButtonText
  GetDlgItem $0 $HWNDPARENT 1
  SendMessage $0 ${WM_SETTEXT} 0 "STR:$(^InstallBtn)"
FunctionEnd

; Puts the button back to plain "Next >" once we're on the installation
; page, so it's not still stuck reading "Install" while the user is
; deciding whether to hit Next to reach the Finish page afterward.
Function MirrylResetNextButtonText
  GetDlgItem $0 $HWNDPARENT 1
  SendMessage $0 ${WM_SETTEXT} 0 "STR:$(^NextBtn)"
FunctionEnd

; Uninstaller Pages
; 1. Confirm uninstall page
Var DeleteAppDataCheckbox
Var DeleteAppDataCheckboxState
!define /ifndef WS_EX_LAYOUTRTL         0x00400000
!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.ConfirmShow
Function un.ConfirmShow ; Add add a `Delete app data` check box
  ; $1 inner dialog HWND
  ; $2 window DPI
  ; $3 style
  ; $4 x
  ; $5 y
  ; $6 width
  ; $7 height
  FindWindow $1 "#32770" "" $HWNDPARENT ; Find inner dialog
  System::Call "user32::GetDpiForWindow(p r1) i .r2"
  ${If} $(^RTL) = 1
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE} | ${WS_EX_LAYOUTRTL}"
    IntOp $4 50 * $2
  ${Else}
    StrCpy $3 "${__NSD_CheckBox_EXSTYLE}"
    IntOp $4 0 * $2
  ${EndIf}
  IntOp $5 100 * $2
  IntOp $6 400 * $2
  IntOp $7 25 * $2
  IntOp $4 $4 / 96
  IntOp $5 $5 / 96
  IntOp $6 $6 / 96
  IntOp $7 $7 / 96
  System::Call 'user32::CreateWindowEx(i r3, w "${__NSD_CheckBox_CLASS}", w "$(deleteAppData)", i ${__NSD_CheckBox_STYLE}, i r4, i r5, i r6, i r7, p r1, i0, i0, i0) i .s'
  Pop $DeleteAppDataCheckbox
  SendMessage $HWNDPARENT ${WM_GETFONT} 0 0 $1
  SendMessage $DeleteAppDataCheckbox ${WM_SETFONT} $1 1
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave
Function un.ConfirmLeave
  SendMessage $DeleteAppDataCheckbox ${BM_GETCHECK} 0 0 $DeleteAppDataCheckboxState
FunctionEnd
!define MUI_PAGE_CUSTOMFUNCTION_PRE un.SkipIfPassive
!insertmacro MUI_UNPAGE_CONFIRM

; 2. Uninstalling Page
!insertmacro MUI_UNPAGE_INSTFILES

;Languages
{{#each languages}}
!insertmacro MUI_LANGUAGE "{{this}}"
{{/each}}
!insertmacro MUI_RESERVEFILE_LANGDLL
{{#each language_files}}
  !include "{{this}}"
{{/each}}

Function .onInit
  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/NS" $NoShortcutMode
  ${IfNot} ${Errors}
    StrCpy $NoShortcutMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}

  !if "${DISPLAYLANGUAGESELECTOR}" == "true"
    !insertmacro MUI_LANGDLL_DISPLAY
  !endif

  !insertmacro SetContext

  ${If} $INSTDIR == "${PLACEHOLDER_INSTALL_DIR}"
    ; Set default install location
    !if "${INSTALLMODE}" == "perMachine"
      ${If} ${RunningX64}
        !if "${ARCH}" == "x64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else if "${ARCH}" == "arm64"
          StrCpy $INSTDIR "$PROGRAMFILES64\${PRODUCTNAME}"
        !else
          StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
        !endif
      ${Else}
        StrCpy $INSTDIR "$PROGRAMFILES\${PRODUCTNAME}"
      ${EndIf}
    !else if "${INSTALLMODE}" == "currentUser"
      StrCpy $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}"
    !endif

    Call RestorePreviousInstallLocation
  ${EndIf}


  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_INIT
  !endif
FunctionEnd


Section EarlyChecks
  ; Abort silent installer if downgrades is disabled
  !if "${ALLOWDOWNGRADES}" == "false"
  ${If} ${Silent}
    ; If downgrading
    ${If} $R0 = -1
      System::Call 'kernel32::AttachConsole(i -1)i.r0'
      ${If} $0 <> 0
        System::Call 'kernel32::GetStdHandle(i -11)i.r0'
        System::call 'kernel32::SetConsoleTextAttribute(i r0, i 0x0004)' ; set red color
        FileWrite $0 "$(silentDowngrades)"
      ${EndIf}
      Abort
    ${EndIf}
  ${EndIf}
  !endif

SectionEnd

Section WebView2
  ; Check if Webview2 is already installed and skip this section
  ${If} ${RunningX64}
    ReadRegStr $4 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $4 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $4 == ""
    ReadRegStr $4 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}

  ${If} $4 == ""
    ; Webview2 installation
    ;
    ; Skip if updating
    ${If} $UpdateMode <> 1
      !if "${INSTALLWEBVIEW2MODE}" == "downloadBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        DetailPrint "$(webview2Downloading)"
        NSISdl::download "https://go.microsoft.com/fwlink/p/?LinkId=2124703" "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Pop $0
        ${If} $0 == "success"
          DetailPrint "$(webview2DownloadSuccess)"
        ${Else}
          DetailPrint "$(webview2DownloadError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "embedBootstrapper"
        Delete "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebview2Setup.exe" "${WEBVIEW2BOOTSTRAPPERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebview2Setup.exe"
        Goto install_webview2
      !endif

      !if "${INSTALLWEBVIEW2MODE}" == "offlineInstaller"
        Delete "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        File "/oname=$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe" "${WEBVIEW2INSTALLERPATH}"
        DetailPrint "$(installingWebview2)"
        StrCpy $6 "$TEMP\MicrosoftEdgeWebView2RuntimeInstaller.exe"
        Goto install_webview2
      !endif

      Goto webview2_done

      install_webview2:
        DetailPrint "$(installingWebview2)"
        ; $6 holds the path to the webview2 installer
        ExecWait "$6 ${WEBVIEW2INSTALLERARGS} /install" $1
        ${If} $1 = 0
          DetailPrint "$(webview2InstallSuccess)"
        ${Else}
          DetailPrint "$(webview2InstallError)"
          Abort "$(webview2AbortError)"
        ${EndIf}
      webview2_done:
    ${EndIf}
  ${Else}
    !if "${MINIMUMWEBVIEW2VERSION}" != ""
      ${VersionCompare} "${MINIMUMWEBVIEW2VERSION}" "$4" $R0
      ${If} $R0 = 1
        update_webview:
          DetailPrint "$(installingWebview2)"
          ${If} ${RunningX64}
            ReadRegStr $R1 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate" "path"
          ${Else}
            ReadRegStr $R1 HKLM "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 == ""
            ReadRegStr $R1 HKCU "SOFTWARE\Microsoft\EdgeUpdate" "path"
          ${EndIf}
          ${If} $R1 != ""
            ; Chromium updater docs: https://source.chromium.org/chromium/chromium/src/+/main:docs/updater/user_manual.md
            ; Modified from "HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Microsoft EdgeWebView\ModifyPath"
            ExecWait `"$R1" /install appguid=${WEBVIEW2APPGUID}&needsadmin=true` $1
            ${If} $1 = 0
              DetailPrint "$(webview2InstallSuccess)"
            ${Else}
              MessageBox MB_ICONEXCLAMATION|MB_ABORTRETRYIGNORE "$(webview2InstallError)" IDIGNORE ignore IDRETRY update_webview
              Quit
              ignore:
            ${EndIf}
          ${EndIf}
      ${EndIf}
    !endif
  ${EndIf}
SectionEnd

Section Install
  SetOutPath $INSTDIR

  !ifmacrodef NSIS_HOOK_PREINSTALL
    !insertmacro NSIS_HOOK_PREINSTALL
  !endif

  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"

  ; ---- Mirryl: release what a running adb server holds ---------------------
  ; The watcher's scheduled task runs adb every 25 s, and the first call
  ; leaves an adb server behind that keeps adb.exe, AdbWinApi.dll and
  ; AdbWinUsbApi.dll mapped: even right after an uninstaller's
  ; `adb kill-server`, the watcher's next tick can start the server again,
  ; and the installer would stop on "Error opening file for writing:
  ; ...\platform-tools\AdbWinApi.dll" (Abort / Retry / Ignore).
  ; Two things, in this order: kill the server again, as late as possible;
  ; and rename each of those files out of the way anyway - a rename works on
  ; a file in use, an overwrite never does - so a tick that lands between
  ; here and the File below does not matter. The engine binary gets the same
  ; treatment: a mirror window left open keeps it mapped, and
  ; CheckIfAppIsRunning only looks at Mirryl.exe.
  Push $R5
  IfFileExists "$INSTDIR\platform-tools\adb.exe" 0 mirryl_install_no_adb
    nsExec::Exec `"$INSTDIR\platform-tools\adb.exe" kill-server`
    Pop $R5
  mirryl_install_no_adb:
  Pop $R5
  !insertmacro MirrylMoveAside "$INSTDIR\platform-tools\adb.exe"
  !insertmacro MirrylMoveAside "$INSTDIR\platform-tools\AdbWinApi.dll"
  !insertmacro MirrylMoveAside "$INSTDIR\platform-tools\AdbWinUsbApi.dll"
  !insertmacro MirrylMoveAside "$INSTDIR\engine\mirryl-engine.exe"
  ; And the DLL that engine maps: a mirror window open during an update holds
  ; it too ("Error opening file for writing: ...\engine\WebView2Loader.dll").
  !insertmacro MirrylMoveAside "$INSTDIR\engine\WebView2Loader.dll"

  ; Copy main executable
  File "${MAINBINARYSRCPATH}"

  ; Copy resources
  {{#each resources_dirs}}
    CreateDirectory "$INSTDIR\\{{this}}"
  {{/each}}
  {{#each resources}}
    File /a "/oname={{this.[1]}}" "{{no-escape @key}}"
  {{/each}}

  ; Copy external binaries
  {{#each binaries}}
    File /a "/oname={{this}}" "{{no-escape @key}}"
  {{/each}}

  ; Create file associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
       !insertmacro APP_ASSOCIATE "{{ext}}" "{{or association.name ext}}" "{{association-description association.description ext}}" "$INSTDIR\${MAINBINARYNAME}.exe,0" "Open with ${PRODUCTNAME}" "$INSTDIR\${MAINBINARYNAME}.exe $\"%1$\""
    {{/each}}
  {{/each}}

  ; Register deep links
  {{#each deep_link_protocols as |protocol| ~}}
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "URL Protocol" ""
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}" "" "URL:${BUNDLEID} protocol"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\DefaultIcon" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\",0"
    WriteRegStr SHCTX "Software\Classes\\{{protocol}}\shell\open\command" "" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
  {{/each}}

  ; Mirryl: the copies moved aside above. One still mapped by a process that
  ; outlived kill-server stays until the next update or the uninstaller.
  Delete "$INSTDIR\platform-tools\adb.exe.old"
  Delete "$INSTDIR\platform-tools\AdbWinApi.dll.old"
  Delete "$INSTDIR\platform-tools\AdbWinUsbApi.dll.old"
  Delete "$INSTDIR\engine\mirryl-engine.exe.old"
  Delete "$INSTDIR\engine\WebView2Loader.dll.old"

  ; Create uninstaller
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Save $INSTDIR in registry for future installations
  WriteRegStr SHCTX "${MANUPRODUCTKEY}" "" $INSTDIR

  !if "${INSTALLMODE}" == "both"
    ; Save install mode to be selected by default for the next installation such as updating
    ; or when uninstalling
    WriteRegStr SHCTX "${UNINSTKEY}" $MultiUser.InstallMode 1
  !endif

  ; Remove old main binary if it doesn't match new main binary name
  ReadRegStr $OldMainBinaryName SHCTX "${UNINSTKEY}" "MainBinaryName"
  ${If} $OldMainBinaryName != ""
  ${AndIf} $OldMainBinaryName != "${MAINBINARYNAME}.exe"
    Delete "$INSTDIR\$OldMainBinaryName"
  ${EndIf}

  ; Save current MAINBINARYNAME for future updates
  WriteRegStr SHCTX "${UNINSTKEY}" "MainBinaryName" "${MAINBINARYNAME}.exe"

  ; Registry information for add/remove programs
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayName" "${PRODUCTNAME}"
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\${MAINBINARYNAME}.exe$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${UNINSTKEY}" "Publisher" "${MANUFACTURER}"
  WriteRegStr SHCTX "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
  WriteRegStr SHCTX "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoModify" "1"
  WriteRegDWORD SHCTX "${UNINSTKEY}" "NoRepair" "1"

  ${GetSize} "$INSTDIR" "/M=uninstall.exe /S=0K /G=0" $0 $1 $2
  IntOp $0 $0 + ${ESTIMATEDSIZE}
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD SHCTX "${UNINSTKEY}" "EstimatedSize" "$0"

  !if "${HOMEPAGE}" != ""
    WriteRegStr SHCTX "${UNINSTKEY}" "URLInfoAbout" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "URLUpdateInfo" "${HOMEPAGE}"
    WriteRegStr SHCTX "${UNINSTKEY}" "HelpLink" "${HOMEPAGE}"
  !endif

  ; Create start menu shortcut
  !insertmacro MUI_STARTMENU_WRITE_BEGIN Application
    Call CreateOrUpdateStartMenuShortcut
  !insertmacro MUI_STARTMENU_WRITE_END

  ; Create desktop shortcut for silent and passive installers
  ; because finish page will be skipped
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    Call CreateOrUpdateDesktopShortcut
  ${EndIf}

  ; ---- Mirryl: leave the PC actually ready, not half ready -----------------
  ; Two things Mirryl needs live OUTSIDE $INSTDIR and need administrator rights
  ; to create: the scheduled task that reconnects phones at startup
  ; ("AutoWifiADB-Watcher", an internal name) and five Windows Firewall rules.
  ; With only the app's Repair button to create them, a new user would finish
  ; installing with no auto-reconnect and nothing telling them a button had to
  ; be pressed, and a PC that LOST them would stay broken silently, because
  ; nothing reports their absence until someone opens the app and reads the
  ; notice.
  ;
  ; The probe is READ-ONLY and answers WITHOUT elevation, which is the whole
  ; reason this is affordable: Get-ScheduledTask and Get-NetFirewallRule both
  ; answer from a filtered token (same property SystemHealth in main.rs relies
  ; on). So the common case - an update where everything is already in place -
  ; costs one PowerShell call and NO UAC prompt. Only a genuinely missing piece
  ; spends a prompt, and only one.
  ;
  ; Exit code 1 means "something is missing". ANY other result, including the
  ; probe itself failing to run, is treated as "do not touch": guessing wrong
  ; here would mean a UAC prompt on every single update.
  ;
  ; Never in silent mode: an unattended install must not stop on a UAC dialog.
  ; Repair still covers that case from inside the app.
  ;
  ; $R5 carries the USERNAME, read here in the NON-elevated installer and passed
  ; explicitly into the elevated command. This is not a detail: the elevated
  ; process can be running as a DIFFERENT account (a standard user types an
  ; administrator's credentials at the UAC prompt), and a task registered for
  ; that administrator would never run for the person who installed Mirryl.
  ; watcher_repair_script() in main.rs can use $env:USERNAME because UAC
  ; elevation from the app keeps the same account; here that assumption is
  ; false, and the same warning is written on that function.
  ${IfNot} ${Silent}
    Push $R4
    Push $R5
    Push $R6

    nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$ErrorActionPreference='SilentlyContinue'; $$n=@('Mirryl (inbound)','Mirryl (outbound)','Mirryl ADB (inbound)','Mirryl ADB (outbound)','Mirryl ADB mDNS (UDP 5353)'); $$t=Get-ScheduledTask -TaskName 'AutoWifiADB-Watcher'; $$r=@(Get-NetFirewallRule -DisplayName $$n); if (-not $$t -or $$r.Count -lt 5) { exit 1 } else { exit 0 }"`
    Pop $R4 ; exit code: 1 = something is missing
    Pop $R5 ; output, unused

    ; Log what it saw. Without this, a declined elevation leaves the PC with
    ; no auto-reconnect and not a single line explaining it - the same kind
    ; of silent failure this block was written to kill.
    FileOpen $R6 "$INSTDIR\setup.log" a
    FileSeek $R6 0 END
    FileWrite $R6 "setup check: probe=$R4 (1=missing, 0=present, other=probe could not run)$\r$\n"
    FileClose $R6

    ${If} $R4 == "1"
      ReadEnvStr $R5 "USERNAME"
      ${If} $R5 != ""
        ; NOTHING is asked here, and that is deliberate: an update can show the
        ; previous uninstaller's question just before, and two nearly identical
        ; dialogs in a row is how one gets answered as if it were the other.
        ; The Windows UAC prompt already IS the question, it says who is asking
        ; for elevation, and declining it leaves things as they were.
        ;
        ; And the repair commands do NOT go here: NSIS cuts any string at 1024
        ; characters (NSIS_MAX_STRLEN on the copy Tauri uses) and the full
        ; recipe is longer, so the elevated PowerShell would receive a command
        ; cut in half. The app is called instead, and it does the work with the
        ; SAME code as the Repair button (handle_setup_repair_argument in
        ; main.rs): the command is a few dozen characters long, and the recipe
        ; lives in one place.
        ;
        ; $R5 is the user read WITHOUT elevation, a few lines above. It is
        ; passed explicitly because the elevated process can run as ANOTHER
        ; account (a standard user types administrator credentials at the UAC)
        ; and the task would be registered for that other person.
        ExecShellWait "runas" "$INSTDIR\${MAINBINARYNAME}.exe" `--repair-setup $R5` SW_HIDE

        ; Look again, instead of trusting that the elevated command ran.
        ; Measure the effect, not the command: ExecShellWait returns the
        ; same whether the user accepts the UAC, declines it, or Windows
        ; cancels it on its own after two minutes.
        nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$ErrorActionPreference='SilentlyContinue'; $$n=@('Mirryl (inbound)','Mirryl (outbound)','Mirryl ADB (inbound)','Mirryl ADB (outbound)','Mirryl ADB mDNS (UDP 5353)'); $$t=Get-ScheduledTask -TaskName 'AutoWifiADB-Watcher'; $$r=@(Get-NetFirewallRule -DisplayName $$n); if (-not $$t -or $$r.Count -lt 5) { exit 1 } else { exit 0 }"`
        Pop $R4
        Pop $R5
        FileOpen $R6 "$INSTDIR\setup.log" a
        FileSeek $R6 0 END
        ${If} $R4 == "0"
          FileWrite $R6 "setup: done - the task and the five rules are in place$\r$\n"
        ${Else}
          FileWrite $R6 "setup: NOT done (probe=$R4). The elevation was declined, cancelled or failed. Press Repair inside Mirryl to finish.$\r$\n"
        ${EndIf}
        FileClose $R6
      ${EndIf}
    ${EndIf}

    Pop $R6
    Pop $R5
    Pop $R4
  ${EndIf}
  ; ---- end Mirryl setup ----------------------------------------------------

  !ifmacrodef NSIS_HOOK_POSTINSTALL
    !insertmacro NSIS_HOOK_POSTINSTALL
  !endif

  ; Auto close this page for passive mode
  ${If} $PassiveMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function .onInstSuccess
  ; Check for `/R` flag only in silent and passive installers because
  ; GUI installer has a toggle for the user to (re)start the app
  ${If} $PassiveMode = 1
  ${OrIf} ${Silent}
    ${GetOptions} $CMDLINE "/R" $R0
    ${IfNot} ${Errors}
      ${GetOptions} $CMDLINE "/ARGS" $R0
      nsis_tauri_utils::RunAsUser "$INSTDIR\${MAINBINARYNAME}.exe" "$R0"
    ${EndIf}
  ${EndIf}
FunctionEnd

Function un.onInit
  !insertmacro SetContext

  !if "${INSTALLMODE}" == "both"
    !insertmacro MULTIUSER_UNINIT
  !endif

  !insertmacro MUI_UNGETLANGUAGE

  ${GetOptions} $CMDLINE "/P" $PassiveMode
  ${IfNot} ${Errors}
    StrCpy $PassiveMode 1
  ${EndIf}

  ${GetOptions} $CMDLINE "/UPDATE" $UpdateMode
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
FunctionEnd

Section Uninstall

  !ifmacrodef NSIS_HOOK_PREUNINSTALL
    !insertmacro NSIS_HOOK_PREUNINSTALL
  !endif

  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"

  ; ---- Mirryl: remove what the app created OUTSIDE $INSTDIR ---------------
  ; Setting Mirryl up registers a scheduled task ("AutoWifiADB-Watcher") and up
  ; to five Windows Firewall rules. Neither lives under $INSTDIR, so every
  ; Delete below misses them, and uninstalling would leave a task pointing at a
  ; watcher.ps1 that no longer exists, plus Allow rules naming a program that is
  ; gone. Both need administrator rights to remove - a root-folder task is a
  ; file under %WINDIR%\System32\Tasks - and this is a currentUser install, so
  ; the uninstaller is NOT elevated. Hence one consent prompt.
  ;
  ; It is only shown when there is actually something to remove, so a user who
  ; never finished setup is never asked. Declining is harmless: the uninstall
  ; continues and simply leaves them in place.
  ; A silent uninstall never prompts.
  ;
  ; Nor does an UPDATE. When a newer installer runs this
  ; uninstaller first ("Uninstall before installing"), it passes `_?=<dir>` -
  ; the only caller that ever does - and the version about to be installed
  ; needs the task and the rules exactly as they are: the new watcher.ps1
  ; lands on the same path. Asking here would make the user choose between a
  ; UAC in the middle of an update (Yes) and nothing (No), and Yes would then
  ; cost a Repair.
  ;
  ; The five names below must stay in sync with $ourNames in
  ; firewall_repair_script() in main.rs - that list has the same warning. Rules
  ; Windows creates on its own ("adb.exe", "mirryl.exe") are deliberately NOT
  ; touched: one named "adb.exe" could belong to Android Studio.
  Push $R4
  Push $R5
  Push $R6
  Push $R7
  StrCpy $R5 "0"

  ; Is this a REAL uninstall, or the inner run of an installer that is about to
  ; put a new copy in the same place? Two independent signals, and either one
  ; saying "an installer called us" is enough to stay quiet.
  ;
  ; The `_?=` test ALONE is not enough: reinstalling the same version over
  ; itself in passive mode launches the inner uninstaller as
  ;   uninstall.exe /P _?=%LOCALAPPDATA%\Mirryl
  ; and the test still misses it. In passive mode the question opens in its
  ; own window behind the installer's progress bar, so it gets answered
  ; without being read, and the PC is left with no scheduled task and none of
  ; the five firewall rules.
  ;
  ; Second signal, and the one that does not depend on parsing anything: WHERE
  ; THIS EXE IS RUNNING FROM. For a real uninstall NSIS copies the uninstaller
  ; into %TEMP% and re-runs it there, so that it is free to delete its own
  ; folder; `_?=<dir>` is the only thing that suppresses that copy. So running
  ; from the install directory means, by construction, that an installer
  ; called us.
  StrCpy $R4 "1" ; assume a real uninstall until something says otherwise

  ClearErrors
  ${GetOptions} $CMDLINE "_?=" $R7
  ${IfNot} ${Errors}
    StrCpy $R4 "0"
  ${EndIf}

  ${If} $EXEDIR == $INSTDIR
    StrCpy $R4 "0"
  ${EndIf}

  ${If} $R4 == "1"      ; nothing said "installer": a real uninstall
  ${AndIfNot} ${Silent}
    ; ExecToStack pushes the exit code FIRST, then the output - so they have to
    ; land in different registers or the output overwrites the code.
    nsExec::ExecToStack 'schtasks /query /TN "AutoWifiADB-Watcher"'
    Pop $R6 ; exit code
    Pop $R7 ; output, unused
    ; 1 = "task not found". Anything else (0 = found, or "error" if the probe
    ; itself could not run) means ask rather than silently skip the cleanup.
    ${If} $R6 != "1"
      StrCpy $R5 "1"
    ${EndIf}
  ${EndIf}

  ${If} $R5 == "1"
    MessageBox MB_YESNO|MB_ICONQUESTION "When you set Mirryl up, it added a startup task and Windows Firewall rules. They live outside the install folder, so removing them needs administrator permission once.$\r$\n$\r$\nRemove them now?$\r$\n$\r$\nChoose No to finish uninstalling and leave them in place." IDNO mirryl_skip_cleanup
      ; The commands travel on the command line of the elevated process, not in
      ; a file: a .ps1 under $PLUGINSDIR - a folder in %TEMP%, with %TEMP%'s
      ; ACL, which can give another account Modify - would leave a window to
      ; swap the script between the write and the run. Fixed text on a command
      ; line has no such window.
      ;
      ; Stop BEFORE unregistering: Unregister-ScheduledTask deletes the
      ; definition but does NOT kill the running instance, and the watcher
      ; would keep connecting to the phone every 25 s until the next reboot. An
      ; app the user has just uninstalled must not keep talking to their phone.
      ;
      ; "runas" puts the UAC prompt on powershell.exe directly - one hop.
      ; Declining UAC just returns here and the uninstall carries on.
      ExecShellWait "runas" "powershell.exe" `-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -Command "$$ErrorActionPreference = 'SilentlyContinue'; Stop-ScheduledTask -TaskName 'AutoWifiADB-Watcher'; Unregister-ScheduledTask -TaskName 'AutoWifiADB-Watcher' -Confirm:$$false; Remove-NetFirewallRule -DisplayName 'Mirryl (inbound)'; Remove-NetFirewallRule -DisplayName 'Mirryl (outbound)'; Remove-NetFirewallRule -DisplayName 'Mirryl ADB (inbound)'; Remove-NetFirewallRule -DisplayName 'Mirryl ADB (outbound)'; Remove-NetFirewallRule -DisplayName 'Mirryl ADB mDNS (UDP 5353)'"` SW_HIDE
    mirryl_skip_cleanup:
  ${EndIf}

  Pop $R7
  Pop $R6
  Pop $R5
  Pop $R4
  ; ---- end Mirryl cleanup -------------------------------------------------

  ; ---- Mirryl: release what was held, and sweep what using the app created --
  ; This ALWAYS runs, whether the user said yes or no to the cleanup above:
  ; it is not the system being touched here, it is the install folder itself.
  ;
  ; The adb daemon the app starts may still be alive, and Windows does not
  ; delete an executable in use, so platform-tools\ would survive whole.
  ; `CheckIfAppIsRunning` only looks at Mirryl.exe, not at the adb Mirryl started.
  ; Killing the daemon releases the file and lets the Delete below work.
  Push $R5
  IfFileExists "$INSTDIR\platform-tools\adb.exe" 0 mirryl_no_adb
    nsExec::Exec `"$INSTDIR\platform-tools\adb.exe" kill-server`
    Pop $R5
  mirryl_no_adb:
  Pop $R5

  ; Then the logs the app writes while in use, not files the installer put
  ; there: the `RMDir "$INSTDIR"` further down is NOT
  ; recursive on purpose (it must never take something the user left there),
  ; so these have to be named one by one. They matter because mirror sessions
  ; record the phone's serial number: this is not neutral junk, it is a trace
  ; of the device of someone who just asked for everything to be uninstalled.
  RMDir /r "$INSTDIR\engine\logs"
  ; The `.old` copies an update moved aside (MirrylMoveAside) - after the
  ; kill-server above nothing holds them.
  Delete "$INSTDIR\platform-tools\adb.exe.old"
  Delete "$INSTDIR\platform-tools\AdbWinApi.dll.old"
  Delete "$INSTDIR\platform-tools\AdbWinUsbApi.dll.old"
  Delete "$INSTDIR\engine\mirryl-engine.exe.old"
  Delete "$INSTDIR\watcher.log"
  Delete "$INSTDIR\qr_pairing.log"
  ; setup.log is written by this installer and by `Mirryl.exe --repair-setup`;
  ; it is a log like the others and goes with the app.
  Delete "$INSTDIR\setup.log"
  Delete "$LOCALAPPDATA\Mirryl\qr_pairing.log"
  Delete "$LOCALAPPDATA\Mirryl\setup.log"
  ; watcher.log lives here when the watcher is given -DataDir, next to the
  ; exe otherwise. Every log goes with the app without asking; the phone list
  ; and the PC's key stay unless "Delete app data" is ticked.
  Delete "$LOCALAPPDATA\Mirryl\watcher.log"
  Delete "$INSTDIR\mirryl-engine-*.log"
  Delete "$INSTDIR\mirryl-engine-*.transport"
  ; The app writes all of that to %LOCALAPPDATA%\Mirryl rather than next
  ; to the exe (local_data_dir in main.rs; needed for a packaged build,
  ; whose folder is read-only). With the folder the installer proposes it is
  ; the same as $INSTDIR and this does nothing new; if someone chose a
  ; different folder at install time, this is where the logs are. And an
  ; `elevated\` left behind would keep $INSTDIR from being deleted.
  RMDir /r "$LOCALAPPDATA\Mirryl\engine\logs"
  RMDir /r "$LOCALAPPDATA\Mirryl\elevated"
  RMDir /r "$INSTDIR\elevated"
  Delete "$LOCALAPPDATA\Mirryl\mirryl-engine-*.log"
  Delete "$LOCALAPPDATA\Mirryl\mirryl-engine-*.transport"
  ; The app's activity log. It records phone app installs, restarts
  ; and pairing-identity problems, with serials and addresses: it goes with
  ; the app, like any other log.
  Delete "$LOCALAPPDATA\Mirryl\activity.log"
  RMDir "$LOCALAPPDATA\Mirryl\engine"
  ; ---- end of the sweep -----------------------------------------------------

  ; Delete the app directory and its content from disk
  ; Copy main executable
  Delete "$INSTDIR\${MAINBINARYNAME}.exe"

  ; Delete resources
  {{#each resources}}
    Delete "$INSTDIR\\{{this.[1]}}"
  {{/each}}

  ; Delete external binaries
  {{#each binaries}}
    Delete "$INSTDIR\\{{this}}"
  {{/each}}

  ; Delete app associations
  {{#each file_associations as |association| ~}}
    {{#each association.ext as |ext| ~}}
      !insertmacro APP_UNASSOCIATE "{{ext}}" "{{or association.name ext}}"
    {{/each}}
  {{/each}}

  ; Delete deep links
  {{#each deep_link_protocols as |protocol| ~}}
    ReadRegStr $R7 SHCTX "Software\Classes\\{{protocol}}\shell\open\command" ""
    ${If} $R7 == "$\"$INSTDIR\${MAINBINARYNAME}.exe$\" $\"%1$\""
      DeleteRegKey SHCTX "Software\Classes\\{{protocol}}"
    ${EndIf}
  {{/each}}


  ; Delete uninstaller
  Delete "$INSTDIR\uninstall.exe"

  {{#each resources_ancestors}}
  RMDir /REBOOTOK "$INSTDIR\\{{this}}"
  {{/each}}
  RMDir "$INSTDIR"

  ; Remove shortcuts if not updating
  ${If} $UpdateMode <> 1
    !insertmacro DeleteAppUserModelId

    ; Remove start menu shortcut
    !insertmacro MUI_STARTMENU_GETFOLDER Application $AppStartMenuFolder
    !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
      RMDir "$SMPROGRAMS\$AppStartMenuFolder"
    ${EndIf}
    !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
    ${EndIf}

    ; Remove desktop shortcuts
    !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Pop $0
    ${If} $0 = 1
      !insertmacro UnpinShortcut "$DESKTOP\${PRODUCTNAME}.lnk"
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}
  ${EndIf}

  ; Remove registry information for add/remove programs
  !if "${INSTALLMODE}" == "both"
    DeleteRegKey SHCTX "${UNINSTKEY}"
  !else if "${INSTALLMODE}" == "perMachine"
    DeleteRegKey HKLM "${UNINSTKEY}"
  !else
    DeleteRegKey HKCU "${UNINSTKEY}"
  !endif

  ; Removes the Autostart entry for ${PRODUCTNAME} from the HKCU Run key if it exists.
  ; This ensures the program does not launch automatically after uninstallation if it exists.
  ; If it doesn't exist, it does nothing.
  ; We do this when not updating (to preserve the registry value on updates)
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${PRODUCTNAME}"
  ${EndIf}

  ; Delete app data if the checkbox is selected
  ; and if not updating
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    ; ---- Mirryl: warn before breaking the pairing identity -------------------
    ; $APPDATA\${BUNDLEID} holds pc_identity.json, the key this PC signs its
    ; pairing with. Each phone pins the FIRST key it sees and accepts no
    ; other, so deleting it breaks the link FOR GOOD: reinstalling generates a
    ; new key the phone already rejects, and there is no way to get the old
    ; one back. The behaviour is deliberate, but the checkbox only says
    ; "Delete app data", and whoever ticks it usually does so to "clean up
    ; properly" before reinstalling -- exactly the case where it hurts most.
    ; Not asked in silent mode: there is nobody to ask there, and whoever
    ; automates an uninstall has already chosen.
    ${IfNot} ${Silent}
      MessageBox MB_YESNO|MB_ICONEXCLAMATION "Deleting the app data also deletes this computer's pairing identity.$\r$\n$\r$\nAny phone you have already set up will stop reconnecting on its own, and reinstalling Mirryl will NOT restore it -- a new identity is generated and the phone no longer accepts it. You would have to set each phone up again from scratch.$\r$\n$\r$\nDelete it anyway?" IDYES mirryl_wipe_ok
        StrCpy $DeleteAppDataCheckboxState 0
        Goto mirryl_wipe_done
      mirryl_wipe_ok:
    ${EndIf}
    ; ---- end of the warning -------------------------------------------------

    ; Clear the install location $INSTDIR from registry
    DeleteRegKey SHCTX "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty SHCTX "${MANUKEY}"

    ; Clear the install language from registry
    DeleteRegValue HKCU "${MANUPRODUCTKEY}" "Installer Language"
    DeleteRegKey /ifempty HKCU "${MANUPRODUCTKEY}"
    DeleteRegKey /ifempty HKCU "${MANUKEY}"

    SetShellVarContext current
    RmDir /r "$APPDATA\${BUNDLEID}"
    RmDir /r "$LOCALAPPDATA\${BUNDLEID}"
  ${EndIf}
  ; Target of the Goto above when the user says no to the warning: the
  ; uninstall carries on, only the app's data is kept.
  mirryl_wipe_done:

  !ifmacrodef NSIS_HOOK_POSTUNINSTALL
    !insertmacro NSIS_HOOK_POSTUNINSTALL
  !endif

  ; Auto close if passive mode or updating
  ${If} $PassiveMode = 1
  ${OrIf} $UpdateMode = 1
    SetAutoClose true
  ${EndIf}
SectionEnd

Function RestorePreviousInstallLocation
  ReadRegStr $4 SHCTX "${MANUPRODUCTKEY}" ""
  StrCmp $4 "" +2 0
    StrCpy $INSTDIR $4
FunctionEnd

Function Skip
  Abort
FunctionEnd

Function SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd
Function un.SkipIfPassive
  ${IfThen} $PassiveMode = 1  ${|} Abort ${|}
FunctionEnd

Function CreateOrUpdateStartMenuShortcut
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  StrCpy $R0 0

  !insertmacro IsShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  !insertmacro IsShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    StrCpy $R0 1
  ${EndIf}

  ${If} $R0 = 1
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  !if "${STARTMENUFOLDER}" != ""
    CreateDirectory "$SMPROGRAMS\$AppStartMenuFolder"
    CreateShortcut "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\$AppStartMenuFolder\${PRODUCTNAME}.lnk"
  !else
    CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  !endif
FunctionEnd

Function CreateOrUpdateDesktopShortcut
  ; We used to use product name as MAINBINARYNAME
  ; migrate old shortcuts to target the new MAINBINARYNAME
  !insertmacro IsShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\$OldMainBinaryName"
  Pop $0
  ${If} $0 = 1
    !insertmacro SetShortcutTarget "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    Return
  ${EndIf}

  ; Skip creating shortcut if in update mode or no shortcut mode
  ; but always create if migrating from wix
  ${If} $WixMode = 0
    ${If} $UpdateMode = 1
    ${OrIf} $NoShortcutMode = 1
      Return
    ${EndIf}
  ${EndIf}

  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
  !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
FunctionEnd
