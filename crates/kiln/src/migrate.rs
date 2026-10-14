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

use crate::{ColumnSchema, Db, Error, Result, TableSchema, Value, dialect::Dialect};

/// A record registered by `#[derive(Record)]`, collected at link time.
pub struct RecordDef {
    pub name: &'static str,
    pub table: &'static str,
    pub schema: fn() -> TableSchema,
    /// `(kind, name, target)`: `("belongs_to", "author", "User")`.
    pub relations: &'static [(&'static str, &'static str, &'static str)],
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// The backend the migrations are written for (`sqlite`, `postgres`); set by the first
    /// `kiln db diff`. Older snapshots have none, meaning SQLite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// The version of the migration `kiln db diff` wrote alongside this snapshot. If that
    /// file is gone, the snapshot is ahead of the migrations (`kiln db undo` restores it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<String>,
    pub tables: BTreeMap<String, TableSnapshot>,
}

/// Snapshots are equal when their tables are; the backend is bookkeeping.
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.tables == other.tables
    }
}

impl Snapshot {
    /// The dialect the app's migrations use.
    pub fn dialect(&self) -> &'static dyn Dialect {
        self.backend.as_deref().and_then(crate::dialect::named).unwrap_or(&crate::dialect::Sqlite)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableSnapshot {
    pub columns: Vec<ColumnSnapshot>,
    /// Indexes over several columns, or partial ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexSnapshot>,
    /// Column groups of which exactly one is set (`#[record(one_of(..))]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexSnapshot {
    pub columns: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub unique: bool,
    /// The SQL condition of a partial index.
    #[serde(default, rename = "where", skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl IndexSnapshot {
    fn name(&self, table: &str) -> String {
        let mut name = format!("idx_{table}_{}", self.columns.join("_"));
        if self.condition.is_some() {
            name.push_str("_partial");
        }
        name
    }

    fn create(&self, table: &str) -> String {
        let columns: Vec<String> = self.columns.iter().map(|c| format!("\"{c}\"")).collect();
        let condition = self.condition.as_ref().map(|c| format!(" WHERE {c}")).unwrap_or_default();
        format!(
            "CREATE {}INDEX \"{}\" ON \"{table}\" ({}){condition};",
            if self.unique { "UNIQUE " } else { "" },
            self.name(table),
            columns.join(", ")
        )
    }
}

/// The CHECK for `one_of(a, b)`: exactly one of the columns is set.
fn one_of_check(table: &str, columns: &[String]) -> (String, String) {
    let name = format!("{table}_one_of_{}", columns.join("_"));
    let terms: Vec<String> =
        columns.iter().map(|c| format!("CASE WHEN \"{c}\" IS NOT NULL THEN 1 ELSE 0 END")).collect();
    (name, format!("CHECK ({} = 1)", terms.join(" + ")))
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
    /// `CASCADE` or `SET NULL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
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
            on_delete: c.on_delete.map(str::to_owned),
            variants: c.variants.iter().map(|v| (*v).to_owned()).collect(),
        }
    }
}

impl From<&TableSchema> for TableSnapshot {
    fn from(t: &TableSchema) -> Self {
        Self {
            columns: t.columns.iter().map(ColumnSnapshot::from).collect(),
            indexes: t
                .indexes
                .iter()
                .map(|i| IndexSnapshot {
                    columns: i.columns.iter().map(|c| (*c).to_owned()).collect(),
                    unique: i.unique,
                    condition: i.condition.map(str::to_owned),
                })
                .collect(),
            one_of: t.one_of.iter().map(|g| g.iter().map(|c| (*c).to_owned()).collect()).collect(),
        }
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
        Self { backend: None, migration: None, tables }
    }

