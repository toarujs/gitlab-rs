//! Project ACL helpers aligned with GitLab CE 19.3.1 Files API.

use sha2::{Digest, Sha256};

pub const VISIBILITY_PRIVATE: i32 = 0;
pub const VISIBILITY_INTERNAL: i32 = 10;
pub const VISIBILITY_PUBLIC: i32 = 20;
pub const ACCESS_GUEST: i32 = 10;
pub const ACCESS_REPORTER: i32 = 20;

/// Repository feature disabled. Matches `ProjectFeature::DISABLED`.
pub const FEATURE_DISABLED: i32 = 0;

/// Project feature visible to members only. Matches `ProjectFeature::PRIVATE`.
pub const FEATURE_PRIVATE: i32 = 10;
/// Project feature enabled for anyone who can read the project.
pub const FEATURE_ENABLED: i32 = 20;

/// Whether a logged-in user can read repository files.
/// Guest (10) on a private project cannot; Reporter+ can.
/// Internal/public projects are readable to any logged-in user.
pub fn can_read_code(visibility_level: i32, access_level: Option<i32>) -> bool {
    if visibility_level >= VISIBILITY_INTERNAL {
        return true;
    }
    access_level.unwrap_or(0) >= ACCESS_REPORTER
}

/// Whether a logged-in user can read CI job traces / builds.
/// Guest members can read builds; non-members cannot read private projects.
/// `builds_access_level == PRIVATE` restricts traces to members even on public projects.
pub fn can_read_build(
    visibility_level: i32,
    builds_access_level: Option<i32>,
    public_builds: bool,
    access_level: Option<i32>,
) -> bool {
    let builds = builds_access_level.unwrap_or(FEATURE_ENABLED);
    if builds <= FEATURE_DISABLED {
        return false;
    }
    let member = access_level.unwrap_or(0) >= ACCESS_GUEST;
    if builds == FEATURE_PRIVATE {
        return member;
    }
    if visibility_level <= VISIBILITY_PRIVATE {
        return member;
    }
    if !public_builds {
        return member;
    }
    true
}

pub fn repository_disabled(repository_access_level: Option<i32>) -> bool {
    matches!(repository_access_level, Some(level) if level == FEATURE_DISABLED)
}

/// GitLab hashed storage path: SHA256 of the decimal project id.
pub fn hashed_disk_path(project_id: i64) -> String {
    let digest = hex::encode(Sha256::digest(project_id.to_string().as_bytes()));
    format!("@hashed/{}/{}/{}.git", &digest[..2], &digest[2..4], digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_cannot_read_private() {
        assert!(!can_read_code(VISIBILITY_PRIVATE, Some(ACCESS_GUEST)));
        assert!(!can_read_code(VISIBILITY_PRIVATE, None));
        assert!(!can_read_code(VISIBILITY_PRIVATE, Some(0)));
    }

    #[test]
    fn reporter_can_read_private() {
        assert!(can_read_code(VISIBILITY_PRIVATE, Some(ACCESS_REPORTER)));
        assert!(can_read_code(VISIBILITY_PRIVATE, Some(30)));
        assert!(can_read_code(VISIBILITY_PRIVATE, Some(50)));
    }

    #[test]
    fn logged_in_can_read_internal_and_public() {
        assert!(can_read_code(VISIBILITY_INTERNAL, None));
        assert!(can_read_code(VISIBILITY_PUBLIC, Some(ACCESS_GUEST)));
        assert!(can_read_code(VISIBILITY_PUBLIC, None));
    }

    #[test]
    fn guest_member_can_read_private_builds() {
        assert!(can_read_build(
            VISIBILITY_PRIVATE,
            Some(FEATURE_ENABLED),
            true,
            Some(ACCESS_GUEST)
        ));
    }

    #[test]
    fn non_member_cannot_read_private_builds() {
        assert!(!can_read_build(
            VISIBILITY_PRIVATE,
            Some(FEATURE_ENABLED),
            true,
            None
        ));
        assert!(!can_read_build(
            VISIBILITY_PRIVATE,
            Some(FEATURE_ENABLED),
            true,
            Some(0)
        ));
    }

    #[test]
    fn disabled_builds_are_denied() {
        assert!(!can_read_build(
            VISIBILITY_PUBLIC,
            Some(FEATURE_DISABLED),
            true,
            Some(ACCESS_REPORTER)
        ));
    }

    #[test]
    fn hashed_path_is_stable() {
        let path = hashed_disk_path(1);
        assert!(path.starts_with("@hashed/"));
        assert!(path.ends_with(".git"));
        assert_eq!(path, hashed_disk_path(1));
        assert_ne!(hashed_disk_path(1), hashed_disk_path(2));
    }
}
