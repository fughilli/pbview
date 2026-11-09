//! C++ typed encoders: per message `Foo`, a `FooEnc` with one setter per field (the C++
//! counterpart of encoder.rs; same guardrails, errors are sticky instead of `Result`s).

use super::{ident, scalar, snake, Gen, TY_BYTES, TY_MESSAGE, TY_STRING};
use crate::descriptor::{FieldDesc, Label, MessageDesc};
use crate::size::{assign_slots, Sizer};
use std::fmt::Write as _;

/// Code writing element `x` of a packed run into Writer `w` (no tag).
fn packed_elem(kind: &str) -> &'static str {
    match kind {
        "Double" => "{ uint64_t u; std::memcpy(&u, &x, sizeof u); ok = w.RawFixed64(u); }",
        "Float" => "{ uint32_t u; std::memcpy(&u, &x, sizeof u); ok = w.RawFixed32(u); }",
        "Int64" | "Uint64" => "ok = w.Varint(static_cast<uint64_t>(x));",
        "Int32" | "Enum" => "ok = w.Varint(static_cast<uint64_t>(static_cast<int64_t>(x)));",
        "Uint32" => "ok = w.Varint(x);",
        "Bool" => "ok = w.Varint(x ? 1 : 0);",
        "Fixed64" | "Sfixed64" => "ok = w.RawFixed64(static_cast<uint64_t>(x));",
        "Fixed32" | "Sfixed32" => "ok = w.RawFixed32(static_cast<uint32_t>(x));",
        "Sint32" => "ok = w.Varint(static_cast<uint32_t>((static_cast<uint32_t>(x) << 1) ^ static_cast<uint32_t>(x >> 31)));",
        "Sint64" => "ok = w.Varint((static_cast<uint64_t>(x) << 1) ^ static_cast<uint64_t>(x >> 63));",
        _ => unreachable!(),
    }
}

fn setter(name: &str) -> String {
    let n = snake(name);
    if matches!(n.as_str(), "error" | "error_field" | "encode") {
        return format!("{n}_");
    }
    ident(&n)
}

impl<'f> Gen<'f> {
    fn enc_of(&self, full: &str) -> Result<String, String> {
        let v = self.ty(full)?;
        let stem = v.strip_suffix("View").ok_or_else(|| format!("{full}: not a message"))?;
        Ok(format!("{stem}Enc"))
    }

    fn sizer(&self) -> Sizer<'_, 'f> {
        Sizer { msgs: &self.msgs, bounds: &self.bounds, scalar }
    }

    fn enc_value_guard(k: &str, num: u32, max_value: Option<u64>, v: &str) -> String {
        match (k, max_value) {
            ("Uint32" | "Uint64", Some(n)) => format!("  if (static_cast<uint64_t>({v}) > {n}ull) {{ Fail(::pbv::EncodeError::kTooLarge, {num}); return *this; }}\n"),
            ("Enum", Some(n)) => format!("  if ({v} < 0 || static_cast<int64_t>({v}) > {n}) {{ Fail(::pbv::EncodeError::kTooLarge, {num}); return *this; }}\n"),
            _ => String::new(),
        }
    }

