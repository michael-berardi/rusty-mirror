# Security policy

Rusty Mirror is development tooling that runs inside debug builds of your
app. The threat model, and what it deliberately does not cover, is in
[docs/security.md](./docs/security.md).

## Reporting a vulnerability

Please report privately through GitHub's
[security advisories](https://github.com/michael-berardi/rusty-mirror/security/advisories/new)
rather than a public issue. Include the version, platform, and the smallest
reproduction you can manage. We aim to acknowledge reports within a week.

Particularly interesting: any way for the mirror window to cause an effect the
allowlist should have refused, reach the network, persist data, become
visible or focused, or survive into a build without the `mirror-debug`
feature.
