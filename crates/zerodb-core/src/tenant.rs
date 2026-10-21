use crate::error::Result;
use crate::{EngineError, ErrorKind};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct Registry {
    names: HashMap<String, u32>,
    pub(crate) entries: Vec<(String, u64)>,
}

impl Registry {
    pub(crate) fn get(&self, name: &str) -> Option<u32> {
        self.names.get(name).copied()
    }
    pub(crate) fn insert(&mut self, name: &str, id: u32, lsn: u64, limit: usize) -> Result<()> {
        if name.len() > 255
            || id as usize != self.entries.len() + 1
            || self.names.contains_key(name)
        {
            return Err(EngineError::rejected(ErrorKind::CorruptIndex));
        }
        if self.entries.len() >= limit.min(10_000) {
            return Err(EngineError::rejected(ErrorKind::ResourceExhausted("tenants")));
        }
        self.names.insert(name.to_owned(), id);
        self.entries.push((name.to_owned(), lsn));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn registry_ids_and_tenant_limit() {
        let mut registry = Registry::default();
        for id in 1..=10_000 {
            registry.insert(&id.to_string(), id, u64::from(id), 10_000).unwrap();
            assert_eq!(registry.get(&id.to_string()), Some(id), "id={id}");
        }
        let error = registry.insert("full", 10_001, 10_001, 10_000).unwrap_err();
        assert!(matches!(error.kind, ErrorKind::ResourceExhausted("tenants")));
    }
}
