Unicode true
!include "MUI2.nsh"
!include "nsDialogs.nsh"
!include "LogicLib.nsh"
!ifndef BUILDTARGET
  !define BUILDTARGET "x86_64-pc-windows-msvc"
!endif
Name "Remote Viewer Host"
OutFile "RemoteViewerHostSetup.exe"
InstallDir "$PROGRAMFILES64\RemoteViewerHost"
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
  SetOutPath "$INSTDIR"
  File "..\host\target\${BUILDTARGET}\release\RemoteViewerHost.exe"
  File "..\host\target\${BUILDTARGET}\release\RemoteViewerCapture.exe"
  InitPluginsDir
  StrCpy $TempConfig "$PLUGINSDIR\setup-input.txt"
  FileOpen $0 $TempConfig w
  FileWrite $0 "$DeviceName$\r$\n$PortValue$\r$\n$PasswordValue$\r$\n"
  FileClose $0
  nsExec::ExecToLog '"$INSTDIR\RemoteViewerHost.exe" --init-file "$TempConfig"'
  Pop $0
  Delete $TempConfig
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Configuration failed. Error code: $0"
    Abort
  ${EndIf}
  SetShellVarContext all
  nsExec::ExecToLog 'icacls "$APPDATA\RemoteViewerHost" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)R"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not secure configuration directory."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'icacls "$APPDATA\RemoteViewerHost\password.hash" /inheritance:r /remove:g "*S-1-5-32-545" /grant:r "*S-1-5-18:F" "*S-1-5-32-544:F"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not secure password hash."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe create RemoteViewerHost binPath= "$INSTDIR\RemoteViewerHost.exe" start= auto DisplayName= "Remote Viewer Host"'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not create Windows Service. Error code: $0"
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe failure RemoteViewerHost reset= 86400 actions= restart/5000/restart/5000/restart/5000'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not configure service recovery."
    Abort
  ${EndIf}
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "RemoteViewerCapture" '"$INSTDIR\RemoteViewerCapture.exe"'
  nsExec::ExecToLog 'netsh advfirewall firewall add rule name="Remote Viewer Host" dir=in action=allow protocol=TCP localport=$PortValue program="$INSTDIR\RemoteViewerHost.exe" profile=private'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not add private-network firewall rule."
    Abort
  ${EndIf}
  nsExec::ExecToLog 'sc.exe start RemoteViewerHost'
  Pop $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Could not start Windows Service. Error code: $0"
    Abort
  ${EndIf}
  Sleep 1000
  nsExec::ExecToStack 'cmd.exe /C "sc.exe query RemoteViewerHost | findstr RUNNING"'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Windows Service verification failed."
    Abort
  ${EndIf}
  Exec '"$INSTDIR\RemoteViewerCapture.exe"'
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteViewerHost" "DisplayName" "Remote Viewer Host"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteViewerHost" "UninstallString" '"$INSTDIR\Uninstall.exe"'
SectionEnd

Function FinishPage
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateLabel} 0 0 100% 110u "Remote Viewer Host installed successfully.$\r$\n$\r$\nDevice: $DeviceName$\r$\nPort: $PortValue$\r$\nMode: VIEW ONLY$\r$\n$\r$\nThe Host starts automatically when Windows starts. The capture agent starts when a user logs in."
  Pop $0
  nsDialogs::Show
FunctionEnd

Section "Uninstall"
  nsExec::ExecToLog 'sc.exe stop RemoteViewerHost'
  nsExec::ExecToLog 'sc.exe delete RemoteViewerHost'
  nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="Remote Viewer Host"'
  DeleteRegValue HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "RemoteViewerCapture"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\RemoteViewerHost"
  Delete "$INSTDIR\RemoteViewerHost.exe"
  Delete "$INSTDIR\RemoteViewerCapture.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
