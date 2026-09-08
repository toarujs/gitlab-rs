# frozen_string_literal: true

# gitlab-rs: disable official GitLab Version Check / Usage Ping phone-home.
# Official CE still ships VersionCheck (version.gitlab.com). This instance
# pins a known CE version and must not fetch or display upstream update notices.
module GitlabRsDisableOfficialVersionCheck
  def url(*)
    nil
  end

  def response
    nil
  end
end

Rails.application.config.to_prepare do
  if defined?(VersionCheck)
    VersionCheck.singleton_class.prepend(GitlabRsDisableOfficialVersionCheck)
  end
end
