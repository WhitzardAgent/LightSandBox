//! ls-guest: guest-side helpers.
//!
//! T00 scope: the strict argument contract for the per-sandbox idle init
//! (`--sandbox-id <opaque>`). No resident HTTP service, no scheduler, no DB.
//!
//! T08 scope: `init` — the tiny per-sandbox init binary (`lsguest-init`)
//! that runs as guest PID 1 under launch_mode=image: it spawns the image
//! entrypoint as its child, forwards signals, reaps orphans, and atomically
//! records the entrypoint's real wait status (observed by init itself) before
//! exiting with the same semantics.

pub mod init;

pub mod initargs {
    use std::fmt;

    /// Verified arguments of the guest idle init.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GuestInitArgs {
        pub sandbox_id: String,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum InitArgError {
        MissingFlag,
        MissingValue,
        UnexpectedExtra(String),
        InvalidSandboxId(&'static str),
    }

    impl fmt::Display for InitArgError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                InitArgError::MissingFlag => write!(f, "expected --sandbox-id <opaque>"),
                InitArgError::MissingValue => write!(f, "missing value after --sandbox-id"),
                InitArgError::UnexpectedExtra(arg) => write!(f, "unexpected extra argument: {arg}"),
                InitArgError::InvalidSandboxId(why) => write!(f, "invalid sandbox id: {why}"),
            }
        }
    }

    impl std::error::Error for InitArgError {}

    const MIN_ID_LEN: usize = 8;
    const MAX_ID_LEN: usize = 128;

    /// Validate the opaque sandbox id: server-generated, never caller-interpreted.
    pub fn validate_sandbox_id(id: &str) -> Result<(), InitArgError> {
        if id.len() < MIN_ID_LEN {
            return Err(InitArgError::InvalidSandboxId("shorter than 8 chars"));
        }
        if id.len() > MAX_ID_LEN {
            return Err(InitArgError::InvalidSandboxId("longer than 128 chars"));
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(InitArgError::InvalidSandboxId(
                "only ASCII alphanumerics, '-' and '_' are allowed",
            ));
        }
        Ok(())
    }

    /// Parse exactly `--sandbox-id <opaque>`; argv form only, never a shell line.
    pub fn parse(argv: &[String]) -> Result<GuestInitArgs, InitArgError> {
        let mut iter = argv.iter();
        match iter.next().map(String::as_str) {
            Some("--sandbox-id") => {}
            _ => return Err(InitArgError::MissingFlag),
        }
        let id = match iter.next() {
            Some(v) => v,
            None => return Err(InitArgError::MissingValue),
        };
        if let Some(extra) = iter.next() {
            return Err(InitArgError::UnexpectedExtra(extra.clone()));
        }
        validate_sandbox_id(id)?;
        Ok(GuestInitArgs {
            sandbox_id: id.clone(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn argv(items: &[&str]) -> Vec<String> {
            items.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn parses_exact_flag_and_value() {
            let args = parse(&argv(&["--sandbox-id", "sb_01j8k3zq-fixture"])).unwrap();
            assert_eq!(args.sandbox_id, "sb_01j8k3zq-fixture");
        }

        #[test]
        fn missing_flag_is_distinct_from_missing_value() {
            assert_eq!(parse(&argv(&[])).unwrap_err(), InitArgError::MissingFlag);
            assert_eq!(parse(&argv(&["--sandbox-id"])).unwrap_err(), InitArgError::MissingValue);
        }

        #[test]
        fn extra_arguments_are_rejected() {
            let err = parse(&argv(&["--sandbox-id", "sb_12345678", "--verbose"])).unwrap_err();
            assert_eq!(err, InitArgError::UnexpectedExtra("--verbose".into()));
        }

        #[test]
        fn id_charset_and_length_rules_hold() {
            assert!(validate_sandbox_id("sb_01j8k3zq").is_ok());
            assert!(validate_sandbox_id("short").is_err());
            assert!(validate_sandbox_id(&"a".repeat(129)).is_err());
            assert!(validate_sandbox_id("sb has space").is_err());
            assert!(validate_sandbox_id("sb/../escape").is_err());
            assert!(validate_sandbox_id("sb$EXPANSION").is_err());
        }
    }
}
