# Pichi's imago integration branch

This repository preserves the upstream imago history and MIT license. Upstream
is https://gitlab.com/hreitz/imago; the initial base is
`ff96e18ce872bb3dec4f1f2d178fc41dc89d3bf6` (0.2.6).

Dillo uses native synchronous imago (`default-features = false`, `sync`) through
its storage boundary. Pichi-specific VM, configuration and virtio code belongs
in dillo, not in this library. Library changes should remain useful to other
imago consumers and preserve the default async API.

Keep each change focused, include a regression test and its rationale, and
record the upstream issue or merge request when submitted. Preserve upstream
attribution and license notices. Do not publish a crates.io release under the
upstream package name. Consumers must pin a reviewed commit, not a moving branch.

Upstream submission is a separate step after review and validation. No upstream
issue or merge request has been opened by setting up this repository.

## Reclamation patch

`Qcow2OpenBuilder::reclaim(true)` opts into required host reclamation. The default
remains unchanged. `Storage::pure_reclaim` must return an error if unsupported;
`StorageExt::reclaim` provides an exclusive, alignment-checked entry point.
The file backend uses Linux/macOS hole punching and Windows sparse zero-data
operations. Required reclamation of external data files is currently refused.

The allocator settles outstanding writes, flushes removed mappings, and syncs
before destructive reclamation. It punches only last-reference data clusters.
It retains their refcount until the punch completes, while holding the allocator
lock. Thus a failed/cancelled punch can leak a cluster but cannot release it for
reuse while the operation might still complete. Shared data and metadata are not
punched. Errors reach the format caller. Offline checking/repair may be necessary
after an error; the patch is not a general qcow2 crash-consistency certification.

Validation commands:

```
cargo test --lib
cargo test --no-default-features --features sync --lib
cargo test --no-default-features --features sync --test reclaim_qemu -- --ignored
cargo clippy --all-targets -- -D warnings
cargo clippy --no-default-features --features sync --all-targets -- -D warnings
```

The explicitly selected QEMU test fails if its tools are unavailable. It checks
host allocation, snapshot retention, reuse and `qemu-img check`. Unit tests cover
sync/reclaim errors, persisted mapping removal and cancelled asynchronous punches.
Full storage-reordering/torn-write testing, native host runtime lanes and upstream
API review remain necessary before treating this branch as production-qualified.
