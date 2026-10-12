/// A table as described by a `#[derive(Record)]` struct.
#[derive(Debug, Clone)]
pub struct TableSchema {
    pub table: &'static str,
    pub columns: Vec<ColumnSchema>,
}

#[derive(Debug, Clone)]
pub struct ColumnSchema {
    pub name: &'static str,
    pub sql_type: &'static str,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    pub indexed: bool,
    pub references: Option<&'static str>,
    pub variants: &'static [&'static str],
}

impl TableSchema {
    /// `CREATE TABLE` plus one `CREATE INDEX` per unique, indexed or foreign-key column.
    pub fn create_sql(&self) -> Vec<String> {
        let definitions: Vec<String> = self.columns.iter().map(ColumnSchema::definition).collect();
        let mut statements =
            vec![format!("CREATE TABLE IF NOT EXISTS \"{}\" (\n  {}\n)", self.table, definitions.join(",\n  "))];
        for column in &self.columns {
            let name = format!("idx_{}_{}", self.table, column.name);
            if column.unique {
                statements.push(format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS \"{name}\" ON \"{}\" (\"{}\")",
                    self.table, column.name
                ));
            } else if column.indexed || column.references.is_some() {
                statements
                    .push(format!("CREATE INDEX IF NOT EXISTS \"{name}\" ON \"{}\" (\"{}\")", self.table, column.name));
            }
        }
        statements
    }
}

impl ColumnSchema {
    fn definition(&self) -> String {
        let mut sql = format!("\"{}\" {}", self.name, self.sql_type);
        if self.primary_key {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
            return sql;
        }
        if !self.nullable {
            sql.push_str(" NOT NULL");
        }
        if let Some(table) = self.references {
            sql.push_str(&format!(" REFERENCES \"{table}\" (\"id\")"));
        }
        if !self.variants.is_empty() {
            let values: Vec<String> = self.variants.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
            sql.push_str(&format!(" CHECK (\"{}\" IN ({}))", self.name, values.join(", ")));
        }
        sql
    }
}
