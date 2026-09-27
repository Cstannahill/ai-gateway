//! Command-line interface: argument parsing and the `--help` text.
//!
//! Hand-rolled on purpose. The binary has one positional argument and two
//! flags, which does not justify a `clap` dependency. [`HELP`] is the single
//! source of truth: `--help` prints it and `COMMANDS.md` embeds it verbatim
//! (a test fails if the two drift apart).

/// Text printed by `--help`. Keep in sync with `COMMANDS.md` (enforced by test).
pub const HELP: &str = "\
ai-gateway - personal LLM gateway (OpenAI-compatible) with PostgreSQL telemetry

USAGE:
    ai-gateway [OPTIONS] [CONFIG]

ARGS:
    [CONFIG]    Path to a YAML config file. Same as --config.

OPTIONS:
    -c, --config <PATH>    Path to a YAML config file
        --env-file <PATH>  Load environment variables from this file (must exist)
    -h, --help             Print this help and exit
    -V, --version          Print the version and exit

CONFIG RESOLUTION (first match wins):
    1. --config <PATH> / [CONFIG]   (an explicit path that fails to load is an error)
    2. config/gateway.yaml
    3. gateway.yaml
    4. built-in defaults (logs a warning)

ENVIRONMENT:
    RUST_LOG              Log filter, e.g. ai_gateway=debug (default: ai_gateway=info,tower_http=info)
    DATABASE_URL          PostgreSQL URL for telemetry (name set by telemetry.database_url_env)
    <api_key_env>         One variable per provider, named in its api_key_env config field,
                          e.g. OPENROUTER_API_KEY, GEMINI_API_KEY, HF_API_KEY, OLLAMA_API_KEY
    Variables are also read from ./.env if it exists, or from --env-file. A variable
    already set in the real environment always wins over the file.

The gateway listens on server.host:server.port (default 0.0.0.0:8999). Stop it
with Ctrl+C for a graceful shutdown. HTTP endpoints, config fields and the
llama-server module are documented in COMMANDS.md and docs/.
";

/// What the process should do, as decided by [`parse`].
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Start the server, optionally with an explicit config path and/or an
    /// explicit environment file.
    Run {
        config_path: Option<String>,
        env_file: Option<String>,
    },
    /// Print [`HELP`] and exit 0.
    Help,
    /// Print the version and exit 0.
    Version,
}

/// Parse arguments (excluding `argv[0]`). `Err` carries a message for stderr;
/// the caller should exit with status 2.
pub fn parse<I, S>(args: I) -> Result<Action, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut config_path: Option<String> = None;
    let mut env_file: Option<String> = None;
    let mut iter = args.into_iter().map(Into::into);

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            // Help/version win over everything else, even alongside errors
            // that would come later on the line.
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            "-c" | "--config" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path argument"))?;
                set_path(&mut config_path, value, "config")?;
            }
            "--env-file" => {
                let value = iter
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path argument"))?;
                set_path(&mut env_file, value, "env file")?;
            }
            other if other.starts_with("--config=") => {
                set_path(&mut config_path, other["--config=".len()..].to_string(), "config")?;
            }
            other if other.starts_with("--env-file=") => {
                set_path(&mut env_file, other["--env-file=".len()..].to_string(), "env file")?;
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option '{other}'"));
            }
            _ => set_path(&mut config_path, arg, "config")?,
        }
    }

    Ok(Action::Run { config_path, env_file })
}

fn set_path(slot: &mut Option<String>, value: String, what: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} path must not be empty"));
    }
    if slot.is_some() {
        return Err(format!("only one {what} path may be given"));
    }
    *slot = Some(value);
    Ok(())
}

/// Load environment variables from a dotenv file so provider API keys,
/// `DATABASE_URL` and `RUST_LOG` can live in `.env`.
///
/// * `explicit = Some(path)`: the file must exist and parse, else `Err`.
/// * `explicit = None`: loads `./.env` if present (and only from the current
///   directory - the parent directories are never searched); absent is fine.
///
/// A variable that is already set in the real environment is never
/// overwritten, so `export FOO=...` on the command line always wins.
/// Returns what was loaded, if anything, including the names of variables whose
/// real-environment value *differs* from the file's (see
/// `EnvFileLoad::shadowed`). Never returns or logs values.
///
/// Call this before the async runtime starts: changing the environment is
/// only sound while no other thread is running.
pub fn load_env_file(explicit: Option<&str>) -> Result<Option<EnvFileLoad>, String> {
    let (path, required) = match explicit {
        Some(p) => (std::path::PathBuf::from(p), true),
        None => (std::path::PathBuf::from(".env"), false),
    };
    if !path.is_file() {
        return if required {
            Err(format!("env file '{}' does not exist or is not a file", path.display()))
        } else {
            Ok(None)
        };
    }
    // Found before loading: `from_path` leaves already-set variables alone, so
    // afterwards there is no way to tell that a value was overridden.
    let shadowed = dotenvy::from_path_iter(&path)
        .map_err(|e| format!("cannot load env file '{}': {e}", path.display()))?
        .filter_map(Result::ok)
        .filter(|(name, value)| std::env::var(name).is_ok_and(|current| current != *value))
        .map(|(name, _)| name)
        .collect();
    dotenvy::from_path(&path)
        .map_err(|e| format!("cannot load env file '{}': {e}", path.display()))?;
    Ok(Some(EnvFileLoad { path, shadowed }))
}

