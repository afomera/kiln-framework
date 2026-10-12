//! Schema snapshots, diffs and migrations.
//!
//! The record structs are the schema. `db/snapshot.json` records the schema as of the last
//! generated migration. `kiln db diff <name>` compares the two and writes a reviewed,
//! plain-SQL migration to `db/migrations/`, then updates the snapshot.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sqlx::{Connection, Row};

use crate::{ColumnSchema, Db, Error, Result, TableSchema};

/// A record registered by `#[derive(Record)]`, collected at link time.
pub struct RecordDef {
    pub name: &'static str,
    pub table: &'static str,
    pub schema: fn() -> TableSchema,
}

inventory::collect!(RecordDef);

/// Every record linked into the binary, sorted by table.
pub fn records() -> Vec<&'static RecordDef> {
    let mut records: Vec<_> = inventory::iter::<RecordDef>.into_iter().collect();
    records.sort_by_key(|r| r.table);
    records
}

// ---------------------------------------------------------------------------------------
// Snapshot

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tables: BTreeMap<String, TableSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub columns: Vec<ColumnSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnSnapshot {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub primary_key: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub indexed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl From<&ColumnSchema> for ColumnSnapshot {
    fn from(c: &ColumnSchema) -> Self {
        Self {
            name: c.name.to_owned(),
            sql_type: c.sql_type.to_owned(),
            nullable: c.nullable,
            primary_key: c.primary_key,
            unique: c.unique,
            // Foreign keys are always indexed.
            indexed: !c.unique && (c.indexed || c.references.is_some()),
            references: c.references.map(str::to_owned),
            variants: c.variants.iter().map(|v| (*v).to_owned()).collect(),
        }
    }
}

impl From<&TableSchema> for TableSnapshot {
    fn from(t: &TableSchema) -> Self {
        Self { columns: t.columns.iter().map(ColumnSnapshot::from).collect() }
    }
}

impl Snapshot {
    /// The schema described by every linked record struct.
    pub fn from_records() -> Self {
        let tables = records()
            .into_iter()
            .map(|r| {
                let schema = (r.schema)();
                (schema.table.to_owned(), TableSnapshot::from(&schema))
            })
            .collect();
        Self { tables }
    }

    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::BadRequest(format!("{} is not a valid schema snapshot: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_string_pretty(self).expect("snapshot serializes");
        json.push('\n');
        fs::write(path, json)?;
        Ok(())
    }
}

impl ColumnSnapshot {
    fn definition(&self) -> String {
        let mut sql = format!("\"{}\" {}", self.name, self.sql_type);
        if self.primary_key {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
            return sql;
        }
        if !self.nullable {
            sql.push_str(" NOT NULL");
        }
        if let Some(table) = &self.references {
            sql.push_str(&format!(" REFERENCES \"{table}\" (\"id\")"));
        }
        if !self.variants.is_empty() {
            let values: Vec<String> = self.variants.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
            sql.push_str(&format!(" CHECK (\"{}\" IN ({}))", self.name, values.join(", ")));
        }
        sql
    }

    /// The column's own constraints, ignoring indexes.
    fn same_shape(&self, other: &Self) -> bool {
        self.sql_type == other.sql_type
            && self.nullable == other.nullable
            && self.primary_key == other.primary_key
            && self.references == other.references
            && self.variants == other.variants
    }

    fn index_statement(&self, table: &str) -> Option<String> {
        let name = index_name(table, &self.name);
        if self.unique {
            Some(format!("CREATE UNIQUE INDEX \"{name}\" ON \"{table}\" (\"{}\");", self.name))
        } else if self.indexed {
            Some(format!("CREATE INDEX \"{name}\" ON \"{table}\" (\"{}\");", self.name))
        } else {
            None
        }
    }

    fn has_index(&self) -> bool {
        self.unique || self.indexed
    }

    /// A placeholder for backfilling a new NOT NULL column.
    fn backfill(&self) -> String {
        if let Some(first) = self.variants.first() {
            return format!("'{first}'");
        }
        match self.sql_type.as_str() {
            "TEXT" => "''".into(),
            "REAL" => "0.0".into(),
            _ => "0".into(),
        }
    }
}

fn index_name(table: &str, column: &str) -> String {
    format!("idx_{table}_{column}")
}

impl TableSnapshot {
    fn create_statements(&self, table: &str) -> Vec<String> {
        let definitions: Vec<String> = self.columns.iter().map(ColumnSnapshot::definition).collect();
        let mut statements = vec![format!("CREATE TABLE \"{table}\" (\n  {}\n);", definitions.join(",\n  "))];
        statements.extend(self.columns.iter().filter_map(|c| c.index_statement(table)));
        statements
    }
}

impl TableSchema {
    /// `CREATE TABLE` plus indexes, as a migration for a new table would contain.
    pub fn create_sql(&self) -> Vec<String> {
        TableSnapshot::from(self).create_statements(self.table)
    }
}

// ---------------------------------------------------------------------------------------
// Diff

/// The SQL needed to move a database from one snapshot to another.
#[derive(Debug, Default)]
pub struct Diff {
    pub statements: Vec<String>,
    /// Things a reviewer must look at, written into the migration as comments.
    pub notes: Vec<String>,
    /// Table rebuilds run with foreign keys off, outside a transaction-per-statement.
    pub rebuilds: bool,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    /// The migration file's contents.
    pub fn to_sql(&self, name: &str) -> String {
        let mut sql = String::new();
        if self.rebuilds {
            sql.push_str("-- kiln: rebuild\n");
        }
        sql.push_str(&format!(
            "-- Migration: {name}\n-- Generated by `kiln db diff` from the record structs. Review before applying.\n"
        ));
        for note in &self.notes {
            sql.push_str(&format!("-- REVIEW: {note}\n"));
        }
        sql.push('\n');
        sql.push_str(&self.statements.join("\n\n"));
        sql.push('\n');
        sql
    }
}

pub fn diff(old: &Snapshot, new: &Snapshot) -> Diff {
    let mut diff = Diff::default();
    for (table, new_table) in &new.tables {
        match old.tables.get(table) {
            None => diff.statements.extend(new_table.create_statements(table)),
            Some(old_table) => diff_table(table, old_table, new_table, &mut diff),
        }
    }
    for table in old.tables.keys().filter(|t| !new.tables.contains_key(*t)) {
        diff.statements.push(format!("DROP TABLE \"{table}\";"));
        diff.notes.push(format!("drops table `{table}` and all of its rows"));
    }
    diff
}

fn diff_table(table: &str, old: &TableSnapshot, new: &TableSnapshot, diff: &mut Diff) {
    let old_cols: BTreeMap<&str, &ColumnSnapshot> = old.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let new_cols: BTreeMap<&str, &ColumnSnapshot> = new.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let added: Vec<&ColumnSnapshot> = new.columns.iter().filter(|c| !old_cols.contains_key(c.name.as_str())).collect();
    let removed: Vec<&ColumnSnapshot> =
        old.columns.iter().filter(|c| !new_cols.contains_key(c.name.as_str())).collect();
    let changed: Vec<(&ColumnSnapshot, &ColumnSnapshot)> = new
        .columns
        .iter()
        .filter_map(|c| old_cols.get(c.name.as_str()).map(|o| (*o, c)))
        .filter(|(o, c)| !o.same_shape(c))
        .collect();

    // SQLite can only add nullable columns and drop plain ones in place.
    let rebuild = !changed.is_empty()
        || added.iter().any(|c| !c.nullable || c.primary_key)
        || removed.iter().any(|c| c.references.is_some() || c.primary_key);
    if rebuild {
        rebuild_table(table, old, new, &old_cols, diff);
        return;
    }

    for c in &removed {
        if c.has_index() {
            diff.statements.push(format!("DROP INDEX \"{}\";", index_name(table, &c.name)));
        }
        diff.statements.push(format!("ALTER TABLE \"{table}\" DROP COLUMN \"{}\";", c.name));
        diff.notes.push(format!("drops column `{table}.{}` and its data", c.name));
        if let Some(added_like) = added.iter().find(|a| a.sql_type == c.sql_type) {
            diff.notes.push(format!(
                "if `{}` is a rename of `{}`, replace the DROP/ADD pair with `ALTER TABLE \"{table}\" RENAME COLUMN \"{}\" TO \"{}\";`",
                added_like.name, c.name, c.name, added_like.name
            ));
        }
    }
    for c in &added {
        diff.statements.push(format!("ALTER TABLE \"{table}\" ADD COLUMN {};", c.definition()));
        diff.statements.extend(c.index_statement(table));
    }
    for c in &new.columns {
        let Some(o) = old_cols.get(c.name.as_str()) else { continue };
        if o.unique != c.unique || o.indexed != c.indexed {
            if o.has_index() {
                diff.statements.push(format!("DROP INDEX \"{}\";", index_name(table, &c.name)));
            }
            diff.statements.extend(c.index_statement(table));
        }
    }
}

fn rebuild_table(
    table: &str,
    old: &TableSnapshot,
    new: &TableSnapshot,
    old_cols: &BTreeMap<&str, &ColumnSnapshot>,
    diff: &mut Diff,
) {
    diff.rebuilds = true;
    let staging = format!("{table}__kiln_new");
    let mut statements = TableSnapshot { columns: new.columns.clone() }.create_statements(&staging);
    statements.truncate(1); // indexes are created after the rename

    let mut columns = Vec::new();
    let mut values = Vec::new();
    for c in &new.columns {
        if old_cols.contains_key(c.name.as_str()) {
            columns.push(format!("\"{}\"", c.name));
            values.push(format!("\"{}\"", c.name));
        } else if !c.nullable {
            columns.push(format!("\"{}\"", c.name));
            values.push(c.backfill());
            diff.notes.push(format!(
                "new NOT NULL column `{table}.{}` is backfilled with {} for existing rows; change the value in the INSERT if that's wrong",
                c.name,
                c.backfill()
            ));
        }
    }
    for c in &old.columns {
        if !new.columns.iter().any(|n| n.name == c.name) {
            diff.notes.push(format!("drops column `{table}.{}` and its data", c.name));
        }
    }
    diff.notes.push(format!(
        "rebuilds `{table}` (SQLite can't change these columns in place); runs with foreign keys off, then checks them before committing"
    ));

    statements.push(format!(
        "INSERT INTO \"{staging}\" ({}) SELECT {} FROM \"{table}\";",
        columns.join(", "),
        values.join(", ")
    ));
    statements.push(format!("DROP TABLE \"{table}\";"));
    statements.push(format!("ALTER TABLE \"{staging}\" RENAME TO \"{table}\";"));
    statements.extend(new.columns.iter().filter_map(|c| c.index_statement(table)));
    diff.statements.extend(statements);
}

// ---------------------------------------------------------------------------------------
// Migrations

#[derive(Debug, Clone)]
pub struct Migration {
    /// `YYYYMMDDHHMMSS`, from the file name.
    pub version: String,
    pub name: String,
    pub sql: String,
    pub path: PathBuf,
}

impl Migration {
    fn rebuild(&self) -> bool {
        self.sql.lines().any(|l| l.trim() == "-- kiln: rebuild")
    }
}

/// Migration files in `dir`, oldest first. Missing directory means no migrations.
pub fn load_migrations(dir: &Path) -> Result<Vec<Migration>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut migrations = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else { continue };
        let Some(stem) = file.strip_suffix(".sql") else { continue };
        let Some((version, name)) = stem.split_once('_') else { continue };
        if version.len() != 14 || !version.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let sql = fs::read_to_string(&path)?;
        migrations.push(Migration { version: version.to_owned(), name: name.to_owned(), sql, path });
    }
    migrations.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(migrations)
}

