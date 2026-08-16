# VM lifecycle research archive

Historical experiments for fresh-instance pools, cache sharding, retained instances and related VM
lifecycle ideas live here. They are not the normal production/runtime path.

ConflictLab's canonical performance runs use benchmark-scoped retained instance reuse with a
non-binding gas meter, backed by fresh/recycle semantic controls. Do not promote archived VM ideas
without a new correctness design and the current Phase-5F acceptance gates.

`ARCHIVE_MANIFEST.txt` preserves the original historical artifact filenames verbatim, including the
old naming, so archived checksums/paths are not rewritten.
