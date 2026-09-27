//! Canonical foreground and idle delivery share the native media pipeline.
use super::*;
use crate::background_routes::attachment as store;

/// Media attempts before a reply is posted as text with a failure notice.
pub(crate) const MAX_MEDIA_ATTEMPTS: u32 = 5;

/// Publish every pending record independently, in delivery order.
///
/// One record's failure never blocks later ones. Transient failures stay
/// durable and are reported in the aggregate error; a record that can never
/// publish (no bound route) is dropped and reported once; media that keeps
/// failing is given up after [`MAX_MEDIA_ATTEMPTS`] and its text is posted
/// with the failure notice.
pub(crate) async fn publish_canonical_outbox(
    ctx: &PromptContext,
    sid: &str,
) -> Result<(), AcpError> {
    let dir = ctx
        .background_routes_dir
        .as_ref()
        .ok_or_else(|| store::invalid("missing outbox directory"))?;
    let mut errors = Vec::new();
    for key in store::load(dir, sid)?.pending_in_delivery_order() {
        if let Err(error) = publish_record(ctx, dir, sid, &key).await {
            errors.push(format!("{key}: {error}"));
        }
    }
    if errors.is_empty() {
        return Ok(());
    }
    Err(store::invalid(&format!(
        "{} canonical record(s) not published: {}",
        errors.len(),
        errors.join("; ")
    )))
}

async fn publish_record(
    ctx: &PromptContext,
    dir: &std::path::Path,
    sid: &str,
    key: &str,
) -> Result<(), AcpError> {
    let state = store::load(dir, sid)?;
    let Some(text) = state.outbound.get(key) else {
        return Ok(());
    };
    if !state.outbound_routes.contains_key(key) && !state.publications.contains_key(key) {
        store::abandon(dir, sid, key)?;
        tracing::error!(%sid, %key, "canonical record has no bound route; dropped");
        return Err(store::invalid("unbound publication route; record dropped"));
    }
    let capture = crate::media_publish::TurnMediaCapture::authoritative(text.clone());
    if capture.references_media() {
        let route = store::publication_route(dir, sid, key)?;
        let media_key = format!("media:{key}");
        let receipt = store::Publication {
            dir,
            sid,
            key: &media_key,
        };
        if !state.published.contains(&media_key) {
            if let Some(event) = state.publications.get(&media_key) {
                receipt.submit(&ctx.rest_client, event).await?;
            } else {
                let target = crate::media_publish::ReplyTarget::for_trigger(
                    route.scope.channel_id(),
                    &route.trigger,
                );
                let report = publish_reply_media_now_with_receipt(
                    ctx,
                    &target,
                    capture,
                    &uuid::Uuid::new_v4().to_string(),
                    Some(&receipt),
                )
                .await;
                if !report.failed.is_empty() || report.event_id.is_none() {
                    let notice = crate::media_publish::failure_notice(&report);
                    if !store::media_failed(dir, sid, key, &notice, MAX_MEDIA_ATTEMPTS)? {
                        return Err(store::invalid(&format!(
                            "canonical media unresolved: {notice}"
                        )));
                    }
                    tracing::error!(%sid, %key, %notice, "canonical media abandoned; posting text");
                }
            }
        }
    }
    let Some(event) = store::prepare_publication(dir, sid, key, &ctx.rest_client.keys)? else {
        return Ok(());
    };
    store::Publication { dir, sid, key }
        .submit(&ctx.rest_client, &event)
        .await
}
