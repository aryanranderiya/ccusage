use std::{collections::HashSet, path::PathBuf};

use crate::{LoadedEntry, PricingMap, Result, cli::SharedArgs, debug_log, parse_tz};

use super::{parser, paths};

pub fn load_entries(
    shared: &SharedArgs,
    custom_path: Option<&str>,
    pricing: Option<&PricingMap>,
) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(
        crate::progress::UsageLoadAgent("fx"),
        shared.json,
        || load_entries_inner(shared, custom_path, pricing),
    )
}

fn load_entries_inner(
    shared: &SharedArgs,
    custom_path: Option<&str>,
    pricing: Option<&PricingMap>,
) -> Result<Vec<LoadedEntry>> {
    load_entries_from_paths(shared, paths::paths(custom_path)?, pricing)
}

fn load_entries_from_paths(
    shared: &SharedArgs,
    paths: Vec<PathBuf>,
    pricing: Option<&PricingMap>,
) -> Result<Vec<LoadedEntry>> {
    let tz = parse_tz(shared.timezone.as_deref());
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        // fx writes every generation fact into a single usage.jsonl at the
        // root of each data directory; session directories only contribute
        // attribution (session id + workspace), never extra token rows.
        let result = parser::read_data_dir(&path, tz.as_ref(), shared.mode, pricing)
            .unwrap_or_else(|error| {
                debug_log(
                    shared,
                    format!("Failed to read fx data dir {}: {error}", path.display()),
                );
                Vec::new()
            });
        for entry in result {
            let id = parser::entry_id(&entry);
            if seen.insert(id) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

pub fn has_data() -> bool {
    paths::paths(None)
        .is_ok_and(|dirs| dirs.iter().any(|dir| dir.join("usage.jsonl").is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PricingMap;
    use ccusage_test_support::{EnvVarGuard, fs_fixture};

    fn shared_args() -> SharedArgs {
        SharedArgs {
            timezone: Some("UTC".to_string()),
            ..SharedArgs::default()
        }
    }

    #[test]
    fn loads_generations_with_session_and_workspace_attribution() {
        let fixture = fs_fixture!({
            ".fx/usage.jsonl": [
                r#"{"schema_version":1,"kind":"coverage","started_at_ms":1787166151090}"#,
                r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_1","created_at_ms":1787166200000,"model":"zai/glm-5.2","input_tokens":100,"output_tokens":10,"cache_read_tokens":5,"cache_write_tokens":0,"reasoning_tokens":0,"billable_web_search_calls":0,"total_cost":0.01}}"#,
                r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_2","created_at_ms":1787166300000,"model":"zai/glm-5.2-fast","input_tokens":200,"output_tokens":20,"cache_read_tokens":0,"cache_write_tokens":2,"reasoning_tokens":0,"billable_web_search_calls":0,"total_cost":0}}"#,
                r#"{"schema_version":1,"kind":"incident","occurred_at_ms":1787166520804,"completeness":"incomplete"}"#,
            ]
            .join("\n"),
            ".fx/sessions/1787166124307-abc/events.jsonl": [
                r#"{"schema_version":1,"seq":1,"event_id":"e1","timestamp_ms":1787166124307,"kind":"session_started","payload":{"id":"1787166124307-abc","created_at_ms":1787166124307,"origin_workspace_root":"/Users/me/Projects/alpha","workspace_root":"/Users/me/Projects/alpha"}}"#,
            ]
            .join("\n"),
        });
        let _home = EnvVarGuard::set("HOME", fixture.root());
        let entries = load_entries(&shared_args(), None, Some(&PricingMap::load_embedded()))
            .expect("load fx entries");

        assert_eq!(entries.len(), 2);
        let first = &entries[0];
        assert_eq!(first.data.message.id.as_deref(), Some("gen_1"));
        assert_eq!(first.session_id.as_ref(), "1787166124307-abc");
        assert_eq!(first.project.as_ref(), "/Users/me/Projects/alpha");
        assert_eq!(first.model.as_deref(), Some("[fx] zai/glm-5.2"));
        assert_eq!(
            first.data.cost_usd,
            Some(0.01),
            "embedded display cost must survive"
        );
        let second = &entries[1];
        assert_eq!(
            second.data.message.usage.cache_creation_input_tokens,
            2,
            "cache write tokens map from cache_write_tokens"
        );
    }

    #[test]
    fn generation_before_any_session_falls_back_to_data_dir_name() {
        let fixture = fs_fixture!({
            ".fx/usage.jsonl": r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_early","created_at_ms":1000,"model":"m","input_tokens":3,"output_tokens":4,"cache_read_tokens":0,"cache_write_tokens":0,"total_cost":0}}"#,
        });
        let _home = EnvVarGuard::set("HOME", fixture.root());

        let entries = load_entries(&shared_args(), None, None).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id.as_ref(), "(no session)");
        assert_eq!(entries[0].project.as_ref(), ".fx");
    }

    #[test]
    fn zero_token_generations_are_skipped() {
        let fixture = fs_fixture!({
            ".fx/usage.jsonl": r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_zero","created_at_ms":1787166150000,"model":"zai/glm-5.2","input_tokens":0,"output_tokens":0,"cache_read_tokens":0,"cache_write_tokens":0,"total_cost":0}}"#,
        });
        let _home = EnvVarGuard::set("HOME", fixture.root());

        let entries = load_entries(&shared_args(), None, None).unwrap();

        assert!(entries.is_empty());
    }

    #[test]
    fn duplicate_directories_dedupe_by_generation_id() {
        let fixture = fs_fixture!({
            "one/usage.jsonl": r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_dup","created_at_ms":1787166150000,"model":"m","input_tokens":1,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0,"total_cost":0}}"#,
            "two/usage.jsonl": r#"{"schema_version":1,"kind":"generation","fact":{"id":"gen_dup","created_at_ms":1787166150000,"model":"m","input_tokens":1,"output_tokens":1,"cache_read_tokens":0,"cache_write_tokens":0,"total_cost":0}}"#,
        });
        let one = fixture.path("one").to_string_lossy().to_string();
        let two = fixture.path("two").to_string_lossy().to_string();
        let combined = format!("{one},{two}");

        let entries =
            load_entries(&shared_args(), Some(&combined), None).unwrap();

        assert_eq!(entries.len(), 1, "same generation id across dirs dedupes");
    }

    #[test]
    fn has_data_detects_usage_jsonl() {
        let fixture = fs_fixture!({
            ".fx/usage.jsonl": "{}\n",
        });
        let _home = EnvVarGuard::set("HOME", fixture.root());
        assert!(has_data());
    }

    #[test]
    fn has_data_is_false_without_usage_jsonl() {
        let fixture = fs_fixture!({
            "sessions/abc/events.jsonl": "{}",
        });
        let _home = EnvVarGuard::set("HOME", fixture.root());
        assert!(!has_data());
    }
}
