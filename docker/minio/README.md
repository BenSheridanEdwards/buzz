# Local MinIO test images

Both root Compose stacks build the `minio` and `mc` targets here. They do not
pull the unavailable `quay.io/minio/*:latest` images or substitute a third-party
S3 implementation. Existing credentials, server arguments, ports, data mounts,
`curl` healthchecks, and `/bin/sh` + `mc` bucket initialization remain unchanged.
These are **development/CI dependencies**, not supported production images.

## Source provenance

Official upstream annotated tags were resolved with `git ls-remote` (the peeled
`^{}` value is the source commit). Downloads use the full commit, not the tag,
and the Dockerfile verifies the SHA-256 before extraction/building.

| Component | Official repository / tag | Source commit | Source archive SHA-256 |
| --- | --- | --- | --- |
| Server | https://github.com/minio/minio — `RELEASE.2025-09-07T16-13-09Z` | `07c3a429bfed433e49018cb0f78a52145d4bedeb` | `8819e3e7817e46b7b3798f8f200ead208562e571563c2e040352378031abe9f2` |
| Client | https://github.com/minio/mc — `RELEASE.2025-08-13T08-35-41Z` | `7394ce0dd2a80935aded936b09fa12cbb3cb8096` | `95cd293c7119f16921a6dc515a1fb74a2227f19fd994b9c8b770a154e802ac44` |

Archive URLs are `https://codeload.github.com/minio/<component>/tar.gz/<commit>`.
The build does not modify upstream source or impersonate official release binaries;
`--version` reports `DEVELOPMENT.GOGET`, while OCI revision labels and the verified
source archive identify the exact snapshot. Both snapshots carry the **GNU Affero
General Public License v3** in `LICENSE`; each final image retains that file and
the exact upstream source archive under `/usr/share/doc/<component>/`, alongside
OCI source/revision/license labels. Do not treat these dependencies as Apache-2.0
because Buzz is. Review corresponding-source and dependency-license obligations
before redistributing images; this setup does not publish any images.

The Go 1.24.7 Alpine 3.22 builder and Alpine 3.22.1 runtime are pinned by
multi-platform manifest digest. Go uses the upstream module lockfiles with
`-mod=readonly`, module checksum verification, and `GOTOOLCHAIN=local` (no implicit
compiler upgrade). Alpine package repositories and the Go module download service
remain network dependencies; this is immutable-source pinning, **not a claim of
bit-for-bit reproducible images** or current security support. These historical
community snapshots require deliberate security review before any production use.

## Validate or update

Run `python3 scripts/test-minio-images.py` from the repository root with Docker
Engine and Compose v2 available. It builds and exercises the actual service
configurations from **both** Compose files in unique, disposable projects, with
no published host ports or shared dev volumes. It checks health, successful and
idempotent initialization, S3 put/get/delete, and denied anonymous bucket listing.
It fails on errors and removes only its own project volumes. No existing CI gate
is bypassed.

To update, resolve an official release's peeled commit, download/hash its archive,
review its license and Go requirements, update the Dockerfile and provenance table,
and run that behavioral test. Keep the `curl`, shell and `mc` service contract.
