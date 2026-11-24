# Building Runtime with in-repository Core

From the monorepo root, use Rust 1.95.0:

```sh
cargo check --manifest-path apps/runtime/Cargo.toml --locked
cargo test --manifest-path apps/runtime/Cargo.toml --locked
cargo build --manifest-path apps/runtime/Cargo.toml --locked --release
```

Runtime links the actual Core source through a relative Cargo dependency while
retaining its existing C ABI. Core and Runtime have separate Cargo workspaces.
No separately prepared Core artifact, credential, clean checkout, or input
provenance file is required.

The normal native ONNX Runtime dependency and Windows ML package are distinct
third-party inputs. GNU CI verifies a public Microsoft ONNX Runtime package,
revision and notices on Ubuntu 22.04 for both architectures. Use `ORT_LIB_PATH`
to point at an absolute verified ONNX Runtime installation for Intel macOS,
musl or custom native builds. Windows retains dynamic MSVC CRT; musl requires
the target C++ toolchain and static CRT. Optional Apple Silicon MLX builds
use an absolute `MLX_RS_METAL_PATH`.

The Cargo build stages selected native runtime libraries and records their
SHA-256 digests. Distributions must retain the applicable third-party notices.
