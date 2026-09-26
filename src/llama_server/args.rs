// ─── llama-server argument handling ──────────────────────────────────────────
//
// Users type arguments as one string (`-ngl all -fa on -ctk iq4_nl ...`); this
// turns it into an argv vector. No shell is ever involved, so there is nothing
// to escape — but the tokenizer must still behave sensibly on Windows, where
// paths contain backslashes. That rules out POSIX shell rules (`shlex`), which
// treat a backslash as an escape and would mangle `C:\models\x.gguf`.
//
// Rules: whitespace separates tokens; `"..."` and `'...'` group (the other
// quote character is literal inside); backslashes are always literal; there
// are no escape sequences.

use serde::Serialize;

/// Flags the module owns. Letting a user override them would let the saved
/// model, the reported "loaded model", and the actual process disagree, or
/// would turn the start endpoint into "run llama-server with any model source"
/// (URL/HF downloads) — or make the process exit immediately (`--help`).
const RESERVED_FLAGS: &[&str] = &[
    "-m",
    "--model",
    "-mu",
    "--model-url",
    "-dr",
    "--docker-repo",
    "-hf",
    "-hfr",
    "--hf-repo",
    "-hff",
    "--hf-file",
    "-mm",
    "--mmproj",
    "-mmu",
    "--mmproj-url",
    "--host",
    "--port",
    "--models-dir",
    "--models-preset",
    "-h",
    "--help",
    "--version",
    "--license",
    "--completion-bash",
    "--list-devices",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArgIssue {
    /// Stable machine-readable code: `unbalanced_quote` | `reserved_flag`.
    pub code: &'static str,
    pub message: String,
    /// Offending token, when there is one.
    pub token: Option<String>,
}

/// Result of parsing a raw argument string.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedArgs {
    pub argv: Vec<String>,
    pub issues: Vec<ArgIssue>,
}

impl ParsedArgs {
    pub fn is_ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Split `input` into tokens. Fails on an unterminated quote.
pub fn tokenize(input: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    // A quoted empty string (`""`) is a real, empty token.
    let mut in_token = false;
    let mut quote: Option<char> = None;

    for ch in input.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_token = true;
            }
            None if ch.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            None => {
                current.push(ch);
                in_token = true;
            }
        }
    }
    if let Some(q) = quote {
        return Err(format!("unterminated {q} quote"));
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

/// If `token` is a flag the module owns (bare, or `--flag=value`), return it.
pub fn reserved_flag(token: &str) -> Option<&'static str> {
    if !token.starts_with('-') {
        return None;
    }
    let name = token.split('=').next().unwrap_or(token);
    RESERVED_FLAGS.iter().copied().find(|f| *f == name)
}

/// Whether any of `flags` appears as a flag in `argv` (bare or `--flag=value`).
pub fn has_flag(argv: &[String], flags: &[&str]) -> bool {
    argv.iter().any(|t| {
        let name = t.split('=').next().unwrap_or(t);
        t.starts_with('-') && flags.contains(&name)
    })
}

/// Tokenize and validate. Never fails outright: problems come back as
/// `issues` so a UI can show them live while the user types.
pub fn parse_and_validate(input: &str) -> ParsedArgs {
    let mut issues = Vec::new();
    let argv = match tokenize(input) {
        Ok(v) => v,
        Err(message) => {
            issues.push(ArgIssue { code: "unbalanced_quote", message, token: None });
            return ParsedArgs { argv: vec![], issues };
        }
    };
    for token in &argv {
        if let Some(flag) = reserved_flag(token) {
            issues.push(ArgIssue {
                code: "reserved_flag",
                message: format!(
                    "'{flag}' is managed by the gateway (model, host, and port come from the saved model and config) and can't be set here"
                ),
                token: Some(token.clone()),
            });
        }
    }
    ParsedArgs { argv, issues }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        tokenize(s).unwrap()
    }

    #[test]
    fn splits_the_users_real_argument_string() {
        assert_eq!(
            toks("-ngl all -fa on -ctk iq4_nl -ctv iq4_nl --tools all -np 1 --ui-mcp-proxy"),
            [
                "-ngl", "all", "-fa", "on", "-ctk", "iq4_nl", "-ctv", "iq4_nl", "--tools", "all", "-np", "1",
                "--ui-mcp-proxy"
            ]
        );
    }

    #[test]
    fn collapses_repeated_and_mixed_whitespace_and_handles_empty_input() {
        assert_eq!(toks("  a \t b\n  c  "), ["a", "b", "c"]);
        assert!(toks("").is_empty());
        assert!(toks("   \n ").is_empty());
    }

    #[test]
    fn backslashes_are_literal_so_windows_paths_survive() {
        assert_eq!(toks(r"--slot-save-path C:\models\slots"), ["--slot-save-path", r"C:\models\slots"]);
        assert_eq!(
            toks(r#"--log-file "C:\Program Files\x\log.txt""#),
            ["--log-file", r"C:\Program Files\x\log.txt"]
        );
        // A trailing backslash inside quotes must not swallow the closing quote.
        assert_eq!(toks(r#""C:\dir\" next"#), [r"C:\dir\", "next"]);
    }

    #[test]
    fn quotes_group_and_the_other_quote_is_literal() {
        assert_eq!(
            toks(r#"--chat-template-kwargs '{"enable_thinking":false}'"#),
            ["--chat-template-kwargs", r#"{"enable_thinking":false}"#]
        );
        assert_eq!(toks(r#"--x "it's fine""#), ["--x", "it's fine"]);
        assert_eq!(toks(r#"--x "" --y"#), ["--x", "", "--y"], "an empty quoted string is a real empty token");
        assert_eq!(toks(r#"a"b c"d"#), ["ab cd"], "adjacent quoted and bare text join into one token");
    }

    #[test]
    fn unterminated_quote_is_an_error() {
        assert!(tokenize(r#"--x "oops"#).unwrap_err().contains("unterminated"));
        assert_eq!(parse_and_validate("--x 'oops").issues[0].code, "unbalanced_quote");
    }

    #[test]
    fn reserved_flags_are_rejected_bare_and_with_equals() {
        for bad in [
            "-m x.gguf",
            "--model=x.gguf",
            "--mmproj p.gguf",
            "-mm p",
            "--host 0.0.0.0",
            "--port=9",
            "-hf user/m",
            "--model-url http://x",
            "--help",
            "-h",
            "--models-dir d",
        ] {
            let parsed = parse_and_validate(bad);
            assert!(!parsed.is_ok(), "{bad} must be rejected");
            assert_eq!(parsed.issues[0].code, "reserved_flag", "{bad}");
        }
    }

    #[test]
    fn similar_looking_allowed_flags_are_not_reserved() {
        let parsed = parse_and_validate(
            "-ngl 99 --mmproj-offload --no-mmproj -np 1 --ui-mcp-proxy --hf-token abc --alias mine",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.issues);
    }

    #[test]
    fn a_value_is_not_mistaken_for_a_flag() {
        // Only tokens starting with '-' can be flags.
        assert!(parse_and_validate("--alias model").is_ok());
    }

    #[test]
    fn has_flag_matches_bare_and_equals_forms() {
        let argv = toks("--alias=foo -ngl 1");
        assert!(has_flag(&argv, &["-a", "--alias"]));
        assert!(!has_flag(&argv, &["--port"]));
    }
}
