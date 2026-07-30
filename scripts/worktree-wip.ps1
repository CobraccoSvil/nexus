<#
.SYNOPSIS
  Censisce e mette al sicuro il lavoro NON COMMITTATO dei worktree git.

.DESCRIPTION
  Il lavoro non committato di un worktree e' invisibile a ogni query basata sulla
  storia (`git log --all`, elenco branch, elenco PR) e viene DISTRUTTO quando il
  worktree viene rimosso: non esiste reflog che copra un working tree. Misurato il
  29-30/07/2026: su tredici sessioni CCD completate, SETTE si sono fermate
  lasciando il lavoro solo nel worktree.

  Questo script risponde alle due sole domande che contano prima di una pulizia:
    -Report  quale worktree ha lavoro che nessun commit conserva (exit 1 se ce n'e')
    -Save    mettilo al sicuro, senza dichiararlo pronto

  MISURA E' SEPARATA DA GIUDIZIO. Un salvataggio non e' un commit sul branch di
  lavoro: nessun gate viene eseguito, quindi il contenuto puo' non compilare. Uno
  dei recuperi del 30/07 (interesting-wozniak) non compilava, e un commit
  automatico l'avrebbe presentato come finito. Il salvataggio vive fuori da
  refs/heads proprio per questo: non e' mergeabile per sbaglio, non compare in
  `git branch`, non parte con `git push --all`. Chi lo promuove lo fa a mano,
  dopo i gate.

  COME METTE AL SICURO (pattern plumbing, lo stesso di
  crates/mcp-core/src/session_autocommit.rs, che risponde a un'altra domanda --
  "una mutazione dell'agente e' avvenuta su un progetto utente" -- e resta suo):
    1. copia dell'index REALE del worktree in un index temporaneo, cosi' cio' che
       la sessione aveva in staging entra nel salvataggio e il suo staging non
       viene toccato;
    2. `add -A` su quell'index: modificati, cancellati e NUOVI file. Gli untracked
       sono la categoria che una patch da `git diff HEAD` perde in silenzio, come
       `git diff` senza HEAD perde lo staging (recupero del 30/07: un terzo del
       lavoro mancante, scoperto solo da `cargo check`);
    3. write-tree + commit-tree + update-ref su `refs/wip/<worktree>`.

  Nessun passo esegue hook. Il salvataggio quindi funziona anche quando il commit
  normale non puo' funzionare -- ed e' il caso che ha prodotto i sette worktree
  appesi: pre-commit bloccato, gate rossi per colpa altrui, cold build piu' lungo
  del turno della sessione.

  I ref stanno nel repo comune, condiviso da tutti i worktree: un salvataggio
  SOPRAVVIVE alla rimozione del worktree che l'ha prodotto. `--create-reflog`
  conserva anche i salvataggi precedenti dello stesso worktree.

  INNESTO CHE NON DIPENDE DALLA SESSIONE. Tutte e sette le sessioni che hanno
  lasciato il lavoro appeso avevano l'istruzione esplicita di committare nel
  proprio prompt: un rimedio che richieda a una sessione di ricordarsi qualcosa
  non copre il caso osservato. Le due strade che non passano da lei:

    1. periodico, indipendente da chiunque (registrare una volta, come utente):
         schtasks /Create /TN "Nexus WIP" /SC MINUTE /MO 15 /F ^
           /TR "powershell -NoProfile -ExecutionPolicy Bypass -File D:\IDEAI\scripts\worktree-wip.ps1 -Save"
       -Save e' idempotente sul contenuto: un tree identico al salvataggio
       precedente non produce un commit nuovo, quindi girare ogni 15 minuti non
       accumula ref.

    2. prima di ogni pulizia, come gate (exit 1 se c'e' lavoro non salvato):
         powershell -File scripts\worktree-wip.ps1 -Report

  Cosa questo script NON puo' fare: rifiutare l'archiviazione di una sessione.
  Rimuovere il worktree e' un'azione dello strumento di sessione (CCD), fuori dal
  repo: nessun hook git viene eseguito e non esiste un punto lato repo in cui
  interporsi. Quel presidio va chiesto a CCD; -Report e -Save sono cio' che si
  puo' presidiare da questa parte.

