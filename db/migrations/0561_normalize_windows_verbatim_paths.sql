-- 0561: normalizza i path Windows "verbatim" persistiti (\\?\D:\...) e i
-- file_path dei quality finding in forma relativa alla root progetto
--
-- Sintomo: nel pannello Problemi i quality finding mostravano path illeggibili
-- (\\?\D:\IDEAI-projects\Beaty-Book\src\...) e il click non apriva il file
-- nell'editor (404 "Percorso non trovato"): il resolver testuale divorava il
-- \\ iniziale del prefisso verbatim lasciando un componente "?" fantasma.
--
-- Root cause (fix nel codice, stessa PR): la registrazione progetto persisteva
-- l'output di std::fs::canonicalize (che su Windows produce la forma verbatim)
-- in projects.repository_root_path / workspaces.absolute_path /
-- repositories.root_path; da li' la root verbatim si propagava ai target dei
-- tool agente e l'auto-scan quality salvava file_path assoluti verbatim.
-- Il codice ora scrive sempre la forma classica (punto unico
-- nexus_types::workspace_paths::path_for_storage) e l'auto-scan persiste il
-- path RELATIVO alla root (stesso formato del full-scan). Questa migrazione
-- allinea i dati storici al nuovo formato (regola H, punto 5).

-- 1) Root di progetto/workspace/repository: strip del prefisso verbatim.
--    substr e' 1-based: '\\?\D:\x' -> substr(...,5) = 'D:\x';
--    '\\?\UNC\server\share' -> '\\' || substr(...,9) = '\\server\share'.
UPDATE projects
   SET repository_root_path = CASE
         WHEN starts_with(repository_root_path, '\\?\UNC\')
           THEN '\\' || substr(repository_root_path, 9)
         ELSE substr(repository_root_path, 5)
       END
 WHERE repository_root_path IS NOT NULL
   AND starts_with(repository_root_path, '\\?\');

UPDATE workspaces
   SET absolute_path = CASE
         WHEN starts_with(absolute_path, '\\?\UNC\')
           THEN '\\' || substr(absolute_path, 9)
         ELSE substr(absolute_path, 5)
       END
 WHERE absolute_path IS NOT NULL
   AND starts_with(absolute_path, '\\?\');

UPDATE repositories
   SET root_path = CASE
         WHEN starts_with(root_path, '\\?\UNC\')
           THEN '\\' || substr(root_path, 9)
         ELSE substr(root_path, 5)
       END
 WHERE root_path IS NOT NULL
   AND starts_with(root_path, '\\?\');

-- 2) Quality finding: file_path assoluti (verbatim o classici) -> relativi POSIX
--    alla root del progetto. Le righe gia' relative non matchano il filtro
--    (nessun backslash e nessun drive ':') e restano intatte. Idempotente: al
--    secondo passaggio norm_file non contiene piu' la root e resta invariato.
WITH norm AS (
    SELECT f.id,
           replace(
             CASE WHEN starts_with(f.file_path, '\\?\')
                  THEN substr(f.file_path, 5)
                  ELSE f.file_path
             END, '\', '/') AS norm_file,
           replace(
             CASE WHEN starts_with(p.repository_root_path, '\\?\')
                  THEN substr(p.repository_root_path, 5)
                  ELSE p.repository_root_path
             END, '\', '/') AS norm_root
      FROM project_quality_findings f
      JOIN projects p ON p.id = f.project_id
     WHERE strpos(f.file_path, '\') > 0
        OR strpos(f.file_path, ':') > 0
)
UPDATE project_quality_findings f
   SET file_path = ltrim(
         CASE
           WHEN n.norm_root IS NOT NULL
            AND n.norm_root <> ''
            AND starts_with(n.norm_file, n.norm_root || '/')
             THEN substr(n.norm_file, length(n.norm_root) + 2)
           ELSE n.norm_file
         END, '/')
  FROM norm n
 WHERE f.id = n.id;
