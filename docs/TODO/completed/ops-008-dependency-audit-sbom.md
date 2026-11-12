# OPS-008 — Dependency and license auditing

**Completed:** 2026-11-12

Pull requests and pushes run the pinned `cargo-deny 0.19.0` policy against the
lockfile. The policy blocks known applicable advisories, unapproved licenses,
wildcard requirements, and unapproved dependency sources. Duplicate versions
are reported for review. Exception policy requires a rationale, responsible
owner, and expiry; there are currently no exceptions. Security updates have
documented two-business-day triage and severity-based remediation targets.

The tagged release workflow installs pinned `cargo-cyclonedx 0.5.9`, generates
a CycloneDX 1.5 JSON SBOM for the release target, embeds it in each Linux and
Windows archive, and publishes a separate SBOM asset beside the archive and
checksum. The SBOM covers Rust crate dependencies; OS libraries and separately
deployed content are outside its scope.

**Verification:**

- Locally installed the pinned generator under the ignored `target/` directory.
- Generated the Lynx World SBOM for `x86_64-pc-windows-msvc`; the output was
  valid CycloneDX 1.5 JSON with 182 listed components.
- Confirmed the generator emits `lynx-world.cdx.json` for the selected Cargo
  manifest, with the binary target represented in that package SBOM; generated
  workspace SBOM files were removed after inspection.
- Release workflow and operator documentation now name the same pinned tool,
  format, target behavior, and archive/asset locations.

All four OPS-008 acceptance criteria are checked in
[`60-operations-release.md`](../60-operations-release.md).