    pub fn load(path: &Path) -> Result<Self> {
        match crate::files::read_string(path) {
            Some(text) => serde_json::from_str(&text)
                .map_err(|e| Error::BadRequest(format!("{} is not a valid schema snapshot: {e}", path.display()))),
            None => Ok(Self::default()),
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
    fn definition(&self, d: &dyn Dialect) -> String {
        if self.primary_key {
            return d.primary_key(&self.name);
        }
        let mut sql = format!("\"{}\" {}", self.name, d.column_type(&self.sql_type));
        if !self.nullable {
            sql.push_str(" NOT NULL");
        }
        if let Some(table) = &self.references {
            sql.push_str(&format!(" REFERENCES \"{table}\" (\"id\")"));
            if let Some(action) = &self.on_delete {
                sql.push_str(&format!(" ON DELETE {action}"));
            }
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
            && self.on_delete == other.on_delete
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
    fn create_statements(&self, table: &str, d: &dyn Dialect) -> Vec<String> {
        let mut definitions: Vec<String> = self.columns.iter().map(|c| c.definition(d)).collect();
        for group in &self.one_of {
            let (name, check) = one_of_check(table, group);
            definitions.push(format!("CONSTRAINT \"{name}\" {check}"));
        }
        let mut statements = vec![format!("CREATE TABLE \"{table}\" (\n  {}\n);", definitions.join(",\n  "))];
        statements.extend(self.index_statements(table));
        statements
    }

    /// Every index: the columns' own, then the table's.
    fn index_statements(&self, table: &str) -> Vec<String> {
        let own = self.columns.iter().filter_map(|c| c.index_statement(table));
        own.chain(self.indexes.iter().map(|i| i.create(table))).collect()
    }

    /// Table indexes in `self` but not `other`.
    fn indexes_not_in<'a>(&'a self, other: &'a Self) -> impl Iterator<Item = &'a IndexSnapshot> {
        self.indexes.iter().filter(|i| !other.indexes.contains(i))
    }
}

impl TableSchema {
    /// `CREATE TABLE` plus indexes, as a migration for a new table would contain.
    pub fn create_sql(&self) -> Vec<String> {
        self.create_sql_for(&crate::dialect::Sqlite)
    }

    pub fn create_sql_for(&self, d: &dyn Dialect) -> Vec<String> {
        TableSnapshot::from(self).create_statements(self.table, d)
    }
}

// ---------------------------------------------------------------------------------------
// Diff

/// The SQL needed to move a database from one snapshot to another.
#[derive(Debug)]
pub struct Diff {
    /// The backend the statements are written for.
    pub backend: &'static str,
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
        sql.push_str(&format!("-- kiln: backend {}\n", self.backend));
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

/// The SQL to go from `old` to `new`, for SQLite.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Diff {
    diff_for(old, new, &crate::dialect::Sqlite)
}

/// The SQL to go from `old` to `new` on backend `d`.
pub fn diff_for(old: &Snapshot, new: &Snapshot, d: &dyn Dialect) -> Diff {
    let mut diff = Diff { backend: d.name(), statements: vec![], notes: vec![], rebuilds: false };
    // New tables go first, each after the tables it references (Postgres refuses a foreign
    // key to a table that doesn't exist yet).
    let mut created: Vec<&str> = old.tables.keys().map(String::as_str).collect();
    let mut pending: Vec<&String> = new.tables.keys().filter(|t| !old.tables.contains_key(*t)).collect();
    while !pending.is_empty() {
        let ready =
            pending
                .iter()
                .position(|t| {
                    new.tables[*t].columns.iter().filter_map(|c| c.references.as_deref()).all(|target| {
                        target == t.as_str() || created.contains(&target) || !new.tables.contains_key(target)
                    })
                })
                .unwrap_or(0); // a reference cycle: create in name order and let review sort it out
        let table = pending.remove(ready);
        diff.statements.extend(new.tables[table].create_statements(table, d));
        created.push(table);
    }
    for (table, new_table) in &new.tables {
        match old.tables.get(table) {
            None => {}
            Some(old_table) if d.alters_columns() => alter_table(table, old_table, new_table, d, &mut diff),
            Some(old_table) => diff_table(table, old_table, new_table, d, &mut diff),
        }
    }
    for table in old.tables.keys().filter(|t| !new.tables.contains_key(*t)) {
        diff.statements.push(format!("DROP TABLE \"{table}\";"));
        diff.notes.push(format!("drops table `{table}` and all of its rows"));
    }
    diff
}

/// SQLite: add nullable columns and drop plain ones in place; rebuild for anything else.
fn diff_table(table: &str, old: &TableSnapshot, new: &TableSnapshot, d: &dyn Dialect, diff: &mut Diff) {
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

    // SQLite can only add nullable columns and drop plain ones in place, and can't change
    // a table's CHECK constraints.
    let rebuild = !changed.is_empty()
        || added.iter().any(|c| !c.nullable || c.primary_key || c.on_delete.is_some())
        || removed.iter().any(|c| c.references.is_some() || c.primary_key)
        || old.one_of != new.one_of;
    if rebuild {
        rebuild_table(table, old, new, &old_cols, d, diff);
        return;
    }

    // Before columns go: SQLite won't drop a column an index still covers.
    for index in old.indexes_not_in(new) {
        diff.statements.push(format!("DROP INDEX \"{}\";", index.name(table)));
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
        diff.statements.push(format!("ALTER TABLE \"{table}\" ADD COLUMN {};", c.definition(d)));
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
    diff.statements.extend(new.indexes_not_in(old).map(|i| i.create(table)));
}

fn rebuild_table(
    table: &str,
    old: &TableSnapshot,
    new: &TableSnapshot,
    old_cols: &BTreeMap<&str, &ColumnSnapshot>,
    d: &dyn Dialect,
    diff: &mut Diff,
) {
    diff.rebuilds = true;
    let staging = format!("{table}__kiln_new");
    let mut statements = new.create_statements(&staging, d);
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
    statements.extend(new.index_statements(table));
    diff.statements.extend(statements);
}

/// Backends that change columns in place (Postgres): one ALTER per change, with names for
/// the constraints so they can be replaced later.
fn alter_table(table: &str, old: &TableSnapshot, new: &TableSnapshot, d: &dyn Dialect, diff: &mut Diff) {
    let old_cols: BTreeMap<&str, &ColumnSnapshot> = old.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let new_cols: BTreeMap<&str, &ColumnSnapshot> = new.columns.iter().map(|c| (c.name.as_str(), c)).collect();
    let added: Vec<&ColumnSnapshot> = new.columns.iter().filter(|c| !old_cols.contains_key(c.name.as_str())).collect();
    let alter = |sql: String| format!("ALTER TABLE \"{table}\" {sql};");

    // First, while every column they cover still exists.
    for index in old.indexes_not_in(new) {
        diff.statements.push(format!("DROP INDEX \"{}\";", index.name(table)));
    }
    for group in old.one_of.iter().filter(|g| !new.one_of.contains(g)) {
        diff.statements.push(alter(format!("DROP CONSTRAINT \"{}\"", one_of_check(table, group).0)));
    }

    for c in old.columns.iter().filter(|c| !new_cols.contains_key(c.name.as_str())) {
        diff.statements.push(alter(format!("DROP COLUMN \"{}\"", c.name)));
        diff.notes.push(format!("drops column `{table}.{}` and its data (and its index)", c.name));
        if let Some(added_like) = added.iter().find(|a| a.sql_type == c.sql_type) {
            diff.notes.push(format!(
                "if `{}` is a rename of `{}`, replace the DROP/ADD pair with `ALTER TABLE \"{table}\" RENAME COLUMN \"{}\" TO \"{}\";`",
                added_like.name, c.name, c.name, added_like.name
            ));
        }
    }
    for c in &added {
        if c.nullable || c.primary_key {
            diff.statements.push(alter(format!("ADD COLUMN {}", c.definition(d))));
        } else {
            // Existing rows need a value; the default is only for them.
            diff.statements.push(alter(format!("ADD COLUMN {} DEFAULT {}", c.definition(d), c.backfill())));
            diff.statements.push(alter(format!("ALTER COLUMN \"{}\" DROP DEFAULT", c.name)));
            diff.notes.push(format!(
                "new NOT NULL column `{table}.{}` is filled with {} for existing rows; change the DEFAULT if that's wrong",
                c.name,
                c.backfill()
            ));
        }
        diff.statements.extend(c.index_statement(table));
    }
    for c in &new.columns {
        let Some(o) = old_cols.get(c.name.as_str()) else { continue };
        let column = &c.name;
        if o.primary_key != c.primary_key {
            diff.notes
                .push(format!("`{table}.{column}` changed whether it's the primary key; write that migration by hand"));
            continue;
        }
        if o.sql_type != c.sql_type {
            let ty = d.column_type(&c.sql_type);
            diff.statements.push(alter(format!("ALTER COLUMN \"{column}\" TYPE {ty} USING \"{column}\"::{ty}")));
            diff.notes.push(format!("changes the type of `{table}.{column}`; check existing values convert"));
        }
        if o.nullable != c.nullable {
            if c.nullable {
                diff.statements.push(alter(format!("ALTER COLUMN \"{column}\" DROP NOT NULL")));
            } else {
                diff.statements.push(alter(format!("ALTER COLUMN \"{column}\" SET NOT NULL")));
                diff.notes.push(format!(
                    "`{table}.{column}` becomes NOT NULL; it fails if any row is NULL, so backfill first"
                ));
            }
        }
        if o.references != c.references || o.on_delete != c.on_delete {
            let name = format!("{table}_{column}_fkey");
            diff.statements.push(alter(format!("DROP CONSTRAINT IF EXISTS \"{name}\"")));
            if let Some(target) = &c.references {
                let on_delete = c.on_delete.as_ref().map(|a| format!(" ON DELETE {a}")).unwrap_or_default();
                diff.statements.push(alter(format!(
                    "ADD CONSTRAINT \"{name}\" FOREIGN KEY (\"{column}\") REFERENCES \"{target}\" (\"id\"){on_delete}"
                )));
            }
        }
        if o.variants != c.variants {
            let name = format!("{table}_{column}_check");
            diff.statements.push(alter(format!("DROP CONSTRAINT IF EXISTS \"{name}\"")));
            if !c.variants.is_empty() {
                let values: Vec<String> = c.variants.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
                diff.statements
                    .push(alter(format!("ADD CONSTRAINT \"{name}\" CHECK (\"{column}\" IN ({}))", values.join(", "))));
            }
        }
        if o.unique != c.unique || o.indexed != c.indexed {
            if o.has_index() {
                diff.statements.push(format!("DROP INDEX \"{}\";", index_name(table, column)));
            }
            diff.statements.extend(c.index_statement(table));
        }
    }
    diff.statements.extend(new.indexes_not_in(old).map(|i| i.create(table)));
    for group in new.one_of.iter().filter(|g| !old.one_of.contains(g)) {
        let (name, check) = one_of_check(table, group);
        diff.statements.push(alter(format!("ADD CONSTRAINT \"{name}\" {check}")));
    }
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

/// One migration's SQL inside a file: the whole file, or one of the migrations a squash
/// holds. Each part is recorded in `kiln_migrations` under its own version.
#[derive(Debug, Clone)]
pub struct Part {
    pub version: String,
    pub name: String,
    pub sql: String,
}

impl Migration {
    /// Written by `kiln db squash`: several migrations in one file, each after a
    /// `-- kiln: part <version>_<name>` line.
    pub fn is_squash(&self) -> bool {
        self.sql.lines().any(|l| l.trim_start().starts_with("-- kiln: part "))
    }

    pub fn parts(&self) -> Vec<Part> {
        if !self.is_squash() {
            return vec![Part { version: self.version.clone(), name: self.name.clone(), sql: self.sql.clone() }];
        }
        let mut parts: Vec<Part> = vec![];
        for line in self.sql.lines() {
            if let Some(id) = line.trim().strip_prefix("-- kiln: part ") {
                let (version, name) = id.trim().split_once('_').unwrap_or((id.trim(), ""));
                parts.push(Part { version: version.to_owned(), name: name.to_owned(), sql: String::new() });
            } else if let Some(part) = parts.last_mut() {
                part.sql.push_str(line);
                part.sql.push('\n');
            }
        }
        parts
    }

    /// Whether every part is recorded as applied.
    fn applied_in(&self, applied: &[String]) -> bool {
        self.parts().iter().all(|p| applied.contains(&p.version))
    }

    fn rebuild(&self) -> bool {
        self.sql.lines().any(|l| l.trim() == "-- kiln: rebuild")
    }

    /// The backend it was written for (`-- kiln: backend postgres`). Files from before the
    /// header existed are SQLite's.
    pub fn backend(&self) -> &str {
        self.sql.lines().find_map(|l| l.trim().strip_prefix("-- kiln: backend ")).map(str::trim).unwrap_or("sqlite")
    }
}

/// Migration files in `dir`, oldest first. Missing directory means no migrations.
pub fn load_migrations(dir: &Path) -> Result<Vec<Migration>> {
    let mut migrations = Vec::new();
    for path in crate::files::walk(dir) {
        if path.parent() != Some(dir) {
            continue;
        }
        let Some(file) = path.file_name().and_then(|f| f.to_str()) else { continue };
        let Some(stem) = file.strip_suffix(".sql") else { continue };
        let Some((version, name)) = stem.split_once('_') else { continue };
        if version.len() != 14 || !version.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let sql = crate::files::read_string(&path)
            .ok_or_else(|| Error::Integrity(format!("couldn't read {}", path.display())))?;
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

/// A year's migrations, squashed into one file by [`squash`].
#[derive(Debug)]
pub struct Squashed {
    pub year: String,
    pub path: PathBuf,
    /// The files it replaced (now deleted).
    pub replaced: Vec<PathBuf>,
    /// How many migrations it holds.
    pub parts: usize,
}

/// Squashes the migrations in `dir` from years before `before_year` into one file per year,
/// `<last version>_squash_<year>.sql`. The SQL is kept as written, part by part, so new
/// databases run exactly what old ones did, and databases that ran the originals skip it.
pub fn squash(dir: &Path, before_year: &str) -> Result<Vec<Squashed>> {
    let mut years: BTreeMap<String, Vec<Migration>> = BTreeMap::new();
    for m in load_migrations(dir)?.into_iter().filter(|m| m.version[..4] < *before_year) {
        years.entry(m.version[..4].to_owned()).or_default().push(m);
    }
    let mut done = vec![];
    for (year, migrations) in years {
        if migrations.len() < 2 {
            continue;
        }
        let backend = migrations[0].backend().to_owned();
        if let Some(other) = migrations.iter().find(|m| m.backend() != backend) {
            return Err(Error::BadRequest(format!(
                "{} is written for {}, but the other {year} migrations for {backend}; squash needs one backend",
                other.path.display(),
                other.backend()
            )));
        }
        let parts: Vec<Part> = migrations.iter().flat_map(Migration::parts).collect();
        let mut sql = format!(
            "-- kiln: backend {backend}\n-- Squash: {} migrations from {year}\n\
             -- Written by `kiln db squash`. Databases that ran these skip them; new ones run each part in order.\n",
            parts.len()
        );
        for part in &parts {
            let body: Vec<&str> =
                part.sql.lines().filter(|l| !l.trim_start().starts_with("-- kiln: backend")).collect();
            sql.push_str(&format!("\n-- kiln: part {}_{}\n{}\n", part.version, part.name, body.join("\n").trim()));
        }
        let last = &parts.last().expect("a year has migrations").version;
        let path = dir.join(format!("{last}_squash_{year}.sql"));
        fs::write(&path, sql)?;
        let mut replaced = vec![];
        for m in migrations.iter().filter(|m| m.path != path) {
            fs::remove_file(&m.path)?;
            replaced.push(m.path.clone());
        }
        done.push(Squashed { year, path, replaced, parts: parts.len() });
    }
    Ok(done)
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
        Ok(load_migrations(dir)?.into_iter().filter(|m| !m.applied_in(&applied)).collect())
    }

    /// Applies every pending migration in `dir`, each in its own transaction, and returns
    /// what was applied.
    pub async fn migrate(&self, dir: &Path) -> Result<Vec<Migration>> {
        let applied = self.applied_migrations().await?;
        let pending: Vec<Migration> = load_migrations(dir)?.into_iter().filter(|m| !m.applied_in(&applied)).collect();
        for migration in &pending {
            self.apply(migration, &applied)
                .await
                .map_err(|e| Error::BadRequest(format!("migration {} failed: {e}", migration.path.display())))?;
            tracing::info!(target: "kiln::migrate", version = %migration.version, name = %migration.name, "applied");
        }
        Ok(pending)
    }

    /// Runs the parts of `migration` not yet in `applied` (all of them, unless it's a squash
    /// some of whose migrations ran before it was squashed) and records each one.
    async fn apply(&self, migration: &Migration, applied: &[String]) -> Result<()> {
        let backend = self.dialect().name();
        if migration.backend() != backend {
            return Err(Error::Config(format!(
                "this migration is written for {}, but the database is {backend}; each app's migrations target one backend (set DATABASE_URL to match, or regenerate them)",
                migration.backend()
            )));
        }
        let parts: Vec<Part> = migration.parts().into_iter().filter(|p| !applied.contains(&p.version)).collect();
        let rebuild = migration.rebuild() && backend == "sqlite";
        let mut conn = self.acquire().await?;
        if rebuild {
            // Rebuilds drop and recreate tables, which foreign keys would block mid-way.
            sqlx::raw_sql("PRAGMA foreign_keys = OFF").execute(&mut *conn).await?;
        }
        let result = async {
            let mut tx = conn.begin().await?;
            let sql: String = parts.iter().map(|p| p.sql.as_str()).collect::<Vec<_>>().join("\n");
            sqlx::raw_sql(&sql).execute(&mut *tx).await?;
            if rebuild {
                let violations = sqlx::query("PRAGMA foreign_key_check").fetch_all(&mut *tx).await?;
                if !violations.is_empty() {
                    return Err(Error::Integrity(format!(
                        "{} foreign key violation(s) after rebuilding; fix the backfill values",
                        violations.len()
                    )));
                }
            }
            let (insert, _) = self.dialect().prepare(
                "INSERT INTO \"kiln_migrations\" (\"version\", \"name\", \"applied_at\") VALUES (?, ?, ?)",
                vec![Value::Integer(0), Value::Integer(0), Value::Integer(0)],
            );
            for part in &parts {
                sqlx::query(&insert)
                    .bind(&part.version)
                    .bind(&part.name)
                    .bind(jiff::Timestamp::now().as_second())
                    .execute(&mut *tx)
                    .await?;
            }
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
            on_delete: None,
            variants: vec![],
        }
    }

    fn snapshot(columns: Vec<ColumnSnapshot>) -> Snapshot {
        Snapshot {
            backend: None,
            migration: None,
            tables: BTreeMap::from([("posts".to_owned(), TableSnapshot { columns, indexes: vec![], one_of: vec![] })]),
        }
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

    #[test]
    fn postgres_alters_columns_in_place() {
        let pg = &crate::dialect::Postgres;
        let old = snapshot(vec![col("id", "INTEGER", false), col("title", "TEXT", true), col("legacy", "TEXT", true)]);
        let mut author = col("author_id", "INTEGER", false);
        author.references = Some("users".into());
        let new = snapshot(vec![
            col("id", "INTEGER", false),
            col("title", "TEXT", false),
            col("views", "INTEGER", false),
            author,
        ]);
        let d = diff_for(&old, &new, pg);
        assert!(!d.rebuilds);
        assert_eq!(
            d.statements,
            [
                "ALTER TABLE \"posts\" DROP COLUMN \"legacy\";",
                "ALTER TABLE \"posts\" ADD COLUMN \"views\" BIGINT NOT NULL DEFAULT 0;",
                "ALTER TABLE \"posts\" ALTER COLUMN \"views\" DROP DEFAULT;",
                "ALTER TABLE \"posts\" ADD COLUMN \"author_id\" BIGINT NOT NULL REFERENCES \"users\" (\"id\") DEFAULT 0;",
                "ALTER TABLE \"posts\" ALTER COLUMN \"author_id\" DROP DEFAULT;",
                "ALTER TABLE \"posts\" ALTER COLUMN \"title\" SET NOT NULL;",
            ]
        );
        assert!(d.to_sql("x").starts_with("-- kiln: backend postgres\n"));
        assert!(d.notes.iter().any(|n| n.contains("becomes NOT NULL")));
    }

    #[test]
    fn postgres_changes_types_and_constraints() {
        let pg = &crate::dialect::Postgres;
        let mut status = col("status", "TEXT", false);
        status.variants = vec!["draft".into()];
        let old = snapshot(vec![col("id", "INTEGER", false), col("score", "INTEGER", true), status.clone()]);
        status.variants.push("published".into());
        status.indexed = true;
        let new = snapshot(vec![col("id", "INTEGER", false), col("score", "REAL", true), status]);
        let d = diff_for(&old, &new, pg);
        assert_eq!(
            d.statements,
            [
                "ALTER TABLE \"posts\" ALTER COLUMN \"score\" TYPE DOUBLE PRECISION USING \"score\"::DOUBLE PRECISION;",
                "ALTER TABLE \"posts\" DROP CONSTRAINT IF EXISTS \"posts_status_check\";",
                "ALTER TABLE \"posts\" ADD CONSTRAINT \"posts_status_check\" CHECK (\"status\" IN ('draft', 'published'));",
                "CREATE INDEX \"idx_posts_status\" ON \"posts\" (\"status\");",
            ]
        );
    }

    #[test]
    fn new_tables_are_created_after_the_tables_they_reference() {
        let mut author = col("author_id", "INTEGER", false);
        author.references = Some("users".into());
        let mut new = snapshot(vec![col("id", "INTEGER", false), author]);
        new.tables.insert(
            "users".into(),
            TableSnapshot { columns: vec![col("id", "INTEGER", false)], indexes: vec![], one_of: vec![] },
        );
        let d = diff_for(&Snapshot::default(), &new, &crate::dialect::Postgres);
        let users = d.statements.iter().position(|s| s.starts_with("CREATE TABLE \"users\"")).unwrap();
        let posts = d.statements.iter().position(|s| s.starts_with("CREATE TABLE \"posts\"")).unwrap();
        assert!(users < posts, "{:?}", d.statements);
        assert!(d.statements[posts].contains("BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY"));
    }

    fn rsvps(indexes: Vec<IndexSnapshot>, one_of: Vec<Vec<String>>) -> Snapshot {
        let mut event = col("event_id", "INTEGER", true);
        event.references = Some("events".into());
        event.on_delete = Some("CASCADE".into());
        let columns = vec![col("id", "INTEGER", false), event, col("user_id", "INTEGER", true)];
        Snapshot {
            backend: None,
            migration: None,
            tables: BTreeMap::from([("rsvps".into(), TableSnapshot { columns, indexes, one_of })]),
        }
    }

    fn pair(unique: bool, condition: Option<&str>) -> IndexSnapshot {
        IndexSnapshot {
            columns: vec!["event_id".into(), "user_id".into()],
            unique,
            condition: condition.map(Into::into),
        }
    }

    #[test]
    fn tables_get_their_indexes_checks_and_cascades() {
        let new = rsvps(vec![pair(true, None)], vec![vec!["event_id".into(), "user_id".into()]]);
        let d = diff(&Snapshot::default(), &new);
        assert!(d.statements[0].contains("REFERENCES \"events\" (\"id\") ON DELETE CASCADE"), "{}", d.statements[0]);
        assert!(d.statements[0].contains(
            "CONSTRAINT \"rsvps_one_of_event_id_user_id\" CHECK (CASE WHEN \"event_id\" IS NOT NULL THEN 1 ELSE 0 END + CASE WHEN \"user_id\" IS NOT NULL THEN 1 ELSE 0 END = 1)"
        ));
        assert_eq!(
            d.statements.last().unwrap(),
            "CREATE UNIQUE INDEX \"idx_rsvps_event_id_user_id\" ON \"rsvps\" (\"event_id\", \"user_id\");"
        );
        // The snapshot round-trips, and an unchanged schema diffs to nothing.
        let json = serde_json::to_string(&new).unwrap();
        assert!(json.contains("\"on_delete\":\"CASCADE\""), "{json}");
        assert!(diff(&serde_json::from_str(&json).unwrap(), &new).is_empty());
    }

    #[test]
    fn table_indexes_change_in_place() {
        let old = rsvps(vec![pair(false, None)], vec![]);
        let new = rsvps(vec![pair(true, Some("\"user_id\" IS NOT NULL"))], vec![]);
        for d in [diff(&old, &new), diff_for(&old, &new, &crate::dialect::Postgres)] {
            assert_eq!(
                d.statements,
                [
                    "DROP INDEX \"idx_rsvps_event_id_user_id\";",
                    "CREATE UNIQUE INDEX \"idx_rsvps_event_id_user_id_partial\" ON \"rsvps\" (\"event_id\", \"user_id\") WHERE \"user_id\" IS NOT NULL;",
                ]
            );
        }
        // SQLite rebuilds for a new CHECK; Postgres adds the constraint.
        let checked = rsvps(vec![pair(false, None)], vec![vec!["event_id".into(), "user_id".into()]]);
        let d = diff(&old, &checked);
        assert!(d.rebuilds);
        assert!(
            d.statements.iter().any(|s| s.starts_with("CREATE INDEX \"idx_rsvps_event_id_user_id\"")),
            "{:?}",
            d.statements
        );
        let d = diff_for(&old, &checked, &crate::dialect::Postgres);
        assert_eq!(d.statements.len(), 1);
        assert!(
            d.statements[0].starts_with("ALTER TABLE \"rsvps\" ADD CONSTRAINT \"rsvps_one_of_event_id_user_id\" CHECK")
        );
    }

    #[tokio::test]
    async fn cascades_and_checks_hold_in_sqlite() {
        let db = Db::memory().await.unwrap();
        let mut events = snapshot(vec![col("id", "INTEGER", false)]);
        events.tables = BTreeMap::from([("events".into(), events.tables.remove("posts").unwrap())]);
        let mut both = rsvps(vec![pair(true, None)], vec![vec!["event_id".into(), "user_id".into()]]);
        both.tables.extend(events.tables);
        for statement in diff(&Snapshot::default(), &both).statements {
            db.execute(&statement).await.unwrap();
        }
        db.execute("INSERT INTO events (id) VALUES (1)").await.unwrap();
        db.execute("INSERT INTO rsvps (event_id) VALUES (1)").await.unwrap();
        assert!(db.execute("INSERT INTO rsvps (event_id, user_id) VALUES (1, 2)").await.is_err(), "one_of holds");
        assert!(db.execute("INSERT INTO rsvps (user_id) VALUES (NULL)").await.is_err(), "one_of needs one");
        db.execute("DELETE FROM events WHERE id = 1").await.unwrap();
        let left = db.fetch_all("SELECT COUNT(*) FROM rsvps", vec![]).await.unwrap();
        assert_eq!(left[0].get::<i64, _>(0), 0, "the RSVP went with its event");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-migrate-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const M1: &str = "-- kiln: backend sqlite\nCREATE TABLE \"a\" (\"id\" INTEGER PRIMARY KEY);\n";
    const M2: &str = "-- kiln: backend sqlite\nCREATE TABLE \"b\" (\"id\" INTEGER PRIMARY KEY);\n";
    const M3: &str = "-- kiln: backend sqlite\nCREATE TABLE \"c\" (\"id\" INTEGER PRIMARY KEY);\n";

    #[test]
    fn squash_keeps_one_file_per_past_year() {
        let dir = temp_dir("squash");
        fs::write(dir.join("20250103120000_create_a.sql"), M1).unwrap();
        fs::write(dir.join("20250611120000_create_b.sql"), M2).unwrap();
        fs::write(dir.join("20260102120000_create_c.sql"), M3).unwrap();
        let done = squash(&dir, "2026").unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].parts, 2);
        let files: Vec<String> =
            load_migrations(&dir).unwrap().iter().map(|m| format!("{}_{}", m.version, m.name)).collect();
        assert_eq!(files, ["20250611120000_squash_2025", "20260102120000_create_c"]);
        let squashed = &load_migrations(&dir).unwrap()[0];
        assert!(squashed.is_squash());
        assert_eq!(squashed.backend(), "sqlite");
        let parts: Vec<String> = squashed.parts().iter().map(|p| p.version.clone()).collect();
        assert_eq!(parts, ["20250103120000", "20250611120000"]);
        // Squashing again changes nothing.
        assert!(squash(&dir, "2026").unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn squashes_run_only_the_parts_a_database_has_not() {
        let dir = temp_dir("partial");
        fs::write(dir.join("20250103120000_create_a.sql"), M1).unwrap();
        let db = Db::memory().await.unwrap();
        assert_eq!(db.migrate(&dir).await.unwrap().len(), 1);
        // A teammate's migration lands, then the year is squashed before this database ran it.
        fs::write(dir.join("20250611120000_create_b.sql"), M2).unwrap();
        squash(&dir, "2026").unwrap();
        assert_eq!(db.pending_migrations(&dir).await.unwrap().len(), 1);
        db.migrate(&dir).await.unwrap(); // would fail if `a` were created again
        assert!(db.pending_migrations(&dir).await.unwrap().is_empty());
        assert_eq!(db.applied_migrations().await.unwrap(), ["20250103120000", "20250611120000"]);
        // A fresh database runs every part.
        let fresh = Db::memory().await.unwrap();
        fresh.migrate(&dir).await.unwrap();
        fresh.execute("INSERT INTO \"b\" (\"id\") VALUES (1)").await.unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
