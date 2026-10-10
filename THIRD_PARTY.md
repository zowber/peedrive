# Third-party code

The SDK helper directly imports Proton's Drive SDK and selected CLI integration
modules from https://github.com/ProtonDriveApps/sdk at commit
`8ffae007b0fbb8bcef53514719d6f093bb32dcf9`.
That repository is MIT licensed, copyright Proton AG. Its license accompanies
the Debian package. No Proton logos or application branding are used.

The SDK's MIT source license does not grant rights to Proton's hosted service.
This alpha identifies itself as `external-drive-peedrive@0.1.0-alpha` and uses
the `external-drive` account client ID. It uses only official Proton endpoints.
Proton currently describes SDK experimentation as personal and non-commercial.
See https://github.com/ProtonDriveApps/sdk#usage-guidelines-for-personal-projects.

The helper embeds Bun, and uses @protontech/crypto, @noble/hashes, ky and ttag.
Exact JavaScript dependencies are in bun.lock; exact Rust dependencies and
their sources are in Cargo.lock. Dependency license files are available in
node_modules and the Cargo registry after bootstrap. The native UI links GTK4
and libadwaita supplied by the operating system.

The package includes collected dependency notices and Bun 1.4.2's license
notice. It also includes the uncompiled helper bundle at
`/usr/lib/peedrive/peedrive-adapter.js`, which can be run with a separately built
or modified Bun runtime. Bun source is available at
https://github.com/oven-sh/bun/tree/bun-v1.4.2 and the full helper source and
dependency lockfiles are in this project.
