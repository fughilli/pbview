// The C++ typed encoders (FooEnc): protoc-identical output and the guardrails. Mirrors
// tests/encoder.rs.
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <vector>

#include "conformance_views.h"
#include "pbview.h"

namespace {

int g_failures = 0;
#define CHECK(c)                                                                  \
  do {                                                                            \
    if (!(c)) {                                                                   \
      std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #c); \
      g_failures++;                                                               \
    }                                                                             \
  } while (0)

namespace t = pbview::test;
using pbv::EncodeError;

std::vector<uint8_t> Golden() {
  std::ifstream f(std::getenv("ALL_BIN"), std::ios::binary);
  return std::vector<uint8_t>(std::istreambuf_iterator<char>(f), {});
}

void ReproducesProtocBytes() {
  uint8_t buf[512];
  const uint8_t bytes[] = {0, 1, 0xff};
  const int32_t packed[] = {1, -2, 300};
  const int64_t sint[] = {-1, 1};
  const double dbl[] = {0.5, -0.25};
  const int32_t colors[] = {1, 2};
  auto r = t::AllEnc::Encode(buf, sizeof buf, [&](t::AllEnc& m) {
    m.f_double(-1.5)
        .f_float(2.25f)
        .f_int64(-9000000000LL)
        .f_uint64(UINT64_MAX)
        .f_int32(-7)
        .f_fixed64(1234567890123ULL)
        .f_fixed32(4000000000u)
        .f_bool(true)
        .f_string("h\xc3\xa9llo")
        .f_inner([](t::InnerEnc& i) {
          i.a(3).label("in").child([](t::InnerEnc& c) { c.a(4); });
        })
        .f_bytes(pbv::Bytes(bytes, 3))
        .f_uint32(300)
        .f_color(2)
        .f_sfixed32(-2)
        .f_sfixed64(-3)
        .f_sint32(-64)
        .f_sint64(-1000000000000LL)
        .opt_int32(0)
        .r_packed(packed, 3)
        .r_sint64(sint, 2)
        .r_double(dbl, 2)
        .add_r_string("a")
        .add_r_string("")
        .add_r_string("ccc")
        .add_r_inner([](t::InnerEnc& i) { i.a(1); })
        .add_r_inner([](t::InnerEnc& i) { i.a(2).label("two"); })
        .add_r_bytes(pbv::Bytes(reinterpret_cast<const uint8_t*>("x"), 1))
        .r_color(colors, 2)
        .add_counts([](t::All_CountsEntryEnc& e) { e.key("k1").value(1); })
        .add_counts([](t::All_CountsEntryEnc& e) { e.key("k2").value(2); })
        .c_text("picked")
        .type(9)
        .match("m")
        .mode(1)
        .nested([](t::All_NestedEnc& x) { x.x(77); });
  });
  CHECK(r.ok());
  auto golden = Golden();
  CHECK(r.size == golden.size());
  CHECK(std::vector<uint8_t>(buf, buf + r.size) == golden);
  t::AllView v;
  CHECK(t::AllView::Parse(pbv::Bytes(buf, r.size), &v).ok());
}

void DefaultsAndPresence() {
  uint8_t buf[64];
  auto r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) {
    m.id(0).payload(pbv::Bytes()).delta(0).stamp(0);
  });
  CHECK(r.ok() && r.size == 0);
  r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) { m.maybe(0).num(0); });
  t::PacketView v;
  CHECK(r.ok() && t::PacketView::Parse(pbv::Bytes(buf, r.size), &v).ok());
  CHECK(v.maybe() && *v.maybe() == 0);
  CHECK(v.num() && *v.num() == 0);
  CHECK(v.body_case() == t::PacketView::kFieldNum);
}

void DuplicateAndOneofConflicts() {
  uint8_t buf[64];
  auto r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) { m.id(1).id(2); });
  CHECK(r.error == EncodeError::kDuplicate && r.field == 1);
  r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) { m.text("a").num(3); });
  CHECK(r.error == EncodeError::kOneofConflict && r.field == 7);
  const uint32_t one[] = {1};
  r = t::PacketEnc::Encode(buf, sizeof buf, [&](t::PacketEnc& m) { m.hops(one, 1).hops(one, 1); });
  CHECK(r.error == EncodeError::kDuplicate && r.field == 3);
}

