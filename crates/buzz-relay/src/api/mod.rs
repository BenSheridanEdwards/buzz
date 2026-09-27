//! HTTP API — media, git, NIP-05, and the Nostr HTTP bridge.

pub mod admin;
pub mod bridge;
pub mod events;
pub mod gifs;
pub mod git;
pub mod invites;
pub mod media;
pub mod mesh_demo;
pub mod nip05;
pub mod operator;
pub mod workflows;

// Re-export imeta helpers used by ingest pipeline.
pub use crate::handlers::imeta::{validate_imeta_tags, verify_imeta_blobs};

use axum::{http::StatusCode, response::Json};

/// Standard error envelope.
pub(crate) fn api_error(status: StatusCode, msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({ "error": msg })))
}

pub(crate) fn internal_error(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    tracing::error!("Internal error: {msg}");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
}

#[allow(dead_code)]
pub(crate) fn not_found(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    api_error(StatusCode::NOT_FOUND, msg)
}

/// Relay membership enforcement — single gate for all authenticated entry points.
///
/// Moved here from the deleted `relay_members` module. Called by `media.rs`, `bridge.rs`,
/// `git/transport.rs`, and `audio/handler.rs`.
pub mod relay_members {
    use axum::{
        http::{HeaderMap, StatusCode},
        response::Json,
    };
    use buzz_core::{tenant::CommunityId, TenantContext};
    use tracing::{debug, info};

    use crate::state::AppState;

