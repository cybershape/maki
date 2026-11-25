use std::collections::HashSet;

use crate::model::ModelEntry;

const NO_PREFIXES: &str = "no prefixes";
const DUPLICATE_PREFIX: &str = "duplicate prefix";
const DUPLICATE_DEFAULT: &str = "second default row";
const OUTPUT_EXCEEDS_WINDOW: &str = "exceeds context_window";

pub(crate) fn check_rows(rows: &[ModelEntry]) -> Result<(), String> {
    let mut prefixes_seen: HashSet<&str> = HashSet::new();
    let mut default_seen = false;

    for (index, row) in rows.iter().enumerate() {
        let Some(name) = row.prefixes.first() else {
            return Err(format!("row {}: {NO_PREFIXES}", index + 1));
        };
        let at = format!("{name:?}");

        for prefix in &row.prefixes {
            if !prefixes_seen.insert(prefix) {
                return Err(format!("{at}: {DUPLICATE_PREFIX} {prefix:?}"));
            }
        }
        if let (Some(max_output), Some(context_window)) =
            (row.max_output_tokens, row.context_window)
            && max_output > context_window
        {
            return Err(format!(
                "{at}: max_output_tokens {max_output} {OUTPUT_EXCEEDS_WINDOW} {context_window}"
            ));
        }
        if row.default {
            if default_seen {
                return Err(format!("{at}: {DUPLICATE_DEFAULT}"));
            }
            default_seen = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(
        prefixes: Vec<&str>,
        default: bool,
        context_window: Option<u32>,
        max_output: Option<u32>,
    ) -> ModelEntry {
        ModelEntry {
            prefixes: prefixes.into_iter().map(String::from).collect(),
            default,
            context_window,
            max_output_tokens: max_output,
            ..ModelEntry::default()
        }
    }

    #[test]
    fn check_rows_rejects_empty_prefixes() {
        let rows = vec![test_entry(vec![], true, Some(1000), None)];
        assert!(check_rows(&rows).unwrap_err().contains(NO_PREFIXES));
    }

    #[test]
    fn check_rows_rejects_duplicate_prefixes() {
        let rows = vec![
            test_entry(vec!["a"], true, Some(1000), None),
            test_entry(vec!["a"], false, Some(1000), None),
        ];
        assert!(check_rows(&rows).unwrap_err().contains(DUPLICATE_PREFIX));
    }

    #[test]
    fn check_rows_rejects_duplicate_defaults() {
        let rows = vec![
            test_entry(vec!["a"], true, Some(1000), None),
            test_entry(vec!["b"], true, Some(1000), None),
        ];
        assert!(check_rows(&rows).unwrap_err().contains(DUPLICATE_DEFAULT));
    }

    #[test]
    fn check_rows_rejects_output_exceeding_window() {
        let rows = vec![test_entry(vec!["a"], true, Some(1000), Some(2000))];
        assert!(
            check_rows(&rows)
                .unwrap_err()
                .contains(OUTPUT_EXCEEDS_WINDOW)
        );
    }
}
