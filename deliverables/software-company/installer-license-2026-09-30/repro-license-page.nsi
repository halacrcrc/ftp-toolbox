Unicode true
Name "LicensePageTest"
OutFile "lictest.exe"
RequestExecutionLevel user
InstallDir "$TEMP\lictest"

!include "MUI2.nsh"
!insertmacro MUI_PAGE_LICENSE "license_file"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section
SectionEnd
