// pbview (C++): zero-copy protobuf views, read straight from the wire bytes.
//
// The C++ counterpart of the Rust `pbview` runtime. A view is a borrowed
// (pointer, length) pair plus generated accessors; nothing is decoded into a
// struct, copied or allocated. Header-only, C++17, no exceptions, no RTTI, no
// heap, no <iostream>; every reader is bounds-checked and never aborts on any
// input.
//
//   WireStatus st = ClientMessageView::Parse({buf, len}, &view);  // validates once
//   if (!st.ok()) ...
//   auto name = view.name();             // std::string_view into buf
//   for (auto it = view.leds(); it.Next(&led);) ...   // repeated, in place
//
// Semantics match the Rust runtime: last occurrence wins for singular fields,
// packed and unpacked repeated encodings (and mixtures) both accepted, unknown
// fields skipped, proto3 enums open (raw int32), groups rejected. A singular
// *message* field that occurs more than once is not merged (last wins).
#ifndef PBVIEW_PBVIEW_H_
#define PBVIEW_PBVIEW_H_

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>
#include <string_view>

namespace pbv {

enum class Error : uint8_t {
  kOk = 0,
  kTruncated,      // the buffer ends inside a field
  kOverlong,       // a varint longer than 10 bytes, or one overflowing 64 bits
  kBadTag,         // field number 0, or a group / reserved wire type
  kWireMismatch,   // a known field arrived with a wire type its type can't take
  kBadUtf8,        // a `string` field that is not UTF-8
  kTooDeep,        // nested messages deeper than the parse limit
  kBadPacked,      // a packed run that is not a whole number of values
};

// An error and, where known, the field number it concerns.
struct WireStatus {
  Error error = Error::kOk;
  uint32_t field = 0;
  constexpr bool ok() const { return error == Error::kOk; }
  constexpr explicit operator bool() const { return ok(); }
};

enum class Wire : uint8_t { kVarint = 0, kI64 = 1, kLen = 2, kI32 = 5 };

// A borrowed byte range.
struct Bytes {
  const uint8_t* data = nullptr;
  size_t size = 0;
  constexpr Bytes() = default;
  constexpr Bytes(const uint8_t* d, size_t n) : data(d), size(n) {}
};

// One field's raw wire value. `len` borrows the input.
struct Value {
  Wire wire = Wire::kVarint;
  uint64_t num = 0;  // varint, or the raw 32/64-bit fixed value
  Bytes len;         // kLen
};

struct Field {
  uint32_t number = 0;
  Value value;
};

constexpr uint32_t kDefaultMaxDepth = 32;

// Read a varint at *pos, advancing it.
inline Error ReadVarint(Bytes b, size_t* pos, uint64_t* out) {
  uint64_t v = 0;
  for (int i = 0; i < 10; i++) {
    if (*pos >= b.size) return Error::kTruncated;
    uint8_t c = b.data[(*pos)++];
    if (i == 9 && c > 1) return Error::kOverlong;  // 10th byte: only the top bit
    v |= static_cast<uint64_t>(c & 0x7f) << (7 * i);
    if (!(c & 0x80)) {
      *out = v;
      return Error::kOk;
    }
  }
  return Error::kOverlong;
}

inline Error Take(Bytes b, size_t* pos, uint64_t n, Bytes* out) {
  if (n > b.size - *pos) return Error::kTruncated;  // *pos <= b.size always
  *out = Bytes(b.data + *pos, static_cast<size_t>(n));
  *pos += static_cast<size_t>(n);
  return Error::kOk;
}

template <class T>
inline T LoadLe(const uint8_t* p) {
  T v = 0;
  for (size_t i = 0; i < sizeof(T); i++) v |= static_cast<T>(p[i]) << (8 * i);
  return v;
}

// Bounded field iterator over an encoded message. Next() returns false at the
// end or at the first malformed field (then error() says which).
class Reader {
 public:
  constexpr Reader() = default;
  explicit constexpr Reader(Bytes b) : buf_(b) {}

  bool Next(Field* f) {
    if (done_ || pos_ >= buf_.size) return false;
    Error e = Read(f);
    if (e != Error::kOk) {
      err_ = e;
      done_ = true;
      return false;
    }
    return true;
  }
  Error error() const { return err_; }