/// Writes `diff` as a new migration in `dir` and returns its path.
pub fn write_migration(dir: &Path, name: &str, diff: &Diff) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let mut version: u64 =
        jiff::Timestamp::now().strftime("%Y%m%d%H%M%S").to_string().parse().expect("timestamp formats as digits");
    // Keep versions unique and increasing even when generated within the same second.
    if let Some(last) = load_migrations(dir)?.last() {
        let last: u64 = last.version.parse().unwrap_or(0);
        version = version.max(last + 1);
    }
    let path = dir.join(format!("{version}_{name}.sql"));
    fs::write(&path, diff.to_sql(name))?;
    Ok(path)
}

const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS \"kiln_migrations\" (\
    \"version\" TEXT PRIMARY KEY, \"name\" TEXT NOT NULL, \"applied_at\" INTEGER NOT NULL)";

impl Db {
    /// Versions already applied.
    pub async fn applied_migrations(&self) -> Result<Vec<String>> {
        self.execute(MIGRATIONS_TABLE).await?;
        let rows = self.fetch_all("SELECT \"version\" FROM \"kiln_migrations\" ORDER BY \"version\"", vec![]).await?;
        Ok(rows.iter().map(|r| r.get::<String, _>(0)).collect())
    }

    /// Migrations in `dir` that haven't been applied.
    pub async fn pending_migrations(&self, dir: &Path) -> Result<Vec<Migration>> {
        let applied = self.applied_migrations().await?;
        Ok(load_migrations(dir)?.into_iter().filter(|m| !applied.contains(&m.version)).collect())
    }

