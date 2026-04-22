use std::collections::HashMap;

#[cfg(test)]
use parking_lot::Mutex;

pub(crate) trait CredentialStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, value: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;

    fn get_many(&self, accounts: &[String]) -> Result<HashMap<String, String>, String> {
        let mut values = HashMap::with_capacity(accounts.len());
        for account in accounts {
            if let Some(value) = self.get(account)? {
                values.insert(account.clone(), value);
            }
        }
        Ok(values)
    }

    fn set_many(&self, entries: &[(String, String)]) -> Result<(), String> {
        for (account, value) in entries {
            if value.trim().is_empty() {
                self.delete(account)?;
            } else {
                self.set(account, value)?;
            }
        }
        Ok(())
    }
}

pub(crate) struct WindowsCredentialStore {
    service: String,
}

impl WindowsCredentialStore {
    pub(crate) fn new(service: &str) -> Self {
        Self {
            service: service.to_string(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, account).map_err(|e| e.to_string())
    }
}

impl CredentialStore for WindowsCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.entry(account)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct InMemoryCredentialStore {
    values: Mutex<HashMap<String, String>>,
}

#[cfg(test)]
impl CredentialStore for InMemoryCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        Ok(self.values.lock().get(account).cloned())
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.values
            .lock()
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.values.lock().remove(account);
        Ok(())
    }
}
