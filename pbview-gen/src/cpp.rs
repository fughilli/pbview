//! C++17 output for `pbview-gen`: the same zero-copy views, targeting the header-only
//! runtime in `cpp/pbview.h` (namespace `pbv`).
//!
//! Naming: a proto package `a.b` becomes `namespace a { namespace b { ... } }`. Message
//! `Foo` is class `FooView`; nested `Foo.Bar` is the flat `Foo_BarView` (a flat layout
//! lets views refer to each other, recursively, with plain forward declarations). Enums
//! become `enum class Foo_Color : int32_t`; an enum-typed field still reads as raw
//! `int32_t` (proto3 enums are open). Accessors are `snake_case`; a oneof `kind` gets
//! `kind_case()`, the field number of the arm set last on the wire (0: none), and the arm
//! accessors read the arms as ordinary optional fields.
//!
//! Not emitted (the Rust output has them): the single-pass `fields()` typed iterator.
//! `fields()` here is the raw `pbv::Reader`.

use crate::descriptor::{EnumDesc, FieldDesc, FileDesc, Label, MessageDesc};
use crate::{camel, snake};
use std::collections::HashMap;
use std::fmt::Write as _;

/// C++ output options.
#[derive(Debug, Clone)]
pub struct CppOptions {
    /// The `#include` for the runtime header (default `pbview.h`).
    pub runtime_include: String,
    /// Declared limits for the typed encoders (see `crate::Bound`).
    pub bounds: HashMap<String, crate::Bound>,
}

impl Default for CppOptions {
    fn default() -> Self {
        CppOptions { runtime_include: "pbview.h".into(), bounds: HashMap::new() }
    }
}

const KEYWORDS: &[&str] = &[
    "alignas", "alignof", "and", "asm", "auto", "bool", "break", "case", "catch", "char",
    "class", "const", "constexpr", "continue", "decltype", "default", "delete", "do", "double",
    "else", "enum", "explicit", "export", "extern", "false", "float", "for", "friend", "goto",
    "if", "inline", "int", "long", "mutable", "namespace", "new", "noexcept", "not", "nullptr",
    "operator", "or", "private", "protected", "public", "register", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "template", "this", "throw", "true", "try", "typedef",
    "typeid", "typename", "union", "unsigned", "using", "virtual", "void", "volatile", "while",
    "xor", "NULL", "EOF",
];
/// Inherent view methods a field accessor must not shadow.
const RESERVED_METHODS: &[&str] = &["parse", "validate", "bytes", "fields", "from_bytes_unchecked"];

const TY_STRING: u32 = 9;
const TY_GROUP: u32 = 10;
const TY_MESSAGE: u32 = 11;
const TY_BYTES: u32 = 12;

fn ident(name: &str) -> String {
    if KEYWORDS.contains(&name) || name.contains("__") || name.starts_with('_') {
        format!("{name}_x")
    } else {
        name.to_string()
    }
}

/// (runtime kind, C++ type, default literal) for a scalar descriptor type.
fn scalar(ty: u32) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match ty {
        1 => ("Double", "double", "0.0"),
        2 => ("Float", "float", "0.0f"),
        3 => ("Int64", "int64_t", "0"),
        4 => ("Uint64", "uint64_t", "0"),
        5 => ("Int32", "int32_t", "0"),
        6 => ("Fixed64", "uint64_t", "0"),
        7 => ("Fixed32", "uint32_t", "0"),
        8 => ("Bool", "bool", "false"),
        13 => ("Uint32", "uint32_t", "0"),
        14 => ("Enum", "int32_t", "0"),
        15 => ("Sfixed32", "int32_t", "0"),
        16 => ("Sfixed64", "int64_t", "0"),
        17 => ("Sint32", "int32_t", "0"),
        18 => ("Sint64", "int64_t", "0"),
        _ => return None,
    })
}

mod enc;

struct Gen<'f> {
    files: &'f [FileDesc],
    /// Full proto name (`.pkg.Outer.Inner`) -> absolute C++ name.
    types: HashMap<String, String>,
    msgs: HashMap<String, &'f MessageDesc>,
    bounds: HashMap<String, crate::Bound>,
}

pub fn generate(fdset: &[u8], opts: &CppOptions) -> Result<String, String> {
    let files = crate::descriptor::parse_set(fdset)?;
    Gen { files: &files, types: HashMap::new(), msgs: HashMap::new(), bounds: opts.bounds.clone() }.run(opts)
}

fn ns_of(package: &str) -> Vec<String> {
    package.split('.').filter(|p| !p.is_empty()).map(ident).collect()
}

