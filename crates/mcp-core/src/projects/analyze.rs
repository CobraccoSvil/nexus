// Analisi statica del progetto: linguaggi, framework, dipendenze, indice vettoriale.

use super::*;

// ── Costanti per la rilevazione framework ─────────────────────────────────────

const EXCLUDED_BUILD_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".next",
    "obj",
    "bin",
    ".svn",
    ".hg",
    "vendor",
    "__pycache__",
    ".cache",
    "coverage",
    ".turbo",
    "out",
    ".output",
    "public",
    "static",
    ".yarn",
];

// ── Handler HTTP ──────────────────────────────────────────────────────────────

/// POST /api/projects/:id/analyze — analizza un progetto esistente e restituisce
/// linguaggi, framework, dipendenze, stato git.
pub async fn analyze_project(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AxumPath(project_id): AxumPath<String>,
) -> ApiResult {
    let user_id = parse_user_id(&claims)?;
    let project_id = Uuid::parse_str(&project_id)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "Project id non valido"))?;

    let context = load_project_context(&state.db, project_id, user_id).await?;
    let root = &context.repository_root_path;

    if !root.is_dir() {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "Directory del progetto non trovata",
        ));
    }

    // Fasi 1-6: scansione statica del progetto (file, linguaggi, framework,
    // dipendenze, git, struttura). Raggruppate in un helper per tenere questo
    // handler sotto soglia (comportamento invariato).
    let meta = collect_project_metadata(root, &context).await;

    let vector_index = bootstrap_project_vectors(&state, project_id, root, &meta).await;

    // Code index: avviato in background per evitare timeout HTTP (420+ file = >120s)
    spawn_code_index(&state, project_id, root);

    let analysis = build_analysis_json(project_id, root, &meta, vector_index);

    // Persiste l'analisi e genera le custom_instructions dallo stack rilevato.
    persist_analysis(&state, project_id, root, &analysis, &meta.frameworks, &meta.dependencies).await;

    // Auto-crea profili basati sullo stack rilevato (in background).
    spawn_profile_creation(&state, user_id, &context, &meta.frameworks, &meta.languages);

    // Pre-calcola i suggerimenti run-config e li salva in cache (in background).
    spawn_run_config_suggestions(&state, project_id, root);

    Ok(Json(analysis))
}

/// Indicizza i vettori di bootstrap del progetto passando i campi di `meta` a
/// `index_project_bootstrap_vectors`. Wrapper introdotto per non elencare gli 8
/// argomenti nell'handler (comportamento invariato).
async fn bootstrap_project_vectors(
    state: &AppState,
    project_id: Uuid,
    root: &Path,
    meta: &ProjectMetadata,
) -> Value {
    index_project_bootstrap_vectors(
        state,
        project_id,
        root,
        meta.total_files,
        &meta.languages,
        &meta.frameworks,
        &meta.dependencies,
        &meta.git_info,
    )
    .await
}

/// Assembla il JSON di risposta dell'analisi a partire dai metadati raccolti e
/// dall'esito dell'indicizzazione vettoriale. Estratto da `analyze_project`
/// (comportamento invariato).
fn build_analysis_json(
    project_id: Uuid,
    root: &Path,
    meta: &ProjectMetadata,
    vector_index: Value,
) -> Value {
    json!({
        "projectId": project_id.to_string(),
        "rootPath": root.to_string_lossy(),
        "totalFiles": meta.total_files,
        "filesByExtension": meta.ext_counts,
        "languages": meta.languages,
        "frameworks": meta.frameworks,
        "dependencies": meta.dependencies,
        "git": meta.git_info,
        "vectorIndex": vector_index,
        "codeIndex": {"status": "indexing", "message": "Indicizzazione codice avviata in background"},
        "structure": meta.structure,
    })
}

/// Risultato della scansione statica (fasi 1-6) di `analyze_project`: file per
/// estensione, totale file, linguaggi, framework, dipendenze, stato git e
/// struttura del repo. Raccolti in un unico valore per non gonfiare l'handler.
struct ProjectMetadata {
    ext_counts: BTreeMap<String, u32>,
    total_files: u32,
    languages: Vec<Value>,
    frameworks: Vec<String>,
    dependencies: Value,
    git_info: Value,
    structure: Value,
}

