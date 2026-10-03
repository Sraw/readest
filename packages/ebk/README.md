# EBK reader for Readest

EBK is a container for e-books made from EPUB: the EPUB's files, byte for byte, with text compressed with brotli
in blocks and JPEG pictures recompressed with Lepton. Readest reads `.ebk` files with the module built here
(`apps/readest-app/src/libs/ebk/`) and renders them as the EPUB they hold.

- `crates/ebk`: the reader of the format (the writer and the tests are in the EBK repository,
  https://github.com/Sraw/ebk).
- `crates/ebk-wasm`: the reader as WebAssembly, with a plain C interface; the worker that drives it is
  `apps/readest-app/src/workers/ebk.worker.ts`.
- `third_party/lepton_jpeg`: the JPEG codec that the format's storage mode 4 is defined by, a fork of
  `lepton_jpeg` 0.5.8 (Apache-2.0, Microsoft), its decoder only; see its `EBK-CHANGES.md`.

`./build.sh` rebuilds `ebk.wasm` (needs the Rust target `wasm32-unknown-unknown`); the built file is committed.

Licence: `crates/` MIT or Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`); `third_party/lepton_jpeg` Apache-2.0.
