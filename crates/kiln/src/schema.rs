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
    /// An `Attachment` column (its files are kept while a row holds them).
    pub attachment: bool,
    /// A `RichText` column (its attachments' files are kept while it links them).
    pub rich_text: bool,
}