 private:
  Error Read(Field* f) {
    uint64_t tag;
    Error e = ReadVarint(buf_, &pos_, &tag);
    if (e != Error::kOk) return e;
    uint64_t number = tag >> 3;
    if (number == 0 || number > 0xffffffffu) return Error::kBadTag;
    f->number = static_cast<uint32_t>(number);
    Bytes b;
    switch (tag & 7) {
      case 0:
        f->value.wire = Wire::kVarint;
        return ReadVarint(buf_, &pos_, &f->value.num);
      case 1:
        f->value.wire = Wire::kI64;
        e = Take(buf_, &pos_, 8, &b);
        if (e == Error::kOk) f->value.num = LoadLe<uint64_t>(b.data);
        return e;
      case 2: {
        f->value.wire = Wire::kLen;
        uint64_t n;
        e = ReadVarint(buf_, &pos_, &n);
        if (e != Error::kOk) return e;
        return Take(buf_, &pos_, n, &f->value.len);
      }
      case 5:
        f->value.wire = Wire::kI32;
        e = Take(buf_, &pos_, 4, &b);
        if (e == Error::kOk) f->value.num = LoadLe<uint32_t>(b.data);
        return e;
      default:
        return Error::kBadTag;
    }
  }

  Bytes buf_;
  size_t pos_ = 0;
  bool done_ = false;
  Error err_ = Error::kOk;
};

// --- scalar kinds -----------------------------------------------------------
//
// One marker type per protobuf scalar type: FromValue maps a wire value to the
// C++ value (false if the wire type is wrong); FromPacked decodes one element
// of a packed run at *pos.

namespace kind {

template <class D, class O>
struct VarintKind {
  using Out = O;
  static constexpr Wire kWire = Wire::kVarint;
  static constexpr bool kPackable = true;
  static bool FromValue(const Value& v, Out* o) {
    if (v.wire != Wire::kVarint) return false;
    *o = D::Conv(v.num);
    return true;
  }
  static Error FromPacked(Bytes b, size_t* pos, Out* o) {
    uint64_t v;
    Error e = ReadVarint(b, pos, &v);
    if (e == Error::kOk) *o = D::Conv(v);
    return e;
  }
};

template <class D, class O, class Raw, Wire W>
struct FixedKind {
  using Out = O;
  static constexpr Wire kWire = W;
  static constexpr bool kPackable = true;
  static bool FromValue(const Value& v, Out* o) {
    if (v.wire != W) return false;
    *o = D::Conv(static_cast<Raw>(v.num));
    return true;
  }
  static Error FromPacked(Bytes b, size_t* pos, Out* o) {
    Bytes t;
    Error e = Take(b, pos, sizeof(Raw), &t);
    if (e == Error::kOk) *o = D::Conv(LoadLe<Raw>(t.data));
    return e;
  }
};

// int32/int64 are sign-extended to 64 bits on the wire; truncation is the
// protobuf-specified narrowing.
struct Int32 : VarintKind<Int32, int32_t> {
  static int32_t Conv(uint64_t v) { return static_cast<int32_t>(static_cast<uint32_t>(v)); }
};
struct Int64 : VarintKind<Int64, int64_t> {
  static int64_t Conv(uint64_t v) { return static_cast<int64_t>(v); }
};
struct Uint32 : VarintKind<Uint32, uint32_t> {
  static uint32_t Conv(uint64_t v) { return static_cast<uint32_t>(v); }
};
struct Uint64 : VarintKind<Uint64, uint64_t> {
  static uint64_t Conv(uint64_t v) { return v; }
};
struct Sint32 : VarintKind<Sint32, int32_t> {
  static int32_t Conv(uint64_t v) {
    uint32_t u = static_cast<uint32_t>(v);
    return static_cast<int32_t>((u >> 1) ^ (~(u & 1) + 1));
  }
};
struct Sint64 : VarintKind<Sint64, int64_t> {
  static int64_t Conv(uint64_t v) { return static_cast<int64_t>((v >> 1) ^ (~(v & 1) + 1)); }
};
struct Bool : VarintKind<Bool, bool> {
  static bool Conv(uint64_t v) { return v != 0; }
};
// Enum: an enum field's raw value (proto3 enums are open: unknown values are kept).
struct Enum : VarintKind<Enum, int32_t> {
  static int32_t Conv(uint64_t v) { return static_cast<int32_t>(static_cast<uint32_t>(v)); }
};
struct Fixed32 : FixedKind<Fixed32, uint32_t, uint32_t, Wire::kI32> {
  static uint32_t Conv(uint32_t v) { return v; }
};
struct Sfixed32 : FixedKind<Sfixed32, int32_t, uint32_t, Wire::kI32> {
  static int32_t Conv(uint32_t v) { return static_cast<int32_t>(v); }
};
struct Float : FixedKind<Float, float, uint32_t, Wire::kI32> {
  static float Conv(uint32_t v) {
    float f;
    std::memcpy(&f, &v, sizeof f);
    return f;
  }
};
struct Fixed64 : FixedKind<Fixed64, uint64_t, uint64_t, Wire::kI64> {
  static uint64_t Conv(uint64_t v) { return v; }
};
struct Sfixed64 : FixedKind<Sfixed64, int64_t, uint64_t, Wire::kI64> {
  static int64_t Conv(uint64_t v) { return static_cast<int64_t>(v); }
};
struct Double : FixedKind<Double, double, uint64_t, Wire::kI64> {
  static double Conv(uint64_t v) {
    double d;
    std::memcpy(&d, &v, sizeof d);
    return d;
  }
};

}  // namespace kind

// --- accessor helpers --------------------------------------------------------

// The last occurrence of singular scalar field `num` (last wins). A packed run
// under a singular field contributes its last element.
template <class K>
std::optional<typename K::Out> LastScalar(Bytes buf, uint32_t num) {
  std::optional<typename K::Out> out;
  Reader r(buf);
  Field f;
  while (r.Next(&f)) {
    if (f.number != num) continue;
    typename K::Out v;
    if (K::FromValue(f.value, &v)) {
      out = v;
    } else if (f.value.wire == Wire::kLen && K::kPackable) {
      size_t pos = 0;
      while (pos < f.value.len.size) {
        if (K::FromPacked(f.value.len, &pos, &v) != Error::kOk) break;
        out = v;
      }
    }
  }
  return out;
}

// The last length-delimited occurrence of field `num`.
inline std::optional<Bytes> LastLen(Bytes buf, uint32_t num) {
  std::optional<Bytes> out;
  Reader r(buf);
  Field f;
  while (r.Next(&f))
    if (f.number == num && f.value.wire == Wire::kLen) out = f.value.len;
  return out;
}

inline bool HasField(Bytes buf, uint32_t num) {
  Reader r(buf);
  Field f;
  while (r.Next(&f))
    if (f.number == num) return true;
  return false;
}

inline std::string_view AsStr(Bytes b) {
  return std::string_view(reinterpret_cast<const char*>(b.data), b.size);
}

// Strict UTF-8 (no overlongs, surrogates, or code points above U+10FFFF).
inline bool Utf8Valid(Bytes b) {
  size_t i = 0;
  const uint8_t* p = b.data;
  while (i < b.size) {
    uint8_t c = p[i];
    if (c < 0x80) {
      i++;
      continue;
    }
    size_t n;
    uint32_t cp, min;
    if ((c & 0xe0) == 0xc0) { n = 2; cp = c & 0x1f; min = 0x80; }
    else if ((c & 0xf0) == 0xe0) { n = 3; cp = c & 0x0f; min = 0x800; }
    else if ((c & 0xf8) == 0xf0) { n = 4; cp = c & 0x07; min = 0x10000; }
    else return false;
    if (b.size - i < n) return false;
    for (size_t k = 1; k < n; k++) {
      if ((p[i + k] & 0xc0) != 0x80) return false;
      cp = (cp << 6) | (p[i + k] & 0x3f);
    }
    if (cp < min || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) return false;
    i += n;
  }
  return true;
}

// The string field's bytes, or "" for one that is not UTF-8 (an unchecked view).
inline std::string_view StrOf(Bytes b) { return Utf8Valid(b) ? AsStr(b) : std::string_view(); }

// Every length-delimited occurrence of field `num`, in order.
class RepeatedLen {
 public:
  RepeatedLen(Bytes buf, uint32_t num) : r_(buf), num_(num) {}
  bool Next(Bytes* out) {
    Field f;
    while (r_.Next(&f)) {
      if (f.number == num_ && f.value.wire == Wire::kLen) {
        *out = f.value.len;
        return true;
      }
    }
    return false;
  }

