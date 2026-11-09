//! Typed encoders for the Rust output: per message `Foo`, a `FooEnc` with one setter per field.
//!
//! What it guards, so a packer cannot easily build an ill-formed or oversized message:
//! - a setter exists for exactly the schema's fields, with the schema's Rust types (no wrong
//!   field number, no wrong wire type);
//! - a singular field (or packed repeated field) set twice is `EncodeError::Duplicate`; two
//!   arms of one oneof is `EncodeError::OneofConflict`;
//! - proto3 defaults are not written (a zero scalar, an empty string), matching protoc, while
//!   `optional` fields and oneof arms are written even at their default;
//! - nested messages are written through a closure taking the sub-encoder, with the length
//!   prefix backpatched in place;
//! - declared limits (`--bounds`) are enforced: `TooLong`, `TooMany`;
//! - when every field of a message is bounded, `FooEnc::MAX_ENCODED_LEN` is a const worst case,
//!   so `const _: () = assert!(FooEnc::MAX_ENCODED_LEN <= 125);` checks a frame budget at
//!   build time.
//!
//! Fields are written in the order the caller sets them. Set them in field-number order to get
//! protoc's canonical output (tests compare against it).

use super::{ident, scalar, snake, Gen, TY_BYTES, TY_MESSAGE, TY_STRING};
use crate::descriptor::{FieldDesc, Label, MessageDesc};
use crate::size::Sizer;
use std::fmt::Write as _;

/// Writer method and packed-element code for a scalar kind.
fn writer_method(kind: &str) -> &'static str {
    match kind {
        "Double" => "double",
        "Float" => "float",
        "Int64" => "int64",
        "Uint64" => "uint64",
        "Int32" => "int32",
        "Fixed64" => "fixed64",
        "Fixed32" => "fixed32",
        "Bool" => "bool",
        "Uint32" => "uint32",
        "Enum" => "enum_",
        "Sfixed32" => "sfixed32",
        "Sfixed64" => "sfixed64",
        "Sint32" => "sint32",
        "Sint64" => "sint64",
        _ => unreachable!(),
    }
}

/// Code writing element `x` of a packed run into writer `w` (no tag).
fn packed_elem(kind: &str) -> &'static str {
    match kind {
        "Double" => "w.raw(&x.to_bits().to_le_bytes())?;",
        "Float" => "w.raw(&x.to_bits().to_le_bytes())?;",
        "Int64" | "Uint64" => "w.varint(x as u64)?;",
        "Int32" | "Enum" => "w.varint(i64::from(x) as u64)?;",
        "Uint32" => "w.varint(u64::from(x))?;",
        "Bool" => "w.varint(u64::from(x))?;",
        "Fixed64" | "Sfixed64" => "w.raw(&x.to_le_bytes())?;",
        "Fixed32" | "Sfixed32" => "w.raw(&x.to_le_bytes())?;",
        "Sint32" => "w.varint(u64::from(((x << 1) ^ (x >> 31)) as u32))?;",
        "Sint64" => "w.varint(((x << 1) ^ (x >> 63)) as u64)?;",
        _ => unreachable!(),
    }
}

impl<'f> Gen<'f> {
    fn sizer(&self) -> Sizer<'_, 'f> {
        Sizer { msgs: &self.msgs, bounds: &self.opts.bounds, scalar }
    }

    fn bound(&self, full: &str, f: &FieldDesc) -> crate::Bound {
        self.sizer().bound(full, f)
    }