    /// The `FooEnc` class declaration (setters are defined after every class, in `emit_enc_defs`).
    pub(super) fn emit_enc_class(&self, out: &mut String, enc: &str, m: &MessageDesc, full: &str, proto3: bool) -> Result<(), String> {
        let slots = assign_slots(m, scalar);
        if slots.bits > 128 {
            return Err(format!("{}: more than 128 tracked fields for the typed encoder", m.name));
        }
        let _ = writeln!(out, "/// Typed encoder for `{}`: one setter per field; errors are sticky (see pbview.h).", m.name);
        let _ = writeln!(out, "class {enc} : public ::pbv::EncBase<{}> {{\n public:", slots.counts);
        if let Some(n) = self.sizer().max_encoded(full, &mut Vec::new()) {
            let _ = writeln!(out, "  /// Worst-case encoded size: every field set, varints at full width or at their max_value.\n  static constexpr size_t kMaxEncodedLen = {n};");
        }
        let _ = writeln!(out, "  explicit {enc}(::pbv::Writer* w) : ::pbv::EncBase<{}>(w) {{}}", slots.counts);
        let _ = writeln!(
            out,
            "  /// Encode into buf with `f(enc)`; the result holds the length or the first error.\n  template <class F>\n  static ::pbv::EncodeResult Encode(uint8_t* buf, size_t cap, F&& f) {{\n    ::pbv::Writer w(buf, cap);\n    {enc} e(&w);\n    f(e);\n    if (e.error() != ::pbv::EncodeError::kOk) return {{0, e.error(), e.error_field()}};\n    return {{w.size(), ::pbv::EncodeError::kOk, 0}};\n  }}"
        );
        for f in m.fields.iter() {
            let name = setter(&f.name);
            let doc = format!("  /// `{}` = {}.", f.name, f.number);
            if f.label == Label::Repeated {
                if let Some((_, ty, _)) = scalar(f.ty) {
                    let _ = writeln!(out, "{doc} Packed; once.\n  {enc}& {name}(const {ty}* v, size_t n);");
                } else if f.ty == TY_STRING {
                    let _ = writeln!(out, "{doc} One element per call.\n  {enc}& add_{name}(std::string_view v);");
                } else if f.ty == TY_BYTES {
                    let _ = writeln!(out, "{doc} One element per call.\n  {enc}& add_{name}(::pbv::Bytes v);");
                } else {
                    let _ = writeln!(out, "{doc} One element per call; `f(sub_encoder)`.\n  template <class F>\n  {enc}& add_{name}(F&& f);");
                }
                continue;
            }
            let _ = proto3;
            if let Some((_, ty, _)) = scalar(f.ty) {
                let _ = writeln!(out, "{doc}\n  {enc}& {name}({ty} v);");
            } else if f.ty == TY_STRING {
                let _ = writeln!(out, "{doc}\n  {enc}& {name}(std::string_view v);");
            } else if f.ty == TY_BYTES {
                let _ = writeln!(out, "{doc}\n  {enc}& {name}(::pbv::Bytes v);");
            } else {
                let _ = writeln!(out, "{doc} `f(sub_encoder)`.\n  template <class F>\n  {enc}& {name}(F&& f);");
            }
        }
        let _ = writeln!(out, "}};");
        Ok(())
    }