    /// Transport-neutral outcome of a relay-membership check.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum MembershipDecision {
        /// Relay membership enforcement is disabled.
        OpenRelay,
        /// Caller is directly present in `relay_members`.
        Member,
        /// Caller is admitted through a NIP-OA owner that is a relay member.
        ViaOwner(nostr::PublicKey),
        /// Caller is not admitted.
        Denied,
    }

    /// Return the sole NIP-OA credential header, if one was supplied.
    ///
    /// Repeated security-sensitive headers are ambiguous across HTTP stacks,
    /// so they are treated as no credential instead of silently selecting one.
    pub fn extract_auth_tag_header(headers: &HeaderMap) -> Option<&str> {
        let mut values = headers.get_all("x-auth-tag").iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return None;
        };
        value.to_str().ok()
    }

    /// Check relay membership without committing to an HTTP response shape.
    ///
    /// `community` is the server-resolved tenant of the request; membership is
    /// scoped to it so admitting a pubkey to community A never admits it to B.
    /// A NIP-OA credential is usable only when `signed_auth_created_at` came
    /// from the already-verified authentication event carrying that request.
    pub async fn check_relay_membership(
        state: &AppState,
        community: CommunityId,
        pubkey_bytes: &[u8],
        auth_tag_header: Option<&str>,
        signed_auth_created_at: Option<u64>,
    ) -> Result<MembershipDecision, String> {
        if !state.config.require_relay_membership {
            return Ok(MembershipDecision::OpenRelay);
        }

        let pubkey_hex = hex::encode(pubkey_bytes);
        let is_member = state
            .db
            .is_relay_member(community, &pubkey_hex)
            .await
            .map_err(|e| format!("relay membership check failed: {e}"))?;
        if is_member {
            return Ok(MembershipDecision::Member);
        }

        if state.config.allow_nip_oa_auth {
            if let Some(tag_json) = auth_tag_header {
                let agent_pubkey = nostr::PublicKey::from_slice(pubkey_bytes)
                    .map_err(|e| format!("invalid agent pubkey for NIP-OA check: {e}"))?;
                let Some(auth_created_at) = signed_auth_created_at else {
                    info!(agent = %pubkey_hex, "NIP-OA auth tag has no verified signed auth timestamp");
                    return Ok(MembershipDecision::Denied);
                };

                match buzz_sdk::nip_oa::verify_auth_tag_for_auth_event(
                    tag_json,
                    &agent_pubkey,
                    auth_created_at,
                ) {
                    Ok(owner_pubkey) => {
                        let owner_hex = owner_pubkey.to_hex();
                        let owner_is_member = state
                            .db
                            .is_relay_member(community, &owner_hex)
                            .await
                            .map_err(|e| format!("relay membership check (owner) failed: {e}"))?;
                        if owner_is_member {
                            debug!(
                                agent = %pubkey_hex,
                                owner = %owner_hex,
                                "NIP-OA membership granted via owner"
                            );
                            return Ok(MembershipDecision::ViaOwner(owner_pubkey));
                        }
                    }
                    Err(e) => {
                        info!(agent = %pubkey_hex, "NIP-OA auth tag invalid: {e}");
                    }
                }
            }
        }

        Ok(MembershipDecision::Denied)
    }

    /// Enforce relay membership for a pubkey, with NIP-OA agent delegation fallback.
    ///
    /// Returns `Ok(Some(owner_pubkey))` when the agent is not a direct member but
    /// its NIP-OA owner *is* — access is granted via delegation.
    ///
    /// On open relays (`require_relay_membership = false`), returns `Ok(None)`
    /// immediately — no membership check is performed. Callers that need NIP-OA
    /// owner extraction on open relays should call [`extract_nip_oa_owner`] directly.
    ///
    /// Direct members also return a cryptographically verified owner when an
    /// attestation is present. Membership admission and owner materialization
    /// are separate: already being a member must not suppress observer ownership.
    pub async fn enforce_relay_membership(
        state: &AppState,
        community: CommunityId,
        pubkey_bytes: &[u8],
        auth_tag_header: Option<&str>,
        signed_auth_created_at: Option<u64>,
    ) -> Result<Option<nostr::PublicKey>, (StatusCode, Json<serde_json::Value>)> {
        match check_relay_membership(
            state,
            community,
            pubkey_bytes,
            auth_tag_header,
            signed_auth_created_at,
        )
        .await
        {
            Ok(MembershipDecision::OpenRelay) => Ok(None),
            Ok(MembershipDecision::Member) => Ok(direct_member_owner(
                pubkey_bytes,
                auth_tag_header,
                signed_auth_created_at,
            )),
            Ok(MembershipDecision::ViaOwner(owner)) => Ok(Some(owner)),
            Ok(MembershipDecision::Denied) => Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "relay_membership_required",
                    "message": "You must be a relay member to access this relay"
                })),
            )),
            Err(e) => {
                tracing::error!("relay membership check errored: {e}");
                Err(super::internal_error(&e))
            }
        }
    }

    // Called only after direct membership is confirmed. Invalid/missing proof
    // leaves membership intact but must never establish an owner relationship.
    fn direct_member_owner(
        pubkey: &[u8],
        tag: Option<&str>,
        signed_at: Option<u64>,
    ) -> Option<nostr::PublicKey> {
        extract_nip_oa_owner(pubkey, tag, signed_at)
    }

    /// Extract NIP-OA owner from an auth tag without membership enforcement.
    ///
    /// Used on open relays (`require_relay_membership = false`) to opportunistically
    /// extract the owner pubkey for agent→owner backfill. The NIP-OA signature is
    /// cryptographically self-proving, so no feature flag is needed. Temporal
    /// conditions are evaluated against `signed_auth_created_at`. Returns
    /// `None` if the tag, timestamp, or conditions are absent or invalid.
    pub fn extract_nip_oa_owner(
        pubkey_bytes: &[u8],
        auth_tag_header: Option<&str>,
        signed_auth_created_at: Option<u64>,
    ) -> Option<nostr::PublicKey> {
        let tag_json = auth_tag_header?;
        let auth_created_at = signed_auth_created_at?;
        let agent_pubkey = nostr::PublicKey::from_slice(pubkey_bytes).ok()?;
        match buzz_sdk::nip_oa::verify_auth_tag_for_auth_event(
            tag_json,
            &agent_pubkey,
            auth_created_at,
        ) {
            Ok(owner) => Some(owner),
            Err(e) => {
                info!("extract_nip_oa_owner: invalid auth tag: {e}");
                None
            }
        }
    }

    /// Persist a cryptographically verified NIP-OA agent→owner relationship.
    ///
    /// Both principals are ensured first because `agent_owner_pubkey` has a
    /// community-scoped foreign key. The mapping is first-write-wins; an
    /// existing mapping is accepted only when it names the same owner.
    pub async fn materialize_nip_oa_owner(
        state: &AppState,
        tenant: &TenantContext,
        agent: &nostr::PublicKey,
        owner: &nostr::PublicKey,
    ) -> bool {
        for (role, pubkey) in [("agent", agent), ("owner", owner)] {
            match state
                .db
                .ensure_user_for_authorization(tenant.community(), pubkey.as_bytes())
                .await
            {
                Ok(true) => {
                    metrics::counter!(
                        "buzz_users_created_total",
                        "community" => tenant.host().to_owned()
                    )
                    .increment(1);
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(%role, error = %e, "ensure_user failed during NIP-OA backfill");
                    return false;
                }
            }
        }

        let materialized = match state
            .db
            .set_agent_owner_for_authorization(
                tenant.community(),
                agent.as_bytes(),
                owner.as_bytes(),
            )
            .await
        {
            Ok(true) => true,
            Ok(false) => state
                .db
                .is_agent_owner(tenant.community(), agent.as_bytes(), owner.as_bytes())
                .await
                .unwrap_or(false),
            Err(e) => {
                tracing::warn!(error = %e, "failed to backfill agent_owner_pubkey");
                false
            }
        };

        if materialized {
            state
                .author_type_cache
                .insert((tenant.community(), agent.to_bytes().to_vec()), true);
            state.observer_owner_cache.insert(
                (
                    tenant.community(),
                    agent.to_bytes().to_vec(),
                    owner.to_bytes().to_vec(),
                ),
                true,
            );
        }
        materialized
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use axum::http::{HeaderMap, HeaderValue};
        use buzz_sdk::nip_oa::compute_auth_tag;
        use nostr::Keys;

        #[test]
        fn auth_tag_header_must_be_unique() {
            let mut headers = HeaderMap::new();
            assert_eq!(extract_auth_tag_header(&headers), None);

            headers.insert("x-auth-tag", HeaderValue::from_static("credential-one"));
            assert_eq!(extract_auth_tag_header(&headers), Some("credential-one"));

            headers.append("x-auth-tag", HeaderValue::from_static("credential-two"));
            assert_eq!(extract_auth_tag_header(&headers), None);
        }

        #[tokio::test]
        async fn direct_member_owner_reaches_authorization_materialization() {
            use std::sync::Arc;
            let mut config = crate::config::Config::from_env().unwrap();
            config.database_url = crate::test_support::database_url();
            config.require_relay_membership = true;
            // Owner extraction must work independently of delegated admission.
            config.allow_nip_oa_auth = false;
            config.redis_url = "redis://127.0.0.1:1".into();
            let pool = sqlx::PgPool::connect(&config.database_url).await.unwrap();
            let db = buzz_db::Db::from_pool(pool.clone());
            let host = format!("owner-regression-{}.test", uuid::Uuid::new_v4());
            let community = db.ensure_configured_community(&host).await.unwrap().id;
            let tenant = TenantContext::resolved(community, host);
            let owner = Keys::generate();
            let agent = Keys::generate().public_key();
            db.add_relay_member(community, &agent.to_hex(), "member", None)
                .await
                .unwrap();
            let redis = deadpool_redis::Config::from_url(&config.redis_url)
                .create_pool(Some(deadpool_redis::Runtime::Tokio1))
                .unwrap();
            let pubsub = Arc::new(
                buzz_pubsub::PubSubManager::new(&config.redis_url, redis.clone())
                    .await
                    .unwrap(),
            );
            let audit = buzz_audit::AuditService::new(pool.clone());
            let auth = buzz_auth::AuthService::new(config.auth.clone());
            let search = buzz_search::SearchService::new(pool.clone());
            let workflow = Arc::new(buzz_workflow::WorkflowEngine::new(
                db.clone(),
                buzz_workflow::WorkflowConfig::default(),
            ));
            let media = buzz_media::MediaStorage::new(&config.media).unwrap();
            let (state, _shutdown) = AppState::new(
                config,
                db,
                redis,
                audit,
                pubsub,
                auth,
                search,
                workflow,
                Keys::generate(),
                media,
            );
            let proof = compute_auth_tag(&owner, &agent, "created_at>100&created_at<300").unwrap();

            // Drive the actual DB-backed gate, not only the extraction helper.
            let extracted = enforce_relay_membership(
                &state,
                community,
                agent.as_bytes(),
                Some(&proof),
                Some(200),
            )
            .await
            .unwrap();
            assert_eq!(extracted, Some(owner.public_key()));
            assert!(materialize_nip_oa_owner(&state, &tenant, &agent, &extracted.unwrap()).await);
            assert!(state
                .db
                .is_agent_owner(community, agent.as_bytes(), owner.public_key().as_bytes())
                .await
                .unwrap());
            // Invalid proof must neither remove membership nor install an owner.
            assert_eq!(
                enforce_relay_membership(
                    &state,
                    community,
                    agent.as_bytes(),
                    Some(&proof),
                    Some(300)
                )
                .await
                .unwrap(),
                None
            );
            assert_eq!(
                enforce_relay_membership(&state, community, agent.as_bytes(), None, Some(200))
                    .await
                    .unwrap(),
                None
            );
            // A different valid owner cannot overwrite the established mapping.
            assert!(
                !materialize_nip_oa_owner(&state, &tenant, &agent, &Keys::generate().public_key())
                    .await
            );
            assert!(state
                .db
                .is_agent_owner(community, agent.as_bytes(), owner.public_key().as_bytes())
                .await
                .unwrap());
        }

        #[test]
        fn existing_member_keeps_verified_owner_for_observer_backfill() {
            let owner = Keys::generate();
            let agent = Keys::generate().public_key();
            let proof = compute_auth_tag(&owner, &agent, "created_at>100&created_at<300").unwrap();
            assert_eq!(
                direct_member_owner(agent.as_bytes(), Some(&proof), Some(200)),
                Some(owner.public_key())
            );
            assert_eq!(
                direct_member_owner(agent.as_bytes(), Some(&proof), Some(300)),
                None
            );
            assert_eq!(
                direct_member_owner(agent.as_bytes(), Some(&proof), None),
                None
            );
            assert_eq!(
                direct_member_owner(
                    Keys::generate().public_key().as_bytes(),
                    Some(&proof),
                    Some(200)
                ),
                None
            );
            assert_eq!(direct_member_owner(agent.as_bytes(), None, Some(200)), None);
        }

        /// Valid NIP-OA auth tag → returns Some(owner_pubkey).
        #[test]
        fn valid_nip_oa_returns_owner() {
            let owner_keys = Keys::generate();
            let agent_keys = Keys::generate();
            let agent_pubkey = agent_keys.public_key();

            let tag_json = compute_auth_tag(&owner_keys, &agent_pubkey, "")
                .expect("compute_auth_tag must succeed");

            let result = extract_nip_oa_owner(
                &agent_pubkey.to_bytes(),
                Some(&tag_json),
                Some(nostr::Timestamp::now().as_secs()),
            );

            assert_eq!(result, Some(owner_keys.public_key()));
        }

        #[test]
        fn nip_oa_time_conditions_use_signed_auth_event_time() {
            let owner_keys = Keys::generate();
            let agent_pubkey = Keys::generate().public_key();

            let expired = compute_auth_tag(&owner_keys, &agent_pubkey, "created_at<200")
                .expect("sign expired credential");
            assert_eq!(
                extract_nip_oa_owner(&agent_pubkey.to_bytes(), Some(&expired), Some(200)),
                None
            );

            let future = compute_auth_tag(&owner_keys, &agent_pubkey, "created_at>200")
                .expect("sign future credential");
            assert_eq!(
                extract_nip_oa_owner(&agent_pubkey.to_bytes(), Some(&future), Some(200)),
                None
            );

            let in_window = compute_auth_tag(
                &owner_keys,
                &agent_pubkey,
                "kind=9&created_at>199&created_at<201",
            )
            .expect("sign in-window credential");
            assert_eq!(
                extract_nip_oa_owner(&agent_pubkey.to_bytes(), Some(&in_window), Some(200)),
                Some(owner_keys.public_key())
            );
            assert_eq!(
                extract_nip_oa_owner(&agent_pubkey.to_bytes(), Some(&in_window), None),
                None,
                "a credential without a verified signed auth timestamp must fail closed"
            );
        }

        /// No auth tag → returns None.
        #[test]
        fn no_auth_tag_returns_none() {
            let agent_keys = Keys::generate();
            let agent_pubkey = agent_keys.public_key();

            let result = extract_nip_oa_owner(
                &agent_pubkey.to_bytes(),
                None,
                Some(nostr::Timestamp::now().as_secs()),
            );

            assert_eq!(result, None);
        }

        /// Invalid auth tag → returns None.
        #[test]
        fn invalid_auth_tag_returns_none() {
            let agent_keys = Keys::generate();
            let agent_pubkey = agent_keys.public_key();

            let result = extract_nip_oa_owner(
                &agent_pubkey.to_bytes(),
                Some("not valid json"),
                Some(nostr::Timestamp::now().as_secs()),
            );

            assert_eq!(result, None);
        }
    }
}
