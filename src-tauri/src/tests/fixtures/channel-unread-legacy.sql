-- Frozen pre-seq query for migration equivalence tests.
select
            c.id,
            c.name,
            c.description,
            c.kind,
            c.dm_agent_id,
            cast(count(m.id) filter (
                where julianday(m.created_at) > julianday(
                    coalesce(r.last_read_at, '0001-01-01T00:00:00+00:00')
                )
                  and m.sender_role <> 'owner'
                  and m.delivery_state <> 'streaming'
                  and not (
                    m.sender_role <> 'system'
                    and m.delivery_state = 'complete'
                    and trim(m.body) = ''
                    and m.stream_key glob '????????-????-????-????-????????????:*'
                    and not exists (
                      select 1 from message_attachments ma where ma.message_id = m.id
                    )
                    and not exists (
                      select 1 from artifacts ar where ar.message_id = m.id
                    )
                  )
            ) as integer) as unread_count,
            cast((
                select count(*)
                from github_review_request_cache review_attention
                where review_attention.channel_id = c.id
                  and review_attention.is_review_requested
                  and review_attention.attention_unread
            ) as integer) as github_unread_count,
            (
                select review_attention_synced_at
                from channel_github_repositories github_binding
                where github_binding.channel_id = c.id
            ) as github_review_synced_at
        from channels c
        left join channel_read_state r on r.channel_id = c.id
        left join messages m on m.channel_id = c.id
        group by c.id, c.name, c.description, c.kind, c.dm_agent_id
        order by
          case
            when c.kind = 'channel' and c.name = 'lantor' then 0
            when c.kind = 'channel' then 1
            else 2
          end,
          c.name
