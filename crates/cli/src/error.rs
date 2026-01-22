//! CLI error types.

use std::fmt;

#[derive(Debug)]
pub enum CliError {
    /// Configuration file error
    Config(String),
    /// Platform API error
    Platform(String),
    /// IO error
    Io(std::io::Error),
    /// TOML parsing error
    Toml(String),
    /// HTTP request error
    Http(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Config(msg) => write!(f, "Configuration error: {}", msg),
            CliError::Platform(msg) => write!(f, "Platform error: {}", msg),
            CliError::Io(err) => write!(f, "IO error: {}", err),
            CliError::Toml(msg) => write!(f, "TOML error: {}", msg),
            CliError::Http(msg) => write!(f, "HTTP error: {}", msg),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        CliError::Io(err)
    }
}

impl From<toml::de::Error> for CliError {
    fn from(err: toml::de::Error) -> Self {
        CliError::Toml(err.to_string())
    }
}

impl From<ureq::Error> for CliError {
    fn from(err: ureq::Error) -> Self {
        CliError::Http(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn test_cli_error_display() {
        assert_eq!(
            format!("{}", CliError::Config("test".into())),
            "Configuration error: test"
        );
        assert_eq!(
            format!("{}", CliError::Platform("test".into())),
            "Platform error: test"
        );
        assert_eq!(
            format!("{}", CliError::Toml("test".into())),
            "TOML error: test"
        );
        assert_eq!(
            format!("{}", CliError::Http("test".into())),
            "HTTP error: test"
        );
    }

    #[test]
    fn test_cli_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let cli_err: CliError = io_err.into();
        match cli_err {
            CliError::Io(_) => {}
            _ => panic!("Expected Io variant"),
        }
    }

    #[test]
    fn test_cli_error_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let cli_err: CliError = io_err.into();
        assert!(cli_err.source().is_some());

        let config_err = CliError::Config("test".into());
        assert!(config_err.source().is_none());
    }
}