/// Esegue le fasi 1-6 dell'analisi statica e ne raccoglie gli esiti. Estratto
/// da `analyze_project` (comportamento invariato).
async fn collect_project_metadata(root: &Path, context: &ProjectContext) -> ProjectMetadata {
    // 1. Conta file per estensione
    let mut ext_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut total_files: u32 = 0;
    count_files_by_extension(root, &mut ext_counts, &mut total_files, 0).await;

    // 2. Rileva linguaggi principali
    let languages = detect_languages(&ext_counts);
    // 3. Rileva framework e build system
    let frameworks = detect_frameworks(root).await;
    // 4. Legge dipendenze da manifest files
    let dependencies = read_dependencies(root).await;
    // 5. Stato git
    let git_info = collect_git_info(root, context).await;
    // 6. Informazioni strutturali
    let structure = detect_structure_info(root);

    ProjectMetadata {
        ext_counts,
        total_files,
        languages,
        frameworks,
        dependencies,
        git_info,
        structure,
    }
}

/// Persiste `analysis_json` + `analyzed_at` sul progetto e imposta le
/// `custom_instructions` (solo se ancora nulle) generandole dallo stack
/// rilevato. Estratto da `analyze_project` (comportamento invariato: l'errore
/// SQL resta ignorato come nell'originale).
async fn persist_analysis(
    state: &AppState,
    project_id: Uuid,
    root: &Path,
    analysis: &Value,
    frameworks: &[String],
    dependencies: &Value,
) {
    let custom_instructions = auto_generate_custom_instructions(root, frameworks, dependencies);
    let _ = sqlx::query(
        "UPDATE projects SET analysis_json = $2, analyzed_at = NOW(),
         custom_instructions = COALESCE(custom_instructions, $3)
         WHERE id = $1",
    )
    .bind(project_id)
    .bind(analysis)
    .bind(custom_instructions.as_deref())
    .execute(&state.db)
    .await;
}

/// Costruisce il blocco `git` dell'analisi: stato porcelain + remotes se il
/// progetto e' un repo, altrimenti solo `isGitRepo: false`. Estratto da
/// `analyze_project` (comportamento invariato).
async fn collect_git_info(root: &Path, context: &ProjectContext) -> Value {
    if !context.is_git_repo {
        return json!({ "isGitRepo": false });
    }
    let (status_out, _) = run_git_command(root, &["status", "--porcelain=v1", "-b"])
        .await
        .unwrap_or_default();
    let (remote_out, _) = run_git_command(root, &["remote", "-v"])
        .await
        .unwrap_or_default();
    let lines: Vec<&str> = status_out.lines().collect();
    let dirty_count = lines.iter().filter(|l| !l.starts_with("##")).count();
    json!({
        "isGitRepo": true,
        "branch": context.current_branch,
        "dirtyFiles": dirty_count,
        "remotes": remote_out.lines().collect::<Vec<_>>(),
    })
}

/// Costruisce il blocco `structure` dell'analisi (presenza di README, gitignore,
/// license, CI). Estratto da `analyze_project` (comportamento invariato).
fn detect_structure_info(root: &Path) -> Value {
    let has_readme = root.join("README.md").is_file() || root.join("readme.md").is_file();
    let has_gitignore = root.join(".gitignore").is_file();
    let has_license = root.join("LICENSE").is_file() || root.join("LICENSE.md").is_file();
    let has_ci = root.join(".github/workflows").is_dir()
        || root.join(".gitlab-ci.yml").is_file()
        || root.join("Jenkinsfile").is_file();
    json!({
        "hasReadme": has_readme,
        "hasGitignore": has_gitignore,
        "hasLicense": has_license,
        "hasCi": has_ci,
    })
}

/// Avvia in background l'indicizzazione dei file di codice (evita timeout HTTP
/// su repo grandi). Estratto da `analyze_project` (comportamento invariato).
fn spawn_code_index(state: &AppState, project_id: Uuid, root: &Path) {
    let root_buf = root.to_path_buf();
    let state_bg = state.clone();
    tokio::spawn(async move {
        let _ = index_project_code_files(&state_bg, project_id, &root_buf).await;
        tracing::info!(
            "code index background: completato per progetto {}",
            project_id
        );
    });
}

