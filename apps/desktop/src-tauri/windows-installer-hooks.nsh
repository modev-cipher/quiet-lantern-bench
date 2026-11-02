!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    SetShellVarContext current
    RmDir /r "$LOCALAPPDATA\txtcaster-cache"
    ${If} $DeleteAppDataCheckboxState = 1
      RmDir /r "$LOCALAPPDATA\txtcaster-data"
    ${EndIf}
  ${EndIf}
!macroend
