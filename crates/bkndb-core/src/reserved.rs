//! The physical KV table names `bkndb-core` reserves for its own internal
//! use — graph's `nodes`/`edges`/`adj_out`/`adj_in`, and the shared
//! graph+relational auto-increment counter table `meta`. A caller-chosen
//! table name (a `RelSchema.name`, or a raw KV `TableSpec`) that collides
//! with one of these silently corrupts data instead of erroring: a full
//! scan of the colliding table would also decode bkndb-core's own internal
//! bytes (e.g. counter values) as if they were the caller's rows. This
//! module is the one place that check lives, so every entry point (schema
//! construction, raw KV access) can share it.

use crate::BknError;

pub const RESERVED_TABLE_NAMES: &[&str] = &["nodes", "edges", "adj_out", "adj_in", "meta"];

pub fn is_reserved(name: &str) -> bool {
    RESERVED_TABLE_NAMES.contains(&name)
}

pub fn check_table_name(name: &'static str) -> Result<(), BknError> {
    if is_reserved(name) {
        Err(BknError::ReservedTableName(name))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_five_reserved_names_are_flagged() {
        for &name in RESERVED_TABLE_NAMES {
            assert!(is_reserved(name));
            assert!(matches!(check_table_name(name), Err(BknError::ReservedTableName(_))));
        }
    }

    #[test]
    fn an_ordinary_name_is_not_flagged() {
        assert!(!is_reserved("widgets"));
        assert!(check_table_name("widgets").is_ok());
    }
}
