Released 2026-07-26.

### Changed

- **Build performance: thin LTO replaces fat LTO.** Fat LTO with a single codegen
  unit serialises whole-program optimisation across the entire dependency tree —
  every translation unit waits for the one before. This repo has a particularly
  heavy tree (kreuzberg with bundled pdfium), so the build cost is significant.
  The runtime benefit for an I/O-bound tool is not measurable. Thin LTO applies
  optimisations per-module (parallelisable, cheaper) at a modest runtime cost
  that never surfaces in wall time between requests. Build time drops from ~5
  minutes to ~45 seconds on the first build; incremental rebuilds are similar.
  Codegen units bumped to 16 (from 1) to parallelise optimisation.