impl<'f> Gen<'f> {
    fn index(&mut self) {
        for f in self.files {
            let ns = ns_of(&f.package);
            let prefix = if f.package.is_empty() { String::new() } else { format!(".{}", f.package) };
            let abs = |ident: &str| format!("::{}{ident}", ns.iter().map(|n| format!("{n}::")).collect::<String>());
            for m in &f.messages {
                self.index_message(&prefix, &abs, "", m);
            }
            for e in &f.enums {
                self.types.insert(format!("{prefix}.{}", e.name), abs(&ident(&e.name)));
            }
        }
    }

    fn index_message(&mut self, prefix: &str, abs: &dyn Fn(&str) -> String, flat: &str, m: &'f MessageDesc) {
        let full = format!("{prefix}.{}", m.name);
        self.msgs.insert(full.clone(), m);
        let flat_name = format!("{flat}{}", m.name);
        self.types.insert(full.clone(), abs(&ident(&format!("{flat_name}View"))));
        for e in &m.enums {
            self.types.insert(format!("{full}.{}", e.name), abs(&ident(&format!("{flat_name}_{}", e.name))));
        }
        for n in &m.nested {
            self.index_message(&full, abs, &format!("{flat_name}_"), n);
        }
    }

    fn ty(&self, full: &str) -> Result<&str, String> {
        self.types.get(full).map(|s| s.as_str()).ok_or_else(|| format!("unknown type {full}"))
    }

    fn run(mut self, opts: &CppOptions) -> Result<String, String> {
        self.index();
        // Three passes so views can refer to each other: forward declarations (and
        // enums), then class declarations, then the inline definitions.
        let mut packages: Vec<(String, Vec<&FileDesc>)> = Vec::new();
        for f in self.files {
            match packages.iter_mut().find(|(p, _)| *p == f.package) {
                Some((_, v)) => v.push(f),
                None => packages.push((f.package.clone(), vec![f])),
            }
        }
        let mut out = String::new();
        out.push_str("// @generated by pbview-gen --lang cpp. Do not edit.\n#pragma once\n\n");
        let _ = writeln!(out, "#include \"{}\"\n", opts.runtime_include);
        for pass in 0..3 {
            for (pkg, files) in &packages {
                let ns = ns_of(pkg);
                for n in &ns {
                    let _ = writeln!(out, "namespace {n} {{");
                }
                for f in files {
                    if pass == 0 {
                        for e in &f.enums {
                            self.emit_enum(&mut out, &ident(&e.name), e);
                        }
                    }
                    for m in &f.messages {
                        let prefix = if f.package.is_empty() { String::new() } else { format!(".{}", f.package) };
                        self.emit_message(&mut out, pass, &prefix, "", m, f.proto3)?;
                    }
                }
                for n in ns.iter().rev() {
                    let _ = writeln!(out, "}}  // namespace {n}");
                }
                out.push('\n');
            }
        }
        Ok(out)
    }

    fn emit_enum(&self, out: &mut String, name: &str, e: &EnumDesc) {
        let prefix = format!("{}_", snake(&e.name).to_ascii_uppercase());
        let mut seen = std::collections::HashSet::new();
        let _ = writeln!(out, "enum class {name} : int32_t {{");
        for (vname, num) in &e.values {
            if !seen.insert(*num) {
                continue; // allow_alias: keep the first name per number
            }
            let stripped = vname.strip_prefix(&prefix).unwrap_or(vname);
            let mut v = camel(stripped);
            if v.is_empty() || v.starts_with(|c: char| c.is_ascii_digit()) {
                v = camel(vname);
            }
            let _ = writeln!(out, "  k{v} = {num},");
        }
        let _ = writeln!(out, "}};");
    }

    fn acc(&self, f: &FieldDesc) -> String {
        let n = snake(&f.name);
        if RESERVED_METHODS.contains(&n.as_str()) {
            return format!("{n}_");
        }
        ident(&n)
    }

    pub(self) fn has_presence(&self, f: &FieldDesc, proto3: bool) -> bool {
        f.ty == TY_MESSAGE || f.oneof_index.is_some() || f.proto3_optional || !proto3
    }

    /// The C++ type of one non-repeated value of `f`.
    fn value_ty(&self, f: &FieldDesc) -> Result<String, String> {
        Ok(if let Some((_, ty, _)) = scalar(f.ty) {
            ty.to_string()
        } else if f.ty == TY_STRING {
            "std::string_view".into()
        } else if f.ty == TY_BYTES {
            "::pbv::Bytes".into()
        } else {
            self.ty(&f.type_name)?.to_string()
        })
    }