/// Avvia in background la creazione automatica dei profili in base allo stack
/// rilevato. Estratto da `analyze_project` (comportamento invariato).
fn spawn_profile_creation(
    state: &AppState,
    user_id: Uuid,
    context: &ProjectContext,
    frameworks: &[String],
    languages: &[Value],
) {
    let db = state.db.clone();
    let cache = state.template_cache.clone();
    let detected_frameworks = frameworks.to_vec();
    let detected_languages: Vec<String> = languages
        .iter()
        .filter_map(|v| {
            v.get("language")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let project_name = context.details.name.clone();
    tokio::spawn(async move {
        auto_create_profiles_from_analysis(
            &db,
            &cache,
            user_id,
            &project_name,
            &detected_languages,
            &detected_frameworks,
        )
        .await;
    });
}

/// Pre-calcola i suggerimenti run-config e li salva in cache, in background.
/// Estratto da `analyze_project` (comportamento invariato).
fn spawn_run_config_suggestions(state: &AppState, project_id: Uuid, root: &Path) {
    let db = state.db.clone();
    let root_bg = root.to_path_buf();
    tokio::spawn(async move {
        let suggestions = crate::project_workspace::compute_run_config_suggestions(&root_bg);
        crate::project_workspace::save_suggestions_cache(&db, project_id, &suggestions).await;
    });
}

// ── Funzioni di analisi helper (pub per uso in crud e indexing) ───────────────

pub async fn count_files_by_extension(
    dir: &Path,
    counts: &mut BTreeMap<String, u32>,
    total: &mut u32,
    depth: u32,
) {
    if depth > 6 {
        return;
    }
    let mut entries = match fs::read_dir(dir).await {
        Ok(e) => e,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if EXCLUDED_NAMES.contains(&name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            Box::pin(count_files_by_extension(&path, counts, total, depth + 1)).await;
        } else if path.is_file() {
            *total += 1;
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                *counts.entry(ext.to_lowercase()).or_insert(0) += 1;
            }
        }
    }
}

pub fn detect_languages(ext_counts: &BTreeMap<String, u32>) -> Vec<Value> {
    let lang_map: &[(&[&str], &str)] = &[
        (&["rs"], "Rust"),
        (&["ts", "tsx"], "TypeScript"),
        (&["js", "jsx", "mjs", "cjs"], "JavaScript"),
        (&["py", "pyi"], "Python"),
        (&["java"], "Java"),
        (&["go"], "Go"),
        (&["cs"], "C#"),
        (&["cpp", "cc", "cxx", "c", "h", "hpp"], "C/C++"),
        (&["rb"], "Ruby"),
        (&["php"], "PHP"),
        (&["swift"], "Swift"),
        (&["kt", "kts"], "Kotlin"),
        (&["sql"], "SQL"),
        (&["html", "htm"], "HTML"),
        (&["css", "scss", "sass", "less"], "CSS"),
        (&["sh", "bash", "zsh"], "Shell"),
        (&["md", "mdx"], "Markdown"),
        (&["json", "yaml", "yml", "toml"], "Config"),
    ];

    let mut results: Vec<(String, u32)> = Vec::new();
    for (exts, lang) in lang_map {
        let count: u32 = exts.iter().filter_map(|e| ext_counts.get(*e)).sum();
        if count > 0 {
            results.push((lang.to_string(), count));
        }
    }
    results.sort_by_key(|r| std::cmp::Reverse(r.1));
    results
        .into_iter()
        .map(|(lang, count)| json!({ "language": lang, "fileCount": count }))
        .collect()
}

/// Raccoglie root + sottodirectory fino a 2 livelli, escludendo cartelle di build/artifact.
pub async fn collect_search_dirs(root: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![root.to_path_buf()];
    let Ok(mut l1) = fs::read_dir(root).await else {
        return dirs;
    };
    while let Ok(Some(e)) = l1.next_entry().await {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if EXCLUDED_BUILD_DIRS.contains(&name) {
            continue;
        }
        dirs.push(p.clone());
        // secondo livello
        if let Ok(mut l2) = fs::read_dir(&p).await {
            while let Ok(Some(e2)) = l2.next_entry().await {
                let p2 = e2.path();
                if !p2.is_dir() {
                    continue;
                }
                let name2 = p2.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if EXCLUDED_BUILD_DIRS.contains(&name2) {
                    continue;
                }
                dirs.push(p2);
            }
        }
    }
    dirs
}

/// Controlla se uno dei dirs contiene un file con certa estensione.
pub fn has_extension_in_dirs(dirs: &[std::path::PathBuf], ext: &str) -> bool {
    dirs.iter().any(|d| {
        std::fs::read_dir(d).ok().is_some_and(|mut e| {
            e.any(|entry| {
                entry.ok().is_some_and(|e| {
                    e.path().extension().and_then(|x| x.to_str()) == Some(ext)
                })
            })
        })
    })
}

/// Tabella (file/directory marker -> tecnologia) usata da
/// `detect_frameworks_by_marker_files`. Estratta come costante a livello di
/// modulo perche' e' pura configurazione dati: mantenerla dentro la funzione ne
/// gonfiava la lunghezza oltre soglia senza aggiungere logica.
const FRAMEWORK_MARKER_FILES: &[(&str, &str)] = &[
        // JavaScript/TypeScript
        ("package.json", "Node.js"),
        ("next.config.js", "Next.js"),
        ("next.config.ts", "Next.js"),
        ("next.config.mjs", "Next.js"),
        ("next.config.cjs", "Next.js"),
        ("nuxt.config.ts", "Nuxt"),
        ("nuxt.config.js", "Nuxt"),
        ("angular.json", "Angular"),
        ("vue.config.js", "Vue.js"),
        ("vue.config.ts", "Vue.js"),
        ("vite.config.ts", "Vite"),
        ("vite.config.js", "Vite"),
        ("vite.config.mts", "Vite"),
        ("svelte.config.js", "SvelteKit"),
        ("svelte.config.ts", "SvelteKit"),
        ("remix.config.js", "Remix"),
        ("gatsby-config.js", "Gatsby"),
        ("gatsby-config.ts", "Gatsby"),
        ("astro.config.mjs", "Astro"),
        ("astro.config.ts", "Astro"),
        ("solid.config.ts", "SolidJS"),
        ("qwik.config.ts", "Qwik"),
        ("expo-app.json", "Expo/React Native"),
        ("metro.config.js", "React Native"),
        ("electron-builder.yml", "Electron"),
        ("electron.vite.config.ts", "Electron+Vite"),
        ("tauri.conf.json", "Tauri"),
        // Build tools JS
        ("webpack.config.js", "Webpack"),
        ("webpack.config.ts", "Webpack"),
        ("rollup.config.js", "Rollup"),
        ("rollup.config.ts", "Rollup"),
        ("esbuild.config.js", "esbuild"),
        ("jest.config.js", "Jest"),
        ("jest.config.ts", "Jest"),
        ("vitest.config.ts", "Vitest"),
        ("playwright.config.ts", "Playwright"),
        ("cypress.config.ts", "Cypress"),
        ("tailwind.config.js", "Tailwind CSS"),
        ("tailwind.config.ts", "Tailwind CSS"),
        ("postcss.config.js", "PostCSS"),
        ("tsconfig.json", "TypeScript"),
        ("jsconfig.json", "JavaScript"),
        ("turbo.json", "Turborepo"),
        ("nx.json", "Nx"),
        ("pnpm-workspace.yaml", "pnpm Workspaces"),
        ("lerna.json", "Lerna"),
        ("rush.json", "Rush"),
        ("deno.json", "Deno"),
        ("deno.jsonc", "Deno"),
        ("bun.lockb", "Bun"),
        // Rust
        ("Cargo.toml", "Rust/Cargo"),
        // Python
        ("requirements.txt", "Python"),
        ("requirements-dev.txt", "Python"),
        ("pyproject.toml", "Python"),
        ("setup.py", "Python"),
        ("setup.cfg", "Python"),
        ("Pipfile", "Python/Pipenv"),
        ("poetry.lock", "Poetry"),
        ("uv.lock", "uv (Python)"),
        ("conda.yaml", "Conda"),
        ("environment.yml", "Conda"),
        ("Procfile", "Heroku/Process"),
        // Go
        ("go.mod", "Go"),
        // Java/JVM
        ("pom.xml", "Java/Maven"),
        ("build.gradle", "Java/Gradle"),
        ("build.gradle.kts", "Kotlin/Gradle"),
        ("settings.gradle", "Java/Gradle"),
        ("settings.gradle.kts", "Kotlin/Gradle"),
        ("gradlew", "Java/Gradle"),
        ("mvnw", "Java/Maven"),
        ("build.xml", "Java/Ant"),
        ("ivy.xml", "Java/Ivy"),
        // Scala
        ("build.sbt", "Scala/SBT"),
        // Kotlin
        ("build.gradle.kts", "Kotlin"),
        // Clojure
        ("project.clj", "Clojure/Leiningen"),
        ("deps.edn", "Clojure/tools.deps"),
        // Ruby
        ("Gemfile", "Ruby/Bundler"),
        ("Rakefile", "Ruby/Rake"),
        ("config/application.rb", "Ruby on Rails"),
        // PHP
        ("composer.json", "PHP/Composer"),
        ("artisan", "Laravel"),
        ("symfony.lock", "Symfony"),
        // Swift/iOS/macOS
        ("Package.swift", "Swift/SPM"),
        ("Podfile", "CocoaPods"),
        ("Cartfile", "Carthage"),
        // C/C++
        ("CMakeLists.txt", "C++/CMake"),
        ("conanfile.txt", "C++/Conan"),
        ("conanfile.py", "C++/Conan"),
        ("vcpkg.json", "C++/vcpkg"),
        ("meson.build", "C++/Meson"),
        ("SConstruct", "C++/SCons"),
        ("configure.ac", "Autotools"),
        ("configure", "Autotools"),
        // Makefile (generico)
        ("Makefile", "Make"),
        ("GNUmakefile", "Make"),
        // Containerizzazione
        ("Dockerfile", "Docker"),
        ("docker-compose.yml", "Docker Compose"),
        ("docker-compose.yaml", "Docker Compose"),
        ("docker-compose.override.yml", "Docker Compose"),
        ("compose.yml", "Docker Compose"),
        ("compose.yaml", "Docker Compose"),
        (".dockerignore", "Docker"),
        ("helm/Chart.yaml", "Helm"),
        ("Chart.yaml", "Helm"),
        ("skaffold.yaml", "Skaffold"),
        ("k8s", "Kubernetes"),
        ("kubernetes", "Kubernetes"),
        ("kustomization.yaml", "Kustomize"),
        ("kustomization.yml", "Kustomize"),
        // CI/CD
        (".github/workflows", "GitHub Actions"),
        (".gitlab-ci.yml", "GitLab CI"),
        (".gitlab-ci.yaml", "GitLab CI"),
        ("Jenkinsfile", "Jenkins"),
        (".circleci/config.yml", "CircleCI"),
        (".travis.yml", "Travis CI"),
        ("bitbucket-pipelines.yml", "Bitbucket Pipelines"),
        ("azure-pipelines.yml", "Azure Pipelines"),
        (".drone.yml", "Drone CI"),
        ("Earthfile", "Earthly"),
        // IaC
        ("terraform.tf", "Terraform"),
        ("main.tf", "Terraform"),
        ("variables.tf", "Terraform"),
        ("Pulumi.yaml", "Pulumi"),
        ("serverless.yml", "Serverless Framework"),
        ("serverless.yaml", "Serverless Framework"),
        ("cdk.json", "AWS CDK"),
        ("samconfig.toml", "AWS SAM"),
        ("template.yaml", "AWS SAM"),
        ("ansible.cfg", "Ansible"),
        ("site.yml", "Ansible"),
        ("playbook.yml", "Ansible"),
        // Database/ORM
        ("prisma/schema.prisma", "Prisma"),
        ("schema.prisma", "Prisma"),
        ("drizzle.config.ts", "Drizzle ORM"),
        ("knexfile.js", "Knex.js"),
        ("knexfile.ts", "Knex.js"),
        ("typeorm.config.ts", "TypeORM"),
        ("ormconfig.json", "TypeORM"),
        ("alembic.ini", "Alembic"),
        ("flyway.conf", "Flyway"),
        ("liquibase.properties", "Liquibase"),
        // Other
        ("proto", "gRPC/Protobuf"),
        ("*.proto", "gRPC/Protobuf"),
        ("graphql", "GraphQL"),
        ("schema.graphql", "GraphQL"),
        (".storybook", "Storybook"),
        ("storybook.main.ts", "Storybook"),
        ("wrangler.toml", "Cloudflare Workers"),
        ("wrangler.json", "Cloudflare Workers"),
        ("netlify.toml", "Netlify"),
        ("vercel.json", "Vercel"),
        (".vercelignore", "Vercel"),
        ("firebase.json", "Firebase"),
        (".firebaserc", "Firebase"),
        ("supabase/config.toml", "Supabase"),
        ("convex/convex.json", "Convex"),
        ("convex.json", "Convex"),
        ("shopify.app.toml", "Shopify"),
        ("Gemfile.lock", "Ruby/Bundler"),
        ("mix.exs", "Elixir/Mix"),
        ("rebar.config", "Erlang/rebar3"),
        ("stack.yaml", "Haskell/Stack"),
        ("cabal.project", "Haskell/Cabal"),
        ("zig.build", "Zig"),
        ("build.zig", "Zig"),
        ("lua", "Lua"),
        ("rockspec", "Lua/LuaRocks"),
        ("pubspec.yaml", "Flutter/Dart"),
        ("pubspec.yml", "Flutter/Dart"),
        ("android/build.gradle", "Android"),
        ("ios/Podfile", "iOS"),
        ("capacitor.config.ts", "Capacitor"),
        ("ionic.config.json", "Ionic"),
        ("nativescript.config.ts", "NativeScript"),
        (".eslintrc.js", "ESLint"),
        (".eslintrc.json", "ESLint"),
        ("eslint.config.js", "ESLint"),
        ("eslint.config.mjs", "ESLint"),
        (".prettierrc", "Prettier"),
        ("prettier.config.js", "Prettier"),
        ("biome.json", "Biome"),
        ("oxlint.json", "oxlint"),
];

/// Rileva i framework in base alla presenza di file/directory marker in ciascun
/// dir candidato, piu' i casi speciali Python (Django/Flask/FastAPI) che
/// richiedono di ispezionare `requirements.txt`. Muta `found` in place.
/// Estratto da `detect_frameworks` per mantenerlo sotto le soglie di
/// lunghezza/complessita (comportamento invariato).
fn detect_frameworks_by_marker_files(
    dirs: &[std::path::PathBuf],
    found: &mut std::collections::HashSet<String>,
) {
    for dir in dirs {
        for (file, tech) in FRAMEWORK_MARKER_FILES {
            if found.contains(*tech) {
                continue;
            }
            let p = dir.join(file);
            if p.exists() {
                found.insert(tech.to_string());
            }
        }
        detect_python_web_frameworks(dir, found);
    }
}

/// Casi speciali Python: Django (manage.py), Flask e FastAPI (presenza di file
/// entry-point + dipendenza dichiarata in requirements.txt). Muta `found`.
fn detect_python_web_frameworks(
    dir: &std::path::Path,
    found: &mut std::collections::HashSet<String>,
) {
    // Django
    if !found.contains("Django") && dir.join("manage.py").exists() {
        found.insert("Django".to_string());
    }
    // Flask
    if !found.contains("Flask")
        && (dir.join("app.py").exists()
            || dir.join("wsgi.py").exists()
            || dir.join("application.py").exists())
    {
        if let Ok(content) = std::fs::read_to_string(dir.join("requirements.txt")) {
            if content.to_lowercase().contains("flask") {
                found.insert("Flask".to_string());
            }
        }
    }
    // FastAPI
    if !found.contains("FastAPI")
        && (dir.join("main.py").exists() || dir.join("app.py").exists())
    {
        if let Ok(content) = std::fs::read_to_string(dir.join("requirements.txt")) {
            if content.to_lowercase().contains("fastapi") {
                found.insert("FastAPI".to_string());
            }
        }
    }
}

/// Rileva framework/linguaggi in base alla presenza di file con estensioni
/// caratteristiche (.sln/.csproj/.fsproj/.vbproj/.proto/.graphql). Muta `found`.
fn detect_frameworks_by_extension(
    dirs: &[std::path::PathBuf],
    found: &mut std::collections::HashSet<String>,
) {
    if has_extension_in_dirs(dirs, "sln") || has_extension_in_dirs(dirs, "csproj") {
        found.insert(".NET/C#".to_string());
    }
    if has_extension_in_dirs(dirs, "fsproj") {
        found.insert("F#/.NET".to_string());
    }
    if has_extension_in_dirs(dirs, "vbproj") {
        found.insert("VB.NET".to_string());
    }
    if has_extension_in_dirs(dirs, "proto") {
        found.insert("gRPC/Protobuf".to_string());
    }
    if has_extension_in_dirs(dirs, "graphql") || has_extension_in_dirs(dirs, "gql") {
        found.insert("GraphQL".to_string());
    }
}

/// Estrae i nomi delle dipendenze (dependencies + devDependencies +
/// peerDependencies) da un `package.json` gia' deserializzato.
fn package_json_dep_names(pkg: &Value) -> Vec<String> {
    let mut d = Vec::new();
    for key in &["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(obj) = pkg.get(key).and_then(|v| v.as_object()) {
            d.extend(obj.keys().cloned());
        }
    }
    d
}

