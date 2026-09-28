# frozen_string_literal: true

# gitlab-rs: reconstruct CE 19.3.2 commits/files file.path handling
# (CVE-2026-85706 class) and cap untrusted CI regex length
# (CVE-2026-89078 / 93577 class). Native Onigmo RCE still needs CE 19.3.3.

module GitlabRsUploadedFilePathGuard
  ALLOWED_PREFIXES = %w[
    /var/opt/gitlab/gitlab-rails/shared
    /var/opt/gitlab/gitlab-rails/tmp
    /opt/gitlab/embedded/service/gitlab-rails/tmp
  ].freeze

  def from_params(*args, **kwargs)
    params = args.first
    path = extract_upload_path(params)
    gitlab_rs_assert_upload_path!(path) if path
    super
  end

  private

  def extract_upload_path(params)
    return unless params.respond_to?(:[])

    params['path'] || params[:path] || params['file.path'] || params[:'file.path']
  end

  def gitlab_rs_assert_upload_path!(path)
    full = File.expand_path(path.to_s)
    allowed = ALLOWED_PREFIXES.any? { |prefix| full == prefix || full.start_with?("#{prefix}/") }
    raise ArgumentError, 'upload path is not allowed' unless allowed
  end
end

module GitlabRsUntrustedRegexpGuard
  MAX_BYTES = 4096

  def initialize(pattern, ...)
    raise RegexpError, 'regular expression is too long' if pattern.to_s.bytesize > MAX_BYTES

    super
  end
end

Rails.application.config.to_prepare do
  if defined?(UploadedFile) && UploadedFile.respond_to?(:from_params)
    UploadedFile.singleton_class.prepend(GitlabRsUploadedFilePathGuard)
  end

  if defined?(Gitlab::UntrustedRegexp)
    Gitlab::UntrustedRegexp.prepend(GitlabRsUntrustedRegexpGuard)
  end
end
