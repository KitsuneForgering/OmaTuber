//! "Lembrar neste computador": the OBS WebSocket password in the desktop's
//! Secret Service (gnome-keyring, KWallet), never in preferences or a URL.
//!
//! Calls may block on D-Bus or on an unlock prompt from the keyring; run them
//! off the UI thread.

use std::sync::Arc;

use keyring_core::{CredentialStore, Error};

const SERVICE: &str = "omatuber";
const USER: &str = "obs-websocket";

/// The Secret Service of this session, or why it is unusable.
pub fn system_store() -> Result<Arc<CredentialStore>, String> {
    let store = zbus_secret_service_keyring_store::Store::new().map_err(describe)?;
    Ok(store)
}

/// The remembered password, if any.
pub fn load(store: &CredentialStore) -> Result<Option<String>, String> {
    let entry = store.build(SERVICE, USER, None).map_err(describe)?;
    match entry.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(describe(e)),
    }
}

pub fn save(store: &CredentialStore, password: &str) -> Result<(), String> {
    let entry = store.build(SERVICE, USER, None).map_err(describe)?;
    entry.set_password(password).map_err(describe)
}

/// Forgets the password; not having one is not an error.
pub fn forget(store: &CredentialStore) -> Result<(), String> {
    let entry = store.build(SERVICE, USER, None).map_err(describe)?;
    match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(describe(e)),
    }
}

fn describe(e: Error) -> String {
    match e {
        Error::NoStorageAccess(_) => {
            "o chaveiro do sistema está bloqueado ou recusou o acesso".to_owned()
        }
        Error::PlatformFailure(e) => format!("serviço de senhas indisponível: {e}"),
        other => format!("serviço de senhas: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_forgets_with_a_mock_store() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        assert_eq!(load(&*store).unwrap(), None);
        save(&*store, "segredo").unwrap();
        assert_eq!(load(&*store).unwrap().as_deref(), Some("segredo"));
        save(&*store, "outro").unwrap();
        assert_eq!(
            load(&*store).unwrap().as_deref(),
            Some("outro"),
            "overwritten"
        );
        forget(&*store).unwrap();
        assert_eq!(load(&*store).unwrap(), None);
        forget(&*store).unwrap();
    }

    /// Touches the real keyring with a throwaway entry, then deletes it.
    /// Run explicitly: `cargo test secret -- --ignored`.
    #[test]
    #[ignore]
    fn real_secret_service_round_trip() {
        let store = system_store().expect("Secret Service");
        let entry = store.build("omatuber-selftest", "check", None).unwrap();
        entry.set_password("x").unwrap();
        assert_eq!(entry.get_password().unwrap(), "x");
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(Error::NoEntry)));
    }
}
