//! Project ACL helpers aligned with GitLab CE 19.3.1 Files API.

use sha2::{Digest, Sha256};

pub const VISIBILITY_PRIVATE: i32 = 0;
pub const VISIBILITY_INTERNAL: i32 = 10;
pub const VISIBILITY_PUBLIC: i32 = 20;
pub const ACCESS_GUEST: i32 = 10;
pub const ACCESS_REPORTER: i32 = 20;
pub const ACCESS_OWNER: i32 = 50;

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
    can_read_code_for(visibility_level, None, access_level, true, false)
}

/// Repository read ACL for anonymous, session, and PAT callers.
/// Public projects are readable without a user. Internal needs a
/// logged-in non-external user. `repository_access_level == PRIVATE`
/// restricts reads to Reporter+ members.
pub fn can_read_code_for(
    visibility_level: i32,
    repository_access_level: Option<i32>,
    access_level: Option<i32>,
    logged_in: bool,
    external_user: bool,
) -> bool {
    if repository_disabled(repository_access_level) {
        return false;
    }
    if access_level.unwrap_or(0) >= ACCESS_REPORTER {
        return true;
    }
    if matches!(repository_access_level, Some(level) if level == FEATURE_PRIVATE) {
        return false;
    }
    if visibility_level >= VISIBILITY_PUBLIC {
        return true;
    }
    if visibility_level >= VISIBILITY_INTERNAL {
        return logged_in && !external_user;
    }
    false
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
    can_read_build_for(
        visibility_level,
        builds_access_level,
        public_builds,
        access_level,
        true,
        false,
    )
}

/// Job-trace ACL for anonymous, session, and PAT callers.
/// Public + `public_builds` is readable without a user. Internal needs a
/// logged-in non-external user (or membership). Private needs Guest+.
pub fn can_read_build_for(
    visibility_level: i32,
    builds_access_level: Option<i32>,
    public_builds: bool,
    access_level: Option<i32>,
    logged_in: bool,
    external_user: bool,
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
    if visibility_level >= VISIBILITY_PUBLIC {
        return public_builds || member;
    }
    if member {
        return true;
    }
    if !logged_in || external_user {
        return false;
    }
    public_builds
}

/// Whether a logged-in user can see a project in `GET /api/v4/projects`.
/// Guest+ members see the project at any visibility.
/// Non-members see public; non-external users also see internal.
/// Private is never shown without membership.
pub fn can_see_project(
    visibility_level: i32,
    access_level: Option<i32>,
    external_user: bool,
) -> bool {
    can_see_project_for(visibility_level, access_level, true, external_user)
}

/// Project-list visibility for anonymous, session, and PAT callers.
pub fn can_see_project_for(
    visibility_level: i32,
    access_level: Option<i32>,
    logged_in: bool,
    external_user: bool,
) -> bool {
    if access_level.unwrap_or(0) >= ACCESS_GUEST {
        return true;
    }
    if visibility_level >= VISIBILITY_PUBLIC {
        return true;
    }
    if visibility_level >= VISIBILITY_INTERNAL {
        return logged_in && !external_user;
    }
    false
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
    fn list_hides_private_from_non_members() {
        assert!(!can_see_project(VISIBILITY_PRIVATE, None, false));
        assert!(!can_see_project(VISIBILITY_PRIVATE, Some(0), false));
        assert!(!can_see_project(VISIBILITY_PRIVATE, None, true));
    }

    #[test]
    fn list_shows_private_to_guest_members() {
        assert!(can_see_project(
            VISIBILITY_PRIVATE,
            Some(ACCESS_GUEST),
            false
        ));
        assert!(can_see_project(VISIBILITY_PRIVATE, Some(ACCESS_OWNER), true));
    }

    #[test]
    fn list_internal_and_public_for_logged_in() {
        assert!(can_see_project(VISIBILITY_INTERNAL, None, false));
        assert!(!can_see_project(VISIBILITY_INTERNAL, None, true));
        assert!(can_see_project(VISIBILITY_INTERNAL, Some(ACCESS_GUEST), true));
        assert!(can_see_project(VISIBILITY_PUBLIC, None, true));
        assert!(can_see_project(VISIBILITY_PUBLIC, None, false));
    }

    #[test]
    fn hashed_path_is_stable() {
        let path = hashed_disk_path(1);
        assert!(path.starts_with("@hashed/"));
        assert!(path.ends_with(".git"));
        assert_eq!(path, hashed_disk_path(1));
        assert_ne!(hashed_disk_path(1), hashed_disk_path(2));
    }

    #[test]
    fn anonymous_can_read_public_only() {
        assert!(can_read_code_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_ENABLED),
            None,
            false,
            false
        ));
        assert!(!can_read_code_for(
            VISIBILITY_INTERNAL,
            Some(FEATURE_ENABLED),
            None,
            false,
            false
        ));
        assert!(!can_read_code_for(
            VISIBILITY_PRIVATE,
            Some(FEATURE_ENABLED),
            None,
            false,
            false
        ));
    }

    #[test]
    fn internal_denies_external_and_anonymous() {
        assert!(can_read_code_for(
            VISIBILITY_INTERNAL,
            None,
            None,
            true,
            false
        ));
        assert!(!can_read_code_for(
            VISIBILITY_INTERNAL,
            None,
            None,
            true,
            true
        ));
    }

    #[test]
    fn private_repo_feature_needs_reporter() {
        assert!(!can_read_code_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_PRIVATE),
            None,
            false,
            false
        ));
        assert!(!can_read_code_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_PRIVATE),
            Some(ACCESS_GUEST),
            true,
            false
        ));
        assert!(can_read_code_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_PRIVATE),
            Some(ACCESS_REPORTER),
            true,
            false
        ));
        assert!(!can_read_code_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_DISABLED),
            Some(ACCESS_OWNER),
            true,
            false
        ));
    }

    #[test]
    fn anonymous_sees_public_projects_only() {
        assert!(can_see_project_for(
            VISIBILITY_PUBLIC,
            None,
            false,
            false
        ));
        assert!(!can_see_project_for(
            VISIBILITY_INTERNAL,
            None,
            false,
            false
        ));
        assert!(!can_see_project_for(
            VISIBILITY_PRIVATE,
            None,
            false,
            false
        ));
    }

    #[test]
    fn anonymous_can_read_public_builds_only() {
        assert!(can_read_build_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_ENABLED),
            true,
            None,
            false,
            false
        ));
        assert!(!can_read_build_for(
            VISIBILITY_PUBLIC,
            Some(FEATURE_ENABLED),
            false,
            None,
            false,
            false
        ));
        assert!(!can_read_build_for(
            VISIBILITY_INTERNAL,
            Some(FEATURE_ENABLED),
            true,
            None,
            false,
            false
        ));
        assert!(!can_read_build_for(
            VISIBILITY_PRIVATE,
            Some(FEATURE_ENABLED),
            true,
            None,
            false,
            false
        ));
    }
}
