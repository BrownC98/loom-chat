CREATE TABLE chat_rooms (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE chat_members (
    room INTEGER NOT NULL REFERENCES chat_rooms(id) ON DELETE CASCADE,
    node INTEGER NOT NULL,
    PRIMARY KEY (room, node)
);

CREATE TABLE chat_archived_nodes (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    repo INTEGER,
    worktree TEXT
);

INSERT INTO chat_archived_nodes (id, name)
SELECT id, name FROM (
    SELECT id, name, row_number() OVER (PARTITION BY id ORDER BY at DESC, seq DESC) AS rank
    FROM (
        SELECT recipient_node_id AS id, recipient_name AS name, created_at AS at, id AS seq FROM messages
        UNION ALL SELECT sender_node_id, sender_name, created_at, id FROM messages
        UNION ALL SELECT entry ->> '$.node.id', entry ->> '$.node.name', at, id FROM events
            WHERE CASE WHEN json_valid(entry) THEN entry ->> 'kind' IN ('created', 'removed', 'renamed') END
        UNION ALL SELECT node, name, closed_at, seq FROM closed_waits
        UNION ALL SELECT CAST(substr(doc, 6) AS INTEGER), name, at, id FROM doc_talk
            WHERE doc = format('room/%d', CAST(substr(doc, 6) AS INTEGER)) AND CAST(substr(doc, 6) AS INTEGER) > 0
    )
    WHERE id IS NOT NULL AND name IS NOT NULL AND id NOT IN (SELECT id FROM nodes)
) WHERE rank = 1;

CREATE TRIGGER chat_archive_node BEFORE DELETE ON nodes BEGIN
    INSERT INTO chat_archived_nodes (id, name, repo, worktree)
    VALUES (old.id, old.name, old.repo, old.worktree);
END;

CREATE VIEW chat_node_places AS
    SELECT id, repo, worktree FROM nodes
    UNION ALL SELECT id, repo, worktree FROM chat_archived_nodes;

CREATE VIEW chat_node_rooms AS
    SELECT id AS node, format('work-%d-%s', repo, hex(worktree)) AS room
    FROM chat_node_places WHERE repo IS NOT NULL AND worktree IS NOT NULL
    UNION ALL
    SELECT node, format('custom-%d', room) FROM chat_members JOIN chat_rooms ON id = room
    WHERE archived = 0;

CREATE TABLE chat_key (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    key BLOB NOT NULL CHECK (length(key) = 32)
);

ALTER TABLE messages ADD COLUMN chat_group INTEGER;
ALTER TABLE messages ADD COLUMN chat_exposed INTEGER NOT NULL DEFAULT 0;
CREATE INDEX chat_group ON messages(chat_group) WHERE chat_group IS NOT NULL;
CREATE INDEX chat_recipient ON messages(recipient_node_id, created_at, id);
CREATE INDEX chat_sender ON messages(sender_node_id, created_at, id);

CREATE TRIGGER chat_expose_message AFTER UPDATE OF sent_at, delivered, printed ON messages
WHEN new.chat_group IS NOT NULL AND new.chat_exposed = 0
    AND (new.sent_at IS NOT NULL OR new.delivered != 0 OR new.printed != 0)
BEGIN
    UPDATE messages SET chat_exposed = 1 WHERE id = new.id;
END;

CREATE TABLE chat_room_messages (
    room TEXT NOT NULL,
    message INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    outside_read INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (room, message)
);
CREATE INDEX chat_message_rooms ON chat_room_messages(message);

CREATE TRIGGER chat_agent_message AFTER UPDATE OF recipient_node_id ON messages
WHEN new.sender IS NOT NULL AND new.recipient IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO chat_room_messages (room, message)
    SELECT sender.room, new.id FROM chat_node_rooms sender JOIN chat_node_rooms recipient USING (room)
    WHERE sender.node = new.sender_node_id AND recipient.node = new.recipient_node_id;
END;

CREATE TRIGGER chat_insert_agent_message AFTER INSERT ON messages
WHEN new.sender IS NOT NULL AND new.recipient IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO chat_room_messages (room, message)
    SELECT sender.room, new.id FROM chat_node_rooms sender JOIN chat_node_rooms recipient USING (room)
    WHERE sender.node = new.sender_node_id AND recipient.node = new.recipient_node_id;
END;

ALTER TABLE doc_talk ADD COLUMN chat_reply INTEGER REFERENCES messages(id);
CREATE INDEX chat_talk_reply ON doc_talk(chat_reply) WHERE chat_reply IS NOT NULL;

CREATE VIEW chat_open_waits AS
    SELECT node, id, wait FROM questions
    UNION ALL SELECT node, id, wait FROM dones
    UNION ALL SELECT node, id, wait FROM alerts
    UNION ALL SELECT id, wait ->> '$.id', wait FROM nodes WHERE wait IS NOT NULL;

CREATE TABLE chat_room_waits (
    room TEXT NOT NULL,
    wait INTEGER NOT NULL,
    PRIMARY KEY (room, wait)
);

CREATE TRIGGER chat_question AFTER INSERT ON questions BEGIN
    INSERT OR IGNORE INTO chat_room_waits (room, wait)
    SELECT room, new.id FROM chat_node_rooms WHERE node = new.node AND room LIKE 'custom-%';
END;

CREATE TRIGGER chat_done AFTER INSERT ON dones BEGIN
    INSERT OR IGNORE INTO chat_room_waits (room, wait)
    SELECT room, new.id FROM chat_node_rooms WHERE node = new.node AND room LIKE 'custom-%';
END;

CREATE TRIGGER chat_alert AFTER INSERT ON alerts BEGIN
    INSERT OR IGNORE INTO chat_room_waits (room, wait)
    SELECT room, new.id FROM chat_node_rooms WHERE node = new.node AND room LIKE 'custom-%';
END;

CREATE TRIGGER chat_prompt AFTER UPDATE OF wait ON nodes
WHEN new.wait IS NOT NULL AND (old.wait ->> '$.id') IS NOT (new.wait ->> '$.id')
BEGIN
    INSERT OR IGNORE INTO chat_room_waits (room, wait)
    SELECT room, new.wait ->> '$.id' FROM chat_node_rooms WHERE node = new.id AND room LIKE 'custom-%';
END;
