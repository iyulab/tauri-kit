; Wait, during an update, for the app that started the installer to let go of its files.
;
; An app that installs its own update starts the installer and then exits. Tauri's NSIS template checks
; for a running app right after NSIS_HOOK_PREINSTALL, with a single Restart Manager shutdown attempt — and
; aborts the update when the app is still on its way out. A helper process the app started (a sidecar)
; keeps its own program open a moment longer still.
;
; TAURI_KIT_WAIT_FOR_EXIT waits, in update mode only, until no process uses the app's executable or the
; helpers named, for at most TAURI_KIT_EXIT_WAIT_MS (default 20 s), then lets the template's own check run
; unchanged. A manual install is untouched: it still asks the person at once.
;
; Each poll opens a new Restart Manager session: a session keeps reporting the processes it saw when the
; files were registered, even after they exit.
;
; Use: copy this file next to the app's installer hooks file, and in that file
;
;   !include "wait-for-exit.nsh"
;   !macro NSIS_HOOK_PREINSTALL
;     !insertmacro TAURI_KIT_WAIT_FOR_EXIT "sidecar\helper.exe"   ; "" when there is no helper
;   !macroend
;
; The path is relative to the install folder. The app still stops its helpers itself before it starts the
; installer (tauri-kit-update's `before_install`); this covers the time the operating system takes to let
; the files go.

!ifndef TAURI_KIT_EXIT_WAIT_MS
  !define TAURI_KIT_EXIT_WAIT_MS 20000
!endif
!ifndef TAURI_KIT_EXIT_POLL_MS
  !define TAURI_KIT_EXIT_POLL_MS 250
!endif

!macro TAURI_KIT_WAIT_FOR_EXIT HELPER
  ${If} $UpdateMode = 1
    Push $0
    Push $1
    Push $2
    Push $3
    Push $R0
    Push $R8
    Push $R9
    System::Call 'kernel32::GetTickCount()i.R9'
    IntOp $R9 $R9 + ${TAURI_KIT_EXIT_WAIT_MS}
    StrCpy $R8 0
    ${Do}
      ; ERROR_MORE_DATA: something still uses them · anything else (a failed session included): stop waiting
      StrCpy $0 0
      !insertmacro RestartManager_StartSession $R0
      ${If} $R0 != ""
        !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${MAINBINARYNAME}.exe"
        ${If} $0 = 0
        ${AndIf} "${HELPER}" != ""
        ${AndIf} ${FileExists} "$INSTDIR\${HELPER}"
          !insertmacro RestartManager_RegisterFile $R0 "$INSTDIR\${HELPER}"
        ${EndIf}
        ${If} $0 = 0
          System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'
        ${EndIf}
        !insertmacro RestartManager_EndSession $R0
      ${EndIf}
      ${If} $0 <> ${ERROR_MORE_DATA}
        ${Break}
      ${EndIf}
      ${If} $R8 = 0
        DetailPrint "Waiting for ${PRODUCTNAME} to close..."
        StrCpy $R8 1
      ${EndIf}
      System::Call 'kernel32::GetTickCount()i.r1'
      ${If} $1 >= $R9
        ${Break}
      ${EndIf}
      Sleep ${TAURI_KIT_EXIT_POLL_MS}
    ${Loop}
    Pop $R9
    Pop $R8
    Pop $R0
    Pop $3
    Pop $2
    Pop $1
    Pop $0
  ${EndIf}
!macroend