/// Tabella (nome dipendenza npm -> tecnologia) usata da
/// `detect_frameworks_by_package_json`. Estratta come costante a livello di
/// modulo (pura configurazione dati) per non gonfiare la funzione.
const FRAMEWORK_PACKAGE_DEPS: &[(&str, &str)] = &[
            ("react", "React"),
            ("react-dom", "React"),
            ("vue", "Vue.js"),
            ("@angular/core", "Angular"),
            ("svelte", "Svelte"),
            ("solid-js", "SolidJS"),
            ("preact", "Preact"),
            ("@qwikdev/qwik", "Qwik"),
            ("express", "Express.js"),
            ("fastify", "Fastify"),
            ("koa", "Koa"),
            ("hono", "Hono"),
            ("@nestjs/core", "NestJS"),
            ("@hapi/hapi", "Hapi"),
            ("socket.io", "Socket.io"),
            ("axios", "Axios"),
            ("@tanstack/query", "TanStack Query"),
            ("redux", "Redux"),
            ("mobx", "MobX"),
            ("zustand", "Zustand"),
            ("jotai", "Jotai"),
            ("recoil", "Recoil"),
            ("@mui/material", "Material UI"),
            ("antd", "Ant Design"),
            ("@chakra-ui/react", "Chakra UI"),
            ("@shadcn/ui", "shadcn/ui"),
            ("framer-motion", "Framer Motion"),
            ("three", "Three.js"),
            ("d3", "D3.js"),
            ("recharts", "Recharts"),
            ("chart.js", "Chart.js"),
            ("mongoose", "Mongoose"),
            ("typeorm", "TypeORM"),
            ("prisma", "Prisma"),
            ("drizzle-orm", "Drizzle ORM"),
            ("sequelize", "Sequelize"),
            ("knex", "Knex.js"),
            ("pg", "PostgreSQL (pg)"),
            ("mysql2", "MySQL"),
            ("ioredis", "Redis (ioredis)"),
            ("redis", "Redis"),
            ("mongodb", "MongoDB"),
            ("graphql", "GraphQL"),
            ("@apollo/client", "Apollo GraphQL"),
            ("@trpc/client", "tRPC"),
            ("zod", "Zod"),
            ("yup", "Yup"),
            ("stripe", "Stripe"),
            ("@supabase/supabase-js", "Supabase"),
            ("firebase", "Firebase"),
            ("aws-sdk", "AWS SDK"),
            ("@aws-sdk/client-s3", "AWS SDK v3"),
            ("openai", "OpenAI SDK"),
            ("langchain", "LangChain"),
            ("electron", "Electron"),
            ("@tauri-apps/api", "Tauri"),
            ("react-native", "React Native"),
];

