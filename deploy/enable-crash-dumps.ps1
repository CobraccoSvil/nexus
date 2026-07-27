# Abilita i crash dump WER per mcp-core.exe (diagnosi stack overflow).
# RICHIEDE una shell amministratore: scrive sotto HKLM. Da lanciare a mano:
#   Start-Process powershell -Verb RunAs -ArgumentList '-File','D:\IDEAI\deploy\enable-crash-dumps.ps1'
# I dump finiscono in D:\IDEAI-runtime\crash-dumps (full dump, max 5).
# Per disabilitare: rimuovere la chiave LocalDumps\mcp-core.exe.
$ErrorActionPreference = 'Stop'
$dumpDir = 'D:\IDEAI-runtime\crash-dumps'
New-Item -ItemType Directory -Force $dumpDir | Out-Null
$key = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps\mcp-core.exe'
New-Item -Path $key -Force | Out-Null
New-ItemProperty -Path $key -Name DumpFolder -Value $dumpDir -PropertyType ExpandString -Force | Out-Null
New-ItemProperty -Path $key -Name DumpType -Value 2 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $key -Name DumpCount -Value 5 -PropertyType DWord -Force | Out-Null
Write-Output "LocalDumps abilitato per mcp-core.exe -> $dumpDir (full dump, max 5)."
Write-Output "Al prossimo crash, analizzare il .dmp per il call stack completo."
