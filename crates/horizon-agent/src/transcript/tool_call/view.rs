/// One filesystem path affected by a successful mutation. A separate list
/// is necessary because one `fs_edit` call carries a whole batch of edits
/// behind a single receipt row and may change many files; its display
/// target alone cannot preserve that cardinality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffect {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    pub created: bool,
}
