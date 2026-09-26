//! Canonical foreground and idle delivery share the native media pipeline.
use super::*;
use crate::background_routes::attachment as store;

pub(crate) async fn publish_canonical_outbox(
    ctx: &PromptContext,
    sid: &str,
) -> Result<(), AcpError> {
    let dir = ctx
        .background_routes_dir
        .as_ref()
        .ok_or_else(|| store::invalid("missing outbox directory"))?;
    let state = store::load(dir, sid)?;

    for (key, text) in &state.outbound {
        if state.published.contains(key) {
            continue;
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
            if !store::load(dir, sid)?.published.contains(&media_key) {
                if let Some(event) = store::load(dir, sid)?.publications.get(&media_key) {
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
                        return Err(store::invalid(&format!(
                            "canonical media unresolved: {}",
                            crate::media_publish::failure_notice(&report)
                        )));
                    }
                }
            }
        }
        let Some(event) = store::prepare_publication(dir, sid, key, &ctx.rest_client.keys)? else {
            continue;
        };
        store::Publication { dir, sid, key }
            .submit(&ctx.rest_client, &event)
            .await?;
    }
    Ok(())
}