/// Rileva framework in base alle dipendenze dichiarate nei `package.json`
/// presenti nei dir candidati. Muta `found`.
fn detect_frameworks_by_package_json(
    dirs: &[std::path::PathBuf],
    found: &mut std::collections::HashSet<String>,
) {
    for dir in dirs {
        let pkg_path = dir.join("package.json");
        if !pkg_path.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&pkg_path) else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        let all_deps = package_json_dep_names(&pkg);
        for (dep, tech) in FRAMEWORK_PACKAGE_DEPS {
            if !found.contains(*tech) && all_deps.iter().any(|d| d == dep) {
                found.insert(tech.to_string());
            }
        }
    }
}

pub async fn detect_frameworks(root: &Path) -> Vec<String> {
    let mut found: std::collections::HashSet<String> = std::collections::HashSet::new();
    let dirs = collect_search_dirs(root).await;

    detect_frameworks_by_marker_files(&dirs, &mut found);
    detect_frameworks_by_extension(&dirs, &mut found);
    detect_frameworks_by_package_json(&dirs, &mut found);

    let mut result: Vec<String> = found.into_iter().collect();
    result.sort();
    result
}

/// Raccoglie i percorsi `package.json` candidati (root + sottodirectory di
/// primo livello, escluse le cartelle di build/artifact) con il relativo subdir.
async fn collect_package_json_candidates(root: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut pkg_candidates: Vec<(std::path::PathBuf, String)> = Vec::new();
    pkg_candidates.push((root.join("package.json"), ".".to_string()));
    if let Ok(mut entries) = fs::read_dir(root).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.is_dir() {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if !matches!(
                    name.as_str(),
                    "node_modules" | ".git" | "target" | "dist" | "build" | ".next" | "obj" | "bin"
                ) {
                    pkg_candidates.push((p.join("package.json"), name));
                }
            }
        }
    }
    pkg_candidates
}

