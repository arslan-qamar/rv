Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!ifndef BUILDTARGET
  !define BUILDTARGET "x86_64-pc-windows-msvc"
!endif
Name "RVHost"
OutFile "RVHostSetup.exe"
InstallDir "$PROGRAMFILES64\RVHost"
RequestExecutionLevel admin
ShowInstDetails show
Var DeviceField
Var PortField
Var PasswordField
Var ConfirmField
Var DeviceName
Var PortValue
Var PasswordValue
Var ConfirmValue
Var TempConfig
Page custom ConfigurePage ConfigureLeave
!insertmacro MUI_PAGE_INSTFILES
Page custom FinishPage
!insertmacro MUI_LANGUAGE "English"

Function ConfigurePage
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateLabel} 0 0 100% 12u "Device name"
  Pop $0
  ${NSD_CreateText} 0 14u 100% 13u "OFFICE-PC"
  Pop $DeviceField
  ${NSD_CreateLabel} 0 32u 100% 12u "Port"
  Pop $0
  ${NSD_CreateNumber} 0 46u 100% 13u "5901"
  Pop $PortField
  ${NSD_CreateLabel} 0 64u 100% 12u "Viewing password (at least 8 characters)"
  Pop $0
  ${NSD_CreatePassword} 0 78u 100% 13u ""
  Pop $PasswordField
  ${NSD_CreateLabel} 0 96u 100% 12u "Confirm password"
  Pop $0
  ${NSD_CreatePassword} 0 110u 100% 13u ""
  Pop $ConfirmField
  nsDialogs::Show
FunctionEnd

Function ConfigureLeave
  ${NSD_GetText} $DeviceField $DeviceName
  ${NSD_GetText} $PortField $PortValue
  ${NSD_GetText} $PasswordField $PasswordValue
  ${NSD_GetText} $ConfirmField $ConfirmValue
  ${If} $DeviceName == ""
    MessageBox MB_ICONSTOP "Enter a device name."
    Abort
  ${EndIf}
  ${If} $PortValue == ""
    MessageBox MB_ICONSTOP "Enter a port."
    Abort
  ${EndIf}
  ${If} $PortValue < 1
  ${OrIf} $PortValue > 65535
    MessageBox MB_ICONSTOP "Port must be between 1 and 65535."
    Abort
  ${EndIf}
  ${If} $PortValue == 45901
    MessageBox MB_ICONSTOP "Port 45901 is reserved for local agent communication."
    Abort
  ${EndIf}
  StrLen $0 $PasswordValue
  ${If} $0 < 8
    MessageBox MB_ICONSTOP "Password must be at least 8 characters."
    Abort
  ${EndIf}
  ${If} $0 > 256
    MessageBox MB_ICONSTOP "Password must be at most 256 characters."
    Abort
  ${EndIf}
  ${If} $PasswordValue != $ConfirmValue
    MessageBox MB_ICONSTOP "Passwords do not match."
    Abort
  ${EndIf}
FunctionEnd

Section "Install"
  ; Permit repair/upgrade over an existing MVP installation.
  nsExec::ExecToLog 'sc.exe stop RVHost'
  nsExec::ExecToLog 'taskkill.exe /F /IM RVCapture.exe'
  nsExec::ExecToLog 'taskkill.exe /F /IM RVHost.exe'
  nsExec::ExecToLog 'sc.exe delete RVHost'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="RVHost"'
  Sleep 750
  SetOutPath "$INSTDIR"
  File "..\host\target\${BUILDTARGET}\release\RVHost.exe"
  File "..\host\target\${BUILDTARGET}\release\RVCapture.exe"
  InitPluginsDir
  StrCpy $TempConfig "$PLUGINSDIR\setup-input.txt"
  FileOpen $0 $TempConfig w
  FileWrite $0 "$DeviceName$\r$\n$PortValue$\r$\n$PasswordValue$\r$\n"
  FileClose $0
  nsExec::ExecToLog '"$INSTDIR\RVHost.exe" --init-file "$TempConfig"'
  Pop $0
  Delete $TempConfig
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Configuration failed. Error code: $0"
    Abort
  ${EndIf}
  SetShellVarContext all
  nsExec::ExecToLog 'icacls "$APPDATA\RVHost" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)R"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not secure configuration directory."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'icacls "$APPDATA\RVHost\password.hash" /inheritance:r /remove:g "*S-1-5-32-545" /grant:r "*S-1-5-18:F" "*S-1-5-32-544:F"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not secure password hash."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe create RVHost binPath= "$INSTDIR\RVHost.exe" start= auto DisplayName= "RVHost"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not create Windows Service. Error code: $0"
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe failure RVHost reset= 86400 actions= restart/5000/restart/5000/restart/5000'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not configure service recovery."
    Abort
  ${EndIf}
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "RVCapture" '"$INSTDIR\RVCapture.exe"'
  ; QEMU and other VM adapters are frequently classified as Public. Apply the
  ; rule to every profile, but only accept peers on directly reachable subnets.
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="RVHost" dir=in action=allow protocol=TCP localport=$PortValue program="$INSTDIR\RVHost.exe" profile=any remoteip=localsubnet enable=yes'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not add the LAN firewall rule."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe start RVHost'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not start Windows Service. Error code: $0"
    Abort
  ${EndIf}
  Sleep 1000
  nsExec::ExecToStack 'cmd.exe /C "sc.exe query RVHost | findstr RUNNING"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Windows Service verification failed."
    Abort
  ${EndIf}
  Exec '"$INSTDIR\RVCapture.exe"'
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RVHost" "DisplayName" "RVHost"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RVHost" "UninstallString" '"$INSTDIR\Uninstall.exe"'
SectionEnd

Function FinishPage
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateLabel} 0 0 100% 110u "RVHost installed successfully.$\r$\n$\r$\nDevice: $DeviceName$\r$\nPort: $PortValue$\r$\nMode: VIEW ONLY$\r$\n$\r$\nRVHost starts automatically when Windows starts. RVCapture starts when a user logs in."
  Pop $0
  nsDialogs::Show
FunctionEnd

Section "Uninstall"
  ; Remove autostart before terminating the interactive capture agent so it
  ; cannot be relaunched during uninstall.
  DeleteRegValue HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "RVCapture"
  nsExec::ExecToLog 'taskkill.exe /F /IM RVCapture.exe'
  nsExec::ExecToLog 'sc.exe stop RVHost'
  Sleep 500
  nsExec::ExecToLog 'taskkill.exe /F /IM RVHost.exe'
  nsExec::ExecToLog 'sc.exe delete RVHost'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="RVHost"'
  Sleep 500
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RVHost"
  Delete "$INSTDIR\RVHost.exe"
  Delete "$INSTDIR\RVCapture.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
