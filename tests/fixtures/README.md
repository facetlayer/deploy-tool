# Test fixtures

Sample projects shared by the end-to-end suites in
`crates/deploy-server/tests` and `crates/deploy-cli/tests`.

A suite copies a fixture into a temp directory before using it, and substitutes
`__DEST_URL__` in the `.qc` file with the test server's address — the port is
chosen at runtime, so it cannot be committed here.

Large files are generated at copy time rather than committed, so the repository
does not carry an 80KB blob just to exercise the multipart upload path.
