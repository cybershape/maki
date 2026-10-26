use std::collections::HashSet;

use crate::model::{ModelEntry, ModelTier};

const NO_PREFIXES: &str = "no prefixes";
const DUPLICATE_PREFIX: &str = "duplicate prefix";
const DUPLICATE_DEFAULT: &str = "second default for tier";
const OUTPUT_EXCEEDS_WINDOW: &str = "exceeds context_window";

pub(crate) fn check_rows(rows: &[ModelEntry]) -> Result<(), String> {
    let mut prefixes_seen: HashSet<&str> = HashSet::new();
    let mut defaults_seen: Vec<ModelTier> = Vec::new();

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
            if defaults_seen.contains(&row.tier) {
                return Err(format!("{at}: {DUPLICATE_DEFAULT} {:?}", row.tier));
            }
            defaults_seen.push(row.tier);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(
        prefixes: Vec<&str>,
        tier: ModelTier,
        default: bool,
        context_window: Option<u32>,
        max_output: Option<u32>,
    ) -> ModelEntry {
        ModelEntry {
            prefixes: prefixes.into_iter().map(String::from).collect(),
            tier,
            default,
            context_window,
            max_output_tokens: max_output,
            ..ModelEntry::default()
        }
    }

    #[test]
    fn check_rows_rejects_empty_prefixes() {
        let rows = vec![test_entry(
            vec![],
            ModelTier::Strong,
            true,
            Some(1000),
            None,
        )];
        assert!(check_rows(&rows).unwrap_err().contains(NO_PREFIXES));
    }

    #[test]
    fn check_rows_rejects_duplicate_prefixes() {
        let rows = vec![
            test_entry(vec!["a"], ModelTier::Strong, true, Some(1000), None),
            test_entry(vec!["a"], ModelTier::Medium, false, Some(1000), None),
        ];
        assert!(check_rows(&rows).unwrap_err().contains(DUPLICATE_PREFIX));
    }

    #[test]
    fn check_rows_rejects_duplicate_defaults() {
        let rows = vec![
            test_entry(vec!["a"], ModelTier::Strong, true, Some(1000), None),
            test_entry(vec!["b"], ModelTier::Strong, true, Some(1000), None),
        ];
        assert!(check_rows(&rows).unwrap_err().contains(DUPLICATE_DEFAULT));
    }

    #[test]
    fn check_rows_rejects_output_exceeding_window() {
        let rows = vec![test_entry(
            vec!["a"],
            ModelTier::Strong,
            true,
            Some(1000),
            Some(2000),
        )];
        assert!(
            check_rows(&rows)
                .unwrap_err()
                .contains(OUTPUT_EXCEEDS_WINDOW)
        );
    }
}
