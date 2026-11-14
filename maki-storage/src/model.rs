use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;
use crate::sessions::StoredThinking;
use crate::{StateDir, atomic_write};

const MODEL_FILE: &str = "model";
const RECENT_FILE: &str = "recent-models";
const THINKING_FILE: &str = "thinking";
const PROJECT_THINKING_FILE: &str = "model_thinking.json";
const EXPANDED_FILE: &str = "expanded-providers";
const MAX_RECENTS: usize = 6;

pub fn persist_model(dir: &StateDir, spec: &str) {
    let _ = atomic_write(&dir.path().join(MODEL_FILE), spec.as_bytes());
}

pub fn read_model(dir: &StateDir) -> Option<String> {
    let raw = fs::read_to_string(dir.path().join(MODEL_FILE)).ok()?;
    let spec = raw.trim();
    (!spec.is_empty()).then(|| spec.to_owned())
}

/// Written in the `/thinking` spelling so the file reads like the command
/// that produced it, and a hand-edited or stale value just fails to parse.
pub fn persist_thinking(dir: &StateDir, thinking: StoredThinking) {
    let _ = atomic_write(
        &dir.path().join(THINKING_FILE),
        thinking.to_string().as_bytes(),
    );
}

pub fn read_thinking(dir: &StateDir) -> Option<StoredThinking> {
    let raw = fs::read_to_string(dir.path().join(THINKING_FILE)).ok()?;
    StoredThinking::parse_setting(&raw).ok()
}

pub fn project_model_path(dir: &StateDir, cwd: &Path) -> PathBuf {
    paths::project_dir(dir, cwd).join(MODEL_FILE)
}

pub fn persist_project_model(dir: &StateDir, cwd: &Path, spec: &str) {
    let path = project_model_path(dir, cwd);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = atomic_write(&path, spec.as_bytes());
}

pub fn read_project_model(dir: &StateDir, cwd: &Path) -> Option<String> {
    let raw = fs::read_to_string(project_model_path(dir, cwd)).ok()?;
    let spec = raw.trim();
    (!spec.is_empty()).then(|| spec.to_owned())
}

pub fn project_model_thinking_path(dir: &StateDir, cwd: &Path) -> PathBuf {
    paths::project_dir(dir, cwd).join(PROJECT_THINKING_FILE)
}

pub fn persist_project_model_thinking(
    dir: &StateDir,
    cwd: &Path,
    model_spec: &str,
    thinking: StoredThinking,
) {
    let path = project_model_thinking_path(dir, cwd);
    let mut map = read_project_model_thinking_map(&path);
    map.insert(model_spec.to_owned(), thinking.to_string());
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&map) {
        let _ = atomic_write(&path, &json);
    }
}

pub fn read_project_model_thinking(
    dir: &StateDir,
    cwd: &Path,
    model_spec: &str,
) -> Option<StoredThinking> {
    let path = project_model_thinking_path(dir, cwd);
    let map = read_project_model_thinking_map(&path);
    let val = map.get(model_spec)?;
    StoredThinking::parse_setting(val).ok()
}

