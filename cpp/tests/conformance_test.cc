// C++ views against protoc's own encoding of testdata/all.textproto (every field kind),
// malformed-input robustness, and the Writer. Mirrors tests/conformance.rs.
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

#include "conformance_views.h"
#include "pbview.h"

namespace {

int g_failures = 0;
#define CHECK(c)                                                       \
  do {                                                                 \
    if (!(c)) {                                                        \
      std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #c); \
      g_failures++;                                                    \
    }                                                                  \
  } while (0)

using pbv::Bytes;
using pbv::Error;
namespace t = pbview::test;

std::vector<uint8_t> Golden() {
  std::ifstream f(std::getenv("ALL_BIN"), std::ios::binary);
  return std::vector<uint8_t>(std::istreambuf_iterator<char>(f), {});
}

Bytes B(const std::vector<uint8_t>& v) { return Bytes(v.data(), v.size()); }

template <class It, class T>
std::vector<T> Collect(It it) {
  std::vector<T> out;
  T v;
  while (it.Next(&v)) out.push_back(v);
  return out;
}

void EveryFieldKindDecodes() {
  auto buf = Golden();
  CHECK(!buf.empty());
  t::AllView m;
  CHECK(t::AllView::Parse(B(buf), &m).ok());
  CHECK(m.f_double() == -1.5);
  CHECK(m.f_float() == 2.25f);
  CHECK(m.f_int64() == -9000000000LL);
  CHECK(m.f_uint64() == UINT64_MAX);
  CHECK(m.f_int32() == -7);
  CHECK(m.f_fixed64() == 1234567890123ULL);
  CHECK(m.f_fixed32() == 4000000000u);
  CHECK(m.f_bool());
  CHECK(m.f_string() == "h\xc3\xa9llo");
  auto fb = m.f_bytes();
  CHECK(fb.size == 3 && fb.data[0] == 0 && fb.data[1] == 1 && fb.data[2] == 0xff);
  CHECK(m.f_uint32() == 300);
  CHECK(m.f_color() == static_cast<int32_t>(t::Color::kGreen));
  CHECK(m.f_sfixed32() == -2);
  CHECK(m.f_sfixed64() == -3);
  CHECK(m.f_sint32() == -64);
  CHECK(m.f_sint64() == -1000000000000LL);

  auto inner = m.f_inner();
  CHECK(inner && inner->a() == 3 && inner->label() == "in");
  CHECK(inner->child() && inner->child()->a() == 4);
  CHECK(!inner->child()->child());

  // proto3 `optional`: presence is visible even for a default value.
  CHECK(m.opt_int32() && *m.opt_int32() == 0);
  CHECK(!m.opt_string());

  CHECK((Collect<decltype(m.r_packed()), int32_t>(m.r_packed()) == std::vector<int32_t>{1, -2, 300}));
  CHECK((Collect<decltype(m.r_sint64()), int64_t>(m.r_sint64()) == std::vector<int64_t>{-1, 1}));
  CHECK((Collect<decltype(m.r_double()), double>(m.r_double()) == std::vector<double>{0.5, -0.25}));
  CHECK((Collect<decltype(m.r_string()), std::string_view>(m.r_string()) ==
         std::vector<std::string_view>{"a", "", "ccc"}));
  {
    auto it = m.r_inner();
    t::InnerView i;
    CHECK(it.Next(&i) && i.a() == 1 && i.label() == "");
    CHECK(it.Next(&i) && i.a() == 2 && i.label() == "two");
    CHECK(!it.Next(&i));
  }
  {
    auto it = m.r_bytes();
    Bytes b;
    CHECK(it.Next(&b) && b.size == 1 && b.data[0] == 'x');
    CHECK(!it.Next(&b));
  }
  CHECK((Collect<decltype(m.r_color()), int32_t>(m.r_color()) == std::vector<int32_t>{1, 2}));

  // Maps are repeated entry views.
  {
    auto it = m.counts();
    t::All_CountsEntryView e;
    CHECK(it.Next(&e) && e.key() == "k1" && e.value() == 1);
    CHECK(it.Next(&e) && e.key() == "k2" && e.value() == 2);
    CHECK(!it.Next(&e));
  }

  // The oneof: the arm set last on the wire, and the members' own accessors.
  CHECK(m.choice_case() == t::AllView::kFieldCText);
  CHECK(m.c_text() && *m.c_text() == "picked");
  CHECK(!m.c_inner());

  CHECK(m.type() == 9);
  CHECK(m.match() == "m");
  CHECK(m.mode() == static_cast<int32_t>(t::All_Mode::kOn));
  CHECK(m.nested() && m.nested()->x() == 77);
}

void AbsentFieldsReadAsDefaults() {
  t::AllView e;
  CHECK(t::AllView::Parse(Bytes(), &e).ok());
  CHECK(e.f_int32() == 0 && e.f_string() == "" && !e.f_bool());
  CHECK(!e.f_inner() && e.choice_case() == 0 && !e.opt_int32());
  auto it = e.r_packed();
  int32_t v;
  CHECK(!it.Next(&v));
  t::EmptyView em;
  CHECK(t::EmptyView::Parse(Bytes(), &em).ok());
}

void ParseRejectsMalformedAndMistyped() {
  auto buf = Golden();
  t::AllView m;
  for (size_t n = 0; n < buf.size(); n++) (void)t::AllView::Parse(Bytes(buf.data(), n), &m);
  CHECK(!t::AllView::Parse(Bytes(buf.data(), buf.size() - 1), &m).ok());
  // f_int32 (5) sent as a string: wire mismatch.
  const uint8_t mism[] = {0x2a, 0x00};
  auto s = t::AllView::Parse(Bytes(mism, 2), &m);
  CHECK(s.error == Error::kWireMismatch && s.field == 5);
  // f_string (9) not UTF-8.
  const uint8_t bad[] = {0x4a, 0x01, 0xff};
  s = t::AllView::Parse(Bytes(bad, 3), &m);
  CHECK(s.error == Error::kBadUtf8 && s.field == 9);
  // Overlong and surrogate UTF-8 are rejected too.
  const uint8_t over[] = {0x4a, 0x02, 0xc0, 0x80};
  CHECK(t::AllView::Parse(Bytes(over, 4), &m).error == Error::kBadUtf8);
  const uint8_t surr[] = {0x4a, 0x03, 0xed, 0xa0, 0x80};
  CHECK(t::AllView::Parse(Bytes(surr, 5), &m).error == Error::kBadUtf8);
  // Nested message garbage is caught by the recursive validation.
  const uint8_t nested[] = {0x5a, 0x02, 0x08, 0x80};
  CHECK(!t::AllView::Parse(Bytes(nested, 4), &m).ok());
}

void NestingDepthIsBounded() {
  // Inner.child nested 40 deep: past kDefaultMaxDepth (32).
  std::vector<uint8_t> msg;
  for (int i = 0; i < 40; i++) {
    std::vector<uint8_t> outer = {0x1a};
    uint64_t n = msg.size();
    do {
      uint8_t b = n & 0x7f;
      n >>= 7;
      outer.push_back(n ? (b | 0x80) : b);
    } while (n);
    outer.insert(outer.end(), msg.begin(), msg.end());
    msg = outer;
  }
  t::InnerView v;
  CHECK(t::InnerView::Parse(B(msg), &v).error == Error::kTooDeep);
  CHECK(t::InnerView::Validate(B(msg), 64).ok());
}

void MutatedGoldensNeverMisbehave() {
  auto buf = Golden();
  uint64_t x = 0x25454f6cdd1dULL;
  for (int round = 0; round < 5000; round++) {
    auto m = buf;
    for (int k = 0; k < 3; k++) {
      x ^= x << 13;
      x ^= x >> 7;
      x ^= x << 17;
      m[x % m.size()] = static_cast<uint8_t>(x >> 32);
    }
    t::AllView v;
    auto touch = [](const t::AllView& a) {
      (void)a.f_double();
      (void)a.f_string();
      (void)a.choice_case();
      auto in = a.f_inner();
      if (in) (void)in->label();
      auto ri = a.r_inner();
      t::InnerView i;
      while (ri.Next(&i)) (void)i.child();
      auto ce = a.counts();
      t::All_CountsEntryView e;
      while (ce.Next(&e)) (void)e.key();
      auto rp = a.r_packed();
      int32_t p;
      while (rp.Next(&p)) {}
      auto rs = a.r_string();
      std::string_view sv;
      while (rs.Next(&sv)) {}
    };
    if (t::AllView::Parse(B(m), &v).ok()) touch(v);
    touch(t::AllView::FromBytesUnchecked(B(m)));  // unchecked views must be just as safe
  }
}

void ViewsAreTwoWords() { CHECK(sizeof(t::AllView) == 2 * sizeof(size_t)); }

void VarintsAndZigzag() {
  // Boundaries of every varint length, through a Writer and back.
  uint8_t buf[64];
  const uint64_t vals[] = {0, 1, 127, 128, 16383, 16384, (1ull << 32) - 1, 1ull << 32, UINT64_MAX};
  for (uint64_t v : vals) {
    pbv::Writer w(buf, sizeof buf);
    CHECK(w.Varint(v));
    CHECK(w.size() == pbv::VarintLen(v));
    size_t pos = 0;
    uint64_t out = 0;
    CHECK(pbv::ReadVarint(w.bytes(), &pos, &out) == Error::kOk && out == v && pos == w.size());
  }
  // Overlong: 11 continuation bytes, and a 10th byte above 1.
  const uint8_t over[11] = {0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01};
  size_t pos = 0;
  uint64_t out;
  CHECK(pbv::ReadVarint(Bytes(over, 11), &pos, &out) == Error::kOverlong);
  const uint8_t top[10] = {0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02};
  pos = 0;
  CHECK(pbv::ReadVarint(Bytes(top, 10), &pos, &out) == Error::kOverlong);
  // Truncated.
  const uint8_t tr[1] = {0x80};
  pos = 0;
  CHECK(pbv::ReadVarint(Bytes(tr, 1), &pos, &out) == Error::kTruncated);
}

void WriterRoundTripsThroughViews() {
  // Encode All { f_int32: -7, f_string: "héllo", f_inner { a: 3, label: <200 bytes> } ,
  // r_packed unpacked } and read it back: the nested body > 127 bytes exercises the
  // length backpatch shift.
  std::vector<uint8_t> out(1024);
  pbv::Writer w(out.data(), out.size());
  std::string big(200, 'z');
  CHECK(w.Int32(5, -7));
  CHECK(w.String(9, "h\xc3\xa9llo"));
  CHECK(w.Message(11, [&](pbv::Writer& in) {
    return in.Int32(1, 3) && in.String(2, big) && in.Message(3, [](pbv::Writer& c) { return c.Int32(1, 4); });
  }));
  CHECK(w.Int32(30, 1) && w.Int32(30, -2) && w.Sint64(31, -5) && w.Sfixed64(16, -3) && w.Double(1, 8.5));
  t::AllView m;
  auto s = t::AllView::Parse(w.bytes(), &m);
  CHECK(s.ok());
  CHECK(m.f_int32() == -7 && m.f_string() == "h\xc3\xa9llo" && m.f_sfixed64() == -3 && m.f_double() == 8.5);
  auto inner = m.f_inner();
  CHECK(inner && inner->a() == 3 && inner->label() == big && inner->child() && inner->child()->a() == 4);
  CHECK((Collect<decltype(m.r_packed()), int32_t>(m.r_packed()) == std::vector<int32_t>{1, -2}));
  CHECK((Collect<decltype(m.r_sint64()), int64_t>(m.r_sint64()) == std::vector<int64_t>{-5}));
  // A full buffer reports failure instead of overrunning.
  uint8_t small[4];
  pbv::Writer ws(small, sizeof small);
  CHECK(!ws.String(9, "too long for four bytes"));
}

}  // namespace

int main() {
  EveryFieldKindDecodes();
  AbsentFieldsReadAsDefaults();
  ParseRejectsMalformedAndMistyped();
  NestingDepthIsBounded();
  MutatedGoldensNeverMisbehave();
  ViewsAreTwoWords();
  VarintsAndZigzag();
  WriterRoundTripsThroughViews();
  if (g_failures) {
    std::fprintf(stderr, "%d check(s) failed\n", g_failures);
    return 1;
  }
  std::puts("ok");
  return 0;
}
