# Censimento dei salvataggi refs/wip

Generato da `powershell -File scripts/worktree-wip.ps1 -Census -Markdown`.
Repo osservato: `D:\IDEAI`. Confronto contro `main` = `032d6c1f`.

## Criterio

Confronto degli ALBERI, mai dei messaggi di commit. Due alberi non bastano:
«differisce da main» non significa «contiene lavoro che main non ha», perche' main
nel frattempo si e' mosso. Il verdetto si da' per path, su TRE alberi (base del
salvataggio, salvataggio, main):

| verdetto | significato |
|---|---|
| `gia-in-main` | ogni file toccato ha in main esattamente quel contenuto: niente da perdere |
| `lavoro-solo-qui` | almeno un file che main non ha MAI toccato: la modifica non e' arrivata, ed e' l'unico caso PROVATO dagli alberi |
| `da-verificare` | main ha evoluto quei file per conto suo: se il lavoro sia incorporato o superato, nessun confronto di alberi lo puo' dire |
| `non-valutabile` | manca la base o git non ha potuto confrontare |
| `vuoto` | il salvataggio non conserva alcuna modifica |

## Conteggio

| verdetto | quanti |
|---|---|
| gia-in-main | 2 |
| lavoro-solo-qui | 16 |
| da-verificare | 51 |
| non-valutabile | 0 |
| vuoto | 1 |
| **totale** | **70** |

## NON si pota da qui

Cancellare un salvataggio e' irreversibile e alcuni sono l'unica copia del loro
lavoro (regola P). Questo file e' un CENSIMENTO: dice cosa c'e', non cosa buttare.
Un `gia-in-main` e' potabile senza perdite; per gli altri serve leggere il
contenuto, e il modo di leggerlo e'
`powershell -File scripts/worktree-wip.ps1 -Restore <nome> -Into <directory>`.

## Dettaglio

