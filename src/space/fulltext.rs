//! Full-text search that forgets: FTS5 keeps a deleted document's terms in its index until
//! segments merge – unless the table's own `secure-delete` option is on.

use rusqlite::Connection;

/// The FTS5 tables Temple has cleared, by name and the root page of their data: a table made
/// anew under an old name counts as new. There from the space's start, so that no table of the
/// app's takes its name: the guard sees only the name a table is renamed from.
pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS main.temple_fulltext (
    name TEXT NOT NULL,
    data INTEGER NOT NULL,
    PRIMARY KEY (name, data)
) STRICT";

/// Turns `secure-delete` on for every FTS5 table. The first time Temple sees a table – and
/// whoever turned the option on – it merges away what deletions left in the index so far.
/// Run as Temple, when a space locks. Every table is named in `main`: a temporary table of
/// the same name would come first.
pub(super) fn forget_deleted(db: &Connection) -> rusqlite::Result<()> {
    // Known by FTS5's own tables, not by the text that made it: its configuration names its
    // version.
    let tables: Vec<(String, i64)> = db
        .prepare(
            "SELECT t.name, d.rootpage FROM main.sqlite_schema AS t
             JOIN main.sqlite_schema AS d ON d.type = 'table' AND d.name = t.name || '_data'
             WHERE t.type = 'table' AND t.sql LIKE 'CREATE VIRTUAL TABLE%'
               AND EXISTS (SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = t.name || '_idx')
               AND EXISTS (SELECT 1 FROM main.sqlite_schema WHERE type = 'table' AND name = t.name || '_config')",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    if tables.is_empty() {
        return Ok(());
    }
    db.execute_batch(SCHEMA)?;
    for (name, data) in &tables {
        let (column, config) = (quote(name), quote(&format!("{name}_config")));
        let table = format!("main.{column}");
        let setting = |key: &str| format!("SELECT count(*) > 0 FROM main.{config} WHERE k = '{key}'");
        if !db.query_row(&setting("version"), [], |row| row.get::<_, bool>(0))? {
            continue;
        }
        let on: bool =
            db.query_row(&format!("{} AND CAST(v AS INTEGER) = 1", setting("secure-delete")), [], |row| row.get(0))?;
        let cleared: bool = db.query_row(
            "SELECT count(*) > 0 FROM main.temple_fulltext WHERE name = ?1 AND data = ?2",
            (name, data),
            |row| row.get(0),
        )?;
        if !on {
            db.execute(&format!("INSERT INTO {table} ({column}, rank) VALUES ('secure-delete', 1)"), [])?;
        }
        if !on || !cleared {
            db.execute(&format!("INSERT INTO {table} ({column}) VALUES ('optimize')"), [])?;
            db.execute("INSERT OR IGNORE INTO main.temple_fulltext VALUES (?1, ?2)", (name, data))?;
        }
    }
    db.execute_batch(
        "DELETE FROM main.temple_fulltext WHERE NOT EXISTS (SELECT 1 FROM main.sqlite_schema
             WHERE type = 'table' AND name = temple_fulltext.name || '_data' AND rootpage = temple_fulltext.data)",
    )
}

fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