 private:
  Reader r_;
  uint32_t num_;
};

// Every occurrence of repeated `string` field `num` (non-UTF-8 entries, which
// Parse rejects, are skipped on an unchecked view).
class RepeatedStr {
 public:
  RepeatedStr(Bytes buf, uint32_t num) : in_(buf, num) {}
  bool Next(std::string_view* out) {
    Bytes b;
    while (in_.Next(&b)) {
      if (Utf8Valid(b)) {
        *out = AsStr(b);
        return true;
      }
    }
    return false;
  }

 private:
  RepeatedLen in_;
};

// Every occurrence of repeated message field `num`, as views of type V
// (V::FromBytesUnchecked).
template <class V>
class RepeatedMsg {
 public:
  RepeatedMsg(Bytes buf, uint32_t num) : in_(buf, num) {}
  bool Next(V* out) {
    Bytes b;
    if (!in_.Next(&b)) return false;
    *out = V::FromBytesUnchecked(b);
    return true;
  }

 private:
  RepeatedLen in_;
};

// Every element of repeated scalar field `num`, packed or unpacked (or a mix),
// in wire order.
template <class K>
class RepeatedScalar {
 public:
  RepeatedScalar(Bytes buf, uint32_t num) : r_(buf), num_(num) {}
  bool Next(typename K::Out* out) {
    for (;;) {
      if (run_pos_ < run_.size) {
        if (K::FromPacked(run_, &run_pos_, out) == Error::kOk) return true;
        run_pos_ = run_.size;
        continue;
      }
      Field f;
      if (!r_.Next(&f)) return false;
      if (f.number != num_) continue;
      if (K::FromValue(f.value, out)) return true;
      if (f.value.wire == Wire::kLen && K::kPackable) {
        run_ = f.value.len;
        run_pos_ = 0;
      }
    }
  }

