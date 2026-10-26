//! Shared application state: database pool, config, live fan-out, tickets.

use std::sync::{Arc, Mutex};

use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::auth::constant_time_eq;
use crate::config::{Config, SETUP_TOKEN_TTL_SECONDS};
use crate::timeutil;

/// One message pushed to live sockets. `session_id` is `None` for dashboard-only
/// invalidations, which every subscriber of that user still receives.
#[derive(Clone, Debug)]
pub struct HubEvent {
    pub user_id: String,
    pub session_id: Option<String>,
    pub json: String,
}

#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<HubEvent>,
}

impl Hub {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.tx.subscribe()
    }

    pub fn publish(&self, event: HubEvent) {
        // A send error only means nobody is listening right now.
        let _ = self.tx.send(event);
    }

    pub fn publish_json(&self, user_id: &str, session_id: Option<&str>, json: String) {
        self.publish(HubEvent {
            user_id: user_id.to_string(),
            session_id: session_id.map(|value| value.to_string()),
            json,
        });
    }
}

struct SetupTokenEntry {
    token: String,
    expires_epoch: i64,
}

/// First-run bootstrap credential. Mirrors upstream's `SetupTokenService`: the
/// value is logged once (never returned over HTTP) and expires.
pub struct SetupTokenState {
    fixed: Option<String>,
    entry: Mutex<Option<SetupTokenEntry>>,
}

impl SetupTokenState {
    pub fn new(fixed: Option<String>) -> Self {
        Self {
            fixed,
            entry: Mutex::new(None),
        }
    }

    /// Current token and its expiry. Rotates (and logs) once expired.
    pub fn current(&self) -> (String, i64) {
        let mut guard = match self.entry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = timeutil::unix_now();
        let expired = guard
            .as_ref()
            .map(|entry| entry.expires_epoch <= now)
            .unwrap_or(true);
        if expired || guard.is_none() {
            let token = self
                .fixed
                .clone()
                .unwrap_or_else(|| format!("setup_{}", crate::ids::token_urlsafe(24)));
            let expires_epoch = now + SETUP_TOKEN_TTL_SECONDS;
            tracing::warn!(
                token = %token,
                expires_at = %timeutil::iso_from_epoch(expires_epoch),
                "first-run setup token (required to register the first admin)"
            );
            *guard = Some(SetupTokenEntry {
                token: token.clone(),
                expires_epoch,
            });
            return (token, expires_epoch);
        }
        let entry = guard.as_ref().expect("checked above");
        (entry.token.clone(), entry.expires_epoch)
    }

    /// `None` when the supplied token is wrong or expired.
    pub fn verify(&self, candidate: &str) -> Option<i64> {
        let (token, expires_epoch) = self.current();
        if timeutil::unix_now() >= expires_epoch {
            return None;
        }
        if constant_time_eq(token.as_bytes(), candidate.as_bytes()) {
            Some(expires_epoch)
        } else {
            None
        }
    }

    /// Invalidate the bootstrap token once the first admin exists.
    pub fn consume(&self) {
        let mut guard = match self.entry.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = None;
    }
}

struct Inner {
    config: Config,
    pool: PgPool,
    hub: Hub,
    setup_token: SetupTokenState,
    timelines: crate::timeline::TimelineHub,
    /// Live connector sockets. Presence and routing live here, not in Postgres:
    /// a socket cannot be shared between processes.
    connectors: crate::ingress::ConnectorRegistry,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let setup_token = SetupTokenState::new(config.setup_token.clone());
        Self {
            inner: Arc::new(Inner {
                config,
                pool,
                hub: Hub::new(1024),
                setup_token,
                timelines: crate::timeline::TimelineHub::new(),
                connectors: crate::ingress::ConnectorRegistry::new(),
            }),
        }
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    pub fn hub(&self) -> &Hub {
        &self.inner.hub
    }

    pub fn setup_token(&self) -> &SetupTokenState {
        &self.inner.setup_token
    }

    /// Domain engine cache plus its Postgres persistence layer.
    pub fn timelines(&self) -> &crate::timeline::TimelineHub {
        &self.inner.timelines
    }

    /// Routable connector connections (connector_id -> live socket).
    pub fn connectors(&self) -> &crate::ingress::ConnectorRegistry {
        &self.inner.connectors
    }
}