| salvataggio | data | verdetto | file toccati | solo qui | da verificare | worktree vivo |
|---|---|---|---:|---:|---:|---|
| `distracted-davinci-scartato` | 2026-07-27 | da-verificare | 1 | 0 | 1 | no |
| `objective-margulis-7da85c` | 2026-07-30 | da-verificare | 2 | 0 | 2 | no |
| `keen-snyder-44bc6b` | 2026-07-30 | da-verificare | 10 | 0 | 5 | no |
| `xenodochial-vaughan-6c91ff` | 2026-07-30 | da-verificare | 4 | 0 | 4 | no |
| `optimistic-haibt-e07e47` | 2026-07-30 | da-verificare | 5 | 0 | 3 | no |
| `angry-galileo-3a5a82` | 2026-07-30 | da-verificare | 5 | 0 | 5 | no |
| `determined-tu-93b8fa` | 2026-07-30 | da-verificare | 4 | 0 | 4 | no |
| `nifty-zhukovsky-superato-da-464292f9` | 2026-07-30 | da-verificare | 12 | 0 | 11 | no |
| `recursing-jennings-ee64ae` | 2026-07-30 | da-verificare | 12 | 0 | 12 | no |
| `clever-heyrovsky-8da7e0` | 2026-07-30 | da-verificare | 1 | 0 | 1 | no |
| `admiring-curie-5e582b` | 2026-07-30 | da-verificare | 2 | 0 | 1 | no |
| `nifty-kapitsa-792770` | 2026-07-31 | da-verificare | 4 | 0 | 3 | no |
| `nifty-tu-f1acf9` | 2026-07-31 | da-verificare | 8 | 0 | 7 | no |
| `heuristic-sutherland-863b63` | 2026-07-31 | da-verificare | 3 | 0 | 3 | no |
| `determined-poitras-b9e866` | 2026-07-31 | da-verificare | 1 | 0 | 1 | no |
| `nostalgic-brahmagupta-091514` | 2026-08-01 | da-verificare | 11 | 0 | 11 | no |
| `quizzical-saha-af50bd` | 2026-08-01 | da-verificare | 20 | 0 | 11 | no |
| `wonderful-bartik-02ce01` | 2026-08-01 | da-verificare | 6 | 0 | 5 | no |
| `hopeful-almeida-aec340` | 2026-08-01 | da-verificare | 9 | 0 | 6 | no |
| `hopeful-austin-5385d3` | 2026-08-01 | da-verificare | 3 | 0 | 1 | no |
| `wf_b3b344c2-b2d-1` | 2026-08-02 | da-verificare | 4 | 0 | 4 | no |
| `adr0042-p0-albero-0208` | 2026-08-02 | da-verificare | 3 | 0 | 2 | no |
| `optimistic-austin-bda113` | 2026-08-02 | da-verificare | 1 | 0 | 1 | no |
| `wf_b3b344c2-b2d-2` | 2026-08-02 | da-verificare | 3 | 0 | 3 | no |
| `adr0042-p0-parziale` | 2026-08-02 | da-verificare | 5 | 0 | 4 | no |
| `wf_12689b80-34e-5` | 2026-08-02 | da-verificare | 3 | 0 | 2 | no |
| `practical-austin-3ac85b` | 2026-08-02 | da-verificare | 3 | 0 | 1 | no |
| `vigilant-hofstadter-99ccba` | 2026-08-02 | da-verificare | 7 | 0 | 6 | no |
| `wf_12689b80-34e-8` | 2026-08-02 | da-verificare | 5 | 0 | 5 | no |
| `wf_b3b344c2-b2d-3` | 2026-08-04 | da-verificare | 6 | 0 | 4 | no |
| `competent-villani-dbcdb3` | 2026-08-05 | da-verificare | 12 | 0 | 9 | no |
| `magical-heyrovsky-794c12` | 2026-08-05 | da-verificare | 18 | 0 | 11 | no |
| `elated-poincare-2f8594` | 2026-08-05 | da-verificare | 17 | 0 | 12 | no |
| `mystifying-zhukovsky-99efeb` | 2026-08-05 | da-verificare | 7 | 0 | 7 | no |
| `objective-brattain-574fc4` | 2026-08-07 | da-verificare | 1 | 0 | 1 | no |
| `nervous-kilby-b12fe8` | 2026-08-08 | da-verificare | 16 | 0 | 7 | no |
| `exciting-swirles-08ad9a` | 2026-08-08 | da-verificare | 10 | 0 | 4 | no |
| `upbeat-sinoussi-1749e0` | 2026-08-08 | da-verificare | 3 | 0 | 3 | no |
| `zealous-kirch-c31de7` | 2026-08-08 | da-verificare | 11 | 0 | 7 | no |
| `unruffled-albattani-1c25ab` | 2026-08-08 | da-verificare | 2 | 0 | 1 | no |
| `jolly-knuth-f55f60` | 2026-08-08 | da-verificare | 12 | 0 | 8 | no |
| `reverent-liskov-43c9a6` | 2026-08-08 | da-verificare | 2 | 0 | 1 | no |
| `festive-kapitsa-672a6a` | 2026-08-08 | da-verificare | 12 | 0 | 7 | no |
| `intelligent-mclaren-eb04d7` | 2026-08-08 | da-verificare | 10 | 0 | 7 | no |
| `agent-a5d260c8d381ddbed` | 2026-08-09 | da-verificare | 11 | 0 | 11 | si |
| `agent-acca403b1879ea7fa` | 2026-08-09 | da-verificare | 13 | 0 | 10 | si |
| `agent-a3b57275bec3a25f9` | 2026-08-09 | da-verificare | 13 | 0 | 10 | si |
| `agent-a4d6981069439042e` | 2026-08-09 | da-verificare | 11 | 0 | 11 | si |
| `agent-ac8f7216226ec24d5` | 2026-08-09 | da-verificare | 2 | 0 | 1 | si |
| `agent-a2425490f07feabc3` | 2026-08-09 | da-verificare | 5 | 0 | 2 | si |
| `trusting-kirch-888784` | 2026-08-09 | da-verificare | 15 | 0 | 4 | no |
| `IDEAI` | 2026-08-09 | gia-in-main | 2 | 0 | 0 | si |
| `optimistic-ptolemy-3920f3` | 2026-08-09 | gia-in-main | 2 | 0 | 0 | no |
| `zen-mahavira-9ed7bf` | 2026-07-31 | lavoro-solo-qui | 3 | 1 | 1 | no |
| `recursing-chebyshev-631ecc` | 2026-08-02 | lavoro-solo-qui | 21 | 1 | 17 | no |
| `wf_b3b344c2-b2d-5` | 2026-08-02 | lavoro-solo-qui | 4 | 2 | 2 | no |
| `wf_04969528-a09-3` | 2026-08-02 | lavoro-solo-qui | 6 | 4 | 1 | no |
| `wf_12689b80-34e-6` | 2026-08-02 | lavoro-solo-qui | 3 | 1 | 1 | no |
| `wf_12689b80-34e-7` | 2026-08-02 | lavoro-solo-qui | 11 | 4 | 7 | no |
| `wf_b3b344c2-b2d-4` | 2026-08-02 | lavoro-solo-qui | 10 | 4 | 6 | no |
| `wf_04969528-a09-4` | 2026-08-02 | lavoro-solo-qui | 16 | 12 | 3 | no |
| `wf_04969528-a09-5` | 2026-08-04 | lavoro-solo-qui | 7 | 6 | 1 | no |
| `agent-ae6969def250f39fc` | 2026-08-09 | lavoro-solo-qui | 9 | 7 | 2 | si |
| `agent-aa96a009faa5cc22b` | 2026-08-09 | lavoro-solo-qui | 2 | 2 | 0 | si |
| `nifty-maxwell-195597` | 2026-08-09 | lavoro-solo-qui | 19 | 16 | 3 | si |
| `agent-ad5e9897ba5b88527` | 2026-08-09 | lavoro-solo-qui | 8 | 6 | 2 | si |
| `wt` | 2026-08-09 | lavoro-solo-qui | 30 | 12 | 9 | no |
| `cool-brattain-1e5521` | 2026-08-09 | lavoro-solo-qui | 30 | 12 | 9 | no |
| `agent-ae1245ef2dea23faa` | 2026-08-09 | lavoro-solo-qui | 7 | 1 | 3 | si |
| `main-non-committato-0208` | 2026-08-02 | vuoto | 0 | 0 | 0 | no |

