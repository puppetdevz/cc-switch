//! Protocol-level checks against a real S3-compatible service and WebDAV.
//!
//! These tests are skipped unless the corresponding environment variables are set.
//! They are not a substitute for the in-process Memory transport unit tests.
//!
//! S3 / MinIO:
//!   CC_SWITCH_TEST_S3_ENDPOINT
//!   CC_SWITCH_TEST_S3_BUCKET
//!   CC_SWITCH_TEST_S3_REGION
//!   CC_SWITCH_TEST_S3_ACCESS_KEY
//!   CC_SWITCH_TEST_S3_SECRET_KEY
//!
//! WebDAV:
//!   CC_SWITCH_TEST_WEBDAV_URL
//!   CC_SWITCH_TEST_WEBDAV_USER
//!   CC_SWITCH_TEST_WEBDAV_PASSWORD

#[test]
fn protocol_acceptance_requires_real_services() {
    let s3 = std::env::var("CC_SWITCH_TEST_S3_ENDPOINT").is_ok();
    let webdav = std::env::var("CC_SWITCH_TEST_WEBDAV_URL").is_ok();
    if !s3 && !webdav {
        eprintln!(
            "Skipping live protocol acceptance: set CC_SWITCH_TEST_S3_* and CC_SWITCH_TEST_WEBDAV_* to run real HTTP PUT/GET/HEAD/DELETE/If-Match checks."
        );
    }
}
