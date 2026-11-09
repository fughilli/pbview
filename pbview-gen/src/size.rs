//! Worst-case encoded sizes from the schema and the bounds file, shared by the Rust and C++
//! typed-encoder generators.

use crate::descriptor::{FieldDesc, Label, MessageDesc};
use crate::Bound;
use std::collections::HashMap;

const TY_STRING: u32 = 9;
const TY_BYTES: u32 = 12;

/// (runtime kind, language type, default literal) for a scalar descriptor type.
pub(crate) type ScalarFn = fn(u32) -> Option<(&'static str, &'static str, &'static str)>;

/// Most bytes one scalar value takes on the wire (varints at full width).
pub(crate) fn scalar_max(kind: &str) -> u64 {
    match kind {
        "Double" | "Fixed64" | "Sfixed64" => 8,
        "Float" | "Fixed32" | "Sfixed32" => 4,
        "Bool" => 1,
        "Uint32" | "Sint32" => 5,
        _ => 10, // int32/enum sign-extend to 10; int64, uint64, sint64
    }
}

pub(crate) fn varint_len(mut v: u64) -> u64 {
    let mut n = 1;
    v >>= 7;
    while v != 0 {
        n += 1;
        v >>= 7;
    }
    n
}

pub(crate) fn tag_len(number: u32) -> u64 {
    varint_len(u64::from(number) << 3)
}

pub(crate) struct Sizer<'a, 'f> {
    pub msgs: &'a HashMap<String, &'f MessageDesc>,
    pub bounds: &'a HashMap<String, Bound>,
    pub scalar: ScalarFn,
}

impl<'a, 'f> Sizer<'a, 'f> {
    pub fn bound(&self, full: &str, f: &FieldDesc) -> Bound {
        let key = format!("{}.{}", full.trim_start_matches('.'), f.name);
        self.bounds.get(&key).copied().unwrap_or_default()
    }

    /// Worst-case encoded size of message `full`, if every field it can carry is bounded.
    pub fn max_encoded(&self, full: &str, stack: &mut Vec<String>) -> Option<u64> {
        if stack.iter().any(|s| s == full) {
            return None; // recursive: unbounded
        }
        let m = *self.msgs.get(full)?;
        stack.push(full.to_string());
        let mut total = 0u64;
        let mut oneof_max: Vec<u64> = vec![0; m.oneofs.len()];
        let mut ok = true;
        for f in &m.fields {
            let Some(size) = self.field_max(full, f, stack) else {
                ok = false;
                break;
            };
            match f.oneof_index {
                Some(i) if !f.proto3_optional => {
                    let slot = &mut oneof_max[i as usize];
                    *slot = (*slot).max(size);
                }
                _ => total += size,
            }
        }
        stack.pop();
        ok.then(|| total + oneof_max.iter().sum::<u64>())
    }

    fn field_max(&self, full: &str, f: &FieldDesc, stack: &mut Vec<String>) -> Option<u64> {
        let b = self.bound(full, f);
        let tag = tag_len(f.number);
        // One element, without its tag.
        let (elem, packable): (u64, bool) = if let Some((k, _, _)) = (self.scalar)(f.ty) {
            match (k, b.max_value) {
                // A field that can only be 0 is never written (proto3 default).
                ("Uint32" | "Uint64" | "Enum", Some(0)) if f.label != Label::Repeated && f.oneof_index.is_none() && !f.proto3_optional => return Some(0),
                ("Uint32" | "Uint64" | "Enum", Some(v)) => (varint_len(v), true),
                _ => (scalar_max(k), true),
            }
        } else if f.ty == TY_STRING || f.ty == TY_BYTES {
            let l = b.max_len? as u64;
            (varint_len(l) + l, false)
        } else {
            let sub = self.max_encoded(&f.type_name, stack)?;
            (varint_len(sub) + sub, false)
        };
        if f.label != Label::Repeated {
            return Some(tag + elem);
        }
        let n = b.max_count? as u64;
        if packable {
            let body = n * elem;
            Some(if n == 0 { 0 } else { tag + varint_len(body) + body })
        } else {
            Some(n * (tag + elem))
        }
    }
}

/// Presence-tracking slots of a message's typed encoder: one bit per singular field, per real
/// oneof and per packed repeated scalar; one counter per other repeated field.
pub(crate) struct Slots {
    pub bit_of_field: Vec<Option<u32>>,
    pub count_of_field: Vec<Option<usize>>,
    pub bits: u32,
    pub counts: usize,
}

pub(crate) fn assign_slots(m: &MessageDesc, scalar: ScalarFn) -> Slots {
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
    Slots { bit_of_field, count_of_field, bits, counts }
}
