use std::collections::HashMap;
use std::sync::Arc;

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

pub(crate) struct CachedCredentialStore {
    inner: Arc<dyn CredentialStore>,
    values: Mutex<HashMap<String, Option<String>>>,
}

impl CachedCredentialStore {
    pub(crate) fn new(inner: Arc<dyn CredentialStore>) -> Self {
        Self {
            inner,
            values: Mutex::new(HashMap::new()),
        }
    }
}

impl CredentialStore for CachedCredentialStore {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        if let Some(value) = self.values.lock().get(account).cloned() {
            return Ok(value);
        }
        let value = self.inner.get(account)?;
        self.values
            .lock()
            .insert(account.to_string(), value.clone());
        Ok(value)
    }

    fn set(&self, account: &str, value: &str) -> Result<(), String> {
        self.inner.set(account, value)?;
        self.values
            .lock()
            .insert(account.to_string(), Some(value.to_string()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.inner.delete(account)?;
        self.values.lock().insert(account.to_string(), None);
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CountingCredentialStore {
        values: Mutex<HashMap<String, String>>,
        get_count: Mutex<usize>,
    }

    impl CredentialStore for CountingCredentialStore {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            *self.get_count.lock() += 1;
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

    #[test]
    fn cached_store_reuses_read_values_until_write_or_delete() {
        let inner = Arc::new(CountingCredentialStore::default());
        inner.set("provider:test:apiKey", "sk-test").unwrap();
        let store = CachedCredentialStore::new(inner.clone());

        assert_eq!(
            store.get("provider:test:apiKey").unwrap(),
            Some("sk-test".to_string())
        );
        assert_eq!(
            store.get("provider:test:apiKey").unwrap(),
            Some("sk-test".to_string())
        );
        assert_eq!(*inner.get_count.lock(), 1);

        store.set("provider:test:apiKey", "sk-next").unwrap();
        assert_eq!(
            store.get("provider:test:apiKey").unwrap(),
            Some("sk-next".to_string())
        );
        assert_eq!(*inner.get_count.lock(), 1);

        store.delete("provider:test:apiKey").unwrap();
        assert_eq!(store.get("provider:test:apiKey").unwrap(), None);
        assert_eq!(*inner.get_count.lock(), 1);
    }

    #[test]
    fn cached_store_reuses_missing_values() {
        let inner = Arc::new(CountingCredentialStore::default());
        let store = CachedCredentialStore::new(inner.clone());

        assert_eq!(store.get("missing").unwrap(), None);
        assert_eq!(store.get("missing").unwrap(), None);
        assert_eq!(*inner.get_count.lock(), 1);
    }
}
