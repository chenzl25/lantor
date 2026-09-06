-- Frozen pre-FTS search query for result/order/filter equivalence tests.
        select
            m.id,
            m.seq,
            m.channel_id,
            m.thread_root_id,
            m.sender_agent_id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.is_task,
            m.thread_followed,
            m.delivery_state,
            m.stream_key,
            t.number as task_number,
            t.status as task_status,
            m.created_at,
            m.updated_at
        from messages m
        join channels c on c.id = m.channel_id
        left join tasks t on t.message_id = m.id
        where (
            lower(m.body) like lower($1) escape '\'
            or lower(m.sender_name) like lower($1) escape '\'
            or lower(c.name) like lower($1) escape '\'
        )
          and ($2 is null or julianday(m.created_at) >= julianday($2))
          and m.delivery_state <> 'streaming'
          and not (
            m.sender_role <> 'system'
            and m.sender_role <> 'owner'
            and m.stream_key glob '????????-????-????-????-????????????:*'
            and m.delivery_state = 'complete'
            and trim(m.body) = ''
            and not exists (
              select 1 from message_attachments ma where ma.message_id = m.id
            )
            and not exists (
              select 1 from artifacts ar where ar.message_id = m.id
            )
          )
        order by m.seq desc
        limit $3
