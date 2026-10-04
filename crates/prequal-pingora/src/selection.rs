use std::{collections::BTreeSet, sync::Arc};

use pingora_load_balancing::{
    Backend,
    selection::{BackendIter, BackendSelection},
};
use prequal_core::Config;

use crate::PrequalSelector;

/// Pingora [`BackendSelection`] routing by Prequal. Build it with
/// `LoadBalancer::<Prequal>::from_backends_with_config(backends, Some(selector))`.
#[derive(Debug)]
pub struct Prequal {
    selector: PrequalSelector,
    backends: Box<[Backend]>,
}

impl BackendSelection for Prequal {
    type Iter = PrequalIter;
    type Config = PrequalSelector;

    fn build_with_config(backends: &BTreeSet<Backend>, selector: &PrequalSelector) -> Self {
        selector.handle().sync(backends.iter());
        Self { selector: selector.clone(), backends: backends.iter().cloned().collect() }
    }

    /// Without a config each rebuild starts from fresh state; prefer `build_with_config`.
    fn build(backends: &BTreeSet<Backend>) -> Self {
        Self::build_with_config(backends, &PrequalSelector::new(Config::default()))
    }

    fn iter(self: &Arc<Self>, _key: &[u8]) -> PrequalIter {
        let start = if self.backends.is_empty() { 0 } else { rand::random_range(0..self.backends.len()) };
        PrequalIter { selection: Arc::clone(self), current: None, first: true, start, fallback: 0 }
    }
}

/// Yields Prequal's pick first, then every backend from a random offset as fallbacks for
/// Pingora's health filtering (a fixed order would herd every proxy onto the same fallback).
#[derive(Debug)]
pub struct PrequalIter {
    selection: Arc<Prequal>,
    current: Option<Backend>,
    first: bool,
    start: usize,
    fallback: usize,
}

impl BackendIter for PrequalIter {
    fn next(&mut self) -> Option<&Backend> {
        if std::mem::take(&mut self.first) {
            let selector = &self.selection.selector;
            let mut targets = Vec::new();
            if let Some(chosen) = selector.handle().choose(&mut targets) {
                targets.into_iter().for_each(|target| selector.driver().request(target));
                self.current = Some(chosen);
                return self.current.as_ref();
            }
        }
        let backends = &self.selection.backends;
        if self.fallback >= backends.len() {
            return None;
        }
        let backend = &backends[(self.start + self.fallback) % backends.len()];
        self.fallback += 1;
        Some(backend)
    }
}