 private:
  Reader r_;
  uint32_t num_;
  Bytes run_;
  size_t run_pos_ = 0;
};

// --- validation (used by generated Validate) ---------------------------------

// A known scalar field's occurrence: its wire type, or (when `repeated`) a
// well-formed packed run.
template <class K>
WireStatus CheckScalar(const Field& f, bool repeated) {
  if (f.value.wire == K::kWire) return {};
  if (f.value.wire == Wire::kLen && repeated && K::kPackable) {
    size_t pos = 0;
    typename K::Out v;
    while (pos < f.value.len.size)
      if (K::FromPacked(f.value.len, &pos, &v) != Error::kOk) return {Error::kBadPacked, f.number};
    return {};
  }
  return {Error::kWireMismatch, f.number};
}

inline WireStatus CheckBytes(const Field& f) {
  return f.value.wire == Wire::kLen ? WireStatus{} : WireStatus{Error::kWireMismatch, f.number};
}

inline WireStatus CheckStr(const Field& f) {
  WireStatus s = CheckBytes(f);
  if (!s.ok()) return s;
  return Utf8Valid(f.value.len) ? WireStatus{} : WireStatus{Error::kBadUtf8, f.number};
}

// A known message field's occurrence, recursing with depth - 1.
template <class V>
WireStatus CheckMsg(const Field& f, uint32_t depth) {
  WireStatus s = CheckBytes(f);
  if (!s.ok()) return s;
  if (depth == 0) return {Error::kTooDeep, f.number};
  return V::Validate(f.value.len, depth - 1);
}

// --- encoding ----------------------------------------------------------------

inline constexpr size_t VarintLen(uint64_t v) {
  size_t n = 1;
  for (v >>= 7; v != 0; v >>= 7) n++;
  return n;
}

// A protobuf encoder writing straight into a caller's buffer. Every call
// returns false if the buffer is too small (the Writer is then unusable).
// Nested messages are written in place and their length prefix backpatched.
class Writer {
 public:
  Writer(uint8_t* buf, size_t cap) : buf_(buf), cap_(cap) {}

  size_t size() const { return pos_; }
  Bytes bytes() const { return Bytes(buf_, pos_); }

