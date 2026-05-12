use std::env;

use dialoguer::Password;
use error_stack::{Report, ResultExt};
use serde::Serialize;

use crate::error::CliError;

const KEYRING_SERVICE: &str = "trusted-server-cli.fastly";
const KEYRING_USERNAME: &str = "api-key";

pub trait CredentialStore {
    fn read(&self) -> Result<Option<String>, Report<CliError>>;
    fn write(&self, value: &str) -> Result<(), Report<CliError>>;
    fn delete(&self) -> Result<(), Report<CliError>>;
}

pub struct SystemCredentialStore;

impl CredentialStore for SystemCredentialStore {
    fn read(&self) -> Result<Option<String>, Report<CliError>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME)
            .change_context(CliError::Authentication)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(Report::new(CliError::Authentication).attach(format!(
                "failed to read Fastly credential from secure storage: {error}"
            ))),
        }
    }

    fn write(&self, value: &str) -> Result<(), Report<CliError>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME)
            .change_context(CliError::Authentication)?;
        entry
            .set_password(value)
            .map_err(|error| {
                Report::new(CliError::Authentication).attach(format!(
                    "failed to store Fastly credential in secure storage: {error}. Hint: use FASTLY_API_KEY if secure storage is unavailable."
                ))
            })
    }

    fn delete(&self) -> Result<(), Report<CliError>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USERNAME)
            .change_context(CliError::Authentication)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(Report::new(CliError::Authentication).attach(format!(
                "failed to delete Fastly credential from secure storage: {error}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialSource {
    Environment,
    SecureStorage,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuthStatusJson {
    pub has_env_credential: bool,
    pub has_stored_credential: bool,
    pub effective_source: Option<CredentialSource>,
}

#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    pub value: String,
    pub source: CredentialSource,
}

pub fn resolve_fastly_api_key(
    store: &dyn CredentialStore,
) -> Result<ResolvedCredential, Report<CliError>> {
    if let Ok(value) = env::var("FASTLY_API_KEY")
        && !value.trim().is_empty()
    {
        return Ok(ResolvedCredential {
            value,
            source: CredentialSource::Environment,
        });
    }

    if let Some(value) = store.read()?
        && !value.trim().is_empty()
    {
        return Ok(ResolvedCredential {
            value,
            source: CredentialSource::SecureStorage,
        });
    }

    Err(Report::new(CliError::Authentication)
        .attach("missing Fastly credential. Run `ts auth fastly login` or set FASTLY_API_KEY."))
}

pub fn fastly_auth_status(store: &dyn CredentialStore) -> Result<AuthStatusJson, Report<CliError>> {
    let has_env_credential = env::var("FASTLY_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_stored_credential = store.read()?.is_some_and(|value| !value.trim().is_empty());
    let effective_source = if has_env_credential {
        Some(CredentialSource::Environment)
    } else if has_stored_credential {
        Some(CredentialSource::SecureStorage)
    } else {
        None
    };

    Ok(AuthStatusJson {
        has_env_credential,
        has_stored_credential,
        effective_source,
    })
}

pub fn login_fastly(store: &dyn CredentialStore) -> Result<(), Report<CliError>> {
    let token = Password::new()
        .with_prompt("Fastly API key")
        .interact()
        .change_context(CliError::Authentication)?;

    let token = token.trim();
    if token.is_empty() {
        return Err(Report::new(CliError::Authentication).attach("Fastly API key cannot be empty"));
    }

    store.write(token)
}

pub fn logout_fastly(store: &dyn CredentialStore) -> Result<(), Report<CliError>> {
    store.delete()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone, Default)]
    struct MemoryCredentialStore {
        value: Arc<Mutex<Option<String>>>,
    }

    impl CredentialStore for MemoryCredentialStore {
        fn read(&self) -> Result<Option<String>, Report<CliError>> {
            Ok(self.value.lock().expect("should lock store").clone())
        }

        fn write(&self, value: &str) -> Result<(), Report<CliError>> {
            *self.value.lock().expect("should lock store") = Some(value.to_string());
            Ok(())
        }

        fn delete(&self) -> Result<(), Report<CliError>> {
            *self.value.lock().expect("should lock store") = None;
            Ok(())
        }
    }

    #[test]
    fn env_credential_wins_over_stored_credential() {
        let _guard = ENV_LOCK.lock().expect("should lock environment");
        let store = MemoryCredentialStore::default();
        store.write("stored-token").expect("should store token");
        unsafe {
            env::set_var("FASTLY_API_KEY", "env-token");
        }

        let resolved = resolve_fastly_api_key(&store).expect("should resolve token");

        assert_eq!(resolved.value, "env-token");
        assert_eq!(resolved.source, CredentialSource::Environment);

        unsafe {
            env::remove_var("FASTLY_API_KEY");
        }
    }

    #[test]
    fn stored_credential_is_used_when_env_is_missing() {
        let _guard = ENV_LOCK.lock().expect("should lock environment");
        let store = MemoryCredentialStore::default();
        store.write("stored-token").expect("should store token");
        unsafe {
            env::remove_var("FASTLY_API_KEY");
        }

        let resolved = resolve_fastly_api_key(&store).expect("should resolve stored token");

        assert_eq!(resolved.value, "stored-token");
        assert_eq!(resolved.source, CredentialSource::SecureStorage);
    }
}
