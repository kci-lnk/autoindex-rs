use std::{
    env,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
};

use chrono_tz::Tz;
use clap::{ArgAction, Parser};
use thiserror::Error;

use crate::path_policy::{
    is_safe_visible_name, validate_configured_path_lexically, validate_configured_root,
};

const DEFAULT_INDEX_FILES: [&str; 2] = ["index.html", "index.htm"];
const MAX_INDEX_FILES: usize = 16;

#[derive(Debug, Parser)]
#[command(name = "autoindex-rs", version, about)]
pub struct Cli {
    /// Directory to serve. Defaults to AUTOINDEX_DIRECTORY or the current directory.
    pub directory: Option<PathBuf>,

    /// IP address to bind. This is not HTTP Host routing.
    #[arg(long)]
    pub bind: Option<IpAddr>,

    /// TCP port to listen on.
    #[arg(short, long, value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,

    /// Render README.md beneath directory listings.
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "no_readme")]
    pub readme: bool,

    /// Disable README.md rendering.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_readme: bool,

    /// Ordered default document name. Repeat to provide more than one.
    #[arg(long = "index-file", action = ArgAction::Append, conflicts_with = "no_index")]
    pub index_files: Vec<String>,

    /// Always show a directory listing, even when index.html exists.
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_index: bool,

    /// Number of visible entries per listing page.
    #[arg(long)]
    pub page_size: Option<usize>,

    /// IANA timezone used for displayed modification times.
    #[arg(long)]
    pub timezone: Option<String>,

    /// Logging verbosity: off, error, warn, info, debug, or trace.
    #[arg(long)]
    pub log_level: Option<String>,

    /// Permit explicitly serving a normally protected system or credential directory.
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_sensitive_paths: bool,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub directory: PathBuf,
    pub bind: IpAddr,
    pub port: u16,
    pub render_readme: bool,
    pub index_files: Vec<String>,
    pub page_size: usize,
    pub timezone: Tz,
    pub timezone_name: String,
    pub log_level: String,
    pub allow_sensitive_paths: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{name} has invalid value {value:?}: {reason}")]
    InvalidEnvironment {
        name: &'static str,
        value: String,
        reason: String,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
    #[error("cannot resolve directory {path}: {source}")]
    ResolveDirectory {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Config {
    pub fn resolve(cli: Cli) -> Result<Self, ConfigError> {
        Self::resolve_with(cli, |name| env::var(name).ok(), PathBuf::from("."))
    }

    pub(crate) fn validate_for_server(&mut self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::Invalid(
                "port must be between 1 and 65535".into(),
            ));
        }
        if !(1..=1000).contains(&self.page_size) {
            return Err(ConfigError::Invalid(
                "page size must be between 1 and 1000".into(),
            ));
        }
        self.index_files = normalize_index_files(std::mem::take(&mut self.index_files))?;
        let parsed_timezone = Tz::from_str(&self.timezone_name).map_err(|_| {
            ConfigError::Invalid(format!("unknown IANA timezone {:?}", self.timezone_name))
        })?;
        if parsed_timezone != self.timezone {
            return Err(ConfigError::Invalid(
                "timezone name does not match the configured timezone".into(),
            ));
        }
        if !matches!(
            self.log_level.as_str(),
            "off" | "error" | "warn" | "info" | "debug" | "trace"
        ) {
            return Err(ConfigError::Invalid(format!(
                "invalid log level {:?}",
                self.log_level
            )));
        }
        Ok(())
    }

    fn resolve_with<F>(
        cli: Cli,
        environment: F,
        default_directory: PathBuf,
    ) -> Result<Self, ConfigError>
    where
        F: Fn(&'static str) -> Option<String>,
    {
        let env_directory = env_value(&environment, "AUTOINDEX_DIRECTORY");
        let directory = cli
            .directory
            .or_else(|| env_directory.map(PathBuf::from))
            .unwrap_or(default_directory);
        validate_configured_path_lexically(&directory)?;
        let directory =
            std::fs::canonicalize(&directory).map_err(|source| ConfigError::ResolveDirectory {
                path: directory.display().to_string(),
                source,
            })?;
        if !directory.is_dir() {
            return Err(ConfigError::Invalid(format!(
                "served path is not a directory: {}",
                directory.display()
            )));
        }

        let bind = cli
            .bind
            .unwrap_or(env_parse(&environment, "AUTOINDEX_BIND", "0.0.0.0")?);
        let port = cli
            .port
            .unwrap_or(env_parse(&environment, "AUTOINDEX_PORT", "6701")?);
        if port == 0 {
            return Err(ConfigError::Invalid(
                "port must be between 1 and 65535".into(),
            ));
        }

        let readme_env = env_bool(&environment, "AUTOINDEX_README")?;
        let render_readme = if cli.readme {
            true
        } else if cli.no_readme {
            false
        } else {
            readme_env.unwrap_or(true)
        };

        let index_files = if cli.no_index {
            Vec::new()
        } else if !cli.index_files.is_empty() {
            cli.index_files
        } else if let Some(value) = env_value_allow_empty(&environment, "AUTOINDEX_INDEX_FILES") {
            if value.is_empty() {
                Vec::new()
            } else {
                value.split(',').map(str::trim).map(str::to_owned).collect()
            }
        } else {
            DEFAULT_INDEX_FILES
                .iter()
                .map(ToString::to_string)
                .collect()
        };
        let index_files = normalize_index_files(index_files)?;

        let page_size =
            cli.page_size
                .unwrap_or(env_parse(&environment, "AUTOINDEX_PAGE_SIZE", "100")?);
        if !(1..=1000).contains(&page_size) {
            return Err(ConfigError::Invalid(
                "page size must be between 1 and 1000".into(),
            ));
        }

        let timezone_name = cli
            .timezone
            .or_else(|| env_value(&environment, "AUTOINDEX_TIMEZONE"))
            .unwrap_or_else(|| "Asia/Shanghai".to_string());
        let timezone = Tz::from_str(&timezone_name).map_err(|_| {
            ConfigError::Invalid(format!("unknown IANA timezone {timezone_name:?}"))
        })?;

        let log_level = cli
            .log_level
            .or_else(|| env_value(&environment, "AUTOINDEX_LOG_LEVEL"))
            .unwrap_or_else(|| "info".to_string())
            .to_ascii_lowercase();
        if !matches!(
            log_level.as_str(),
            "off" | "error" | "warn" | "info" | "debug" | "trace"
        ) {
            return Err(ConfigError::Invalid(format!(
                "invalid log level {log_level:?}"
            )));
        }

        let allow_sensitive_paths = if cli.allow_sensitive_paths {
            true
        } else {
            env_bool(&environment, "AUTOINDEX_ALLOW_SENSITIVE_PATHS")?.unwrap_or(false)
        };
        validate_configured_root(&directory, allow_sensitive_paths)?;

        Ok(Self {
            directory,
            bind,
            port,
            render_readme,
            index_files,
            page_size,
            timezone,
            timezone_name,
            log_level,
            allow_sensitive_paths,
        })
    }
}

fn normalize_index_files(values: Vec<String>) -> Result<Vec<String>, ConfigError> {
    if values.len() > MAX_INDEX_FILES {
        return Err(ConfigError::Invalid(format!(
            "no more than {MAX_INDEX_FILES} index files are allowed"
        )));
    }
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let name = value.trim();
        if name.len() > 255 || name.chars().count() > 255 || !is_safe_visible_name(name) {
            return Err(ConfigError::Invalid(format!(
                "invalid index file name {value:?}"
            )));
        }
        if !result.iter().any(|existing| existing == name) {
            result.push(name.to_string());
        }
    }
    Ok(result)
}