    pub(super) fn emit_encoder(
        &self,
        out: &mut String,
        module: &[String],
        m: &MessageDesc,
        full: &str,
        proto3: bool,
    ) -> Result<(), String> {
        let rt = &self.rt;
        let enc = format!("{}Enc", m.name);

        // Presence bits: one per singular field, one per real oneof, one per packed repeated
        // scalar. Counters: one per repeated string / bytes / message field.
        let mut bit_of_field: Vec<Option<u32>> = vec![None; m.fields.len()];
        let mut oneof_bit: Vec<Option<u32>> = vec![None; m.oneofs.len()];
        let mut count_of_field: Vec<Option<usize>> = vec![None; m.fields.len()];
        let (mut bits, mut counts) = (0u32, 0usize);
        for (i, f) in m.fields.iter().enumerate() {
            if f.label == Label::Repeated {
                if scalar(f.ty).is_some() {
                    bit_of_field[i] = Some(bits);
                    bits += 1;
                } else {
                    count_of_field[i] = Some(counts);
                    counts += 1;
                }
            } else if let (Some(o), false) = (f.oneof_index, f.proto3_optional) {
                let o = o as usize;
                if oneof_bit[o].is_none() {
                    oneof_bit[o] = Some(bits);
                    bits += 1;
                }
                bit_of_field[i] = oneof_bit[o];
            } else {
                bit_of_field[i] = Some(bits);
                bits += 1;
            }
        }
        if bits > 128 {
            return Err(format!("{}: more than 128 tracked fields for the typed encoder", m.name));
        }

        let _ = writeln!(out, "/// Typed encoder for `{}` (see `pbview_gen::encoder`).", m.name);
        let _ = writeln!(out, "pub struct {enc}<'a, 'b> {{ w: &'a mut {rt}::Writer<'b>, seen: u128, counts: [u32; {}] }}", counts.max(1));
        let _ = writeln!(out, "impl<'a, 'b> {enc}<'a, 'b> {{");
        if let Some(n) = self.sizer().max_encoded(full, &mut Vec::new()) {
            let _ = writeln!(out, "    /// Worst-case encoded size: every field set, every varint at full width, every bounded value at its limit.\n    pub const MAX_ENCODED_LEN: usize = {n};");
        }
        let _ = writeln!(out, "    pub fn new(w: &'a mut {rt}::Writer<'b>) -> Self {{ {enc} {{ w, seen: 0, counts: [0; {}] }} }}", counts.max(1));
        let _ = writeln!(
            out,
            "    /// Encode into `buf` with `f`; returns the encoded length.\n    pub fn encode(buf: &mut [u8], f: impl FnOnce(&mut {enc}<'_, '_>) -> Result<(), {rt}::EncodeError>) -> Result<usize, {rt}::EncodeError> {{\n        let mut w = {rt}::Writer::new(buf);\n        {{ let mut e = {enc}::new(&mut w); f(&mut e)?; }}\n        Ok(w.len())\n    }}"
        );

        for (i, f) in m.fields.iter().enumerate() {
            let mut name = snake(&f.name);
            if name == "new" || name == "encode" {
                name.push('_');
            }
            let num = f.number;
            let b = self.bound(full, f);
            let pres = self.has_presence(f, proto3);
            let in_oneof = f.oneof_index.is_some() && !f.proto3_optional;
            let doc = format!("    /// `{}` = {}.", f.name, f.number);

            // Presence guard shared by the once-only setters.
            let once = |bit: u32| -> String {
                let err = if in_oneof {
                    format!("{rt}::EncodeError::OneofConflict {{ field: {num} }}")
                } else {
                    format!("{rt}::EncodeError::Duplicate {{ field: {num} }}")
                };
                format!("        if self.seen & (1u128 << {bit}) != 0 {{ return Err({err}); }}\n        self.seen |= 1u128 << {bit};\n")
            };

            if f.label == Label::Repeated {
                if let Some((k, ty, _)) = scalar(f.ty) {
                    // Packed run, set once.
                    let bit = bit_of_field[i].unwrap();
                    let max = match b.max_count {
                        Some(n) => format!("        if v.len() > {n} {{ return Err({rt}::EncodeError::TooMany {{ field: {num}, max: {n} }}); }}\n"),
                        None => String::new(),
                    };
                    let value_guard = match (k, b.max_value) {
                        ("Uint32" | "Uint64", Some(n)) => format!("        if v.iter().any(|&x| u64::from(x) > {n}) {{ return Err({rt}::EncodeError::TooLarge {{ field: {num}, max: {n} }}); }}\n"),
                        ("Enum", Some(n)) => format!("        if v.iter().any(|&x| x < 0 || i64::from(x) > {n}) {{ return Err({rt}::EncodeError::TooLarge {{ field: {num}, max: {n} }}); }}\n"),
                        _ => String::new(),
                    };
                    let _ = writeln!(out, "{doc} Packed; once.\n    pub fn {}(&mut self, v: &[{ty}]) -> Result<&mut Self, {rt}::EncodeError> {{", ident(&name));
                    out.push_str(&once(bit));
                    out.push_str(&max);
                    out.push_str(&value_guard);
                    let _ = writeln!(out, "        if v.is_empty() {{ return Ok(self); }}");
                    let _ = writeln!(out, "        self.w.message_try({num}, |w| {{ for &x in v {{ {} }} Ok::<(), {rt}::EncodeError>(()) }})?;", packed_elem(k));
                    let _ = writeln!(out, "        Ok(self)\n    }}");
                    continue;
                }
                let c = count_of_field[i].unwrap();
                let count_guard = match b.max_count {
                    Some(n) => format!("        if self.counts[{c}] as usize >= {n} {{ return Err({rt}::EncodeError::TooMany {{ field: {num}, max: {n} }}); }}\n        self.counts[{c}] += 1;\n"),
                    None => format!("        self.counts[{c}] += 1;\n"),
                };
                let method = format!("add_{name}");
                if f.ty == TY_STRING || f.ty == TY_BYTES {
                    let (argty, call) = if f.ty == TY_STRING { ("&str", "string") } else { ("&[u8]", "bytes") };
                    let len_guard = match b.max_len {
                        Some(n) => format!("        if v.len() > {n} {{ return Err({rt}::EncodeError::TooLong {{ field: {num}, max: {n} }}); }}\n"),
                        None => String::new(),
                    };
                    let _ = writeln!(out, "{doc} One element per call.\n    pub fn {method}(&mut self, v: {argty}) -> Result<&mut Self, {rt}::EncodeError> {{");
                    out.push_str(&count_guard);
                    out.push_str(&len_guard);
                    let _ = writeln!(out, "        self.w.{call}({num}, v)?;\n        Ok(self)\n    }}");
                } else if f.ty == TY_MESSAGE {
                    let sub = self.enc_path(module, &f.type_name)?;
                    let _ = writeln!(out, "{doc} One element per call.\n    pub fn {method}(&mut self, f: impl FnOnce(&mut {sub}<'_, '_>) -> Result<(), {rt}::EncodeError>) -> Result<&mut Self, {rt}::EncodeError> {{");
                    out.push_str(&count_guard);
                    let _ = writeln!(out, "        self.w.message_try({num}, |w| {{ let mut e = {sub}::new(w); f(&mut e) }})?;\n        Ok(self)\n    }}");
                }
                continue;
            }

            let bit = bit_of_field[i].unwrap();
            if let Some((k, ty, def)) = scalar(f.ty) {
                let skip = if pres {
                    String::new()
                } else if k == "Float" || k == "Double" {
                    "        if v.to_bits() == 0 { return Ok(self); }\n".to_string()
                } else {
                    format!("        if v == {def} {{ return Ok(self); }}\n")
                };
                let value_guard = match (k, b.max_value) {
                    ("Uint32" | "Uint64", Some(n)) => format!("        if u64::from(v) > {n} {{ return Err({rt}::EncodeError::TooLarge {{ field: {num}, max: {n} }}); }}\n"),
                    ("Enum", Some(n)) => format!("        if v < 0 || i64::from(v) > {n} {{ return Err({rt}::EncodeError::TooLarge {{ field: {num}, max: {n} }}); }}\n"),
                    _ => String::new(),
                };
                let _ = writeln!(out, "{doc}\n    pub fn {}(&mut self, v: {ty}) -> Result<&mut Self, {rt}::EncodeError> {{", ident(&name));
                out.push_str(&once(bit));
                out.push_str(&value_guard);
                out.push_str(&skip);
                let _ = writeln!(out, "        self.w.{}({num}, v)?;\n        Ok(self)\n    }}", writer_method(k));
            } else if f.ty == TY_STRING || f.ty == TY_BYTES {
                let (argty, call) = if f.ty == TY_STRING { ("&str", "string") } else { ("&[u8]", "bytes") };
                let len_guard = match b.max_len {
                    Some(n) => format!("        if v.len() > {n} {{ return Err({rt}::EncodeError::TooLong {{ field: {num}, max: {n} }}); }}\n"),
                    None => String::new(),
                };
                let skip = if pres { String::new() } else { "        if v.is_empty() { return Ok(self); }\n".to_string() };
                let _ = writeln!(out, "{doc}\n    pub fn {}(&mut self, v: {argty}) -> Result<&mut Self, {rt}::EncodeError> {{", ident(&name));
                out.push_str(&once(bit));
                out.push_str(&len_guard);
                out.push_str(&skip);
                let _ = writeln!(out, "        self.w.{call}({num}, v)?;\n        Ok(self)\n    }}");
            } else {
                let sub = self.enc_path(module, &f.type_name)?;
                let _ = writeln!(out, "{doc}\n    pub fn {}(&mut self, f: impl FnOnce(&mut {sub}<'_, '_>) -> Result<(), {rt}::EncodeError>) -> Result<&mut Self, {rt}::EncodeError> {{", ident(&name));
                out.push_str(&once(bit));
                let _ = writeln!(out, "        self.w.message_try({num}, |w| {{ let mut e = {sub}::new(w); f(&mut e) }})?;\n        Ok(self)\n    }}");
            }
        }
        let _ = writeln!(out, "}}");
        Ok(())
    }

    /// Path to the encoder type of message `full` as seen from `module` (the view's path with
    /// `View` replaced by `Enc`).
    fn enc_path(&self, module: &[String], full: &str) -> Result<String, String> {
        let p = self.path(module, full)?;
        let stem = p.strip_suffix("View").ok_or_else(|| format!("{full}: not a message"))?;
        Ok(format!("{stem}Enc"))
    }
}
