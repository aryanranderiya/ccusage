use std::{collections::HashSet, env, path::PathBuf};

use crate::Result;

const FX_HOME_ENV: &str = "FX_HOME";

pub fn paths(custom_path: Option<&str>) -> Result<Vec<PathBuf>> {
    if let Some(custom_path) = custom_path.filter(|path| !path.trim().is_empty()) {
        return Ok(existing_path_list(custom_path));
    }
    if let Ok(env_paths) = env::var(FX_HOME_ENV)
        && !env_paths.trim().is_empty()
    {
        return Ok(existing_path_list(&env_paths));
    }

    let home =
        crate::home::home_dir().ok_or_else(|| crate::cli_error("home directory is not set"))?;
    let path = home.join(".fx");
    Ok(path.is_dir().then_some(path).into_iter().collect())
}

fn existing_path_list(raw: &str) -> Vec<PathBuf> {
    existing_paths(raw, |path| PathBuf::from(path))
}

fn existing_paths(raw: &str, to_path: impl Fn(&str) -> PathBuf) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(to_path)
        .filter(|path| path.is_dir() && seen.insert(path.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ccusage_test_support::{EnvVarGuard, fs_fixture};

    #[test]
    fn paths_defaults_to_dotfx_under_home() {
        let fixture = fs_fixture!({});
        let _fx = fixture.create_dir_all(".fx");
        let _home = EnvVarGuard::set("HOME", fixture.root());

        let paths = paths(None).unwrap();

        assert_eq!(paths.len(), 1);
        assert!(paths[0].ends_with(".fx"));
    }

    #[test]
    fn fx_home_env_overrides_default() {
        let fixture = fs_fixture!({});
        let custom = fixture.create_dir_all("custom-fx");
        let _guard = EnvVarGuard::set(FX_HOME_ENV, custom.to_string_lossy().to_string());

        let paths = paths(None).unwrap();

        assert_eq!(paths, vec![custom]);
    }

    #[test]
    fn custom_path_arg_overrides_env() {
        let fixture = fs_fixture!({});
        let arg_dir = fixture.create_dir_all("arg-fx");
        let _env = fixture.create_dir_all("env-fx");
        let _guard = EnvVarGuard::set(FX_HOME_ENV, fixture.path("env-fx").to_string_lossy().to_string());

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
