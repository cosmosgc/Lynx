# Security reporting guidance

This is interim guidance, not a complete vulnerability reporting and response
policy. The repository does not currently document a verified private reporting
channel or response commitments. The remaining decisions are tracked in
[AUTH-001](docs/issues/AUTH-SecurityDocumentationGap.md).

## Reporting a vulnerability

Please do not publish vulnerability details in a public issue, pull request, or
discussion. Do not include credentials, tokens, personal data, or sensitive
production data in a report. If you already have a verified private way to
contact the maintainers, use it; this document cannot identify or verify one.
Otherwise, retain the details privately until a channel is documented here.

When a private channel is available, useful information includes the affected
version or commit, the potential impact, and minimal reproduction steps using
test data. Avoid testing against systems or data without authorization.

## Response expectations

No acknowledgement or remediation timeline, supported-version policy, or
disclosure process has been established in this document. Do not assume that a
report has been received or that a fix will be available by a particular date.
Maintainers must establish and document these expectations alongside a verified
private reporting channel.

For the project's security design, see [security and moderation](docs/security.md).
