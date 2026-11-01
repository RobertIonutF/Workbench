---
name: release-preparation
description: "Prepare a reviewable release with version and artifact checks."
---

Inspect the repository's release instructions, version files, changelog, license notices and build configuration. Check that version identifiers agree. Build the requested artifacts using the documented commands and record actual results. Verify archive inventories exclude local credentials, data stores, caches and private logs. Compute SHA-256 hashes of final files. Draft release notes describing concrete behavior, installation and known verification limits. Publishing a release or pushing a branch requires the user's explicit request; return the prepared artifacts and notes when publishing was not requested.
