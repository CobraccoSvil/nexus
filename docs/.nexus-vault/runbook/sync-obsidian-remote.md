---
id: runbook-sync-obsidian-remote
kind: runbook
title: "Sync vault remoto a Obsidian locale (Windows/Mac)"
tags: [runbook, obsidian, sync, remote, vault]
auto_generated: false
created_at: 2026-05-23T00:00:00Z
updated_at: 2026-05-23T00:00:00Z
nexus_meta_version: 1
---

# Sync vault remoto a Obsidian locale (Windows/Mac)

Nexus gira su un server remoto (es. `192.168.0.6`, `nexus.cobracco.it`) ma il vault `docs/.nexus-vault/` deve essere accessibile da Obsidian sul tuo Mac o Windows. Hai 3 opzioni in ordine di complessita' crescente.

## Opzione A - Git clone locale (raccomandato)

**Pro**: zero setup extra, versionato, conflitti gestiti da git, niente credenziali in piu'.
**Contro**: modifiche locali non visibili sul server finche' non fai push.

### Procedura

1. Sul tuo Mac/Windows:
   ```
   git clone https://github.com/CobraccoSvil/nexus.git ~/nexus-repo
   ```

2. Apri Obsidian -> File -> Open vault -> Open folder as vault -> seleziona `~/nexus-repo/docs/.nexus-vault/`.

3. Dai un nome al vault (es. `Nexus-MetaVault`) e annotalo nel pannello `/admin/nexus-docs` di Nexus, sezione "Setup vault Obsidian".

4. **Aggiornare il vault** dopo modifiche sul server (auto-update da commit):
   ```
   cd ~/nexus-repo && git pull
   ```
   Obsidian rileva i nuovi file/modifiche entro pochi secondi (auto-reload nativo).

5. **Pubblicare modifiche locali** (note curate a mano):
   ```
   cd ~/nexus-repo
   git add docs/.nexus-vault/
   git commit -m "docs: aggiornamento manuale note vault"
   git push
   ```
   Il post-commit hook del server Nexus rileva il push al prossimo deploy e aggiorna il DB.

## Opzione B - Scarica zip via UI admin (one-shot)

**Pro**: rapidissimo, niente strumenti esterni.
**Contro**: e' uno snapshot, non si aggiorna. Modifiche locali non risincano.

### Procedura

1. Vai a `https://nexus.tuoserver.it/admin/nexus-docs` (autenticato come admin).
2. Click sul pulsante verde **Scarica vault (.zip)**.
3. Estrai lo zip in una cartella locale: `~/nexus-vault-snapshot/`.
4. Apri Obsidian -> seleziona quella cartella come vault.

Per aggiornare: ripeti la procedura (scarica zip nuovo, sostituisci cartella).

## Opzione C - SFTP mount come drive di rete (bidirezionale realtime)

**Pro**: modifiche bidirezionali in tempo reale, niente sync manuale.
**Contro**: richiede installazione tool extra, latenza disco maggiore, dipende dalla connessione di rete.

### Su Windows

1. Installa WinFsp (https://winfsp.dev/) e SSHFS-Win (https://github.com/winfsp/sshfs-win).
2. Apri Esplora file -> click destro su "Questo PC" -> "Connetti unita' di rete".
3. Percorso: `\\sshfs\administrator@192.168.0.6\opt\ideai\docs\.nexus-vault`.
4. Mappa come drive `Z:`.
5. Apri Obsidian -> Open folder as vault -> `Z:\`.

### Su Mac (Cyberduck/Mountain Duck)

1. Installa Cyberduck (https://cyberduck.io/) o Mountain Duck (https://mountainduck.io/).
2. Mountain Duck: aggiungi connessione SFTP a `administrator@192.168.0.6`, monta path `/opt/ideai/docs/.nexus-vault/`.
3. La cartella appare in `/Volumes/<nome>/`.
4. Apri Obsidian -> Open folder as vault -> seleziona la cartella montata.

### Su Mac (terminale, sshfs)

```
brew install --cask macfuse
brew install gromgit/fuse/sshfs-mac
mkdir -p ~/nexus-vault-remote
sshfs administrator@192.168.0.6:/opt/ideai/docs/.nexus-vault ~/nexus-vault-remote -o reconnect,ServerAliveInterval=15
```

Apri `~/nexus-vault-remote/` in Obsidian.

Per smontare:
```
umount ~/nexus-vault-remote
```

## Quale scegliere

| Scenario | Opzione |
|---|---|
| Lavori spesso sul vault, hai familiarita' con git | **A (git)** |
| Vuoi solo dare un'occhiata o presentare a qualcuno | **B (zip)** |
| Modifichi continuamente in tempo reale, multi-utente | **C (SFTP)** |

## Configurazione vault Obsidian in Nexus

Dopo aver aperto il vault in Obsidian, **annota il nome scelto** nella UI:

- Per il meta-vault (admin): pagina `/admin/nexus-docs` -> "Setup vault Obsidian"
- Per il KB di un progetto: tab Grafo del progetto -> "Configura vault Obsidian"

In questo modo i pulsanti **Apri in Obsidian** della UI Nexus aprono direttamente la nota selezionata via deep-link `obsidian://open?vault=<nome>&file=<percorso>`.

## Vedi anche

- [[meta-vault-architettura]] - come funziona il meta-vault
- [[knowledge-base-funzionamento]] - KB per-progetto
- [[adr-0005-meta-docs-vault]] - design rationale Obsidian-compatible
