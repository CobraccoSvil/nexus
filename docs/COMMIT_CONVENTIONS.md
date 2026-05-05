# Commit Conventions

Seguiamo **Conventional Commits** per una storia repository pulita e automazione changelog.

## Format

```
<type>(<scope>): <subject>

<body>

<footer>
```

## Type

- **feat**: nuova feature
- **fix**: bug fix
- **refactor**: rifattorizzazione senza cambio di behavior
- **perf**: performance improvement
- **test**: aggiunta/modifica test
- **docs**: documentazione
- **chore**: build, dependencies, versioning (non tocca codice sorgente)
- **ci**: CI/CD changes

## Scope

Prefisso del pacchetto o area affetta:

- `llm-gateway`
- `shared`
- `embeddings`
- `rag`
- `audit`
- `api`
- `infra`
- `config`

## Subject

- Imperativo: "add", "fix", "refactor", non "adds", "added", "fixes"
- No maiuscola iniziale
- No punto finale
- Max 50 caratteri

## Body (opzionale ma consigliato)

- Explainare **what** e **why**, non **how**
- Wrap a 72 caratteri
- Separare da subject con una riga vuota
- Linkare issue: `Fixes #123`, `Related to #456`

## Footer (opzionale)

```
Breaking-Change: description se il commit rompe compatibilità
Closes #123
```

## Esempi

```
feat(llm-gateway): add Anthropic provider adapter

Implement AnthropicProvider class implementing LLMProvider interface.
Supports streaming, tool calling, and model aliasing.

Tested against Anthropic SDK with mock fixtures.
Fixes #42
```

```
fix(shared): config loader validation for missing env vars

Improve error message when required env vars are absent.
Add CONFIG_ERROR type with details field for debugging.

Closes #89
```

```
perf(embeddings): cache embedding queries in Redis

Query embeddings are now cached for 1h with TTL.
Reduces vector store lookups by ~70% in typical workloads.

Related to #150
```

## Validation in CI

Tutti i commit in PR sono validati contro queste regole. Merge bloccato se non conformi.
