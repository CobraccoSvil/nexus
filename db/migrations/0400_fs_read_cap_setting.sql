-- 0400_fs_read_cap_setting.sql
--
-- Governance filesystem (classe 'file'): cap dimensione lettura singolo file
-- via read_file. Un file enorme (bundle/dump/lock) caricato integralmente
-- satura il contesto del modello e la memoria; sopra soglia read_file invita a
-- usare read_file_lines / search_file_semantic. Regola G: soglia nel DB, niente
-- hardcode. 0 = nessun cap.
--
-- La regola 'file/read_max_bytes' del catalogo nexus_resource_policies (mig
-- 0397) porta lo stesso valore in params; questo setting e' la fonte letta dal
-- tool (cache get_setting). Idempotente.

INSERT INTO settings (key, value, category, description) VALUES
  ('agent.fs.read_max_bytes', '2097152', 'agent',
   'Cap dimensione (byte) per la lettura integrale di un file via read_file. Sopra soglia il tool invita a read_file_lines/search_file_semantic. 0 = nessun cap. Governance filesystem.')
ON CONFLICT (key) DO NOTHING;
