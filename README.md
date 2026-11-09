# pbview — zero-copy protobuf views for `no_std`

`pbview` reads protobuf messages **straight from their wire bytes**. Instead of
decoding into a struct, the generator emits a _view_ per message: a borrowed
`&[u8]` with one lazy accessor per field. Strings and bytes come back as
borrows of the input, nested messages as further views, repeated fields as
iterators over the buffer, and a oneof as a borrowed enum you `match` on.

It was built for a heap-starved microcontroller (ESP32-C6) where by-value
generated structs were the problem: every decoded envelope is sized by its
_largest_ possible arm, with inline capacity for every repeated field, and it
lands on the stack of whatever task handles the frame. A view is two words,
whatever the schema.

```rust
let env = ClientMessageView::parse(rx_frame)?;      // validates once, copies nothing
match env.msg() {
    Some(client_message::Msg::Hello(h)) => greet(h.client(), h.app_version()),
    Some(client_message::Msg::SubmitMap(m)) => {
        let map = m.map().unwrap();
        for led in map.leds() {                       // iterates the frame in place
            store(led.id(), led.xyz());               // xyz: packed doubles, read lazily
        }
    }
    _ => {}
}
```

## What you get

- **No allocation, no copy, no `unsafe`** (`#![forbid(unsafe_code)]`), `no_std`.
- **`parse` validates once** (framing, wire types per the schema, UTF-8,
  packed runs, nesting depth ≤ `DEFAULT_MAX_DEPTH`), so accessors on a parsed
  view never silently skip anything. `new_unchecked` skips it; accessors stay
  bounds-safe and simply stop at malformed data. Nothing panics on any input
  (fuzz-tested).
- **No capacity limits.** There is nothing to size, so nothing truncates.
- **Protobuf semantics**: last occurrence wins for singular scalars; packed and
  unpacked repeated encodings (and mixtures) are both accepted; unknown fields
  are skipped; proto3 enums stay open (raw `i32` + a generated
  `#[repr(i32)]` enum with `TryFrom<i32>`); proto3 `optional` and message
  fields have presence (`Option`).
- **Single-pass access**: `view.fields()` yields a typed `AnyField` per wire
  field, for hot paths that read most of a message.
- **Encoding**: `Writer` writes into a `&mut [u8]`; `Writer::message` writes a
  nested message in place and backpatches its length (one in-buffer move when
  the body exceeds 127 bytes), so replies need no by-value struct either.

## Design: lazy accessors

Each accessor re-scans the message's top level (O(fields) per call, zero extra
state). For control-sized messages that read a handful of fields this beats
building an offset index (whose size grows with the field count and would
land on the stack again). Code that reads most fields of a large message uses
the single-pass `fields()` iterator instead.

Deliberate deviation: a singular _message_ field that occurs more than once is
not merged (protobuf merges); the last occurrence wins. Groups are rejected.

## Using it

The generator reads a `FileDescriptorSet` (`protoc --descriptor_set_out`) and
needs nothing else — it parses descriptors with `pbview` itself.

**Bazel** (`rules_rust`):

```starlark
load("//oss/pbview:defs.bzl", "pbview_library")

pbview_library(name = "my_views", fdset = ":my_proto_fdset")
```

**Cargo**: run `protoc --descriptor_set_out=…` in your build, then in
`build.rs`:

```rust
let fdset = std::fs::read(fdset_path)?;
let code = pbview_gen::generate(&fdset, &pbview_gen::Options::default())?;
std::fs::write(out_dir.join("views.rs"), code)?;
```

and `include!(concat!(env!("OUT_DIR"), "/views.rs"));`. Or use the CLI:
`pbview-gen descriptor_set.bin views.rs [--crate-root] [--runtime PATH]`.

Generated layout: package `a.b` becomes `mod a { mod b { … } }`; message `Foo`
becomes `FooView<'a>`, with its nested types, oneof enums and `AnyField` in
`mod foo`. Field accessors are `snake_case` (Rust keywords as raw identifiers).

## Typed encoders (Rust)

`Writer` is untyped: nothing stops a wrong field number, a doubled field or an oversized
value. The generator also emits a typed encoder per message, `FooEnc`:

```rust
let n = ManifestEnc::encode(&mut buf, |m| {
    m.body(&body_bytes)?.signature(&sig)?;     // one setter per field, with the schema's types
    Ok(())
})?;                                           // Result<usize, EncodeError>
```

- Setters exist for exactly the schema's fields; a nested message is a closure over its own
  encoder (`m.inner(|i| { i.a(3)?; Ok(()) })`, length backpatched in place); a repeated scalar
  is one packed slice, other repeated fields are `add_<name>`, one element per call.
- A singular field set twice is `EncodeError::Duplicate`, two arms of a oneof
  `OneofConflict`, a full buffer `Full`. proto3 defaults are not written (as protoc does);
  `optional` fields and oneof arms are.
- Limits the schema cannot say come from a sidecar file passed with `--bounds`
  (`pbview_library(bounds = ...)`): lines of `pkg.Message.field max_len N` and
  `... max_count N`, and (unsigned fields) `... max_value N`. The setters enforce them (`TooLong`,
  `TooMany`, `TooLarge`), and when every field of a
  message is bounded it gets `FooEnc::MAX_ENCODED_LEN`, an exact worst case (every varint at full
  width, or at its `max_value`): `const _: () = assert!(FooEnc::MAX_ENCODED_LEN + 10 <= 125);` is a frame budget
  checked at build time. Recursive or unbounded messages have no such constant.
- Set fields in field-number order to get protoc's canonical output (the tests compare the
  builder's bytes with protoc's for every field kind).

## C++

The same views for C++17 firmware: a header-only runtime (`cpp/pbview.h`, namespace `pbv`: no
exceptions, RTTI or heap; compiles with `-fno-exceptions -fno-rtti`) and the generator's
`--lang cpp`:

```starlark
load("@pbview//:defs.bzl", "pbview_cc_library")

pbview_cc_library(name = "my_views", fdset = ":my_proto_fdset")   # my_views.h
```

```cpp
my::ClientMessageView msg;
if (!my::ClientMessageView::Parse({rx, len}, &msg)) return;   // validates once
if (msg.msg_case() == my::ClientMessageView::kFieldHello) {
  auto name = msg.hello()->client();                          // std::string_view into rx
}
for (auto it = msg.leds(); it.Next(&led);) use(led.id());     // iterates the frame in place
uint8_t out[128];
pbv::Writer w(out, sizeof out);                               // in-place encoding
w.Message(2, [&](pbv::Writer& m) { return m.Int32(1, 7) && m.String(2, "hi"); });
```

Package `a.b` becomes `namespace a { namespace b {`; message `Foo` is `FooView`, nested
`Foo.Bar` is the flat `Foo_BarView`; enums are `enum class Foo_Color : int32_t` (fields read as
raw `int32_t`: proto3 enums are open). A oneof `kind` gets `kind_case()`, the field number of
the arm set last on the wire. `parse`, `validate`, `bytes` and `fields` are reserved accessor
names (a field with that name gets a trailing `_`). The Rust output's typed single-pass
`fields()` iterator is not generated for C++; `fields()` is the raw `pbv::Reader`.

## Tests

- `runtime_test`: varints, zigzag, every scalar kind, last-wins, packed/unpacked
  mixes, the writer's length backpatching across varint size boundaries, and
  20k random buffers through every reader.
- `cpp_conformance_test`: the same suite against the C++ output (built `-Werror
  -fno-exceptions -fno-rtti`), plus varint boundaries and `Writer` round trips through
  views.
- `encoder_test`: the typed encoders against protoc's bytes, the guardrails, and a tight
  `MAX_ENCODED_LEN`.
- `conformance_test`: views over **protoc's own encoding** of a message
  covering every field kind (all 15 scalar types, optional, repeated
  packed/unpacked, strings, bytes, nested and recursive messages, maps, a
  oneof, enums, keyword field names); every truncation of it; mistyped and
  non-UTF-8 fields; the nesting limit; 5k mutated encodings through every
  accessor, parsed and unchecked.

## Status

Extracted from the splanc firmware (where it was `oss/pbview`). Licensed under
MIT OR Apache-2.0. MSRV 1.80. Not yet on crates.io.