/// Aggrega deps/devDeps/scripts di tutti i `package.json` candidati in un blocco
/// `node`, oppure `Value::Null` se non c'e' nulla di rilevante.
async fn aggregate_node_dependencies(root: &Path) -> Value {
    let pkg_candidates = collect_package_json_candidates(root).await;

    let mut all_scripts: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut total_deps = 0usize;
    let mut total_dev_deps = 0usize;
    for (pkg_path, subdir) in &pkg_candidates {
        let Ok(content) = fs::read_to_string(pkg_path).await else {
            continue;
        };
        let Ok(pkg) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        total_deps += object_len(&pkg, "dependencies");
        total_dev_deps += object_len(&pkg, "devDependencies");
        if let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) {
            for (k, v) in scripts {
                let key = if subdir == "." {
                    k.clone()
                } else {
                    format!("{subdir}/{k}")
                };
                all_scripts.insert(key, v.clone());
            }
        }
    }
    if total_deps > 0 || total_dev_deps > 0 || !all_scripts.is_empty() {
        json!({
            "dependencies": total_deps,
            "devDependencies": total_dev_deps,
            "scripts": all_scripts,
        })
    } else {
        Value::Null
    }
}

/// Numero di chiavi nell'oggetto JSON figlio `key`, o 0 se assente/non-oggetto.
fn object_len(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(|d| d.as_object())
        .map(|o| o.len())
        .unwrap_or(0)
}

pub async fn read_dependencies(root: &Path) -> Value {
    let mut deps = json!({});

    let node = aggregate_node_dependencies(root).await;
    if !node.is_null() {
        deps["node"] = node;
    }

    // Cargo.toml: stima grezza contando le righe chiave=valore fuori dalle sezioni.
    if let Ok(content) = fs::read_to_string(root.join("Cargo.toml")).await {
        let dep_lines = content
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.starts_with('#') && !t.starts_with('[') && t.contains('=') && !t.is_empty()
            })
            .count();
        deps["rust"] = json!({ "estimatedDependencies": dep_lines });
    }

    // requirements.txt: conta le righe non vuote e non commentate.
    if let Ok(content) = fs::read_to_string(root.join("requirements.txt")).await {
        let count = content
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .count();
        deps["python"] = json!({ "requirements": count });
    }

    deps
}