void LimitsAreEnforced() {
  uint8_t buf[256];
  uint8_t pl[21] = {};
  auto r = t::PacketEnc::Encode(buf, sizeof buf, [&](t::PacketEnc& m) { m.payload(pbv::Bytes(pl, 21)); });
  CHECK(r.error == EncodeError::kTooLong && r.field == 2);
  r = t::PacketEnc::Encode(buf, sizeof buf, [&](t::PacketEnc& m) { m.payload(pbv::Bytes(pl, 20)); });
  CHECK(r.ok());
  const uint32_t h5[] = {1, 2, 3, 4, 5};
  r = t::PacketEnc::Encode(buf, sizeof buf, [&](t::PacketEnc& m) { m.hops(h5, 5); });
  CHECK(r.error == EncodeError::kTooMany && r.field == 3);
  r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) {
    for (int i = 0; i < 4; i++) m.add_tags([](t::TagEnc& t) { t.name("n"); });
  });
  CHECK(r.error == EncodeError::kTooMany && r.field == 4);
  r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) {
    m.add_tags([](t::TagEnc& t) { t.name("123456789"); });
  });
  CHECK(r.error == EncodeError::kTooLong && r.field == 1);  // limits apply inside nested messages
  r = t::PacketEnc::Encode(buf, sizeof buf, [](t::PacketEnc& m) { m.id(65536); });
  CHECK(r.error == EncodeError::kTooLarge && r.field == 1);
}

void BufferFullIsAnError() {
  uint8_t small[8];
  uint8_t pl[20] = {};
  auto r = t::PacketEnc::Encode(small, sizeof small, [&](t::PacketEnc& m) { m.payload(pbv::Bytes(pl, 20)); });
  CHECK(r.error == EncodeError::kFull);
  uint8_t tiny[3];
  r = t::TagEnc::Encode(tiny, sizeof tiny, [](t::TagEnc& t) { t.name("abcdef"); });
  CHECK(r.error == EncodeError::kFull);
}

void MaxEncodedLenIsTight() {
  // 4 + 22 + 22 + 63 + 21 + 6 + 6 + 5, from testdata/bounds.txt.
  static_assert(t::PacketEnc::kMaxEncodedLen == 149, "worst-case size");
  uint8_t buf[256];
  uint8_t pl[20];
  for (auto& c : pl) c = 0xaa;
  const uint32_t hops[] = {UINT32_MAX, UINT32_MAX, UINT32_MAX, UINT32_MAX};
  auto r = t::PacketEnc::Encode(buf, sizeof buf, [&](t::PacketEnc& m) {
    m.id(65535).payload(pbv::Bytes(pl, 20)).hops(hops, 4);
    for (int i = 0; i < 3; i++)
      m.add_tags([](t::TagEnc& t) { t.name("12345678").value(INT64_MIN); });
    m.tag([](t::TagEnc& t) { t.name("12345678").value(INT64_MIN); })
        .maybe(UINT32_MAX)
        .delta(INT32_MIN)
        .stamp(UINT32_MAX);
  });
  CHECK(r.ok());
  CHECK(r.size == t::PacketEnc::kMaxEncodedLen);  // the worst case is reached: the bound is exact
  t::PacketView v;
  CHECK(t::PacketView::Parse(pbv::Bytes(buf, r.size), &v).ok());
  CHECK(v.id() == 65535);
  auto tags = v.tags();
  t::TagView tg;
  int n = 0;
  while (tags.Next(&tg)) n++;
  CHECK(n == 3);
}

}  // namespace

int main() {
  ReproducesProtocBytes();
  DefaultsAndPresence();
  DuplicateAndOneofConflicts();
  LimitsAreEnforced();
  BufferFullIsAnError();
  MaxEncodedLenIsTight();
  if (g_failures) {
    std::fprintf(stderr, "%d check(s) failed\n", g_failures);
    return 1;
  }
  std::puts("ok");
  return 0;
}
