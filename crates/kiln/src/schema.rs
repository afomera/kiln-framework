/// A table as described by a `#[derive(Record)]` struct.
#[derive(Debug, Clone)]
pub struct TableSchema {
    pub table: &'static str,
    pub columns: Vec<ColumnSchema>,
    /// Indexes over several columns, or partial ones: `#[record(unique(event_id, user_id))]`.
    pub indexes: Vec<IndexSchema>,
    /// `#[record(one_of(work_item_id, comment_id))]`: exactly one of these columns is set.
    pub one_of: Vec<&'static [&'static str]>,
}

/// `#[record(unique(a, b))]`, `#[record(index(a, b))]`, or with `where = ".."` a partial
/// index (only rows matching the SQL condition are indexed).
#[derive(Debug, Clone)]
pub struct IndexSchema {
    pub columns: &'static [&'static str],
    pub unique: bool,
    pub condition: Option<&'static str>,
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
    /// `#[on_delete(cascade)]` / `#[on_delete(set_null)]` on a foreign key: `CASCADE`,
    /// `SET NULL`. Without one, deleting a referenced row fails while rows point at it.
    pub on_delete: Option<&'static str>,
    pub variants: &'static [&'static str],
    /// An `Attachment` column (its files are kept while a row holds them).
    pub attachment: bool,
    /// A `RichText` column (its attachments' files are kept while it links them).
    pub rich_text: bool,
}
