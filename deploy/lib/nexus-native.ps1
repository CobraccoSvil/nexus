# Punto unico (regola L) per INVOCARE un eseguibile nativo da PowerShell e
# leggerne l'esito dal segnale strutturato.
#
# Dot-source dai chiamanti:
#   . (Join-Path $PSScriptRoot 'lib\nexus-native.ps1')
#
# Perche' esiste. Uno script che imposta `$ErrorActionPreference = 'Stop'` -- cosa
# giusta e che tutti i deploy fanno -- non puo' piu' invocare un eseguibile che
# scriva su stderr: Windows PowerShell 5.1 avvolge ogni riga di stderr di un
# comando nativo in un ErrorRecord (NativeCommandError), e con 'Stop' quello
# TERMINA lo script. Il risultato e' che un avviso innocuo del programma uccide
# la sessione, e la gestione d'errore che il chiamante aveva scritto -- il
# `continue`, l'accumulo in `$failures`, il riepilogo finale -- non viene mai
# raggiunta, quindi non e' nemmeno sbagliata: e' irraggiungibile.
#
# MISURATO il 02/08/2026 su db-backup.ps1: la suite `#[sqlx::test]` distrugge i
# propri database effimeri mentre il backup gira, `pg_dump` ha scritto su stderr
# "il database non esiste", e lo script e' morto al terzo database su decine --
# lasciando un set di backup PARZIALE che nessun riepilogo dichiarava tale. Il
# `continue` per singolo database c'era gia', dieci righe piu' sotto.
#
# Regola M: l'esito di un processo e' il suo EXIT CODE, un intero. Lo stderr e'
# il testo per l'operatore e non decide niente -- e infatti un programma che
# scrive un avviso ed esce 0 e' riuscito, mentre uno che tace ed esce 1 e'
# fallito. Scambiare i due canali e' esattamente cio' che 'Stop' fa qui.
#
# Due funzioni perche' i chiamanti vogliono due cose diverse, e la differenza e'
# nella firma invece che in un flag:
#   - Invoke-NexusNative  -> RITORNA l'exit code. Per chi accumula i fallimenti
#                            e prosegue (un backup non si ferma al primo dump).
#   - Invoke-NexusNativeOrThrow -> SOLLEVA. Per chi si ferma al primo errore
#                            (un deploy non prosegue su un build fallito).
# La seconda delega alla prima: la sospensione della preferenza sta in UN posto.

# Esegue lo scriptblock con la preferenza d'errore sospesa e ne ritorna l'exit
# code. La preferenza precedente e' ripristinata anche se lo scriptblock solleva.
function Invoke-NexusNative {
  param(
    [Parameter(Mandatory)][scriptblock]$Comando
  )
  $preferenzaPrec = $ErrorActionPreference
  $ErrorActionPreference = 'Continue'
  try { & $Comando } finally { $ErrorActionPreference = $preferenzaPrec }
  return $LASTEXITCODE
}

# Come sopra, ma un exit code diverso da zero interrompe: $Cosa nomina l'azione
# nel messaggio, perche' "exit 1" da solo non dice a chi legge che cosa e'
# fallito.
function Invoke-NexusNativeOrThrow {
  param(
    [Parameter(Mandatory)][scriptblock]$Comando,
    [Parameter(Mandatory)][string]$Cosa
  )
  $codice = Invoke-NexusNative -Comando $Comando
  if ($codice -ne 0) { throw "$Cosa fallito (exit $codice)" }
}