  bool Raw(const uint8_t* p, size_t n) {
    if (n > cap_ - pos_) return false;
    std::memcpy(buf_ + pos_, p, n);
    pos_ += n;
    return true;
  }
  bool Varint(uint64_t v) {
    uint8_t tmp[10];
    size_t n = 0;
    do {
      uint8_t b = v & 0x7f;
      v >>= 7;
      tmp[n++] = v ? (b | 0x80) : b;
    } while (v);
    return Raw(tmp, n);
  }
  bool Tag(uint32_t field, Wire w) { return Varint((static_cast<uint64_t>(field) << 3) | static_cast<uint64_t>(w)); }

  bool Uint64(uint32_t f, uint64_t v) { return Tag(f, Wire::kVarint) && Varint(v); }
  bool Int64(uint32_t f, int64_t v) { return Uint64(f, static_cast<uint64_t>(v)); }
  bool Int32(uint32_t f, int32_t v) { return Uint64(f, static_cast<uint64_t>(static_cast<int64_t>(v))); }
  bool Uint32(uint32_t f, uint32_t v) { return Uint64(f, v); }
  bool Sint32(uint32_t f, int32_t v) {
    return Uint64(f, static_cast<uint32_t>((static_cast<uint32_t>(v) << 1) ^ static_cast<uint32_t>(v >> 31)));
  }
  bool Sint64(uint32_t f, int64_t v) {
    return Uint64(f, (static_cast<uint64_t>(v) << 1) ^ static_cast<uint64_t>(v >> 63));
  }
  bool Bool(uint32_t f, bool v) { return Uint64(f, v ? 1 : 0); }
  bool Enum(uint32_t f, int32_t v) { return Int32(f, v); }
  bool Fixed32(uint32_t f, uint32_t v) {
    uint8_t b[4];
    for (int i = 0; i < 4; i++) b[i] = static_cast<uint8_t>(v >> (8 * i));
    return Tag(f, Wire::kI32) && Raw(b, 4);
  }
  bool Sfixed32(uint32_t f, int32_t v) { return Fixed32(f, static_cast<uint32_t>(v)); }
  bool Float(uint32_t f, float v) {
    uint32_t u;
    std::memcpy(&u, &v, sizeof u);
    return Fixed32(f, u);
  }
  bool Fixed64(uint32_t f, uint64_t v) {
    uint8_t b[8];
    for (int i = 0; i < 8; i++) b[i] = static_cast<uint8_t>(v >> (8 * i));
    return Tag(f, Wire::kI64) && Raw(b, 8);
  }
  bool Sfixed64(uint32_t f, int64_t v) { return Fixed64(f, static_cast<uint64_t>(v)); }
  bool Double(uint32_t f, double v) {
    uint64_t u;
    std::memcpy(&u, &v, sizeof u);
    return Fixed64(f, u);
  }
  bool BytesField(uint32_t f, Bytes v) { return Tag(f, Wire::kLen) && Varint(v.size) && Raw(v.data, v.size); }
  bool String(uint32_t f, std::string_view v) {
    return BytesField(f, Bytes(reinterpret_cast<const uint8_t*>(v.data()), v.size()));
  }

  // Write nested message `field`; `body(Writer&)` encodes its fields in place and
  // returns false when the buffer is full. The length prefix is reserved as one
  // byte; a longer body is shifted right in the buffer (one memmove, no second
  // buffer).
  template <class F>
  bool Message(uint32_t field, F&& body) {
    if (!Tag(field, Wire::kLen)) return false;
    size_t len_at = pos_;
    if (!Raw(reinterpret_cast<const uint8_t*>("\0"), 1)) return false;
    size_t start = pos_;
    Writer inner(buf_ + start, cap_ - start);
    if (!body(inner)) return false;
    pos_ = start + inner.pos_;
    size_t n = pos_ - start;
    size_t need = VarintLen(n);
    if (need > 1) {
      size_t extra = need - 1;
      if (extra > cap_ - pos_) return false;
      std::memmove(buf_ + start + extra, buf_ + start, n);
      pos_ += extra;
    }
    uint64_t v = n;
    size_t at = len_at;
    do {
      uint8_t b = v & 0x7f;
      v >>= 7;
      buf_[at++] = v ? (b | 0x80) : b;
    } while (v);
    return true;
  }

 private:
  uint8_t* buf_;
  size_t cap_;
  size_t pos_ = 0;
};

}  // namespace pbv

#endif  // PBVIEW_PBVIEW_H_