fn read_project_model_thinking_map(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Providers the model picker lists unfolded, by slug, since it folds the rest.
/// Kept as a sorted set, so unfolding and folding the same provider leaves the
/// file as it was.
pub fn read_expanded_providers(dir: &StateDir) -> BTreeSet<String> {
    fs::read_to_string(dir.path().join(EXPANDED_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn persist_expanded_providers(dir: &StateDir, slugs: &BTreeSet<String>) {
    if let Ok(json) = serde_json::to_vec_pretty(slugs) {
        let _ = atomic_write(&dir.path().join(EXPANDED_FILE), &json);
    }
}

#[derive(Serialize, Deserialize, Default)]
struct RecentList(Vec<String>);

pub fn push_recent(dir: &StateDir, spec: &str) -> Vec<String> {
    let mut recents = read_recents(dir);
    recents.retain(|s| s != spec);
    recents.insert(0, spec.to_owned());
    recents.truncate(MAX_RECENTS);
    write_recents(dir, &recents);
    recents
}

/// Newest first, so a list written by a launch that predates the migration
/// does not bury the history it replaced. Identical to `push_recent` for one
/// spec, applied to a whole list.
pub fn merge_recents(new: &[String], old: &[String]) -> Vec<String> {
    let mut merged = new.to_vec();
    for spec in old {
        if !merged.contains(spec) {
            merged.push(spec.clone());
        }
    }
    merged.truncate(MAX_RECENTS);
    merged
}

pub fn read_recents(dir: &StateDir) -> Vec<String> {
    let Ok(raw) = fs::read_to_string(dir.path().join(RECENT_FILE)) else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<RecentList>(trimmed) {
        Ok(list) => list.0,
        Err(_) => Vec::new(),
    }
}

fn write_recents(dir: &StateDir, recents: &[String]) {
    let json = match serde_json::to_vec_pretty(&RecentList(recents.to_vec())) {
        Ok(v) => v,
        Err(_) => return,
    };
    let _ = atomic_write(&dir.path().join(RECENT_FILE), &json);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::Effort;
    use tempfile::TempDir;
    use test_case::test_case;

    #[test]
    fn round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        assert!(read_model(&dir).is_none());

        persist_model(&dir, "anthropic/claude-sonnet-4");
        assert_eq!(
            read_model(&dir).as_deref(),
            Some("anthropic/claude-sonnet-4")
        );

        persist_model(&dir, "openai/gpt-5.4-nano");
        assert_eq!(read_model(&dir).as_deref(), Some("openai/gpt-5.4-nano"));
    }

    #[test]
    fn whitespace_and_empty_treated_as_none() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        fs::write(dir.path().join(MODEL_FILE), "  \n").unwrap();
        assert!(read_model(&dir).is_none());

        fs::write(dir.path().join(MODEL_FILE), "").unwrap();
        assert!(read_model(&dir).is_none());
    }

    #[test_case(StoredThinking::Off)]
    #[test_case(StoredThinking::Adaptive)]
    #[test_case(StoredThinking::Effort { level: Effort::High })]
    #[test_case(StoredThinking::Budget { tokens: 8192 })]
    fn thinking_round_trip(thinking: StoredThinking) {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        assert!(read_thinking(&dir).is_none());
        persist_thinking(&dir, thinking);
        assert_eq!(read_thinking(&dir), Some(thinking));
    }

    #[test_case("" ; "empty")]
    #[test_case("  \n" ; "whitespace")]
    #[test_case("turbo" ; "unknown level")]
    #[test_case("0" ; "zero budget")]
    fn read_thinking_ignores_invalid(raw: &str) {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        fs::write(dir.path().join(THINKING_FILE), raw).unwrap();
        assert!(read_thinking(&dir).is_none());
    }

    #[test]
    fn push_recent_dedupes_and_orders_most_recent_first() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        push_recent(&dir, "anthropic/claude-sonnet-4");
        push_recent(&dir, "openai/gpt-5.4-nano");
        assert_eq!(
            read_recents(&dir),
            ["openai/gpt-5.4-nano", "anthropic/claude-sonnet-4"]
        );

        push_recent(&dir, "anthropic/claude-sonnet-4");
        assert_eq!(
            read_recents(&dir),
            ["anthropic/claude-sonnet-4", "openai/gpt-5.4-nano"]
        );
    }

    #[test]
    fn merge_recents_keeps_both_sides_newest_first() {
        let merged = merge_recents(
            &["new/one".to_owned()],
            &["old/one".to_owned(), "new/one".to_owned()],
        );
        assert_eq!(merged, ["new/one", "old/one"]);
    }

    #[test]
    fn merge_recents_caps_at_max() {
        let new: Vec<String> = (0..MAX_RECENTS).map(|i| format!("new/{i}")).collect();
        let old: Vec<String> = (0..MAX_RECENTS).map(|i| format!("old/{i}")).collect();
        assert_eq!(merge_recents(&new, &old).len(), MAX_RECENTS);
    }

    #[test]
    fn push_recent_caps_at_max() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        for i in 0..(MAX_RECENTS + 3) {
            push_recent(&dir, &format!("p/model-{i}"));
        }
        let recents = read_recents(&dir);
        assert_eq!(recents.len(), MAX_RECENTS);
        assert_eq!(recents[0], format!("p/model-{}", MAX_RECENTS + 2));
    }

    #[test]
    fn push_recent_returns_final_list() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        let recents = push_recent(&dir, "a/b");
        assert_eq!(recents, vec!["a/b".to_string()]);
        assert_eq!(recents, read_recents(&dir));
    }

    #[test]
    fn read_recents_handles_missing_or_invalid() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        assert!(read_recents(&dir).is_empty());

        fs::write(dir.path().join(RECENT_FILE), "not json").unwrap();
        assert!(read_recents(&dir).is_empty());

        fs::write(dir.path().join(RECENT_FILE), "  \n").unwrap();
        assert!(read_recents(&dir).is_empty());
    }

    #[test]
    fn project_model_thinking_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let cwd = tmp.path().join("my-project");
        fs::create_dir_all(&cwd).unwrap();

        const MODEL_A: &str = "anthropic/claude-3-7-sonnet";
        const MODEL_B: &str = "openai/gpt-5";

        assert_eq!(read_project_model_thinking(&dir, &cwd, MODEL_A), None);
        assert_eq!(read_project_model_thinking(&dir, &cwd, MODEL_B), None);

        persist_project_model_thinking(&dir, &cwd, MODEL_A, StoredThinking::Adaptive);
        persist_project_model_thinking(
            &dir,
            &cwd,
            MODEL_B,
            StoredThinking::Effort {
                level: Effort::High,
            },
        );

        assert_eq!(
            read_project_model_thinking(&dir, &cwd, MODEL_A),
            Some(StoredThinking::Adaptive)
        );
        assert_eq!(
            read_project_model_thinking(&dir, &cwd, MODEL_B),
            Some(StoredThinking::Effort {
                level: Effort::High,
            })
        );

        // Overwrite
        persist_project_model_thinking(&dir, &cwd, MODEL_A, StoredThinking::Off);
        assert_eq!(
            read_project_model_thinking(&dir, &cwd, MODEL_A),
            Some(StoredThinking::Off)
        );
        assert_eq!(
            read_project_model_thinking(&dir, &cwd, MODEL_B),
            Some(StoredThinking::Effort {
                level: Effort::High,
            })
        );
    }

    #[test]
    fn project_model_thinking_shares_repo_root() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let repo_root = tmp.path().join("repo");
        let sub_dir = repo_root.join("src").join("nested");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        fs::create_dir_all(&sub_dir).unwrap();

        const MODEL: &str = "model/spec";
        persist_project_model_thinking(&dir, &sub_dir, MODEL, StoredThinking::Adaptive);
        assert_eq!(
            read_project_model_thinking(&dir, &repo_root, MODEL),
            Some(StoredThinking::Adaptive)
        );
    }

    #[test]
    fn project_model_thinking_ignores_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let path = project_model_thinking_path(&dir, &cwd);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "invalid json content").unwrap();

        assert_eq!(read_project_model_thinking(&dir, &cwd, "any/model"), None);
    }

    #[test]
    fn project_model_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let cwd = tmp.path().join("my-project");
        fs::create_dir_all(&cwd).unwrap();

        const MODEL_A: &str = "anthropic/claude-3-7-sonnet";
        const MODEL_B: &str = "openai/gpt-5";

        assert_eq!(read_project_model(&dir, &cwd), None);

        persist_project_model(&dir, &cwd, MODEL_A);
        assert_eq!(read_project_model(&dir, &cwd).as_deref(), Some(MODEL_A));

        // Overwrite
        persist_project_model(&dir, &cwd, MODEL_B);
        assert_eq!(read_project_model(&dir, &cwd).as_deref(), Some(MODEL_B));
    }

    #[test]
    fn project_model_shares_repo_root() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let repo_root = tmp.path().join("repo");
        let sub_dir = repo_root.join("src").join("nested");
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        fs::create_dir_all(&sub_dir).unwrap();

        const MODEL: &str = "anthropic/claude-3-7-sonnet";
        persist_project_model(&dir, &sub_dir, MODEL);
        assert_eq!(read_project_model(&dir, &repo_root).as_deref(), Some(MODEL));
    }

    #[test]
    fn project_model_ignores_empty() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let cwd = tmp.path().join("project");
        fs::create_dir_all(&cwd).unwrap();

        let path = project_model_path(&dir, &cwd);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "  \n").unwrap();

        assert_eq!(read_project_model(&dir, &cwd), None);
    }
}