.PARAMETER Report
  Censimento (default). Exit 1 se almeno un worktree ha lavoro non salvato.

.PARAMETER Save
  Crea/aggiorna refs/wip/<worktree> per ogni worktree sporco. Idempotente: se il
  contenuto e' identico al salvataggio precedente non crea un commit nuovo.

.PARAMETER List
  Elenca i salvataggi esistenti.

.PARAMETER Restore
  Nome del salvataggio (label o ref) da riapplicare. Verifica il proprio esito
  confrontando i tree: dichiara "ripristinato" solo se il contenuto dell'albero
  coincide con quello salvato.

.PARAMETER Into
  Directory dove riapplicare. Default: il worktree con quel nome, se esiste.

.EXAMPLE
  powershell -File scripts/worktree-wip.ps1 -Report
.EXAMPLE
  powershell -File scripts/worktree-wip.ps1 -Save
.EXAMPLE
  powershell -File scripts/worktree-wip.ps1 -Restore bold-boyd-bf2f39 -Into D:\IDEAI-worktrees\bold-boyd-bf2f39
#>
[CmdletBinding(DefaultParameterSetName = 'Report')]
param(
  [Parameter(ParameterSetName = 'Report')][switch]$Report,
  [Parameter(ParameterSetName = 'Save')][switch]$Save,
  [Parameter(ParameterSetName = 'List')][switch]$List,
  [Parameter(ParameterSetName = 'Restore', Mandatory = $true)][string]$Restore,
  [Parameter(ParameterSetName = 'Restore')][string]$Into
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$WipPrefix = 'refs/wip/'

# git con stdout e stderr SEPARATI. Non si usa '2>&1': su Windows PowerShell la
# redirezione dello stderr di un eseguibile nativo produce un NativeCommandError
# per ogni riga (un warning innocuo diventa un errore terminante) e, peggio,
# mescola quelle righe nell'output -- uno SHA letto da 'write-tree' arriverebbe
# con un warning CRLF attaccato. Misurato su questo stesso script prima del fix.
# L'esito si legge dall'exit code, segnale strutturato, non dal testo (regola M).
function Format-CmdArg {
  param([string]$Value)
  if ($Value -eq '') { return '""' }
  if ($Value -notmatch '[\s"]') { return $Value }
  $sb = New-Object System.Text.StringBuilder
  [void]$sb.Append('"')
  $backslashes = 0
  foreach ($ch in $Value.ToCharArray()) {
    if ($ch -eq '\') { $backslashes++; continue }
    if ($ch -eq '"') {
      [void]$sb.Append('\' * ($backslashes * 2 + 1)).Append('"')
      $backslashes = 0
      continue
    }
    if ($backslashes -gt 0) { [void]$sb.Append('\' * $backslashes); $backslashes = 0 }
    [void]$sb.Append($ch)
  }
  [void]$sb.Append('\' * ($backslashes * 2)).Append('"')
  return $sb.ToString()
}

function Invoke-Git {
  param([string[]]$Arguments, [string]$In, [hashtable]$EnvVars)

  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = 'git'
  # ProcessStartInfo.ArgumentList non esiste su .NET Framework (Windows
  # PowerShell 5.1, l'unico host presente su questa macchina: 'pwsh' non e'
  # installato), quindi la riga di comando si compone a mano con il quoting di
  # CommandLineToArgvW. Serve davvero: i percorsi dei worktree sono assoluti con
  # backslash e possono contenere spazi.
  $psi.Arguments = (($Arguments | ForEach-Object { Format-CmdArg $_ }) -join ' ')
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $psi.RedirectStandardInput = $true
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  if ($EnvVars) {
    foreach ($k in $EnvVars.Keys) { $psi.EnvironmentVariables[$k] = $EnvVars[$k] }
  }

  $proc = [System.Diagnostics.Process]::Start($psi)
  # Lettura asincrona di entrambi i flussi PRIMA dell'attesa: con la lettura
  # sincrona un output che riempie il buffer della pipe (git diff, git status su
  # molti file) blocca il processo per sempre.
  $tOut = $proc.StandardOutput.ReadToEndAsync()
  $tErr = $proc.StandardError.ReadToEndAsync()
  if ($PSBoundParameters.ContainsKey('In') -and $null -ne $In) {
    $proc.StandardInput.Write($In)
  }
  $proc.StandardInput.Close()
  $proc.WaitForExit()

  return [pscustomobject]@{
    Ok     = ($proc.ExitCode -eq 0)
    Code   = $proc.ExitCode
    Output = $tOut.Result.Trim()
    Err    = $tErr.Result.Trim()
  }
}

function Assert-Git {
  param([string[]]$Arguments, [string]$What, [string]$In, [hashtable]$EnvVars)
  $p = @{ Arguments = $Arguments }
  if ($PSBoundParameters.ContainsKey('In')) { $p['In'] = $In }
  if ($EnvVars) { $p['EnvVars'] = $EnvVars }
  $r = Invoke-Git @p
  if (-not $r.Ok) { throw "$What (git exit $($r.Code)): $($r.Err)" }
  return $r.Output
}

# Il repo COMUNE, non la root dell'albero in cui vive questo script. La differenza
# conta due volte: i ref di salvataggio vivono nel repo comune, e uno strumento che
# dichiara di guardare un albero mentre ne guarda un altro non e' una misura
# (regola O, incidente 2ae08818). La radice di partenza e' la posizione DELLO
# SCRIPT, mai la cwd.
function Resolve-CommonRepo {
  $commonDir = Assert-Git -Arguments @('-C', $PSScriptRoot, 'rev-parse', '--path-format=absolute', '--git-common-dir') -What 'risoluzione del repo comune'
  return (Resolve-Path (Split-Path $commonDir.Trim() -Parent)).Path
}
$RepoRoot = Resolve-CommonRepo

# Elenco dei worktree dalla fonte autoritativa di git, non dal contenuto di una
# directory: un worktree registrato fuori dalla convenzione di percorso sarebbe
# invisibile a un 'ls', e il suo lavoro non censito.
function Get-Worktrees {
  $lines = Assert-Git -Arguments @('-C', $RepoRoot, 'worktree', 'list', '--porcelain') -What 'elenco worktree'
  $res = @()
  $cur = $null
  foreach ($line in ($lines -split "`r?`n")) {
    if ($line -like 'worktree *') {
      if ($cur) { $res += $cur }
      $path = $line.Substring(9)
      $cur = [pscustomobject]@{
        Path   = $path
        Label  = (Split-Path $path -Leaf)
        Branch = '(detached)'
      }
    }
    elseif ($line -like 'branch *' -and $null -ne $cur) {
      $cur.Branch = $line.Substring(7) -replace '^refs/heads/', ''
    }
  }
  if ($cur) { $res += $cur }
  return $res
}

# Conteggi PER CATEGORIA, non un totale: "staging" e "non tracciati" sono le due
# categorie che i due recuperi sbagliati perdono ciascuno per conto proprio
# ('git diff' perde la prima, 'git diff HEAD' la seconda). Tenerle distinte e'
# l'unico modo di vedere quanto costerebbe il recupero fatto male.
function Get-DirtyState {
  param([string]$WorktreePath)
  $porcelain = Invoke-Git -Arguments @('-C', $WorktreePath, 'status', '--porcelain')
  if (-not $porcelain.Ok) {
    return [pscustomobject]@{
      Readable = $false; Error = $porcelain.Err
      Staged = 0; Modified = 0; Untracked = 0; Total = 0; IsDirty = $false
    }
  }
  $staged = 0; $modified = 0; $untracked = 0
  foreach ($line in ($porcelain.Output -split "`r?`n")) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    if ($line.Length -lt 2) { continue }
    $x = $line[0]; $y = $line[1]
    if ($x -eq '?' -and $y -eq '?') { $untracked++; continue }
    if ($x -ne ' ') { $staged++ }
    if ($y -ne ' ') { $modified++ }
  }
  return [pscustomobject]@{
    Readable = $true; Error = ''
    Staged = $staged; Modified = $modified; Untracked = $untracked
    Total = ($staged + $modified + $untracked)
    IsDirty = (($staged + $modified + $untracked) -gt 0)
  }
}

function Get-WipRef {
  param([string]$Label)
  $ref = "$WipPrefix$Label"
  $r = Invoke-Git -Arguments @('-C', $RepoRoot, 'rev-parse', '--verify', '--quiet', $ref)
  if (-not $r.Ok -or [string]::IsNullOrWhiteSpace($r.Output)) { return $null }
  $sha = $r.Output.Trim()
  $tree = (Assert-Git -Arguments @('-C', $RepoRoot, 'rev-parse', "$sha^{tree}") -What 'tree del salvataggio').Trim()
  $when = (Assert-Git -Arguments @('-C', $RepoRoot, 'log', '-1', '--format=%cI', $sha) -What 'data del salvataggio').Trim()
  return [pscustomobject]@{ Ref = $ref; Sha = $sha; Tree = $tree; When = $when }
}

# Il tree che il salvataggio conserverebbe ADESSO. Costruito su un index
# temporaneo che PARTE DA QUELLO REALE del worktree: lo staging della sessione
# entra nel salvataggio senza essere toccato.
function New-WipTree {
  param([string]$WorktreePath)
  $realIndex = (Assert-Git -Arguments @('-C', $WorktreePath, 'rev-parse', '--path-format=absolute', '--git-path', 'index') -What 'percorso index').Trim()
  $tmpIndex = Join-Path ([System.IO.Path]::GetTempPath()) ("nexus-wip-" + [guid]::NewGuid().ToString('N') + ".idx")
  try {
    $envIdx = @{ GIT_INDEX_FILE = ($tmpIndex -replace '\\', '/') }
    if (Test-Path -LiteralPath $realIndex) {
      Copy-Item -LiteralPath $realIndex -Destination $tmpIndex -Force
    }
    else {
      # Worktree senza index (mai popolato): si parte da HEAD.
      Assert-Git -Arguments @('-C', $WorktreePath, 'read-tree', 'HEAD') -What 'read-tree HEAD' -EnvVars $envIdx | Out-Null
    }
    # -A: modificati, cancellati e NUOVI. I file ignorati restano fuori:
    # .gitignore continua a valere, quindi target/ e node_modules non entrano.
    Assert-Git -Arguments @('-C', $WorktreePath, 'add', '-A') -What 'stage del lavoro' -EnvVars $envIdx | Out-Null
    return (Assert-Git -Arguments @('-C', $WorktreePath, 'write-tree') -What 'write-tree' -EnvVars $envIdx).Trim()
  }
  finally {
    if (Test-Path -LiteralPath $tmpIndex) { Remove-Item -LiteralPath $tmpIndex -Force }
  }
}

function Save-Worktree {
  param([pscustomobject]$Worktree, [pscustomobject]$State)
  $tree = New-WipTree -WorktreePath $Worktree.Path
  $head = (Assert-Git -Arguments @('-C', $Worktree.Path, 'rev-parse', 'HEAD') -What 'HEAD del worktree').Trim()
  $existing = Get-WipRef -Label $Worktree.Label

  if ($existing -and $existing.Tree -eq $tree) {
    return [pscustomobject]@{ Action = 'invariato'; Sha = $existing.Sha }
  }
  # Tree identico a HEAD: non c'e' nulla da conservare. Accade quando il working
  # tree differisce da HEAD solo per fine-riga (autocrlf).
  $headTree = (Assert-Git -Arguments @('-C', $Worktree.Path, 'rev-parse', "$head^{tree}") -What 'tree di HEAD').Trim()
  if ($tree -eq $headTree) {
    return [pscustomobject]@{ Action = 'solo-fine-riga'; Sha = '' }
  }

  $msg = @(
    "wip($($Worktree.Label)): salvataggio del lavoro non committato",
    '',
    'SALVATAGGIO NON VERIFICATO. Su questo contenuto non e stato eseguito nessun',
    'gate e nessun hook: puo non compilare. Non e lavoro dichiarato pronto e non va',
    'mergiato cosi come e. Per promuoverlo: ripristinalo in un worktree, esegui',
    'pnpm verify, poi committa a mano sul branch di lavoro.',
    '',
    "worktree: $($Worktree.Path)",
    "branch di lavoro: $($Worktree.Branch)",
    "base: $head",
    "contenuto: $($State.Staged) in staging, $($State.Modified) modificati, $($State.Untracked) non tracciati",
    '',
    'Prodotto da scripts/worktree-wip.ps1 -Save'
  ) -join "`n"

  $commit = (Assert-Git -Arguments @('-C', $Worktree.Path, 'commit-tree', $tree, '-p', $head, '-F', '-') `
      -What 'commit-tree' -In $msg -EnvVars @{
      GIT_AUTHOR_NAME     = 'Nexus WIP'
      GIT_AUTHOR_EMAIL    = 'wip@nexus.local'
      GIT_COMMITTER_NAME  = 'Nexus WIP'
      GIT_COMMITTER_EMAIL = 'wip@nexus.local'
    }).Trim()

  # --create-reflog: il salvataggio precedente resta raggiungibile dal reflog del
  # ref, quindi un -Save successivo che catturasse lavoro peggiore non distrugge
  # quello buono. Senza, refs/wip non avrebbe alcuna storia.
  Assert-Git -Arguments @('-C', $RepoRoot, 'update-ref', '--create-reflog', "$WipPrefix$($Worktree.Label)", $commit) -What 'update-ref' | Out-Null

  $action = 'creato'
  if ($existing) { $action = 'aggiornato' }
  return [pscustomobject]@{ Action = $action; Sha = $commit }
}

function Show-Header {
  Write-Host ''
  Write-Host "Repo comune osservato: $RepoRoot" -ForegroundColor DarkGray
  Write-Host "Salvataggi in:         ${WipPrefix}<worktree>  (fuori da refs/heads: non mergeabili per sbaglio)" -ForegroundColor DarkGray
  Write-Host ''
}

function Invoke-Report {
  param([switch]$DoSave)
  Show-Header
  $dirty = 0
  $unsafe = 0
  $rows = @()

  foreach ($wt in (Get-Worktrees)) {
    if (-not (Test-Path -LiteralPath $wt.Path)) {
      $rows += [pscustomobject]@{ Worktree = $wt.Label; Stato = 'ASSENTE DAL DISCO'; Salvataggio = '-'; Nota = $wt.Path }
      continue
    }
    $state = Get-DirtyState -WorktreePath $wt.Path
    if (-not $state.Readable) {
      $rows += [pscustomobject]@{ Worktree = $wt.Label; Stato = 'NON LEGGIBILE'; Salvataggio = '-'; Nota = $state.Error }
      continue
    }
    if (-not $state.IsDirty) {
      $rows += [pscustomobject]@{ Worktree = $wt.Label; Stato = 'pulito'; Salvataggio = '-'; Nota = 'archiviabile senza perdite' }
      continue
    }

    $dirty++
    $desc = "$($state.Total) file (staging $($state.Staged), mod $($state.Modified), nuovi $($state.Untracked))"

    if ($DoSave) {
      $res = Save-Worktree -Worktree $wt -State $state
      $sha = '-'
      if ($res.Sha) { $sha = $res.Sha.Substring(0, 8) }
      $rows += [pscustomobject]@{ Worktree = $wt.Label; Stato = $desc; Salvataggio = "$($res.Action) $sha"; Nota = '' }
      continue
    }

    $existing = Get-WipRef -Label $wt.Label
    $current = New-WipTree -WorktreePath $wt.Path
    if ($existing -and $existing.Tree -eq $current) {
      $rows += [pscustomobject]@{
        Worktree = $wt.Label; Stato = $desc
        Salvataggio = "al sicuro $($existing.Sha.Substring(0,8))"; Nota = $existing.When
      }
    }
    else {
      $unsafe++
      $nota = 'nessun salvataggio'
      if ($existing) { $nota = "salvataggio STANTIO del $($existing.When)" }
      $rows += [pscustomobject]@{ Worktree = $wt.Label; Stato = $desc; Salvataggio = 'DA SALVARE'; Nota = $nota }
    }
  }

  $rows | Format-Table -AutoSize | Out-String -Width 220 | Write-Host

  if ($DoSave) {
    Write-Host "Worktree con lavoro non committato: $dirty. Messi al sicuro in ${WipPrefix}." -ForegroundColor Green
    Write-Host 'Un salvataggio non e un commit: nessun gate e stato eseguito. Verifica prima di promuoverlo.' -ForegroundColor Yellow
    return 0
  }
  if ($unsafe -gt 0) {
    Write-Host "$unsafe worktree hanno lavoro che nessun commit e nessun salvataggio conserva." -ForegroundColor Red
    Write-Host 'Rimuovere ora uno di questi worktree DISTRUGGE quel lavoro: per un working tree non esiste reflog.' -ForegroundColor Red
    Write-Host 'Mettilo al sicuro:  powershell -File scripts/worktree-wip.ps1 -Save' -ForegroundColor Yellow
    return 1
  }
  Write-Host "Nessun lavoro a rischio: $dirty worktree sporchi, tutti con salvataggio aggiornato." -ForegroundColor Green
  return 0
}

function Invoke-List {
  Show-Header
  $refs = Assert-Git -Arguments @('-C', $RepoRoot, 'for-each-ref', '--sort=-committerdate',
    '--format=%(refname:short)|%(objectname:short)|%(committerdate:iso-strict)', "$WipPrefix*") -What 'elenco salvataggi'
  if ([string]::IsNullOrWhiteSpace($refs)) {
    Write-Host 'Nessun salvataggio presente.' -ForegroundColor DarkGray
    return 0
  }
  $live = @{}
  foreach ($wt in (Get-Worktrees)) { $live[$wt.Label] = $true }

  $rows = @()
  foreach ($line in ($refs -split "`r?`n")) {
    if ([string]::IsNullOrWhiteSpace($line)) { continue }
    $p = $line -split '\|'
    $label = $p[0] -replace '^wip/', ''
    $stat = Invoke-Git -Arguments @('-C', $RepoRoot, 'diff', '--shortstat', "$($p[1])^", $p[1])
    $contenuto = '?'
    if ($stat.Ok) { $contenuto = $stat.Output }
    # Il worktree vivo si cerca fra quelli REGISTRATI, non ricostruendo il
    # percorso per convenzione: un worktree fuori convenzione risulterebbe morto
    # mentre e' vivo, e il salvataggio sembrerebbe l'unica copia del lavoro.
    $rows += [pscustomobject]@{
      Salvataggio  = $label
      Commit       = $p[1]
      Data         = $p[2]
      Contenuto    = $contenuto
      WorktreeVivo = $live.ContainsKey($label)
    }
  }
  $rows | Format-Table -AutoSize | Out-String -Width 220 | Write-Host
  Write-Host 'Ripristino:  powershell -File scripts/worktree-wip.ps1 -Restore <salvataggio> -Into <directory>' -ForegroundColor Yellow
  return 0
}

function Invoke-Restore {
  param([string]$Label, [string]$Target)
  Show-Header
  $clean = $Label -replace '^refs/wip/', '' -replace '^wip/', ''
  $wip = Get-WipRef -Label $clean
  if (-not $wip) { throw "Nessun salvataggio '$clean'. Elencali con -List." }

  if ([string]::IsNullOrWhiteSpace($Target)) {
    $candidate = (Get-Worktrees | Where-Object { $_.Label -eq $clean } | Select-Object -First 1)
    if (-not $candidate) {
      throw "Il worktree '$clean' non esiste piu': indica dove ripristinare con -Into <directory>."
    }
    $Target = $candidate.Path
  }
  if (-not (Test-Path -LiteralPath $Target)) { throw "Directory inesistente: $Target" }

  Write-Host "Salvataggio:  $($wip.Sha) del $($wip.When)"
  Write-Host "Destinazione: $Target"

  # Ripristino per contenuto, non per patch: 'checkout <commit> -- .' riporta i
  # file del salvataggio senza far passare megabyte di diff (nemmeno binario)
  # attraverso una stringa, dove l'encoding li corromperebbe in silenzio.
  Assert-Git -Arguments @('-C', $Target, 'checkout', $wip.Sha, '--', '.') -What 'checkout del salvataggio' | Out-Null

  # Le cancellazioni non le porta il checkout (copia solo cio' che nel commit
  # esiste): si leggono dal name-status, segnale strutturato, e si applicano.
  $status = Assert-Git -Arguments @('-C', $RepoRoot, 'diff', '--name-status', "$($wip.Sha)^", $wip.Sha) -What 'name-status del salvataggio'
  $deleted = @()
  foreach ($line in ($status -split "`r?`n")) {
    if ($line -match '^D\s+(.+)$') { $deleted += $Matches[1].Trim() }
  }
  foreach ($path in $deleted) {
    Assert-Git -Arguments @('-C', $Target, 'rm', '-f', '--ignore-unmatch', '--', $path) -What "rimozione di $path" | Out-Null
  }

  # Lo strumento verifica il PROPRIO esito invece di dichiararlo: il tree
  # dell'albero ripristinato deve coincidere con quello salvato. Un "ripristinato"
  # che non lo avesse controllato e' esattamente il tipo di verde che il recupero
  # del 30/07 ha prodotto per un terzo del lavoro (regola O).
  #
  # PREMESSA DEL CONFRONTO, da non leggere per piu' di quel che dice: prova che il
  # ripristino e' FEDELE al salvataggio, non che il salvataggio fosse COMPLETO.
  # Misurato: mutando 'add -A' in 'add -u' (cattura che perde i file nuovi) questo
  # confronto resta verde e solo scripts/worktree-wip-selftest.sh rossegga. La
  # completezza della cattura la prova quel test, non questa riga.
  $after = New-WipTree -WorktreePath $Target
  if ($after -ne $wip.Tree) {
    Write-Host 'RIPRISTINO INCOMPLETO: il contenuto dell albero non coincide con il salvataggio.' -ForegroundColor Red
    Write-Host "  tree salvato:    $($wip.Tree)" -ForegroundColor Red
    Write-Host "  tree ottenuto:   $after" -ForegroundColor Red
    Write-Host "  differenze:      git diff $($wip.Sha) -- ." -ForegroundColor Yellow
    return 1
  }
  Write-Host 'Ripristinato, contenuto identico al salvataggio (tree confrontato).' -ForegroundColor Green
  Write-Host 'NON e verificato: esegui i gate prima di committare.' -ForegroundColor Yellow
  return 0
}

switch ($PSCmdlet.ParameterSetName) {
  'Save' { exit (Invoke-Report -DoSave) }
  'List' { exit (Invoke-List) }
  'Restore' { exit (Invoke-Restore -Label $Restore -Target $Into) }
  default { exit (Invoke-Report) }
}