    fn emit_message(&self, out: &mut String, pass: u32, prefix: &str, flat: &str, m: &MessageDesc, proto3: bool) -> Result<(), String> {
        let full = format!("{prefix}.{}", m.name);
        let flat_name = format!("{flat}{}", m.name);
        let view = ident(&format!("{flat_name}View"));
        for f in &m.fields {
            if f.ty == TY_GROUP {
                return Err(format!("{}.{}: groups are not supported", m.name, f.name));
            }
        }
        match pass {
            0 => {
                for e in &m.enums {
                    self.emit_enum(out, &ident(&format!("{flat_name}_{}", e.name)), e);
                }
                let _ = writeln!(out, "class {view};");
                let _ = writeln!(out, "class {};", ident(&format!("{flat_name}Enc")));
            }
            1 => {
                self.emit_class(out, &view, m, proto3)?;
                self.emit_enc_class(out, &ident(&format!("{flat_name}Enc")), m, &full, proto3)?;
            }
            _ => {
                self.emit_defs(out, &view, m, proto3)?;
                self.emit_enc_defs(out, &ident(&format!("{flat_name}Enc")), m, &full, proto3)?;
            }
        }
        for n in &m.nested {
            self.emit_message(out, pass, &full, &format!("{flat_name}_"), n, proto3)?;
        }
        Ok(())
    }

    fn real_oneof(m: &MessageDesc, i: u32) -> bool {
        m.fields.iter().any(|f| f.oneof_index == Some(i) && !f.proto3_optional)
    }

    fn oneof_acc(oname: &str) -> String {
        ident(&format!("{}_case", snake(oname)))
    }

    fn accessor_decl(&self, f: &FieldDesc, proto3: bool) -> Result<(String, String), String> {
        // (return type, name)
        let acc = self.acc(f);
        if f.label == Label::Repeated {
            let ret = if let Some((k, _, _)) = scalar(f.ty) {
                format!("::pbv::RepeatedScalar<::pbv::kind::{k}>")
            } else if f.ty == TY_STRING {
                "::pbv::RepeatedStr".into()
            } else if f.ty == TY_BYTES {
                "::pbv::RepeatedLen".into()
            } else {
                format!("::pbv::RepeatedMsg<{}>", self.ty(&f.type_name)?)
            };
            return Ok((ret, acc));
        }
        let vt = self.value_ty(f)?;
        let ret = if self.has_presence(f, proto3) { format!("std::optional<{vt}>") } else { vt };
        Ok((ret, acc))
    }

    fn emit_class(&self, out: &mut String, view: &str, m: &MessageDesc, proto3: bool) -> Result<(), String> {
        let _ = writeln!(out, "/// Zero-copy view of `{}`.\nclass {view} {{\n public:", m.name);
        for f in &m.fields {
            let _ = writeln!(out, "  static constexpr uint32_t kField{} = {};", camel(&f.name), f.number);
        }
        let _ = writeln!(out, "  {view}() = default;");
        let _ = writeln!(
            out,
            "  /// Validate `buf` (framing, wire types, UTF-8, nesting <= ::pbv::kDefaultMaxDepth) and wrap it.\n  static ::pbv::WireStatus Parse(::pbv::Bytes buf, {view}* out);\n  static ::pbv::WireStatus Validate(::pbv::Bytes buf, uint32_t depth);\n  /// Wrap without validating (accessors stay bounds-safe).\n  static {view} FromBytesUnchecked(::pbv::Bytes buf) {{ {view} v; v.buf_ = buf; return v; }}\n  ::pbv::Bytes bytes() const {{ return buf_; }}\n  /// Raw field iterator.\n  ::pbv::Reader fields() const {{ return ::pbv::Reader(buf_); }}"
        );
        out.push_str(&self.accessor_decls(m, proto3)?);
        for (i, oname) in m.oneofs.iter().enumerate() {
            if Self::real_oneof(m, i as u32) {
                let _ = writeln!(out, "  /// Field number of the `{oname}` arm set last on the wire (0: none).\n  uint32_t {}() const;", Self::oneof_acc(oname));
            }
        }
        let _ = writeln!(out, "\n private:\n  ::pbv::Bytes buf_;\n}};");
        Ok(())
    }

    fn accessor_decls(&self, m: &MessageDesc, proto3: bool) -> Result<String, String> {
        let mut s = String::new();
        for f in &m.fields {
            let (ret, acc) = self.accessor_decl(f, proto3)?;
            let _ = writeln!(s, "  /// `{}` = {}.\n  {ret} {acc}() const;", f.name, f.number);
        }
        Ok(s)
    }