    pub(super) fn emit_enc_defs(&self, out: &mut String, enc: &str, m: &MessageDesc, full: &str, proto3: bool) -> Result<(), String> {
        let slots = assign_slots(m, scalar);
        let sizer = self.sizer();
        for (i, f) in m.fields.iter().enumerate() {
            let name = setter(&f.name);
            let num = f.number;
            let b = sizer.bound(full, f);
            let pres = self.has_presence(f, proto3);
            let in_oneof = f.oneof_index.is_some() && !f.proto3_optional;
            let once = |bit: u32| format!("  if (!Once({bit}, {num}, {})) return *this;\n", in_oneof);

            if f.label == Label::Repeated {
                if let Some((k, ty, _)) = scalar(f.ty) {
                    let bit = slots.bit_of_field[i].unwrap();
                    let _ = writeln!(out, "inline {enc}& {enc}::{name}(const {ty}* v, size_t n) {{");
                    out.push_str(&once(bit));
                    if let Some(max) = b.max_count {
                        let _ = writeln!(out, "  if (n > {max}) {{ Fail(::pbv::EncodeError::kTooMany, {num}); return *this; }}");
                    }
                    if matches!((k, b.max_value), ("Uint32" | "Uint64" | "Enum", Some(_))) {
                        let g = Self::enc_value_guard(k, num, b.max_value, "v[j]");
                        let _ = writeln!(out, "  for (size_t j = 0; j < n; j++) {{\n  {}  }}", g.trim_start());
                    }
                    let _ = writeln!(out, "  if (n == 0) return *this;");
                    let _ = writeln!(out, "  Wr(w_->Message({num}, [&](::pbv::Writer& w) {{\n    for (size_t j = 0; j < n; j++) {{\n      {ty} x = v[j];\n      bool ok = true;\n      {}\n      if (!ok) return false;\n    }}\n    return true;\n  }}));\n  return *this;\n}}", packed_elem(k));
                    continue;
                }
                let slot = slots.count_of_field[i].unwrap();
                let max = b.max_count.unwrap_or(0);
                if f.ty == TY_STRING || f.ty == TY_BYTES {
                    let (argty, size, write) = if f.ty == TY_STRING { ("std::string_view", "v.size()", "String") } else { ("::pbv::Bytes", "v.size", "BytesField") };
                    let _ = writeln!(out, "inline {enc}& {enc}::add_{name}({argty} v) {{\n  if (!Count({slot}, {num}, {max})) return *this;");
                    if let Some(l) = b.max_len {
                        let _ = writeln!(out, "  if ({size} > {l}) {{ Fail(::pbv::EncodeError::kTooLong, {num}); return *this; }}");
                    }
                    let _ = writeln!(out, "  Wr(w_->{write}({num}, v));\n  return *this;\n}}");
                } else {
                    let sub = self.enc_of(&f.type_name)?;
                    let _ = writeln!(out, "template <class F>\ninline {enc}& {enc}::add_{name}(F&& f) {{\n  if (!Count({slot}, {num}, {max})) return *this;\n  Wr(w_->Message({num}, [&](::pbv::Writer& iw) {{\n    {sub} s(&iw);\n    f(s);\n    if (s.error() != ::pbv::EncodeError::kOk) {{ Fail(s.error(), s.error_field()); return false; }}\n    return true;\n  }}));\n  return *this;\n}}");
                }
                continue;
            }

            let bit = slots.bit_of_field[i].unwrap();
            if let Some((k, ty, def)) = scalar(f.ty) {
                let _ = writeln!(out, "inline {enc}& {enc}::{name}({ty} v) {{");
                out.push_str(&once(bit));
                out.push_str(&Self::enc_value_guard(k, num, b.max_value, "v"));
                if !pres {
                    match k {
                        "Float" => out.push_str("  { uint32_t u; std::memcpy(&u, &v, sizeof u); if (u == 0) return *this; }\n"),
                        "Double" => out.push_str("  { uint64_t u; std::memcpy(&u, &v, sizeof u); if (u == 0) return *this; }\n"),
                        _ => {
                            let _ = writeln!(out, "  if (v == {def}) return *this;");
                        }
                    }
                }
                let _ = writeln!(out, "  Wr(w_->{k}({num}, v));\n  return *this;\n}}");
            } else if f.ty == TY_STRING || f.ty == TY_BYTES {
                let (argty, size, empty, write) = if f.ty == TY_STRING {
                    ("std::string_view", "v.size()", "v.empty()", "String")
                } else {
                    ("::pbv::Bytes", "v.size", "v.size == 0", "BytesField")
                };
                let _ = writeln!(out, "inline {enc}& {enc}::{name}({argty} v) {{");
                out.push_str(&once(bit));
                if let Some(l) = b.max_len {
                    let _ = writeln!(out, "  if ({size} > {l}) {{ Fail(::pbv::EncodeError::kTooLong, {num}); return *this; }}");
                }
                if !pres {
                    let _ = writeln!(out, "  if ({empty}) return *this;");
                }
                let _ = writeln!(out, "  Wr(w_->{write}({num}, v));\n  return *this;\n}}");
            } else {
                debug_assert!(f.ty == TY_MESSAGE);
                let sub = self.enc_of(&f.type_name)?;
                let _ = writeln!(out, "template <class F>\ninline {enc}& {enc}::{name}(F&& f) {{");
                out.push_str(&once(bit));
                let _ = writeln!(out, "  Wr(w_->Message({num}, [&](::pbv::Writer& iw) {{\n    {sub} s(&iw);\n    f(s);\n    if (s.error() != ::pbv::EncodeError::kOk) {{ Fail(s.error(), s.error_field()); return false; }}\n    return true;\n  }}));\n  return *this;\n}}");
            }
        }
        Ok(())
    }
}
