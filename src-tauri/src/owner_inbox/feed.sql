with reads as (
    select item_id, max(julianday(read_until)) as read_until from (
        select item_id, read_until from owner_inbox_read_state
        union all
        select item_id, dismissed_until from owner_inbox_dismissals
    ) group by item_id
), visible as materialized (
    select m.id, m.seq, m.channel_id, m.thread_root_id, m.sender_role,
        m.thread_followed, m.created_at,
        lower(substr(hex(m.id),1,8)||'-'||substr(hex(m.id),9,4)||'-'||
              substr(hex(m.id),13,4)||'-'||substr(hex(m.id),17,4)||'-'||substr(hex(m.id),21,12)) as key,
        m.sender_role <> 'owner' and exists (
            select 1 from json_each(?1) handle where instr(lower(m.body), lower(handle.value)) > 0
        ) as mentions_owner,
        m.sender_role <> 'owner' and (
            (cr.last_read_seq is not null and m.seq > cr.last_read_seq) or
            (cr.last_read_seq is null and julianday(m.created_at) > coalesce(julianday(cr.last_read_at), 0))
        ) as channel_unread
    from messages m
    left join channel_read_state cr on cr.channel_id = m.channel_id
    where m.delivery_state <> 'streaming'
      and not (
        m.sender_role not in ('owner', 'system') and m.delivery_state = 'complete'
        and m.stream_key glob '????????-????-????-????-????????????:*' and trim(m.body) = ''
        and not exists (select 1 from message_attachments ma where ma.message_id = m.id)
        and not exists (select 1 from artifacts ar where ar.message_id = m.id)
      )
), replies as (
    select reply.*,
        reply.channel_unread and julianday(reply.created_at) > coalesce(r.read_until, 0) as unread,
        row_number() over (partition by reply.thread_root_id order by julianday(reply.created_at) desc, reply.seq desc, reply.key desc) as rank
    from visible reply
    join visible root on root.id = reply.thread_root_id
    left join reads r on r.item_id = 'thread:' || root.key
), thread_totals as (
    select thread_root_id, count(*) as reply_count, sum(unread) as unread_count,
        min(case when unread then seq end) as first_unread_seq
    from replies group by thread_root_id
), threads as (
    select root.id, root.key, root.channel_id, latest.id as latest_id,
        latest.created_at, totals.reply_count, totals.unread_count,
        coalesce(first_unread.id, latest.id) as target_id
    from visible root
    join replies latest on latest.thread_root_id = root.id and latest.rank = 1
    join thread_totals totals on totals.thread_root_id = latest.thread_root_id
    left join visible first_unread on first_unread.seq = totals.first_unread_seq
    where root.thread_followed or totals.unread_count > 0
), channel_totals as (
    select channel_id, sum(channel_unread) as unread_count from visible group by channel_id
), latest_channels as (
    select *, row_number() over (partition by channel_id order by seq desc, key desc) as rank from visible
), candidates as (
    select 'thread:' || key as id, 'thread' as kind, channel_id, id as thread_id,
        target_id as message_id, latest_id as source_id, null as task_id, null as reminder_id,
        created_at as timestamp, unread_count > 0 as base_unread, reply_count, unread_count as new_count
    from threads
    union all
    select 'mention:' || m.key, 'mention', m.channel_id, coalesce(m.thread_root_id,m.id),
        m.id, m.id, null, null, m.created_at,
        case when m.thread_root_id is null then m.channel_unread else coalesce(r.unread,0) end,
        coalesce(t.reply_count,0), 0
    from visible m
    left join replies r on r.id = m.id
    left join thread_totals t on t.thread_root_id = coalesce(m.thread_root_id,m.id)
    where m.mentions_owner
    union all
    select c.kind || ':' || lower(substr(hex(c.id),1,8)||'-'||substr(hex(c.id),9,4)||'-'||
        substr(hex(c.id),13,4)||'-'||substr(hex(c.id),17,4)||'-'||substr(hex(c.id),21,12)),
        c.kind, c.id, latest.thread_root_id, latest.id, latest.id, null, null,
        latest.created_at, 1, coalesce(t.reply_count,0), totals.unread_count
    from channels c
    join latest_channels latest on latest.channel_id = c.id and latest.rank = 1
    join channel_totals totals on totals.channel_id = latest.channel_id and totals.unread_count > 0
    left join thread_totals t on t.thread_root_id = latest.thread_root_id
    where not exists (select 1 from threads where id = latest.thread_root_id)
    union all
    select 'task:' || lower(substr(hex(t.id),1,8)||'-'||substr(hex(t.id),9,4)||'-'||
        substr(hex(t.id),13,4)||'-'||substr(hex(t.id),17,4)||'-'||substr(hex(t.id),21,12)),
        'task', t.channel_id, t.message_id, t.message_id, null, t.id, null,
        t.updated_at, t.status = 'in_review', coalesce(tt.reply_count,0), 0
    from tasks t left join thread_totals tt on tt.thread_root_id = t.message_id where t.status <> 'done'
    union all
    select 'reminder:' || lower(substr(hex(r.id),1,8)||'-'||substr(hex(r.id),9,4)||'-'||
        substr(hex(r.id),13,4)||'-'||substr(hex(r.id),17,4)||'-'||substr(hex(r.id),21,12)),
        'reminder', r.channel_id, r.thread_root_id, r.message_id, null, null, r.id,
        coalesce(r.fired_at,r.due_at), 1, coalesce(tt.reply_count,0), 1
    from reminders r left join thread_totals tt on tt.thread_root_id = r.thread_root_id where r.status = 'fired'
), eligible as (
    select c.*, c.base_unread and julianday(c.timestamp) > coalesce(r.read_until,0) as unread
    from candidates c
    left join reads r on r.item_id = c.id
    left join owner_inbox_hidden_items h on h.item_id = c.id
    where julianday(c.timestamp) > coalesce(julianday(h.hidden_until),0)
)
