; Installer hooks for the hf-mount desktop app (Tauri NSIS bundle).
;
; WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos
; (https://github.com/winfsp/winfsp) provides the drive letters. The
; installer ships the unmodified official WinFsp MSI and offers to run it
; when WinFsp is missing. Windows Installer then shows its own wizard and
; asks for administrator approval; nothing is installed silently, and a
; silent (/S) install of hf-mount never installs WinFsp.

!define HFM_WINFSP_MSI "winfsp-2.1.25156.msi"

!macro NSIS_HOOK_PREINSTALL
  ; Updating: the background supervisor runs hf-mount.exe from this folder.
  ; Ask it to unmount (finishing uploads) and exit so the file can be replaced.
  IfFileExists "$INSTDIR\hf-mount.exe" 0 +2
    nsExec::Exec '"$INSTDIR\hf-mount.exe" shutdown --wait-secs 600'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\WinFsp" "InstallDir"
  StrCmp $0 "" 0 hfm_winfsp_done
  ReadRegStr $0 HKLM "SOFTWARE\WinFsp" "InstallDir"
  StrCmp $0 "" 0 hfm_winfsp_done
  IfSilent hfm_winfsp_done
  IfFileExists "$INSTDIR\winfsp\${HFM_WINFSP_MSI}" 0 hfm_winfsp_done
  MessageBox MB_YESNO|MB_ICONQUESTION "hf-mount needs WinFsp, a free file system driver for Windows, to show repositories as drives.$\r$\n$\r$\nInstall WinFsp now? Windows will ask for administrator approval.$\r$\n$\r$\nYou can also install it later from the hf-mount window." IDNO hfm_winfsp_done
  ExecWait '"$SYSDIR\msiexec.exe" /i "$INSTDIR\winfsp\${HFM_WINFSP_MSI}" /norestart' $1
  hfm_winfsp_done:
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Unmount this user's drives and stop the supervisor so hf-mount.exe can be
  ; removed. Settings, saved drives, the cache, the stored token and WinFsp
  ; are left in place.
  IfFileExists "$INSTDIR\hf-mount.exe" 0 +2
    nsExec::Exec '"$INSTDIR\hf-mount.exe" shutdown --wait-secs 600'
!macroend
