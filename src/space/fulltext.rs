//! Full-text search that forgets: FTS5 keeps a deleted document's terms in its index until
//! segments merge – unless the table's own `secure-delete` option is on.

use rusqlite::Connection;

/// Turns `secure-delete` on for every FTS5 table that doesn't have it yet, and merges away
/// what deletions left in its index so far. Run when a space locks.
pub(super) fn forget_deleted(db: &Connection) -> rusqlite::Result<()> {
    let tables: Vec<(String, String)> = db
        .prepare("SELECT name, sql FROM sqlite_schema WHERE type = 'table' AND sql LIKE 'CREATE VIRTUAL TABLE%'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (name, _) in tables.iter().filter(|(_, sql)| uses_fts5(sql)) {
        let config = quote(&format!("{name}_config"));
        let on: bool = db.query_row(
            &format!("SELECT count(*) > 0 FROM {config} WHERE k = 'secure-delete' AND CAST(v AS INTEGER) = 1"),
            [],
            |row| row.get(0),
        )?;
        if !on {
            let table = quote(name);
            db.execute(&format!("INSERT INTO {table} ({table}, rank) VALUES ('secure-delete', 1)"), [])?;
            db.execute(&format!("INSERT INTO {table} ({table}) VALUES ('optimize')"), [])?;
        }
    }
    Ok(())
}

/// Whether a `CREATE VIRTUAL TABLE` statement names the module fts5.
fn uses_fts5(sql: &str) -> bool {
    let mut words = sql.split(|c: char| c.is_whitespace() || c == '(').filter(|word| !word.is_empty());
    words.any(|word| word.eq_ignore_ascii_case("using"))
        && words.next().is_some_and(|word| word.eq_ignore_ascii_case("fts5"))
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    #[test]
    fn recognizes_fts5_tables() {
        assert!(uses_fts5("CREATE VIRTUAL TABLE verse USING fts5(body)"));
        assert!(uses_fts5("CREATE VIRTUAL TABLE \"my verse\"\nUSING FTS5 (body, tokenize = 'unicode61')"));
        assert!(!uses_fts5("CREATE VIRTUAL TABLE old USING fts4(body)"));
        assert!(!uses_fts5("CREATE VIRTUAL TABLE \"using\" USING rtree(id, a, b)"));
    }
}
