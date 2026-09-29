# PostgreSQL schema fixture

A small Rails-shaped source tree with a PostgreSQL `structure.sql` dump.
It is checked in and read statically; no Rails installation or database is
needed. The `:sql` configuration is intentional.

Run `cargo test --test structure_sql` or
`cargo run --bin roundhouse -- check --continue fixtures/postgres-blog`.
The integration tests also exercise unsupported DDL, source selection, and
attribute parity with an equivalent in-memory Ruby schema.
