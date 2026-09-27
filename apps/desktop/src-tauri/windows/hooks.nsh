; Lanroam NSIS installer hooks (Windows)
;
; The first time a program listens on a UDP port, Windows asks whether to
; let it through the firewall; a stray "Cancel" silently breaks discovery and
; every link. The installer registers the inbound rule itself instead, for
; private and domain networks only, and removes it on uninstall.

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Lanroam"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Lanroam" dir=in action=allow program="$INSTDIR\Lanroam.exe" enable=yes profile=private,domain'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Lanroam"'
!macroend
