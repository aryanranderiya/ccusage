use std::{collections::HashSet, env, path::PathBuf};

use crate::Result;

const PRIME_AGENT_DIR_ENV: &str = "PRIME_AGENT_DIR";

pub fn paths(custom_path: Option<&str>) -> Result<Vec<PathBuf>> {
    if let Some(custom_path) = custom_path.filter(|path| !path.trim().is_empty()) {
        return Ok(existing_path_list(custom_path));
    }
    if let Ok(env_paths) = env::var(PRIME_AGENT_DIR_ENV)
        && !env_paths.trim().is_empty()
    {
        return Ok(existing_path_list(&env_paths));
    }

    let home =
        crate::home::home_dir().ok_or_else(|| crate::cli_error("home directory is not set"))?;
    let path = home.join(".prime/agent/sessions");
    Ok(path.is_dir().then_some(path).into_iter().collect())
}

fn existing_path_list(raw: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir() && seen.insert(path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccusage_test_support::{EnvVarGuard, fs_fixture};

    #[test]
    fn defaults_to_prime_agent_sessions_under_home() {
        let fixture = fs_fixture!({});
        let sessions = fixture.create_dir_all(".prime/agent/sessions");
        let _home = EnvVarGuard::set("HOME", fixture.root());

        let paths = paths(None).unwrap();

        assert_eq!(paths, vec![sessions]);
    }

    #[test]
    fn prime_agent_dir_env_overrides_default() {
        let fixture = fs_fixture!({});
        let custom = fixture.create_dir_all("custom-prime");
        let _guard = EnvVarGuard::set(
            PRIME_AGENT_DIR_ENV,
            custom.to_string_lossy().to_string(),
        );

        let paths = paths(None).unwrap();

        assert_eq!(paths, vec![custom]);
    }

    #[test]
    fn custom_path_arg_overrides_env() {
        let fixture = fs_fixture!({});
        let arg_dir = fixture.create_dir_all("arg-prime");
        let _env = fixture.create_dir_all("env-prime");
        let _guard = EnvVarGuard::set(
            PRIME_AGENT_DIR_ENV,
            fixture.path("env-prime").to_string_lossy().to_string(),
        );

        let paths = paths(Some(&arg_dir.to_string_lossy())).unwrap();

        assert_eq!(paths, vec![arg_dir]);
    }

    #[test]
    fn missing_dir_yields_empty() {
        let fixture = fs_fixture!({});
        let _home = EnvVarGuard::set("HOME", fixture.root());

        let paths = paths(None).unwrap();

        assert!(paths.is_empty());
    }
}
