-- ERC-20 fixture for crates/server/tests/integration.rs.
--
-- Shaped exactly as superquery-node's DDL would create it for the SDL in that
-- test (see .claude/docs/CONTRACTS.md §3–§5): one Postgres schema per project,
-- a key/value `_metadata`, one table per entity named
-- underscored(pluralize(Entity)), columns underscored(field), BigInt as
-- numeric, Bytes as bytea, a foreign key stored as the target's text id.
--
-- Data is chosen for the cases that break first:
--   * t1.value is 2^256 - 1, which no f64 represents;
--   * t1, t2, t3 tie on block_number, so pagination must lean on the id
--     tiebreaker;
--   * account `c` owns no transfers, so its reverse relation is `[]`.

DROP SCHEMA IF EXISTS itest_query CASCADE;
CREATE SCHEMA itest_query;

CREATE TABLE itest_query._metadata (
    key   text PRIMARY KEY,
    value text NOT NULL
);

INSERT INTO itest_query._metadata (key, value) VALUES
    ('lastProcessedHeight', '120'),
    ('lastFinalizedVerifiedHeight', '110'),
    ('targetHeight', '130'),
    ('chain', 'evm:1'),
    ('schemaMigrationCount', '1');

CREATE TABLE itest_query.accounts (
    id text PRIMARY KEY
);

CREATE TABLE itest_query.transfers (
    id           text PRIMARY KEY,
    "from"       text    NOT NULL,
    value        numeric NOT NULL,
    block_number integer NOT NULL,
    data         bytea,
    from_account text    NOT NULL
);

INSERT INTO itest_query.accounts (id) VALUES ('a'), ('b'), ('c');

INSERT INTO itest_query.transfers (id, "from", value, block_number, data, from_account) VALUES
    ('t1', '0xaaa', 115792089237316195423570985008687907853269984665640564039457584007913129639935, 100, '\xdeadbeef', 'a'),
    ('t2', '0xaaa', 9000, 100, NULL, 'a'),
    ('t3', '0xbbb', 1,    100, NULL, 'b'),
    ('t4', '0xbbb', 42,   101, NULL, 'b'),
    ('t5', '0xccc', 7,    102, NULL, 'a');
