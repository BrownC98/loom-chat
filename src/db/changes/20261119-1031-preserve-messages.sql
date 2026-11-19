CREATE TABLE messages_preserved (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    sender INTEGER REFERENCES nodes(id) ON DELETE SET NULL,
    sender_name TEXT NOT NULL,
    kind TEXT NOT NULL,
    body TEXT NOT NULL,
    delivered INTEGER NOT NULL DEFAULT 0,
    sent_at INTEGER,
    what TEXT,
    printed INTEGER NOT NULL DEFAULT 0,
    refused INTEGER NOT NULL DEFAULT 0,
    recipient_name TEXT CHECK (recipient IS NULL OR recipient_name IS NOT NULL),
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    held INTEGER NOT NULL DEFAULT 0 CHECK (held IN (0, 1) AND (held = 0 OR recipient IS NULL))
);

WITH event_entries AS (
    SELECT message, at, CASE WHEN json_valid(entry) THEN entry END AS entry FROM events
),
message_times AS (
    SELECT message, min(at) AS first_at,
        min(at) FILTER (WHERE entry ->> 'kind' = 'message' AND entry ->> 'step' IN ('sent', 'held')) AS created_at,
        max(entry ->> '$.to.name') FILTER (WHERE entry ->> 'kind' = 'message' AND entry ->> 'step' = 'sent') AS recipient_name
    FROM event_entries WHERE message IS NOT NULL GROUP BY message
),
inbox_answers AS (
    SELECT entry ->> 'wait' AS wait, min(at) AS at
    FROM event_entries WHERE entry ->> 'kind' = 'answered' GROUP BY wait
),
closed_answers AS (
    SELECT node, CASE WHEN json_valid(wait) THEN wait ->> 'id' END AS wait_id, min(closed_at) AS at
    FROM closed_waits
    WHERE CASE WHEN json_valid(how) THEN how ->> 'kind' = 'answered' END
    GROUP BY node, wait_id
)
INSERT INTO messages_preserved (
    id, recipient, sender, sender_name, kind, body, delivered, sent_at, what, printed, refused,
    recipient_name, created_at, held
)
SELECT m.id, m.recipient, m.sender, m.sender_name, m.kind, m.body, m.delivered, m.sent_at, m.what, m.printed, m.refused,
    coalesce(n.name, e.recipient_name),
    coalesce(e.created_at, (SELECT min(at) FROM doc_talk WHERE message = m.id),
        a.at, c.at, m.sent_at, e.first_at, unixepoch()),
    m.recipient IS NULL AND m.sender IS NOT NULL
FROM messages m
LEFT JOIN nodes n ON n.id = m.recipient
LEFT JOIN message_times e ON e.message = m.id
LEFT JOIN inbox_answers a ON m.what ->> 'kind' = 'answer' AND m.what ->> '$.question.in' = 'inbox'
    AND a.wait = m.what ->> '$.question.id'
LEFT JOIN closed_answers c ON m.what ->> 'kind' = 'answer' AND m.what ->> '$.question.in' = 'inbox'
    AND c.node = m.recipient AND c.wait_id = m.what ->> '$.question.id';

DELETE FROM sqlite_sequence WHERE name = 'messages_preserved';
INSERT INTO sqlite_sequence (name, seq)
    SELECT 'messages_preserved', seq FROM sqlite_sequence WHERE name = 'messages';

DROP TABLE messages;
ALTER TABLE messages_preserved RENAME TO messages;

CREATE INDEX messages_queued ON messages(recipient) WHERE delivered = 0;
CREATE INDEX messages_held ON messages(sender) WHERE recipient IS NULL AND held = 1;
CREATE INDEX messages_replies ON messages(recipient, what ->> '$.question.id')
    WHERE what ->> 'kind' = 'answer' AND what ->> '$.question.in' = 'message';
