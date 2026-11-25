//! The spelling of a model combo: `main-provider/main-id+sub-provider/sub-id[@effort]`.
//! Lives here so the model policy can check both halves without depending on
//! maki-providers, which does the resolving.

use maki_storage::sessions::Effort;

pub const COMBO_SEPARATOR: char = '+';
pub const EFFORT_SEPARATOR: char = '@';

/// A combo spec split into its parts. The sub half never carries an effort
/// suffix; that belongs to the combo as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComboParts<'a> {
    pub main: &'a str,
    pub sub: &'a str,
    pub effort: Option<Effort>,
}

/// Splits a combo spec, or `None` when the string is not one. A `+` only
/// separates when both sides look like a spec (`provider/id`), so model ids
/// that merely contain `+` stay ordinary specs. An `@effort` suffix counts
/// only when it names a known effort level.
pub fn split(spec: &str) -> Option<ComboParts<'_>> {
    let (body, effort) = match spec.rsplit_once(EFFORT_SEPARATOR) {
        Some((body, suffix)) => match suffix.parse::<Effort>() {
            Ok(effort) => (body, Some(effort)),
            Err(_) => (spec, None),
        },
        None => (spec, None),
    };
    let mut splits = body.match_indices(COMBO_SEPARATOR).filter(|&(i, _)| {
        looks_like_spec(&body[..i]) && looks_like_spec(&body[i + COMBO_SEPARATOR.len_utf8()..])
    });
    let (i, _) = splits.next()?;
    if splits.next().is_some() {
        return None;
    }
    Some(ComboParts {
        main: &body[..i],
        sub: &body[i + COMBO_SEPARATOR.len_utf8()..],
        effort,
    })
}

pub fn is_combo(spec: &str) -> bool {
    split(spec).is_some()
}

/// Builds the canonical spelling. `effort` of `None` leaves the suffix off,
/// which reads as "follow the session".
pub fn join(main: &str, sub: &str, effort: Option<Effort>) -> String {
    let mut spec = format!("{main}{COMBO_SEPARATOR}{sub}");
    if let Some(effort) = effort {
        spec.push(EFFORT_SEPARATOR);
        spec.push_str(effort.as_str());
    }
    spec
}

fn looks_like_spec(s: &str) -> bool {
    s.split_once('/')
        .is_some_and(|(provider, id)| !provider.is_empty() && !id.is_empty())
}

/// The next effort in the picker's cycle: follow the session, then every
/// level ascending, then back to following the session.
pub fn next_effort(current: Option<Effort>) -> Option<Effort> {
    match current {
        None => Some(Effort::ALL[0]),
        Some(effort) => Effort::ALL
            .iter()
            .position(|&e| e == effort)
            .and_then(|i| Effort::ALL.get(i + 1))
            .copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::{join, split};
    use maki_storage::sessions::Effort;
    use test_case::test_case;

    const MAIN: &str = "anthropic/claude-opus-4-6";
    const SUB: &str = "anthropic/claude-haiku-4-5";

    #[test]
    fn plain_combo_splits_into_halves() {
        let spec = format!("{MAIN}+{SUB}");
        let parts = split(&spec).unwrap();
        assert_eq!(parts.main, MAIN);
        assert_eq!(parts.sub, SUB);
        assert_eq!(parts.effort, None);
    }

    #[test_case(Effort::Minimal ; "minimal")]
    #[test_case(Effort::High ; "high")]
    #[test_case(Effort::Max ; "max")]
    fn effort_suffix_roundtrips(effort: Effort) {
        let spec = join(MAIN, SUB, Some(effort));
        let parts = split(&spec).unwrap();
        assert_eq!(parts.main, MAIN);
        assert_eq!(parts.sub, SUB);
        assert_eq!(parts.effort, Some(effort));
    }

    #[test_case("anthropic/claude-opus-4-6" ; "plain_spec")]
    #[test_case("anthropic/gpt+4" ; "plus_in_id_without_second_slash")]
    #[test_case("+anthropic/haiku" ; "empty_main")]
    #[test_case("anthropic/opus+" ; "empty_sub")]
    #[test_case("anthropic/a+anthropic/b+anthropic/c" ; "ambiguous_split")]
    fn non_combos_stay_ordinary(spec: &str) {
        assert_eq!(split(spec), None);
    }

    #[test]
    fn unknown_effort_suffix_stays_part_of_the_id() {
        let spec = format!("{MAIN}+{SUB}@turbo");
        let parts = split(&spec).unwrap();
        assert_eq!(parts.sub, format!("{SUB}@turbo"));
        assert_eq!(parts.effort, None);
    }

    #[test]
    fn join_without_effort_omits_the_suffix() {
        assert_eq!(join(MAIN, SUB, None), format!("{MAIN}+{SUB}"));
    }
}
