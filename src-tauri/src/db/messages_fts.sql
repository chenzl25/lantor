create virtual table messages_fts using fts5(
    body,
    sender_name,
    content = 'messages',
    content_rowid = 'rowid',
    tokenize = 'trigram case_sensitive 0'
);

create trigger messages_fts_after_insert after insert on messages
when new.delivery_state <> 'streaming'
begin
    insert into messages_fts(rowid, body, sender_name)
    values (new.rowid, new.body, new.sender_name);
end;

create trigger messages_fts_before_delete before delete on messages
when old.delivery_state <> 'streaming'
begin
    insert into messages_fts(messages_fts, rowid, body, sender_name)
    values ('delete', old.rowid, old.body, old.sender_name);
end;

-- Deltas and metadata-only updates do not tokenize or write to the index.
-- Leaving the completed state removes the old entry; finishing a stream
-- inserts its final text exactly once, including interrupted/failed output.
create trigger messages_fts_before_update
before update of rowid, body, sender_name, delivery_state on messages
when old.delivery_state <> 'streaming' and (
    new.delivery_state = 'streaming'
    or new.rowid <> old.rowid
    or new.body <> old.body
    or new.sender_name <> old.sender_name
)
begin
    insert into messages_fts(messages_fts, rowid, body, sender_name)
    values ('delete', old.rowid, old.body, old.sender_name);
end;

create trigger messages_fts_after_update
after update of rowid, body, sender_name, delivery_state on messages
when new.delivery_state <> 'streaming' and (
    old.delivery_state = 'streaming'
    or new.rowid <> old.rowid
    or new.body <> old.body
    or new.sender_name <> old.sender_name
)
begin
    insert into messages_fts(rowid, body, sender_name)
    values (new.rowid, new.body, new.sender_name);
end;

insert into messages_fts(messages_fts) values ('rebuild');
-- rebuild includes every external-content row. Remove in-flight messages in
-- the same transaction so subsequent deltas cannot leave stale index terms.
insert into messages_fts(messages_fts, rowid, body, sender_name)
select 'delete', rowid, body, sender_name from messages
where delivery_state = 'streaming';
