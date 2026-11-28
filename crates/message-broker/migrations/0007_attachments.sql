-- Files a message carries, as `[{id, name, size}]` (D106). File Storage decides who
-- may read them; the name and size are here so a reader knows what to fetch.
ALTER TABLE messages ADD COLUMN attachments TEXT NOT NULL DEFAULT '[]';
