# Audit system prompt -- token weights

Generato: 2026-05-07T19:56:33

## Sintesi

- Template attivi: **98**
- Token totali: **24,518**
- Token medio per template: **250**
- Template oltre soglia warning (2000 tok): **4**

## Sopra soglia warning (2000 tok)

| Key | Categoria | Tokens | Issues |
|-----|-----------|-------:|--------|
| `agent.general.debugger` | agent | 3,097 | - |
| `agent.coder.base` | agent | 2,735 | - |
| `system.nexus_base` | system | 2,200 | - |
| `agent.project.analyzer` | agent | 2,079 | - |

## Top 10 template piu' pesanti (>= 1000 tok)

| Key | Categoria | Schema | Tokens | Char | Issues |
|-----|-----------|--------|-------:|-----:|--------|
| `agent.general.debugger` | agent | xml | 3,097 | 10,542 | - |
| `agent.coder.base` | agent | xml | 2,735 | 9,386 | - |
| `system.nexus_base` | system | plain | 2,200 | 7,676 | - |
| `agent.project.analyzer` | agent | xml | 2,079 | 7,347 | - |
| `agent.reviewer.general` | agent | xml | 1,783 | 6,082 | - |
| `agent.tester.base` | agent | xml | 1,629 | 5,650 | - |

## Tutti i template (ordine alfabetico)

| Key | Tokens | Char |
|-----|-------:|-----:|
| `agent.architect.api_design` | 145 | 534 |
| `agent.architect.database_schema` | 147 | 544 |
| `agent.architect.general` | 130 | 469 |
| `agent.architect.system_architecture` | 165 | 606 |
| `agent.coder.base` | 2,735 | 9,386 |
| `agent.general.accessibility_engineer` | 91 | 451 |
| `agent.general.automation_engineer` | 74 | 399 |
| `agent.general.benchmark_engineer` | 89 | 442 |
| `agent.general.chatbot_engineer` | 77 | 409 |
| `agent.general.compliance_officer` | 75 | 383 |
| `agent.general.database_admin` | 91 | 441 |
| `agent.general.data_engineer` | 89 | 430 |
| `agent.general.debugger` | 3,097 | 10,542 |
| `agent.general.embedding_engineer` | 92 | 446 |
| `agent.general.etl_engineer` | 90 | 425 |
| `agent.general.i18n_engineer` | 90 | 462 |
| `agent.general.infra_engineer` | 87 | 417 |
| `agent.general.integration_engineer` | 89 | 459 |
| `agent.general.migration_engineer` | 73 | 426 |
| `agent.general.monitoring_engineer` | 88 | 419 |
| `agent.general.product_owner` | 78 | 436 |
| `agent.general.profiler` | 77 | 419 |
| `agent.general.refactorer` | 79 | 456 |
| `agent.general.reporting_engineer` | 83 | 418 |
| `agent.general.security_auditor` | 87 | 445 |
| `agent.general.tech_writer` | 89 | 448 |
| `agent.general.test_automation_engineer` | 87 | 436 |
| `agent.general.ui_designer` | 92 | 453 |
| `agent.github.actions_optimizer` | 59 | 345 |
| `agent.github.code_reviewer` | 81 | 384 |
| `agent.github.dependency_manager` | 74 | 364 |
| `agent.github.discussion_moderator` | 57 | 324 |
| `agent.github.integration_bot` | 75 | 370 |
| `agent.github.issue_analyzer` | 77 | 379 |
| `agent.github.pr_manager` | 76 | 393 |
| `agent.github.project_manager` | 60 | 308 |
| `agent.github.release_manager` | 75 | 362 |
| `agent.github.security_analyzer` | 75 | 420 |
| `agent.github.status_monitor` | 70 | 368 |
| `agent.github.wiki_manager` | 55 | 333 |
| `agent.github.workflow_manager` | 66 | 364 |
| `agent.project.analyzer` | 2,079 | 7,347 |
| `agent.reviewer.bug_detection` | 120 | 464 |
| `agent.reviewer.code_review` | 112 | 434 |
| `agent.reviewer.general` | 1,783 | 6,082 |
| `agent.reviewer.security_audit` | 141 | 547 |
| `agent.specialized.agent_engineer` | 82 | 426 |
| `agent.specialized.analyst` | 58 | 339 |
| `agent.specialized.api_designer` | 86 | 420 |
| `agent.specialized.backend_specialist` | 85 | 395 |
| `agent.specialized.cloud_architect` | 73 | 378 |
| `agent.specialized.database_designer` | 75 | 409 |
| `agent.specialized.data_scientist` | 89 | 398 |
| `agent.specialized.devops_engineer` | 79 | 372 |
| `agent.specialized.documenter` | 73 | 367 |
| `agent.specialized.frontend_specialist` | 89 | 393 |
| `agent.specialized.ml_engineer` | 96 | 400 |
| `agent.specialized.mobile_specialist` | 76 | 416 |
| `agent.specialized.optimizer` | 63 | 339 |
| `agent.specialized.performance_engineer` | 86 | 437 |
| `agent.specialized.prompt_engineer` | 84 | 402 |
| `agent.specialized.qa_specialist` | 75 | 411 |
| `agent.specialized.researcher` | 66 | 382 |
| `agent.specialized.security_architect` | 88 | 397 |
| `agent.specialized.sre_engineer` | 95 | 420 |
| `agent.specialized.tech_lead` | 70 | 427 |
| `agent.tester.base` | 1,629 | 5,650 |
| `automation.learning_bundle_format` | 14 | 69 |
| `automation.mode_automatic_instruction` | 119 | 334 |
| `automation.mode_confirm_instruction` | 48 | 178 |
| `automation.mode_study_instruction` | 47 | 165 |
| `automation.profile_system_prompt_generator` | 199 | 705 |
| `automation.run_resume_instruction` | 64 | 261 |
| `automation.supervisor_monitoring` | 739 | 2,539 |
| `chat.feedback_assist` | 214 | 721 |
| `chat.precheck_message` | 591 | 1,918 |
| `docs.er_diagram` | 181 | 621 |
| `docs.functional_analysis` | 704 | 2,184 |
| `docs.project_management` | 180 | 603 |
| `docs.release_notes` | 180 | 578 |
| `docs.technical_analysis` | 212 | 694 |
| `profile.data_science_ml` | 74 | 288 |
| `profile.developer_csharp_dotnet` | 102 | 390 |
| `profile.developer_mobile` | 80 | 330 |
| `profile.developer_python` | 77 | 284 |
| `profile.developer_react_typescript` | 83 | 387 |
| `profile.developer_rust` | 79 | 306 |
| `profile.developer_vue_nuxt` | 64 | 237 |
| `profile.devops_infrastructure` | 96 | 384 |
| `quality.deep_review_code_analysis` | 80 | 301 |
| `quality.n_plus_one` | 55 | 260 |
| `system.architect` | 235 | 895 |
| `system.coder` | 227 | 794 |
| `system.documenter` | 236 | 852 |
| `system.nexus_base` | 2,200 | 7,676 |
| `system.reviewer` | 218 | 788 |
| `system.security_auditor` | 265 | 991 |
| `system.tester` | 217 | 790 |

## Raccomandazioni

Per ogni template segnalato:
1. Rimuovere preamboli conversazionali ('Sei un agente...')
2. Sostituire 2+ esempi con 1 solo paradigmatico
3. Compattare bullet point >200 char in frasi singole
4. Verificare coerenza con `<safety_progetto>` (mig 0096)
5. Riferimento target: < 1000 token per template specializzato
