//! The typed encoders (`FooEnc`): protoc-identical output, and the guardrails.

use conformance_views::pbview::test::{
    AllEnc, AllView, InnerView, PacketEnc, PacketView, TagEnc, TagView, packet,
};
use pbview::{EncodeError, View};

fn golden() -> Vec<u8> {
    std::fs::read(std::env::var("ALL_BIN").expect("ALL_BIN")).expect("golden encoding")
}

#[test]
fn builder_reproduces_protoc_bytes_for_every_field_kind() {
    // The same message as testdata/all.textproto, set in field-number order.
    let mut buf = [0u8; 512];
    let n = AllEnc::encode(&mut buf, |m| {
        m.f_double(-1.5)?
            .f_float(2.25)?
            .f_int64(-9_000_000_000)?
            .f_uint64(u64::MAX)?
            .f_int32(-7)?
            .f_fixed64(1_234_567_890_123)?
            .f_fixed32(4_000_000_000)?
            .f_bool(true)?
            .f_string("héllo")?
            .f_inner(|i| {
                i.a(3)?.label("in")?.child(|c| {
                    c.a(4)?;
                    Ok(())
                })?;
                Ok(())
            })?
            .f_bytes(&[0, 1, 0xff])?
            .f_uint32(300)?
            .f_color(2)?
            .f_sfixed32(-2)?
            .f_sfixed64(-3)?
            .f_sint32(-64)?
            .f_sint64(-1_000_000_000_000)?
            .opt_int32(0)?
            .r_packed(&[1, -2, 300])?
            .r_sint64(&[-1, 1])?
            .r_double(&[0.5, -0.25])?
            .add_r_string("a")?
            .add_r_string("")?
            .add_r_string("ccc")?
            .add_r_inner(|i| {
                i.a(1)?;
                Ok(())
            })?
            .add_r_inner(|i| {
                i.a(2)?.label("two")?;
                Ok(())
            })?
            .add_r_bytes(b"x")?
            .r_color(&[1, 2])?
            .add_counts(|e| {
                e.key("k1")?.value(1)?;
                Ok(())
            })?
            .add_counts(|e| {
                e.key("k2")?.value(2)?;
                Ok(())
            })?
            .c_text("picked")?
            .r#type(9)?
            .r#match("m")?
            .mode(1)?
            .nested(|x| {
                x.x(77)?;
                Ok(())
            })?;
        Ok(())
    })
    .unwrap();
    assert_eq!(&buf[..n], &golden()[..], "builder output == protoc's encoding");
    assert!(AllView::parse(&buf[..n]).is_ok());
}

#[test]
fn proto3_defaults_are_not_written_but_optional_and_oneof_arms_are() {
    let mut buf = [0u8; 64];
    let n = PacketEnc::encode(&mut buf, |m| {
        m.id(0)?.payload(&[])?.delta(0)?.stamp(0)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(n, 0, "all defaults: empty message");
    let n = PacketEnc::encode(&mut buf, |m| {
        m.maybe(0)?.num(0)?;
        Ok(())
    })
    .unwrap();
    let v = PacketView::parse(&buf[..n]).unwrap();
    assert_eq!(v.maybe(), Some(0));
    assert_eq!(v.num(), Some(0));
    assert!(matches!(v.body(), Some(packet::Body::Num(0))));
}

#[test]
fn duplicate_and_oneof_conflicts_are_errors() {
    let mut buf = [0u8; 64];
    let e = PacketEnc::encode(&mut buf, |m| {
        m.id(1)?.id(2)?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::Duplicate { field: 1 }));
    let e = PacketEnc::encode(&mut buf, |m| {
        m.text("a")?.num(3)?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::OneofConflict { field: 7 }));
    let e = PacketEnc::encode(&mut buf, |m| {
        m.hops(&[1])?.hops(&[2])?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::Duplicate { field: 3 }));
}

#[test]
fn declared_limits_are_enforced() {
    let mut buf = [0u8; 256];
    let e = PacketEnc::encode(&mut buf, |m| {
        m.payload(&[0u8; 21])?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::TooLong { field: 2, max: 20 }));
    assert!(PacketEnc::encode(&mut buf, |m| { m.payload(&[0u8; 20])?; Ok(()) }).is_ok());
    let e = PacketEnc::encode(&mut buf, |m| {
        m.hops(&[1, 2, 3, 4, 5])?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::TooMany { field: 3, max: 4 }));
    let e = PacketEnc::encode(&mut buf, |m| {
        for _ in 0..4 {
            m.add_tags(|t| { t.name("n")?; Ok(()) })?;
        }
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::TooMany { field: 4, max: 3 }));
    let e = PacketEnc::encode(&mut buf, |m| {
        m.add_tags(|t| { t.name("123456789")?; Ok(()) })?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::TooLong { field: 1, max: 8 }), "limits apply inside nested messages");
}

#[test]
fn buffer_full_is_an_error_not_a_truncation() {
    let mut buf = [0u8; 8];
    let e = PacketEnc::encode(&mut buf, |m| {
        m.payload(&[7u8; 20])?;
        Ok(())
    });
    assert_eq!(e, Err(EncodeError::Full));
    let mut tiny = [0u8; 3];
    assert_eq!(TagEnc::encode(&mut tiny, |t| { t.name("abcdef")?; Ok(()) }), Err(EncodeError::Full));
}

#[test]
fn max_encoded_len_is_tight() {
    // 6 + 22 + 22 + 63 + 21 + 6 + 6 + 5, from testdata/bounds.txt (see the BUILD comment).
    assert_eq!(PacketEnc::MAX_ENCODED_LEN, 151);
    let mut buf = [0u8; 256];
    let n = PacketEnc::encode(&mut buf, |m| {
        m.id(u32::MAX)?
            .payload(&[0xaa; 20])?
            .hops(&[u32::MAX; 4])?;
        for _ in 0..3 {
            m.add_tags(|t| { t.name("12345678")?.value(i64::MIN)?; Ok(()) })?;
        }
        m.tag(|t| { t.name("12345678")?.value(i64::MIN)?; Ok(()) })?
            .maybe(u32::MAX)?
            .delta(i32::MIN)?
            .stamp(u32::MAX)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(n, PacketEnc::MAX_ENCODED_LEN, "the worst case is reached, so the bound is exact");
    // And it round-trips through the views.
    let v = PacketView::parse(&buf[..n]).unwrap();
    assert_eq!(v.id(), u32::MAX);
    assert_eq!(v.tags().count(), 3);
    assert_eq!(v.hops().collect::<Vec<_>>(), [u32::MAX; 4]);
    let t: TagView = v.tag().unwrap();
    assert_eq!((t.name(), t.value()), ("12345678", i64::MIN));
    // A frame budget is a compile-time assertion.
    const _: () = assert!(PacketEnc::MAX_ENCODED_LEN + 10 <= 200);
}

#[test]
fn unbounded_messages_have_no_max_len() {
    // `Inner` is recursive and `All` has unbounded strings: no MAX_ENCODED_LEN is generated
    // (it would not compile to reference it), only the runtime guards exist.
    let _ = (InnerView::FIELD_A, AllView::FIELD_F_INT32);
}
