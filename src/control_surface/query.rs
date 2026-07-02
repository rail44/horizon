pub(super) fn palette_matches(query: &str, fields: &[&str]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .any(|field| normalize_palette_query(field).contains(query))
}

pub(super) fn normalize_palette_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