fn env_value<F>(environment: &F, name: &'static str) -> Option<String>
where
    F: Fn(&'static str) -> Option<String>,
{
    environment(name).filter(|value| !value.trim().is_empty())
}

fn env_value_allow_empty<F>(environment: &F, name: &'static str) -> Option<String>
where
    F: Fn(&'static str) -> Option<String>,
{
    environment(name).map(|value| value.trim().to_string())
}

fn env_parse<T, F>(environment: &F, name: &'static str, default: &str) -> Result<T, ConfigError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
    F: Fn(&'static str) -> Option<String>,
{
    let value = env_value(environment, name).unwrap_or_else(|| default.to_string());
    value
        .parse()
        .map_err(|error: T::Err| ConfigError::InvalidEnvironment {
            name,
            value,
            reason: error.to_string(),
        })
}

fn env_bool<F>(environment: &F, name: &'static str) -> Result<Option<bool>, ConfigError>
where
    F: Fn(&'static str) -> Option<String>,
{
    let Some(value) = env_value(environment, name) else {
        return Ok(None);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(ConfigError::InvalidEnvironment {
            name,
            value,
            reason: "expected true/false, yes/no, on/off, or 1/0".to_string(),
        }),
    }
}

pub(crate) fn path_is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::TempDir;

    use super::*;

    fn cli() -> Cli {
        Cli {
            directory: None,
            bind: None,
            port: None,
            readme: false,
            no_readme: false,
            index_files: Vec::new(),
            no_index: false,
            page_size: None,
            timezone: None,
            log_level: None,
            allow_sensitive_paths: false,
        }
    }

    fn visible_directory(root: &TempDir) -> PathBuf {
        let directory = root.path().join("public");
        std::fs::create_dir(&directory).unwrap();
        directory
    }

    #[test]
    fn index_names_are_trimmed_and_deduplicated() {
        let values = normalize_index_files(vec![
            " index.html ".to_string(),
            "index.html".to_string(),
            "home.htm".to_string(),
        ])
        .unwrap();
        assert_eq!(values, ["index.html", "home.htm"]);
    }

    #[test]
    fn invalid_index_names_are_rejected() {
        for value in ["", ".hidden", "../index.html", "dir/index.html"] {
            assert!(normalize_index_files(vec![value.to_string()]).is_err());
        }
    }

    #[test]
    fn documented_defaults_are_applied() {
        let root = TempDir::new().unwrap();
        let directory = visible_directory(&root);
        let config = Config::resolve_with(cli(), |_| None, directory.clone()).unwrap();
        assert_eq!(config.directory, directory.canonicalize().unwrap());
        assert_eq!(config.bind, "0.0.0.0".parse::<IpAddr>().unwrap());
        assert_eq!(config.port, 6701);
        assert!(config.render_readme);
        assert_eq!(config.index_files, ["index.html", "index.htm"]);
        assert_eq!(config.page_size, 100);
        assert_eq!(config.timezone_name, "Asia/Shanghai");
        assert_eq!(config.log_level, "info");
        assert!(!config.allow_sensitive_paths);
    }

    #[test]
    fn command_line_overrides_environment() {
        let cli_root = TempDir::new().unwrap();
        let env_root = TempDir::new().unwrap();
        let cli_directory = visible_directory(&cli_root);
        let env_directory = visible_directory(&env_root);
        let values = HashMap::from([
            ("AUTOINDEX_DIRECTORY", env_directory.display().to_string()),
            ("AUTOINDEX_BIND", "127.0.0.2".to_string()),
            ("AUTOINDEX_PORT", "7100".to_string()),
            ("AUTOINDEX_README", "false".to_string()),
            ("AUTOINDEX_INDEX_FILES", "env.html".to_string()),
            ("AUTOINDEX_PAGE_SIZE", "25".to_string()),
            ("AUTOINDEX_TIMEZONE", "UTC".to_string()),
            ("AUTOINDEX_LOG_LEVEL", "debug".to_string()),
            ("AUTOINDEX_ALLOW_SENSITIVE_PATHS", "false".to_string()),
        ]);
        let config = Config::resolve_with(
            Cli {
                directory: Some(cli_directory.clone()),
                bind: Some("127.0.0.1".parse().unwrap()),
                port: Some(7200),
                readme: true,
                index_files: vec!["cli.html".to_string()],
                page_size: Some(50),
                timezone: Some("Europe/London".to_string()),
                log_level: Some("warn".to_string()),
                allow_sensitive_paths: true,
                ..cli()
            },
            |name| values.get(name).cloned(),
            PathBuf::from("unused"),
        )
        .unwrap();
        assert_eq!(config.directory, cli_directory.canonicalize().unwrap());
        assert_eq!(config.bind, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(config.port, 7200);
        assert!(config.render_readme);
        assert_eq!(config.index_files, ["cli.html"]);
        assert_eq!(config.page_size, 50);
        assert_eq!(config.timezone_name, "Europe/London");
        assert_eq!(config.log_level, "warn");
        assert!(config.allow_sensitive_paths);
    }

    #[test]
    fn empty_environment_index_list_disables_default_documents() {
        let root = TempDir::new().unwrap();
        let directory = visible_directory(&root);
        let config = Config::resolve_with(
            cli(),
            |name| (name == "AUTOINDEX_INDEX_FILES").then(String::new),
            directory,
        )
        .unwrap();
        assert!(config.index_files.is_empty());
    }

    #[test]
    fn invalid_environment_values_fail_resolution() {
        let root = TempDir::new().unwrap();
        let directory = visible_directory(&root);
        for (name, value) in [
            ("AUTOINDEX_PORT", "0"),
            ("AUTOINDEX_README", "sometimes"),
            ("AUTOINDEX_PAGE_SIZE", "1001"),
            ("AUTOINDEX_TIMEZONE", "Mars/Olympus"),
            ("AUTOINDEX_LOG_LEVEL", "verbose"),
        ] {
            let result = Config::resolve_with(
                cli(),
                |candidate| (candidate == name).then(|| value.to_string()),
                directory.clone(),
            );
            assert!(result.is_err(), "{name}={value}");
        }
    }

    #[test]
    fn direct_config_values_are_revalidated_before_server_start() {
        let root = TempDir::new().unwrap();
        let directory = visible_directory(&root);
        let mut config = Config::resolve_with(cli(), |_| None, directory).unwrap();
        config.page_size = 0;
        assert!(config.validate_for_server().is_err());
    }
}
