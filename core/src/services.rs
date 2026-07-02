//! Cross-plugin service registry.
//!
//! When plugin A wants to expose a capability that plugin B can consume,
//! it sets it here. Plugin B reads it. Both plugins depend on `ccc-core`
//! for the trait that bridges them — they never need to import each other.
//!
//! The current API is specialized rather than generic over `Arc<dyn T>`
//! because trait-object bounds make a fully generic TypeMap awkward.
//! As more services land, add more fields here. Adding a service is a
//! one-time additive change everyone can ignore until they need it.

use std::sync::{Arc, PoisonError, RwLock};

/// Membership-check service plugins can publish into the registry.
///
/// `is_member` returns true iff the request carries a valid, active
/// membership token recognized by the implementing plugin. Implementations
/// should be cheap and side-effect-free — they're called per request.
pub trait MembershipService: Send + Sync {
    fn is_member(&self, headers: &axum::http::HeaderMap) -> bool;
}

#[derive(Default)]
pub struct ServiceRegistry {
    membership: RwLock<Option<Arc<dyn MembershipService>>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the membership service. Idempotent — last writer wins.
    /// If a membership service is already registered, the new one
    /// replaces it. A `tracing::warn!` is emitted so concurrent
    /// registrations are visible in operator logs.
    pub fn set_membership(&self, s: Arc<dyn MembershipService>) {
        {
            // A poisoned lock means some earlier holder panicked mid-access.
            // The registry only stores a plain `Arc` — there's no multi-step
            // invariant a panic could leave half-applied — so recover the
            // guard and keep serving instead of crashing the whole server.
            let guard = self
                .membership
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.is_some() {
                tracing::warn!(
                    "membership service being replaced; the new provider will be active \
                     immediately and the old one dropped. This is expected only if you run \
                     more than one membership-aware plugin in the same process — please \
                     check the plugin list in ccc.toml if this is unexpected."
                );
            }
        }
        *self
            .membership
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(s);
    }

    /// Look up the membership service. Returns `None` when no membership
    /// provider is active — consumers should degrade gracefully (e.g. fall
    /// back to a static token).
    pub fn membership(&self) -> Option<Arc<dyn MembershipService>> {
        // See `set_membership` — a poisoned lock degrades to the last
        // registered value rather than taking the server down.
        self.membership
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysYes;
    impl MembershipService for AlwaysYes {
        fn is_member(&self, _headers: &axum::http::HeaderMap) -> bool {
            true
        }
    }

    #[test]
    fn set_and_get_roundtrip() {
        let reg = ServiceRegistry::new();
        assert!(reg.membership().is_none());
        reg.set_membership(Arc::new(AlwaysYes));
        assert!(reg.membership().is_some());
    }

    #[test]
    fn registry_survives_poisoned_lock() {
        let reg = Arc::new(ServiceRegistry::new());
        // Poison the RwLock: panic on another thread while holding the
        // write guard.
        let poisoner = Arc::clone(&reg);
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.membership.write().unwrap();
            panic!("deliberately poisoning the services lock");
        })
        .join();
        assert!(reg.membership.is_poisoned());
        // Poisoned lock must degrade, not crash: both halves of the API
        // keep working.
        assert!(reg.membership().is_none());
        reg.set_membership(Arc::new(AlwaysYes));
        assert!(reg.membership().is_some());
    }
}
