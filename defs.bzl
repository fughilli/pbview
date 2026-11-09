"""pbview_library: zero-copy protobuf views generated from a descriptor set."""

load("@rules_cc//cc:defs.bzl", "cc_library")
load("@rules_rust//rust:defs.bzl", "rust_library")

def pbview_library(name, fdset, crate_name = None, bounds = None, visibility = None, **kwargs):
    """A no_std Rust crate of pbview views for every file in `fdset`.

    Args:
      name: target name.
      fdset: label of a protoc `--descriptor_set_out` FileDescriptorSet.
      crate_name: crate name (default: `name`).
      bounds: optional sidecar file of limits (`pkg.Message.field max_len N` / `max_count N`)
        enforced by the typed encoders and summed into `FooEnc::MAX_ENCODED_LEN`.
      visibility: visibility of the library.
      **kwargs: passed to rust_library.
    """
    gen = name + "_gen"
    native.genrule(
        name = gen,
        srcs = [fdset] + ([bounds] if bounds else []),
        outs = [name + ".rs"],
        cmd = "$(execpath @pbview//:pbview_gen) $(location " + fdset + ") $@ --crate-root" +
              ((" --bounds $(location " + bounds + ")") if bounds else ""),
        tools = ["@pbview//:pbview_gen"],
    )
    rust_library(
        name = name,
        srcs = [":" + gen],
        crate_root = ":" + gen,
        crate_name = crate_name,
        edition = "2021",
        deps = ["@pbview//:pbview"],
        visibility = visibility,
        **kwargs
    )

def pbview_cc_library(name, fdset, header = None, bounds = None, visibility = None, **kwargs):
    """A C++17 header of pbview views for every file in `fdset`.

    The header includes "pbview.h" (@pbview//:pbview_cc is added as a dependency).

    Args:
      name: target name.
      fdset: label of a protoc `--descriptor_set_out` FileDescriptorSet.
      header: generated header name (default: name + ".h").
      bounds: optional sidecar file of limits for the typed encoders (see pbview_library).
      visibility: visibility of the library.
      **kwargs: passed to cc_library.
    """
    header = header or name + ".h"
    gen = name + "_gen"
    native.genrule(
        name = gen,
        srcs = [fdset] + ([bounds] if bounds else []),
        outs = [header],
        cmd = "$(execpath @pbview//:pbview_gen) $(location " + fdset + ") $@ --lang cpp" +
              ((" --bounds $(location " + bounds + ")") if bounds else ""),
        tools = ["@pbview//:pbview_gen"],
    )
    cc_library(
        name = name,
        hdrs = [":" + gen],
        deps = ["@pbview//:pbview_cc"],
        visibility = visibility,
        **kwargs
    )
