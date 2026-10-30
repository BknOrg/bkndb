# Persistent repository context

Read `docs/REPO_MEMORY.md` at the start of a new session in this repository. It records the repository's purpose, architecture, usage, build commands, and verified caveats so the user does not need to explain the project again.

- The user requested this persistent context and communicates in Indonesian. Prefer Indonesian explanations unless asked otherwise.
- Treat this memory as an onboarding snapshot, not a replacement for checking the current source. Update it when relevant implementation or workflows change.
- Prefer actual manifests, source, and tests over outdated README examples. Workspace and Python package versions were 0.3.0 when analyzed.
- BknDb is an embedded Rust library, with Python, Go, Kotlin, and Swift bindings. `BknDb::open` defaults to Redb; LSM is optional. Never assume the two storage formats are interchangeable merely because both use `.bkndb`.
- Do not place credentials, private data, or transient machine state in this memory.
