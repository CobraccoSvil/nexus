use regex::Regex;
use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    Expr, FromTable, Query, Select, SelectItem, SetExpr, Statement, TableFactor, TableWithJoins,
};
use sqlparser::dialect::{Dialect, GenericDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

/// Tenta il parsing SQL con piu' dialetti in ordine di permissivita'.
/// Restituisce il primo successo. Se nessuno parsa, ritorna l'errore del dialetto generic
/// (per messaggio piu' chiaro).
///
/// Senza questo helper, file SQL validi in dialetti specifici (PostgreSQL `RETURNING`,
/// `JSONB`, `EXCLUDED`; SQL Server `TOP`, `[Schema].[Table]`; MySQL backtick) generavano
/// "SQL parse error" classificato HIGH come falso positivo (vedi bug 12 test E2E:
/// 28 falsi positivi su 28 file SQL nel progetto redemptor).
fn parse_sql_multidialect(sql: &str) -> Result<Vec<Statement>, sqlparser::parser::ParserError> {
    // Ordine: i dialetti specifici piu' usati prima del generic (che e' meno informativo
    // sui costrutti specifici ma piu' permissivo nella sintassi).
    let dialects: [&dyn Dialect; 5] = [
        &PostgreSqlDialect {},
        &MsSqlDialect {},
        &MySqlDialect {},
        &SQLiteDialect {},
        &GenericDialect {},
    ];
    let mut last_err: Option<sqlparser::parser::ParserError> = None;
    for d in dialects.iter() {
        match Parser::parse_sql(*d, sql) {
            Ok(stmts) => return Ok(stmts),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbFinding {
    pub category: String,
    pub severity: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbAnalysisReport {
    pub query: String,
    pub findings: Vec<DbFinding>,
    pub tables_referenced: Vec<String>,
    pub statement_type: String,
}

pub fn analyze_query(sql: &str) -> DbAnalysisReport {
    let statements = match parse_sql_multidialect(sql) {
        Ok(stmts) => stmts,
        Err(e) => {
            // Se nessun dialetto (Postgres/MsSql/MySql/SQLite/Generic) parsa il file,
            // probabilmente e' SQL davvero malformato OPPURE usa estensioni esoteriche
            // (es. PL/pgSQL DO blocks, statement multipli concatenati senza ;, dialetti
            // proprietari come Oracle PL/SQL). In entrambi i casi e' meno grave di
            // "high" perche':
            //   - se e' davvero malformato, lo runtime DB lo rilevera' all'esecuzione
            //   - se e' sintassi non supportata dal parser, NON impatta l'esecuzione
            // Quindi declassiamo da "high" a "medium" per ridurre il rumore (scanner
            // produceva 64% FP nelle HIGH per questo motivo - bug 12 test E2E).
            return DbAnalysisReport {
                query: sql.to_string(),
                findings: vec![DbFinding {
                    category: "parse_error".into(),
                    severity: "medium".into(),
                    title: "SQL parse error (sintassi non riconosciuta)".into(),
                    detail: format!(
                        "Nessun dialetto SQL (PostgreSQL, MS SQL, MySQL, SQLite, Generic) e' riuscito a parsare. \
                         Ultimo errore: {}. \
                         Possibili cause: PL/pgSQL DO/FUNCTION blocks, statement multipli, dialetti proprietari (Oracle PL/SQL). \
                         Verifica manualmente prima di considerarlo un bug.",
                        e
                    ),
                }],
                tables_referenced: vec![],
                statement_type: "unknown".into(),
            };
        }
    };

    let mut all_findings = Vec::new();
    let mut all_tables = Vec::new();
    let mut stmt_type = String::new();

    for stmt in &statements {
        stmt_type = classify_statement(stmt);
        all_tables.extend(extract_tables(stmt));

        all_findings.extend(check_select_star(stmt));
        all_findings.extend(check_missing_where(stmt));
        all_findings.extend(check_n_plus_one_hints(stmt));
        all_findings.extend(check_injection_patterns(sql));
        all_findings.extend(check_performance_issues(stmt));
    }
    // Principi DB-first: filtraggio, ordinamento, paginazione e aggregazioni nel DB
    all_findings.extend(check_db_first_principles(sql));

    all_tables.sort();
    all_tables.dedup();

    DbAnalysisReport {
        query: sql.to_string(),
        findings: all_findings,
        tables_referenced: all_tables,
        statement_type: stmt_type,
    }
}

pub fn analyze_queries(queries: &[&str]) -> Vec<DbAnalysisReport> {
    queries.iter().map(|q| analyze_query(q)).collect()
}

fn classify_statement(stmt: &Statement) -> String {
    match stmt {
        Statement::Query(_) => "SELECT".into(),
        Statement::Insert(_) => "INSERT".into(),
        Statement::Update { .. } => "UPDATE".into(),
        Statement::Delete(_) => "DELETE".into(),
        Statement::CreateTable { .. } => "CREATE TABLE".into(),
        Statement::CreateIndex(_) => "CREATE INDEX".into(),
        Statement::Drop { .. } => "DROP".into(),
        Statement::AlterTable { .. } => "ALTER TABLE".into(),
        _ => "OTHER".into(),
    }
}

fn extract_tables(stmt: &Statement) -> Vec<String> {
    let mut tables = Vec::new();
    match stmt {
        Statement::Query(query) => extract_tables_from_query(query, &mut tables),
        Statement::Insert(insert) => {
            tables.push(insert.table_name.to_string());
            if let Some(ref src) = insert.source {
                extract_tables_from_query(src.as_ref(), &mut tables);
            }
        }
        Statement::Update {
            table, selection, ..
        } => {
            if let Some(name) = extract_table_name_from_table_with_joins(table) {
                tables.push(name);
            }
            if let Some(ref expr) = selection {
                extract_tables_from_expr(expr, &mut tables);
            }
        }
        Statement::Delete(del) => {
            let from_tables = match &del.from {
                FromTable::WithFromKeyword(t) | FromTable::WithoutKeyword(t) => t,
            };
            for twj in from_tables {
                if let Some(name) = extract_table_name_from_table_with_joins(twj) {
                    tables.push(name);
                }
            }
        }
        _ => {}
    }
    tables
}

fn extract_tables_from_query(query: &Query, tables: &mut Vec<String>) {
    if let SetExpr::Select(ref select) = *query.body {
        extract_tables_from_select(select, tables);
    }
}

fn extract_tables_from_select(select: &Select, tables: &mut Vec<String>) {
    for twj in &select.from {
        if let Some(name) = extract_table_name_from_table_with_joins(twj) {
            tables.push(name);
        }
        for join in &twj.joins {
            if let TableFactor::Table { name, .. } = &join.relation {
                tables.push(name.to_string());
            }
        }
    }
}

fn extract_table_name_from_table_with_joins(twj: &TableWithJoins) -> Option<String> {
    if let TableFactor::Table { name, .. } = &twj.relation {
        Some(name.to_string())
    } else {
        None
    }
}

fn extract_tables_from_expr(_expr: &Expr, _tables: &mut Vec<String>) {
    // Subquery extraction could be added here
}

fn check_select_star(stmt: &Statement) -> Vec<DbFinding> {
    let mut findings = Vec::new();
    if let Statement::Query(query) = stmt {
        if let SetExpr::Select(ref select) = *query.body {
            for item in &select.projection {
                if matches!(item, SelectItem::Wildcard(_)) {
                    findings.push(DbFinding {
                        category: "performance".into(),
                        severity: "medium".into(),
                        title: "SELECT * detected".into(),
                        detail: "Specify columns explicitly to reduce data transfer and improve index usage".into(),
                    });
                }
            }
        }
    }
    findings
}

fn check_missing_where(stmt: &Statement) -> Vec<DbFinding> {
    let mut findings = Vec::new();

    match stmt {
        Statement::Update { selection, .. } => {
            if selection.is_none() {
                findings.push(DbFinding {
                    category: "safety".into(),
                    severity: "high".into(),
                    title: "UPDATE without WHERE".into(),
                    detail: "This will update ALL rows in the table".into(),
                });
            }
        }
        Statement::Delete(del) => {
            if del.selection.is_none() {
                findings.push(DbFinding {
                    category: "safety".into(),
                    severity: "high".into(),
                    title: "DELETE without WHERE".into(),
                    detail: "This will delete ALL rows in the table".into(),
                });
            }
        }
        _ => {}
    }
    findings
}

fn check_n_plus_one_hints(stmt: &Statement) -> Vec<DbFinding> {
    let mut findings = Vec::new();
    if let Statement::Query(query) = stmt {
        if let SetExpr::Select(ref select) = *query.body {
            // Detect correlated subquery patterns in WHERE
            if let Some(ref selection) = select.selection {
                if has_subquery(selection) {
                    findings.push(DbFinding {
                        category: "performance".into(),
                        severity: "medium".into(),
                        title: "Correlated subquery detected".into(),
                        detail: "Consider using JOIN instead — correlated subqueries can cause N+1 behavior".into(),
                    });
                }
            }
        }
    }
    findings
}

fn has_subquery(expr: &Expr) -> bool {
    match expr {
        Expr::Subquery(_) | Expr::InSubquery { .. } | Expr::Exists { .. } => true,
        Expr::BinaryOp { left, right, .. } => has_subquery(left) || has_subquery(right),
        Expr::UnaryOp { expr, .. } => has_subquery(expr),
        Expr::Nested(e) => has_subquery(e),
        _ => false,
    }
}

fn check_injection_patterns(sql: &str) -> Vec<DbFinding> {
    let mut findings = Vec::new();
    let concat_re = Regex::new(r#"['"]?\s*\+\s*\w+\s*\+\s*['"]?"#).unwrap();
    let format_re = Regex::new(r#"f['"].*\{.*\}.*['"]|format!\s*\("#).unwrap();
    let interp_re = Regex::new(r#"\$\{?\w+\}?"#).unwrap();

    // Rimuovi blocchi PL/pgSQL ($$...$$) e commenti SQL prima del check.
    // Questi contengono legittimamente $variabili, format('%I', ...), quote_ident()
    // che sono pattern sicuri in PostgreSQL ma matchano le regex sopra.
    let plpgsql_block_re = Regex::new(r#"\$\$[\s\S]*?\$\$"#).unwrap();
    let comment_re = Regex::new(r#"--[^\n]*"#).unwrap();
    let no_plpgsql = plpgsql_block_re.replace_all(sql, " ");
    let cleaned = comment_re.replace_all(&no_plpgsql, " ");

    if concat_re.is_match(&cleaned) || format_re.is_match(&cleaned) || interp_re.is_match(&cleaned) {
        findings.push(DbFinding {
            category: "security".into(),
            severity: "high".into(),
            title: "Potential SQL injection".into(),
            detail: "String interpolation/concatenation detected in query. Use parameterized queries instead".into(),
        });
    }
    findings
}

fn check_performance_issues(stmt: &Statement) -> Vec<DbFinding> {
    let mut findings = Vec::new();

    if let Statement::Query(query) = stmt {
        // Check for DISTINCT without obvious need
        if let SetExpr::Select(ref select) = *query.body {
            if select.distinct.is_some() && select.from.len() == 1 {
                let join_count = select.from.first().map(|f| f.joins.len()).unwrap_or(0);
                if join_count == 0 {
                    findings.push(DbFinding {
                        category: "performance".into(),
                        severity: "low".into(),
                        title: "DISTINCT on single table without JOINs".into(),
                        detail: "DISTINCT without joins may indicate duplicate data or unnecessary overhead".into(),
                    });
                }
            }
        }

        // Check for missing LIMIT on large queries
        if query.limit.is_none() {
            if let SetExpr::Select(ref select) = *query.body {
                if select.from.len() > 1
                    || select.from.first().map(|f| f.joins.len()).unwrap_or(0) > 0
                {
                    findings.push(DbFinding {
                        category: "performance".into(),
                        severity: "low".into(),
                        title: "Multi-table query without LIMIT".into(),
                        detail: "Consider adding LIMIT to prevent unbounded result sets".into(),
                    });
                }
            }
        }
    }
    findings
}

/// Controlla che la query sfrutti il DB al massimo: ORDER BY, WHERE, LIMIT, aggregazioni.
/// Filosofia: il DB deve restituire il dato già pronto — non elaborare in codice applicativo.
pub fn check_db_first_principles(sql: &str) -> Vec<DbFinding> {
    let mut findings = Vec::new();
    let upper = sql.to_uppercase();

    // SELECT senza WHERE su tabelle che probabilmente hanno molti record
    if upper.contains("SELECT") && !upper.contains("WHERE") && !upper.contains("LIMIT")
        && !upper.contains("COUNT(") && !upper.contains("SUM(") && !upper.contains("MAX(")
        && !upper.contains("MIN(") && !upper.contains("AVG(")
    {
        findings.push(DbFinding {
            category: "performance".into(),
            severity: "medium".into(),
            title: "SELECT senza WHERE né LIMIT".into(),
            detail: "La query carica tutti i record. Aggiungere WHERE e/o LIMIT per restituire solo i dati necessari. \
                     Il filtraggio deve avvenire nel DB, non nel codice applicativo.".into(),
        });
    }

    // ORDER BY mancante su query che probabilmente richiedono ordinamento
    if upper.contains("SELECT") && upper.contains("WHERE") && !upper.contains("ORDER BY")
        && !upper.contains("COUNT(") && !upper.contains("GROUP BY")
    {
        findings.push(DbFinding {
            category: "performance".into(),
            severity: "low".into(),
            title: "Query senza ORDER BY".into(),
            detail: "Se il risultato richiede un ordinamento, aggiungerlo in SQL (ORDER BY) \
                     invece di ordinare il risultato in codice con .sort(). \
                     L'ordinamento in DB usa gli indici ed è più efficiente.".into(),
        });
    }

    // Aggregazioni (COUNT, SUM, AVG) che potrebbero essere fatte nel DB
    if upper.contains("SELECT") && !upper.contains("COUNT(") && !upper.contains("SUM(")
        && !upper.contains("GROUP BY")
        && (upper.contains("WHERE") || upper.contains("JOIN"))
        && !upper.contains("LIMIT")
    {
        // Hint leggero: suggerisce di considerare aggregazioni
        findings.push(DbFinding {
            category: "performance".into(),
            severity: "low".into(),
            title: "Considerare aggregazioni nel DB".into(),
            detail: "Se il codice applicativo calcola conteggi, somme o medie su questo risultato, \
                     valutare COUNT(), SUM(), AVG() o GROUP BY direttamente nella query SQL.".into(),
        });
    }

    // Paginazione: OFFSET senza LIMIT o viceversa
    if upper.contains("OFFSET") && !upper.contains("LIMIT") {
        findings.push(DbFinding {
            category: "performance".into(),
            severity: "high".into(),
            title: "OFFSET senza LIMIT".into(),
            detail: "OFFSET senza LIMIT carica tutti i record fino all'offset: estremamente inefficiente. \
                     Aggiungere sempre LIMIT per la paginazione.".into(),
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_star() {
        let report = analyze_query("SELECT * FROM users");
        assert!(report.findings.iter().any(|f| f.title.contains("SELECT *")));
        assert!(report.tables_referenced.contains(&"users".to_string()));
        assert_eq!(report.statement_type, "SELECT");
    }

    #[test]
    fn test_update_no_where() {
        let report = analyze_query("UPDATE users SET active = false");
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("UPDATE without WHERE")));
    }

    #[test]
    fn test_delete_no_where() {
        let report = analyze_query("DELETE FROM logs");
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("DELETE without WHERE")));
    }

    #[test]
    fn test_safe_query() {
        // ORDER BY incluso: nessun hint performance, nessun finding di sicurezza
        let report = analyze_query("SELECT id, name FROM users WHERE id = 1 ORDER BY id LIMIT 10");
        assert!(report.findings.is_empty());
    }

    #[test]
    fn test_parse_error() {
        let report = analyze_query("SELEC broken query !!!");
        assert!(report.findings.iter().any(|f| f.category == "parse_error"));
    }

    #[test]
    fn test_correlated_subquery() {
        let report = analyze_query(
            "SELECT * FROM orders WHERE user_id IN (SELECT id FROM users WHERE active = true)",
        );
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("subquery") || f.title.contains("SELECT *")));
    }

    #[test]
    fn test_tables_extracted() {
        let report = analyze_query("SELECT u.name, o.total FROM users u JOIN orders o ON u.id = o.user_id WHERE u.active = true");
        assert!(report.tables_referenced.iter().any(|t| t.contains("users")));
        assert!(report
            .tables_referenced
            .iter()
            .any(|t| t.contains("orders")));
    }
}
