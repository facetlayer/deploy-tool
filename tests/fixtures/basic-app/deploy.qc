# Sample project used by the end-to-end suites in crates/*/tests.
#
# __DEST_URL__ is substituted with the test server's address when the fixture
# is copied into a temp directory, because the port is chosen at runtime.

deploy-settings
  project-name=basic-app
  dest-url=__DEST_URL__
  update-in-place

include **
exclude node_modules/**

# Destination-only state. Without this rule the leftover scan in finishUploads
# would treat it as orphaned and delete it — the 2026-05-23 incident.
ignore server-data