/// The result of loading an env file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFileLoad {
    pub path: std::path::PathBuf,
    /// Variables set in the file AND already set to a *different* value in the
    /// real environment (a shell profile, the system settings, an `export`).
    /// The real environment wins, so the file's value is ignored; callers
    /// should warn, because a stale key hiding behind a correct `.env` is
    /// otherwise invisible. Names only.
    pub shadowed: Vec<String>,
}

/// `ai-gateway <version>` line printed by `--version`.
pub fn version_line() -> String {
    format!("ai-gateway {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Action, String> {
        parse(args.iter().copied())
    }

    #[test]
    fn no_args_runs_with_default_resolution() {
        assert_eq!(run(&[]).unwrap(), Action::Run { config_path: None, env_file: None });
    }

    #[test]
    fn positional_and_flag_forms_are_equivalent() {
        let expected = Action::Run { config_path: Some("my.yaml".into()), env_file: None };
        assert_eq!(run(&["my.yaml"]).unwrap(), expected);
        assert_eq!(run(&["-c", "my.yaml"]).unwrap(), expected);
        assert_eq!(run(&["--config", "my.yaml"]).unwrap(), expected);
        assert_eq!(run(&["--config=my.yaml"]).unwrap(), expected);
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(run(&["--help"]).unwrap(), Action::Help);
        assert_eq!(run(&["-h"]).unwrap(), Action::Help);
        assert_eq!(run(&["-V"]).unwrap(), Action::Version);
        assert_eq!(run(&["--version"]).unwrap(), Action::Version);
        assert_eq!(run(&["my.yaml", "--help"]).unwrap(), Action::Help);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(run(&["--bogus"]).unwrap_err().contains("unknown option"));
        assert!(run(&["-c"]).unwrap_err().contains("requires a path"));
        assert!(run(&["a.yaml", "b.yaml"]).unwrap_err().contains("only one"));
        assert!(run(&["-c", "a.yaml", "b.yaml"]).unwrap_err().contains("only one"));
        assert!(run(&["--config="]).unwrap_err().contains("must not be empty"));
    }

    #[test]
    fn windows_style_paths_are_positional() {
        let a = run(&[r"C:\cfg\gateway.yaml"]).unwrap();
        assert_eq!(
            a,
            Action::Run { config_path: Some(r"C:\cfg\gateway.yaml".into()), env_file: None }
        );
    }

    #[test]
    fn env_file_flag_forms_and_errors() {
        let expected = Action::Run { config_path: None, env_file: Some("prod.env".into()) };
        assert_eq!(run(&["--env-file", "prod.env"]).unwrap(), expected);
        assert_eq!(run(&["--env-file=prod.env"]).unwrap(), expected);
        let both = run(&["a.yaml", "--env-file", "prod.env"]).unwrap();
        assert_eq!(
            both,
            Action::Run { config_path: Some("a.yaml".into()), env_file: Some("prod.env".into()) }
        );
        assert!(run(&["--env-file"]).unwrap_err().contains("requires a path"));
        assert!(run(&["--env-file="]).unwrap_err().contains("must not be empty"));
        assert!(run(&["--env-file", "a", "--env-file", "b"]).unwrap_err().contains("only one env file"));
    }

    fn temp_env_file(contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ai-gateway-test-{}.env", uuid::Uuid::now_v7()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn env_file_sets_variables_but_never_overrides_the_real_environment() {
        let id = uuid::Uuid::now_v7().simple().to_string();
        let fresh = format!("AIGW_TEST_FRESH_{id}");
        let preset = format!("AIGW_TEST_PRESET_{id}");
        let same = format!("AIGW_TEST_SAME_{id}");
        // SAFETY: the names are unique to this test, so no other thread reads them.
        unsafe {
            std::env::set_var(&preset, "from-shell");
            std::env::set_var(&same, "identical");
        }

        let file = temp_env_file(&format!(
            "# comment\n{fresh}=from-file\nexport {fresh}_EXPORTED=exported\n{preset}=from-file\n{same}=identical\n{fresh}_QUOTED=\"has spaces # not a comment\"\n"
        ));
        let loaded = load_env_file(Some(file.to_str().unwrap())).unwrap();
        std::fs::remove_file(&file).ok();

        let loaded = loaded.expect("file was loaded");
        assert_eq!(loaded.path, file);
        assert_eq!(
            loaded.shadowed,
            std::slice::from_ref(&preset),
            "only a variable whose real value differs is reported: not values, not identical ones, not unset ones"
        );
        assert_eq!(std::env::var(&fresh).unwrap(), "from-file");
        assert_eq!(std::env::var(format!("{fresh}_EXPORTED")).unwrap(), "exported");
        assert_eq!(std::env::var(format!("{fresh}_QUOTED")).unwrap(), "has spaces # not a comment");
        assert_eq!(std::env::var(&preset).unwrap(), "from-shell", "real env must win over the file");
    }

    #[test]
    fn explicit_env_file_must_exist_and_parse() {
        let missing = load_env_file(Some("definitely/not/here.env")).unwrap_err();
        assert!(missing.contains("does not exist"), "{missing}");

        let bad = temp_env_file("THIS LINE IS NOT VALID\n");
        let err = load_env_file(Some(bad.to_str().unwrap())).unwrap_err();
        std::fs::remove_file(&bad).ok();
        assert!(err.contains("cannot load env file"), "{err}");
    }

    #[test]
    fn commands_md_embeds_the_help_text_verbatim() {
        let doc = include_str!("../COMMANDS.md");
        assert!(
            doc.contains(HELP),
            "COMMANDS.md must contain the exact `--help` output from src/cli.rs::HELP; \
             update the fenced block under '## `--help` output'"
        );
    }
}
