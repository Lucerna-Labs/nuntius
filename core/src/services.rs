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

use std::sync::{Arc, RwLock};

/// Membership-check service plugins can publish into the registry.
///
/// `is_member` returns true iff the request carries a valid, active
/// membership token recognized by the implementing plugin (currently
/// the Patreon plugin). Implementations should be cheap and
/// side-effect-free — they're called per request.
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
    pub fn set_membership(&self, s: Arc<dyn MembershipService>) {
        *self.membership.write().expect("services poisoned") = Some(s);
    }

    /// Look up the membership service. Returns `None` when the
    /// providing plugin (Patreon) is disabled — consumers should
    /// degrade gracefully (e.g. fall back to a static token).
    pub fn membership(&self) -> Option<Arc<dyn MembershipService>> {
        self.membership.read().expect("services poisoned").clone()
    }
}