    /// Applies every pending migration in `dir`, each in its own transaction, and returns
    /// what was applied.
    pub async fn migrate(&self, dir: &Path) -> Result<Vec<Migration>> {
        let pending = self.pending_migrations(dir).await?;
        for migration in &pending {
            self.apply(migration)
                .await
                .map_err(|e| Error::BadRequest(format!("migration {} failed: {e}", migration.path.display())))?;
            tracing::info!(target: "kiln::migrate", version = %migration.version, name = %migration.name, "applied");
        }
        Ok(pending)
    }

    async fn apply(&self, migration: &Migration) -> Result<()> {
        let rebuild = migration.rebuild();
        let mut conn = self.acquire().await?;
        if rebuild {
            // Rebuilds drop and recreate tables, which foreign keys would block mid-way.
            sqlx::raw_sql("PRAGMA foreign_keys = OFF").execute(&mut *conn).await?;
        }
        let result = async {
            let mut tx = conn.begin().await?;
            sqlx::raw_sql(&migration.sql).execute(&mut *tx).await?;
            if rebuild {
                let violations = sqlx::query("PRAGMA foreign_key_check").fetch_all(&mut *tx).await?;
                if !violations.is_empty() {
                    return Err(Error::Integrity(format!(
                        "{} foreign key violation(s) after rebuilding; fix the backfill values",
                        violations.len()
                    )));
                }
            }
            sqlx::query("INSERT INTO \"kiln_migrations\" (\"version\", \"name\", \"applied_at\") VALUES (?, ?, ?)")
                .bind(&migration.version)
                .bind(&migration.name)
                .bind(jiff::Timestamp::now().as_second())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if rebuild {
            sqlx::raw_sql("PRAGMA foreign_keys = ON").execute(&mut *conn).await?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, sql_type: &str, nullable: bool) -> ColumnSnapshot {
        ColumnSnapshot {
            name: name.into(),
            sql_type: sql_type.into(),
            nullable,
            primary_key: name == "id",
            unique: false,
            indexed: false,
            references: None,
            variants: vec![],
        }
    }

    fn snapshot(columns: Vec<ColumnSnapshot>) -> Snapshot {
        Snapshot { tables: BTreeMap::from([("posts".to_owned(), TableSnapshot { columns })]) }
    }

    #[test]
    fn new_table_is_created_with_indexes() {
        let mut slug = col("slug", "TEXT", false);
        slug.unique = true;
        let d = diff(&Snapshot::default(), &snapshot(vec![col("id", "INTEGER", false), slug]));
        assert_eq!(d.statements.len(), 2);
        assert!(d.statements[0].starts_with("CREATE TABLE \"posts\""));
        assert_eq!(d.statements[1], "CREATE UNIQUE INDEX \"idx_posts_slug\" ON \"posts\" (\"slug\");");
        assert!(!d.rebuilds);
    }

    #[test]
    fn nullable_column_is_added_in_place() {
        let old = snapshot(vec![col("id", "INTEGER", false)]);
        let new = snapshot(vec![col("id", "INTEGER", false), col("subtitle", "TEXT", true)]);
        let d = diff(&old, &new);
        assert_eq!(d.statements, ["ALTER TABLE \"posts\" ADD COLUMN \"subtitle\" TEXT;"]);
    }

    #[test]
    fn not_null_column_rebuilds_with_a_backfill_note() {
        let old = snapshot(vec![col("id", "INTEGER", false)]);
        let new = snapshot(vec![col("id", "INTEGER", false), col("views", "INTEGER", false)]);
        let d = diff(&old, &new);
        assert!(d.rebuilds);
        assert!(d.to_sql("add_views").starts_with("-- kiln: rebuild\n"));
        assert!(
            d.statements
                .iter()
                .any(|s| s == "INSERT INTO \"posts__kiln_new\" (\"id\", \"views\") SELECT \"id\", 0 FROM \"posts\";")
        );
        assert!(d.notes.iter().any(|n| n.contains("backfilled with 0")));
    }

    #[test]
    fn identical_snapshots_produce_no_changes() {
        let s = snapshot(vec![col("id", "INTEGER", false)]);
        assert!(diff(&s, &s).is_empty());
    }
}
