# Server release, install, upgrade, and removal

Tagged releases use the `v<workspace-version>` convention. The release workflow
builds `lynx-world` for Linux x86-64 and Windows x86-64, puts the binary,
`BUILD-INFO.txt`, and the operations guide in an archive, and uploads a detached
SHA-256 checksum beside it. `BUILD-INFO.txt` records the exact tag, engine
version, protocol version, and whether content was bundled. Content is currently
deployed separately and is therefore recorded as `not-bundled`.

## Install and run

1. Download the archive and its `.sha256` file from the matching release.
2. Verify the checksum (`sha256sum -c <file>.sha256` on Linux; compare
   `Get-FileHash -Algorithm SHA256 <file>` on Windows).
3. Extract into a dedicated service directory with a writable snapshot path and
   no credentials in the archive.
4. Read `BUILD-INFO.txt`, configure secrets through the OS environment or a
   secret store, then run `lynx-world serve` on loopback behind the documented
   trusted proxy boundary.
5. Confirm `/readyz` and `/metrics` only on the private admin listener.

## Upgrade and removal

Follow [operations deploy and rollback](operations.md#deploy-and-rollback):
review `CHANGELOG.md` for breaking protocol or migration entries, preserve the
previous verified archive, apply only forward-compatible migrations, smoke the
new artifact in staging, and roll back content before runtime code. Stop the
service before replacing its binary; do not delete durable snapshots or the
database as part of an uninstall. To remove the server, stop and unregister the
OS service, delete the extracted release directory, then separately retain or
delete data according to the operator's retention policy.
