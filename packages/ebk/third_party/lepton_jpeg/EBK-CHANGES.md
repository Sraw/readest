# The EBK fork of lepton_jpeg

This directory is a fork of `lepton_jpeg` 0.5.8 (upstream: https://github.com/microsoft/lepton_jpeg_rust, tag
`v0.5.8`, commit 90fdc27828676892fbb41777cfcc6bad1e470516, directory `lib/`, as published on crates.io), licensed
under Apache-2.0 (`LICENSE.txt`, `NOTICE.txt`, from the same tag). Every changed source file says so in its first
lines.

**This copy, in Readest, holds only the decoder.** The encoder (`lepton_encoder.rs`, `lepton_file_writer.rs`,
`vpx_bool_writer.rs`, `jpeg/jpeg_read.rs`, `jpeg/bit_reader.rs`), what only it used, the tests and the benchmarks
are left out, as are upstream's `DESIGN.md`, the files of the crates.io package and the `git-version` and test
dependencies. What is kept is unchanged but for the imports and items that only the left-out code used, and the
blank lines around them. Changes 2, 3 and 19 below are in files left out of this copy. The tools, the tests, the
test corpus and the specification named below are in the EBK repository, https://github.com/Sraw/ebk.

EBK storage mode 4 is defined as "what this implementation decodes" (specification, section 7), and the decoder
runs on files from anywhere, in a browser among other places. Upstream was not written for that. So the code is
maintained here, under two rules:

- **The coded stream does not change.** What the decoder gives for a Lepton file it accepts stays what 0.5.8
  gives, and what the encoder writes for a JPEG stays what 0.5.8 writes, with one exception: where 0.5.8 writes
  a file that does not decode back to the JPEG, the fork may write another one, in the same format, that 0.5.8
  decodes back to the JPEG (change 19). Checked after every change by `tools/lepton_compare.py`, which runs the
  fork and the crate from crates.io side by side on every JPEG of the test corpus (4,421 files: the same Lepton
  bytes from both encoders, the same JPEG back from both decoders); by the files in
  `crates/ebk/tests/data/*.lep`; and by `tools/ebk_check.py`, which decodes what the converter wrote with the
  unchanged crate.
- **A damaged or hostile file gives an error.** Not a panic, an overflow, an allocation taken from a number in
  the file, or work out of proportion to the file. Found by review and with the fuzz targets `lepton` (the
  decoder alone), `lepton_header` (the same, with the header given before deflate so that its records can be
  changed), `jpeg` (through the EBK reader) and `jpeg_encode` (the encoder, through the EBK writer), which are
  built with overflow checks and debug assertions and count every panic as a failure; inputs that failed are
  kept in `fuzz/regressions/`.

What the fork does not do: store kinds of JPEG that the format has no place for (four components, sampling
factors over two, 12 bits, arithmetic coding, lossless). That needs another coded stream, which the published
decoder could not read; `tools/lepton-check` (`why`) says for a given file which of these it is.

## Changes

Portability and reproducible output:

1. `src/metrics.rs`: `CpuTimeMeasure` does not call `std::time::Instant::now()` on `wasm32-unknown-unknown`
   (there is no clock there and the call panics); elapsed time is zero on that target.
2. `src/structs/lepton_file_writer.rs`: the wall-clock measurement uses `CpuTimeMeasure` for the same reason.
3. `src/structs/lepton_file_writer.rs`: `GIT_VERSION` is the constant `"0"` instead of the output of
   `git describe` in the build directory. Upstream writes four bytes of it into every Lepton header, so the
   encoder's output depended on the repository the crate was built in. `"0"` is what a build from crates.io
   gives. The `git-version` dependency is removed from `Cargo.toml`.

Memory and work bounded by the file:

4. `src/structs/lepton_header.rs`: three buffers whose length is read from the file (the JPEG header, the
   restart-error list, the bytes after the image) are filled by reading, growing with the data, instead of being
   allocated at their declared length first (up to 4 GiB for a file of a few dozen bytes).
5. `src/enabled_features.rs`, `src/jpeg/jpeg_header.rs`: a new limit `max_jpeg_pixels` (off by default), and a
   check that the image has no more 8x8 blocks than `max_jpeg_file_size` has bytes. The decoder allocates the
   coefficients of the whole image from the declared dimensions before it reads any image data, and decodes
   every block whether or not there is data for it. This is the one change that refuses files upstream
   accepts: images of less than one byte per block (nearly blank ones; 2 of 4,421 in the EBK corpus), which
   EBK keeps as plain JPEG.
6. `src/structs/lepton_header.rs`: a Lepton file whose JPEG header ends before the first scan is refused.
   Upstream ignores the parser's "no scan" result and decodes with component sizes that were never computed,
   which ends in an allocation of 512 GiB.
7. `src/structs/lepton_header.rs`: the inflated header is bounded by the length of the JPEG file plus 64 KiB,
   and its buffers are reserved fallibly. The header is a zlib stream and its records may repeat: 800 KB of it
   inflated to 800 MiB. (This also removes an assertion that nothing is left after the header.)