    fn emit_defs(&self, out: &mut String, view: &str, m: &MessageDesc, proto3: bool) -> Result<(), String> {
        // Parse / Validate.
        let _ = writeln!(
            out,
            "inline ::pbv::WireStatus {view}::Parse(::pbv::Bytes buf, {view}* out) {{\n  ::pbv::WireStatus s = Validate(buf, ::pbv::kDefaultMaxDepth);\n  if (s.ok()) out->buf_ = buf;\n  return s;\n}}"
        );
        let _ = writeln!(out, "inline ::pbv::WireStatus {view}::Validate(::pbv::Bytes buf, uint32_t depth) {{");
        let _ = writeln!(out, "  (void)depth;\n  ::pbv::Reader r(buf);\n  ::pbv::Field f;\n  while (r.Next(&f)) {{\n    ::pbv::WireStatus s;\n    switch (f.number) {{");
        for f in &m.fields {
            let rep = f.label == Label::Repeated;
            let chk = if let Some((k, _, _)) = scalar(f.ty) {
                format!("::pbv::CheckScalar<::pbv::kind::{k}>(f, {rep})")
            } else if f.ty == TY_STRING {
                "::pbv::CheckStr(f)".to_string()
            } else if f.ty == TY_BYTES {
                "::pbv::CheckBytes(f)".to_string()
            } else {
                format!("::pbv::CheckMsg<{}>(f, depth)", self.ty(&f.type_name)?)
            };
            let _ = writeln!(out, "      case {}: s = {chk}; break;", f.number);
        }
        let _ = writeln!(
            out,
            "      default: break;\n    }}\n    if (!s.ok()) return s;\n  }}\n  if (r.error() != ::pbv::Error::kOk) return {{r.error(), 0}};\n  return {{}};\n}}"
        );

        for f in &m.fields {
            let (ret, acc) = self.accessor_decl(f, proto3)?;
            let num = f.number;
            let head = format!("inline {ret} {view}::{acc}() const");
            if f.label == Label::Repeated {
                let ctor = ret.clone();
                let _ = writeln!(out, "{head} {{ return {ctor}(buf_, {num}); }}");
                continue;
            }
            let pres = self.has_presence(f, proto3);
            if let Some((k, _, def)) = scalar(f.ty) {
                let get = format!("::pbv::LastScalar<::pbv::kind::{k}>(buf_, {num})");
                if pres {
                    let _ = writeln!(out, "{head} {{ return {get}; }}");
                } else {
                    let _ = writeln!(out, "{head} {{ return {get}.value_or({def}); }}");
                }
            } else if f.ty == TY_STRING || f.ty == TY_BYTES {
                let is_str = f.ty == TY_STRING;
                if pres {
                    let conv = if is_str { "::pbv::StrOf(*b)" } else { "*b" };
                    let _ = writeln!(
                        out,
                        "{head} {{ auto b = ::pbv::LastLen(buf_, {num}); if (!b) return std::nullopt; return {conv}; }}"
                    );
                } else {
                    let (conv, def) = if is_str { ("::pbv::StrOf(*b)", "std::string_view()") } else { ("*b", "::pbv::Bytes()") };
                    let _ = writeln!(out, "{head} {{ auto b = ::pbv::LastLen(buf_, {num}); return b ? {conv} : {def}; }}");
                }
            } else {
                let sub = self.ty(&f.type_name)?;
                let _ = writeln!(
                    out,
                    "{head} {{ auto b = ::pbv::LastLen(buf_, {num}); if (!b) return std::nullopt; return {sub}::FromBytesUnchecked(*b); }}"
                );
            }
        }

        // Oneofs: the field number of the arm set last on the wire.
        for (i, oname) in m.oneofs.iter().enumerate() {
            if !Self::real_oneof(m, i as u32) {
                continue;
            }
            let acc = Self::oneof_acc(oname);
            let _ = writeln!(out, "inline uint32_t {view}::{acc}() const {{\n  uint32_t last = 0;\n  ::pbv::Reader r(buf_);\n  ::pbv::Field f;\n  while (r.Next(&f)) {{\n    switch (f.number) {{");
            for f in m.fields.iter().filter(|f| f.oneof_index == Some(i as u32)) {
                let ok = if let Some((k, _, _)) = scalar(f.ty) {
                    format!("{{ ::pbv::kind::{k}::Out v; if (::pbv::kind::{k}::FromValue(f.value, &v)) last = {}; }}", f.number)
                } else if f.ty == TY_STRING {
                    format!("if (f.value.wire == ::pbv::Wire::kLen && ::pbv::Utf8Valid(f.value.len)) last = {};", f.number)
                } else {
                    format!("if (f.value.wire == ::pbv::Wire::kLen) last = {};", f.number)
                };
                let _ = writeln!(out, "      case {}: {ok} break;", f.number);
            }
            let _ = writeln!(out, "      default: break;\n    }}\n  }}\n  return last;\n}}");
        }
        Ok(())
    }
}
