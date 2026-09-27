# Hermes gateway attachment v1

`hermes acp --attach` is a lightweight client of an existing Hermes GatewayRunner,
not a replacement runtime. This integration does not start or stop a live gateway,
change deployment configuration, or authorize managed-agent cutover.

## Negotiated behavior

The harness advertises `hermesAttachment.version = 1` and requires the server's
versioned features: history-free replay, delivery replay, active-turn snapshots,
terminal receipts, canonical asynchronous wakes and retained admission. Native ACP
continues to use JSON-RPC prompt responses. Unnegotiated custom receipts cannot
complete an unrelated native prompt.

- Durable Buzz scope/session routes restore canonical sessions after a harness
  restart. Trigger event IDs identify retained admissions.
- Admission intent is saved before `_hermes/turn/admit`. Recovery queries
  `_hermes/turn/status`; ambiguous `unknown` status does not create fresh work.
- `session/load` requests history-free cursor replay and pages until the explicit
  replay-complete fence. The active-turn identity is retained for cancellation.
- Authoritative final replacement parts and the replay cursor are stored together.
  A successful terminal receipt requires a complete authoritative final.
- Durable text notices and terminal output enter an outbox before the journal
  cursor is persisted. Unsupported notices fail without advancing that cursor.
- Foreground completion and background recovery share the canonical publisher.
  Media goes through the existing path validation/upload/signing pipeline. Signed
  outgoing events are persisted before relay submission and their acknowledgements
  afterward, allowing retries with the same event ID. This is not a claim of
  exactly-once network delivery.
- Revision checks and Unix file locks reject stale state overwrites, including
  outgoing acknowledgement versus multipart/replay updates.
- Canonical background recovery observes gateway-owned work, never dispatching a
  synthetic delivery prompt. Canonical new sessions skip synthetic initial turns.
- Explicit `/approve` and `/deny` retain native control handling, with unrelated
  context blocks removed. Cancellation requests carry the active turn identity
  and cleanup waits for that turn's receipt.

Desktop transcript handling uses canonical session/turn/message identities,
authoritative multipart replacement, notice deduplication and separate terminal
lifecycle rows. Native request ownership remains separate from canonical receipts.
The existing owner-encrypted observer pipeline remains the telemetry transport;
ordinary signed channel replies are not described as end-to-end encrypted.

## Boundaries and limitations

Canonical durable writes currently require Unix file locking; unsupported
platforms fail closed without changing native ACP. State capacity and final-size
limits fail explicitly rather than silently dropping data. Gateway-provided tools
are authoritative; attachment does not forward a replacement client MCP setup.
A live cutover and production credentials remain separate operator decisions.

## Verification

From the Buzz repository after `. ./bin/activate-hermit`:

```sh
cargo test -p buzz-acp --lib
cargo clippy -p buzz-acp --lib --tests -- -D warnings
cargo fmt --all -- --check
cd desktop
node --import ./test-loader.mjs --experimental-strip-types --test src/features/agents/ui/hermesAttachmentTranscript.test.mjs
```

`wire.json` captures legacy/unnegotiated behavior, including fail-closed replay
and native-response ownership. `canonical-v1.json` is produced by the real Hermes
frame/journal/replay code and consumed by Rust through the production NDJSON loop
and by the desktop transcript tests. The generators isolate session lookup and
storage; they do not execute a model.

To regenerate v1, set `PYTHONPATH` to the compatible Hermes checkout and run:

```sh
PYTHONDONTWRITEBYTECODE=1 /path/to/hermes/venv/bin/python \
  test-fixtures/hermes-attachment/generate_v1.py
```

The local integration harness uses a real GatewayRunner, ACPListener, attach CLI
and Rust consumer, replacing only model execution and isolating HERMES_HOME.
Run from the compatible Hermes checkout:

```sh
HERMES_PYTHON=/path/to/hermes/venv/bin/python PYTHONDONTWRITEBYTECODE=1 \
  scripts/run_tests.sh /absolute/path/to/buzz/test-fixtures/hermes-attachment/test_rust_consumer.py -v --tb=short
```

The gateway E2E requires that separate compatible Hermes checkout. It is not
silently counted as executed by ordinary Buzz unit CI.
