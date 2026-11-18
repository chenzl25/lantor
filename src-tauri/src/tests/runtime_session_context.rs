use super::{
    forget_session_context, load_injected_memory, load_surface_delivery, store_injected_memory,
    store_surface_delivery, SurfaceContextDelta, SurfaceDelivery,
};
use crate::freshness::advance_agent_target_watermark;
use crate::runtime::process::upsert_runtime_thread_id;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

#[test]
fn only_the_session_a_delta_was_made_for_gets_it() {
    let delivery = SurfaceDelivery {
        delivered_seq: 9,
        delta: Some(SurfaceContextDelta {
            thread_id: "session-1".to_owned(),
            prompt: "since last turn".to_owned(),
        }),
    };
    assert_eq!(
        delivery.delta_prompt_for(Some("session-1")),
        Some("since last turn")
    );
    assert_eq!(delivery.delta_prompt_for(Some("session-2")), None);
    // A process that started a new session does not know its id yet.
    assert_eq!(delivery.delta_prompt_for(None), None);
    assert_eq!(
        SurfaceDelivery::default().delta_prompt_for(Some("session-1")),
        None
    );
}

#[tokio::test]
async fn a_session_keeps_what_it_was_given_until_it_is_compacted() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "session-context-agent").await?;
        let channel_id = insert_test_channel(&pool, "session-context-channel").await?;
        // A turn's start creates the surface's watermark row and the
        // runtime's session row.
        advance_agent_target_watermark(&pool, agent_id, Some(channel_id), None, 12).await?;
        upsert_runtime_thread_id(&pool, agent_id, "claude", "session-1", "idle").await?;
        assert_eq!(
            load_surface_delivery(&pool, agent_id, channel_id, None).await?,
            None
        );
        assert_eq!(
            load_injected_memory(&pool, agent_id, "claude", "session-1").await?,
            None
        );

        store_surface_delivery(&pool, agent_id, channel_id, None, "session-1", 12).await?;
        // An older turn finishing later does not move the record back.
        store_surface_delivery(&pool, agent_id, channel_id, None, "session-1", 8).await?;
        assert_eq!(
            load_surface_delivery(&pool, agent_id, channel_id, None).await?,
            Some(("session-1".to_owned(), 12))
        );
        store_injected_memory(&pool, agent_id, "claude", "session-1", Some("memory v1")).await?;
        assert_eq!(
            load_injected_memory(&pool, agent_id, "claude", "session-1").await?,
            Some("memory v1".to_owned())
        );
        assert_eq!(
            load_injected_memory(&pool, agent_id, "claude", "session-2").await?,
            None,
            "another session never got the snapshot"
        );

        // A new session starts from what it was given itself.
        store_surface_delivery(&pool, agent_id, channel_id, None, "session-2", 5).await?;
        assert_eq!(
            load_surface_delivery(&pool, agent_id, channel_id, None).await?,
            Some(("session-2".to_owned(), 5))
        );

        forget_session_context(&pool, agent_id, "claude").await?;
        assert_eq!(
            load_surface_delivery(&pool, agent_id, channel_id, None).await?,
            None
        );
        assert_eq!(
            load_injected_memory(&pool, agent_id, "claude", "session-1").await?,
            None
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