Panics and overflows turned into errors (each was reached by a fuzz target):

8. `src/structs/lepton_header.rs`: a header without any thread handoff is refused (upstream indexes the list at
   "last", that is at -1); the lengths taken off the file size are subtracted with checks; thread handoffs whose
   first rows go backwards are refused (upstream subtracts a part's first row from its last).
9. `src/structs/thread_handoff.rs`: more than 8 overhang bits are refused (the bit writer subtracts the count
   from 64).
10. `src/jpeg/block_based_image.rs`: `merge` returns an error where upstream asserts that the parts of the image
    fit together; `get_block` subtracts with wrapping, which is what upstream does in release builds (a position
    before the part reads as an empty block).
11. `src/jpeg/bit_writer.rs`, `src/jpeg/jpeg_write.rs`: writing a value that does not fit its number of bits - a
    coefficient too large for the codes of the JPEG's Huffman table - sets a flag that the row writer turns into
    an error. Upstream asserts this in debug builds only and writes other bits in release builds. (A symbol that
    the table has no code for, with a value that fits, is still written as bits without a code; the output is
    then not the JPEG, which the length and checksum of the EBK member show.)
12. `src/structs/lepton_file_reader.rs`: the bytes after the image are taken off the file size with a check.
13. `src/jpeg/jpeg_write.rs`: the difference of two DC coefficients is taken with wrapping (as in upstream's
    release builds; out of range it has no Huffman code, see 11); the search for the last coefficient of a
    progressive refinement scan no longer counts a `u8` down past zero.

14. `src/jpeg/block_based_image.rs`, `src/structs/lepton_decoder.rs`: appending a block to a part of the image
    that is full is an error (upstream asserts, in release builds too). A damaged file reaches it with a thread
    handoff at a row where a subsampled component has no boundary.
15. `src/jpeg/jpeg_write.rs`: a list of restart counts shorter than the number of scans no longer indexes past
    its end (upstream panics, in release builds too); the DC difference of a progressive scan is taken with
    wrapping, as in 13.
16. `src/structs/lepton_file_reader.rs`: the index of a restart marker written after the image is added with
    wrapping. `src/jpeg/truncate_components.rs`: one is added to the last block position of a truncated file
    with saturation.
20. `src/structs/lepton_header.rs`: a JPEG header with four components is refused when the Lepton file is read,
    as the encoder refuses it (upstream asserts three when it works out the rows, in release builds too).
21. `src/structs/lepton_file_reader.rs`: a progressive scan of AC coefficients with more than one component is
    refused, as the encoder refuses it (upstream's writer checks the runs of empty blocks of such a scan against
    one component's table, with a debug assertion). The parts of a progressive image are freed once they are
    merged; upstream keeps their memory, a second copy of the coefficients, while it writes the scans.

Refusals that upstream does not have (a reader of EBK depends on them; the specification states them):

17. `src/structs/lepton_file_reader.rs`, `src/consts.rs` (`MAX_SCAN_WORK`): in a progressive image the number of
    scans times the number of blocks of all components is at most 16 times the length of the JPEG file.
    Upstream writes every scan with a pass over the whole image however many there are, and a scan costs a
    damaged file a few bytes of header that deflate away: 500 bytes kept the decoder busy for 23 seconds.
    This also refuses real files: with the usual ten scans, progressive files under 0.16 bits per pixel
    (4:2:2) or 0.23 (4:4:4); for 4:2:0 change 5 refuses more (0.125). EBK's writer stores those as they are.
18. `src/structs/lepton_header.rs`: one list of thread handoffs, of at most 16 entries (the limit the format
    has for the encoder). Upstream appends every list it finds and decodes each entry into an image of its own.

Changes 5 and 7 are refusals of this kind too.

A file the encoder got wrong:

19. `src/jpeg/jpeg_read.rs`: a baseline JPEG that ends with its last block, without the end-of-image marker.
    The decoder takes "no bytes after the image" to mean "ends with the marker" and writes the marker over the
    last two bytes, so upstream encodes such a file to something that decodes to other bytes. The fork stores
    the last two bytes as bytes after the image, which is what upstream does for a file that ends in the middle
    of the image, and both decoders give the file back. A progressive file without the marker is still encoded
    as upstream does (EBK's writer notices that it does not come back and stores the file as it is).

## Comparing with upstream

The changed files have Unix line endings here and DOS line endings in the published crate: compare with
`diff --strip-trailing-cr`. `Cargo.toml.orig`, `Cargo.lock`, `t1.txt` and `t2.txt` are part of the published
crate. Fixes from later upstream versions are not merged automatically; a change that alters the coded stream
cannot be taken at all.

## Settings EBK uses

Encoding and decoding both use `EnabledFeatures::compat_lepton_vector_write()` (Huffman tables the JPEG standard
does not allow and quantisation tables with zeros are refused, no side over 16386 pixels), with one partition and
one thread; the decoder also gets `max_jpeg_file_size` = the length of the JPEG and `max_jpeg_pixels`
(`crates/ebk/src/jpeg.rs`).
