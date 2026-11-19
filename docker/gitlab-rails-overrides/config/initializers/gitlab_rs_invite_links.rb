# frozen_string_literal: true

# gitlab-rs overlay: admin one-time invite links for self-registration.
#   POST /-/gitlab-rs/admin/invite_links
#   GET  /-/gitlab-rs/invite/:token
#   POST /-/gitlab-rs/invite/:token/email
#   POST /-/gitlab-rs/invite/:token/verify
#   POST /-/gitlab-rs/invite/:token

module GitlabRs
  module InviteLinks
    REDIS_PREFIX = 'gitlab-rs:invite-link:'
    TTL_SECONDS = 24 * 60 * 60
    CODE_TTL_SECONDS = 10 * 60
    RESEND_SECONDS = 60
    MAX_CODE_ATTEMPTS = 5
    TOKEN_PATTERN = /\A[A-Za-z0-9_-]{16,128}\z/.freeze
    SETTINGS_KEY = 'gitlab-rs:invite-settings'
    MIN_EXPIRY_HOURS = 1
    MAX_EXPIRY_HOURS = 720
    DEFAULT_EXPIRY_HOURS = 24

    class << self
      def redis(&block)
        Gitlab::Redis::SharedState.with(&block)
      end

      def key(token)
        "#{REDIS_PREFIX}#{token}"
      end

      def store!(payload, ttl = TTL_SECONDS)
        token = SecureRandom.urlsafe_base64(24)
        write!(token, payload, ttl)
        token
      end

      def store_many!(payloads, ttl = TTL_SECONDS)
        payloads = Array(payloads)
        return [] if payloads.empty?

        tokens = payloads.map { SecureRandom.urlsafe_base64(24) }
        redis do |r|
          if r.respond_to?(:pipelined)
            r.pipelined do |pipe|
              tokens.each_with_index do |token, index|
                pipe.set(key(token), Gitlab::Json.dump(payloads[index]), ex: ttl)
              end
            end
          else
            tokens.each_with_index do |token, index|
              r.set(key(token), Gitlab::Json.dump(payloads[index]), ex: ttl)
            end
          end
        end
        tokens
      end

      def write!(token, payload, ttl)
        redis { |r| r.set(key(token), Gitlab::Json.dump(payload), ex: ttl) }
      end

      def remaining_ttl(token)
        ttl = redis { |r| r.ttl(key(token)) }.to_i
        ttl.positive? ? ttl : TTL_SECONDS
      end

      def read(token)
        return unless token.to_s.match?(TOKEN_PATTERN)

        raw = redis { |r| r.get(key(token)) }
        return if raw.blank?

        Gitlab::Json.parse(raw)
      rescue JSON::ParserError
        nil
      end

      def update!(token, payload)
        return unless token.to_s.match?(TOKEN_PATTERN)

        write!(token, payload, remaining_ttl(token))
      end

      def consume!(token)
        return unless token.to_s.match?(TOKEN_PATTERN)

        raw = redis do |r|
          if r.respond_to?(:getdel)
            r.getdel(key(token))
          else
            value = r.get(key(token))
            r.del(key(token)) if value
            value
          end
        end
        return if raw.blank?

        Gitlab::Json.parse(raw)
      rescue JSON::ParserError
        nil
      end

      def restore!(token, payload, ttl = TTL_SECONDS)
        return unless token.to_s.match?(TOKEN_PATTERN)

        redis { |r| r.set(key(token), Gitlab::Json.dump(payload), nx: true, ex: ttl) }
      end

      def public_url(token)
        base = Gitlab.config.gitlab.url.to_s.chomp('/')
        "#{base}/-/gitlab-rs/invite/#{token}"
      end

      def generate_code
        format('%06d', SecureRandom.random_number(1_000_000))
      end

      def email_taken?(email)
        normalized = email.to_s.strip.downcase
        return true if normalized.blank?

        if User.respond_to?(:find_by_any_email)
          User.find_by_any_email(normalized).present?
        else
          User.where('LOWER(email) = ?', normalized).exists?
        end
      end

      def send_code_email!(email, code)
        subject = s_('GitlabRs|Your email verification code')
        body = format(
          s_('GitlabRs|Your GitLab verification code is %{code}. It expires in 10 minutes.'),
          code: code
        )
        send_plain_email!(email, subject, body)
      end

      def deliver_via_smtp!(from, email, subject, body)
        require 'net/smtp'
        require 'time'
        require 'base64'

        host = Gitlab.config.gitlab.host.to_s.presence || 'localhost'
        gateway = smtp_gateway
        raise 'smtp gateway missing' if gateway.blank?

        rfc822 = <<~MAIL
          From: #{from}
          To: #{email}
          Subject: #{encode_mail_header(subject)}
          Date: #{Time.now.rfc2822}
          Content-Type: text/plain; charset=UTF-8

          #{body}
        MAIL

        Net::SMTP.start(gateway, 25, helo: host, starttls: false, tls_verify: false) do |smtp|
          smtp.send_message(rfc822, from, email)
        end
      end

      def smtp_gateway
        env_addr = ENV['GITLAB_RS_SMTP_ADDRESS'].to_s.strip
        return env_addr if env_addr.present?

        File.readlines('/proc/net/route').drop(1).each do |line|
          _iface, dest, gateway, = line.split
          next unless dest == '00000000' && gateway.to_s != '00000000'

          return gateway.scan(/../).reverse.map { |part| part.to_i(16) }.join('.')
        end
        nil
      end

      def encode_mail_header(value)
        text = value.to_s
        return text if text.ascii_only?

        "=?UTF-8?B?#{Base64.strict_encode64(text)}?="
      end

      def parse_expiry_hours(raw)
        return DEFAULT_EXPIRY_HOURS if raw.nil? || raw.to_s.strip.empty?

        text = raw.to_s.strip
        return :invalid unless text.match?(/\A0*[1-9]\d*\z/)

        hours = text.to_i
        return :invalid unless hours.between?(MIN_EXPIRY_HOURS, MAX_EXPIRY_HOURS)

        hours
      end

      def parse_anonymous_count(raw)
        return 0 if raw.nil? || raw.to_s.strip.empty?

        text = raw.to_s.strip
        return :invalid unless text.match?(/\A\d+\z/)

        text.to_i
      end

      def parse_emails(raw)
        lines = raw.is_a?(Array) ? raw : raw.to_s.split(/\r?\n/)
        lines.flat_map { |line| line.to_s.split(/[,;]+/) }.map { |email| email.to_s.strip.downcase }.reject(&:blank?)
      end

      def cast_flag(raw)
        value = raw.is_a?(Array) ? raw.last : raw
        ActiveModel::Type::Boolean.new.cast(value) == true
      end

      def normalize_domain(domain)
        text = domain.to_s.strip.downcase.sub(/\A@/, '').sub(%r{\Ahttps?://}, '')
        text.split(%r{[/:?#]}).first.to_s.sub(/\.\z/, '')
      end

      def settings
        raw = redis { |r| r.get(SETTINGS_KEY) }
        parsed = raw.present? ? Gitlab::Json.parse(raw) : {}
        {
          'allowlist_enabled' => cast_flag(parsed['allowlist_enabled']),
          'allowlist_domains' => Array(parsed['allowlist_domains']).map { |domain| normalize_domain(domain) }.reject(&:blank?)
        }
      rescue JSON::ParserError
        { 'allowlist_enabled' => false, 'allowlist_domains' => [] }
      end

      def save_settings_from_params!(params)
        return unless params.key?(:gitlab_rs_invite_allowlist_enabled) || params.key?(:gitlab_rs_invite_allowlist_domains)

        enabled = cast_flag(params[:gitlab_rs_invite_allowlist_enabled])
        domains = params[:gitlab_rs_invite_allowlist_domains].to_s.split(/[\s,;]+/).map do |domain|
          normalize_domain(domain)
        end.reject(&:blank?).uniq
        redis do |r|
          r.set(SETTINGS_KEY, Gitlab::Json.dump(
            'allowlist_enabled' => enabled,
            'allowlist_domains' => domains
          ))
        end
      end

      def email_reject_reason(email, cfg = nil)
        normalized = email.to_s.strip.downcase
        return 'invalid_email' unless normalized.match?(URI::MailTo::EMAIL_REGEXP)
        return 'email_taken' if email_taken?(normalized)

        cfg ||= settings
        return unless cfg['allowlist_enabled']

        domain = normalized.split('@', 2)[1].to_s
        return 'domain_not_allowed' unless cfg['allowlist_domains'].include?(domain)

        nil
      end

      def send_invite_email!(email, url)
        subject = s_('GitlabRs|Your GitLab invite link')
        body = format(
          s_('GitlabRs|An administrator invited you to GitLab. Open this one-time link to register: %{url}'),
          url: url
        )
        send_plain_email!(email, subject, body)
      end

      def send_plain_email!(email, subject, body)
        from = Gitlab.config.gitlab.email_from.presence || "gitlab@#{Gitlab.config.gitlab.host}"
        begin
          ActionMailer::Base.mail(
            from: from,
            to: email,
            subject: subject,
            body: body,
            content_type: 'text/plain; charset=UTF-8'
          ).deliver_now
        rescue StandardError
          deliver_via_smtp!(from, email, subject, body)
        end
      end

      def create_batch(base_payload, expiry_hours, emails, anonymous_count, send_email)
        ttl = expiry_hours * 3600
        items = []
        seen = {}
        cfg = settings

        store_many!(Array.new(anonymous_count) { base_payload.dup }, ttl).each do |token|
          items << batch_item('anonymous', nil, public_url(token), 'created', nil, false)
        end

        emails.each do |email|
          if seen[email]
            items << batch_item('bound', email, nil, 'skipped', 'duplicate_email', false)
            next
          end

          seen[email] = true
          reason = email_reject_reason(email, cfg)
          if reason
            items << batch_item('bound', email, nil, 'skipped', reason, false)
            next
          end

          token = store!(base_payload.merge('bound_email' => email), ttl)
          url = public_url(token)
          mailed = false
          mail_reason = nil
          if send_email
            begin
              send_invite_email!(email, url)
              mailed = true
            rescue StandardError => e
              Gitlab::AppLogger.error("gitlab-rs invite link email failed: #{e.class}: #{e.message}") if defined?(Gitlab::AppLogger)
              mail_reason = 'mail_failed'
            end
          end
          items << batch_item('bound', email, url, 'created', mail_reason, mailed)
        end

        items
      end

      def batch_item(kind, email, url, status, reason, mailed)
        {
          'kind' => kind,
          'email' => email,
          'url' => url,
          'status' => status,
          'reason' => reason,
          'mailed' => mailed
        }
      end
    end
  end
end

module GitlabRs
  class InviteLinksMiddleware
    ADMIN_PATH = %r{\A/-/gitlab-rs/admin/invite_links/?\z}.freeze
    EMAIL_PATH = %r{\A/-/gitlab-rs/invite/([A-Za-z0-9_-]+)/email/?\z}.freeze
    VERIFY_PATH = %r{\A/-/gitlab-rs/invite/([A-Za-z0-9_-]+)/verify/?\z}.freeze
    INVITE_PATH = %r{\A/-/gitlab-rs/invite/([A-Za-z0-9_-]+)/?\z}.freeze

    def initialize(app)
      @app = app
    end

    def call(env)
      path = env['PATH_INFO'].to_s
      method = env['REQUEST_METHOD']

      if method == 'POST' && path.match?(ADMIN_PATH)
        return dispatch(env, 'create')
      end

      if (match = EMAIL_PATH.match(path)) && method == 'POST'
        return dispatch(env, 'send_code', token: match[1])
      end

      if (match = VERIFY_PATH.match(path)) && method == 'POST'
        return dispatch(env, 'verify_code', token: match[1])
      end

      if (match = INVITE_PATH.match(path))
        action = method == 'POST' ? 'accept' : 'show'
        return dispatch(env, action, token: match[1])
      end

      @app.call(env)
    end

    private

    def dispatch(env, action, token: nil)
      env = env.dup
      params = { controller: 'gitlab_rs/invite_links', action: action }
      params[:token] = token if token
      env['action_dispatch.request.path_parameters'] = params
      GitlabRs::InviteLinksController.action(action).call(env)
    end
  end
end

Rails.application.config.middleware.use GitlabRs::InviteLinksMiddleware

module GitlabRs
  module InviteAllowlistSettings
    def perform_update
      GitlabRs::InviteLinks.save_settings_from_params!(params)
      return super if params[:application_setting].present?

      redirect_to general_admin_application_settings_path(anchor: 'js-signup-settings'),
        notice: _('Application settings saved successfully')
    end
  end
end

Rails.application.config.to_prepare do
  if defined?(Admin::ApplicationSettingsController)
    unless Admin::ApplicationSettingsController.ancestors.include?(GitlabRs::InviteAllowlistSettings)
      Admin::ApplicationSettingsController.prepend(GitlabRs::InviteAllowlistSettings)
    end
  end
end

Rails.application.config.after_initialize do
  next if defined?(GitlabRs::InviteLinksController) && GitlabRs::InviteLinksController < ApplicationController

  controller = Class.new(ApplicationController) do
    skip_before_action :authenticate_user!, only: [:show, :accept, :send_code, :verify_code], raise: false
    skip_before_action :check_password_expiration, only: [:show, :accept, :send_code, :verify_code], raise: false
    skip_before_action :rejected_user, only: [:show, :accept, :send_code, :verify_code], raise: false
    skip_before_action :verify_authenticity_token, only: [:show, :accept, :send_code, :verify_code], raise: false

    layout :gitlab_rs_invite_layout

    def create
      unless current_user&.admin?
        return render json: { message: '403 Forbidden' }, status: :forbidden
      end

      access_level = params[:access_level].to_s
      access_level = 'regular' unless %w[regular admin].include?(access_level)
      flag = ActiveModel::Type::Boolean.new
      external = flag.cast(params[:external]) == true
      send_email = if params.key?(:send_email)
        flag.cast(params[:send_email]) == true
      else
        true
      end

      expiry_hours = GitlabRs::InviteLinks.parse_expiry_hours(params[:expiry_hours])
      if expiry_hours == :invalid
        return render json: {
          message: s_('GitlabRs|Expiry hours must be an integer from 1 to 720.'),
          error: 'invalid_expiry'
        }, status: :unprocessable_entity
      end

      anonymous_count = GitlabRs::InviteLinks.parse_anonymous_count(params[:anonymous_count])
      if anonymous_count == :invalid
        return render json: {
          message: s_('GitlabRs|Anonymous count must be a non-negative integer.'),
          error: 'invalid_anonymous_count'
        }, status: :unprocessable_entity
      end

      emails = GitlabRs::InviteLinks.parse_emails(params[:emails])
      if anonymous_count.zero? && emails.empty?
        return render json: {
          message: s_('GitlabRs|Provide an anonymous count or at least one email.'),
          error: 'empty_batch'
        }, status: :unprocessable_entity
      end

      payload = {
        'access_level' => access_level,
        'external' => external,
        'can_create_group' => access_level == 'admin' ? true : !external,
        'private_profile' => flag.cast(params[:private_profile]) == true,
        'projects_limit' => invite_projects_limit,
        'created_by_id' => current_user.id,
        'organization_id' => invite_organization_id
      }

      items = GitlabRs::InviteLinks.create_batch(payload, expiry_hours, emails, anonymous_count, send_email)
      created = items.find { |item| item['status'] == 'created' }
      render json: {
        url: created && created['url'],
        expires_in_hours: expiry_hours,
        items: items
      }, status: :created
    end

    def show
      assign_invite_from_token
      @user = User.new(email: @verified_email.presence || @pending_email)
      render template: 'gitlab_rs/invite_links/show', formats: [:html]
    end

    def send_code
      assign_invite_from_token
      return render_invalid_invite if @invite_invalid

      bound = @invite['bound_email'].to_s.strip.downcase
      requested = params[:email].to_s.strip.downcase
      if bound.present?
        if requested.present? && requested != bound
          @errors = [s_('GitlabRs|This invite is bound to a different email address.')]
          @user = User.new(email: bound)
          @bound_email = bound
          return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
        end
        email = bound
      else
        email = requested
      end

      reason = GitlabRs::InviteLinks.email_reject_reason(email)
      if reason
        @errors = [invite_email_error_message(reason)]
        @user = User.new(email: email)
        @bound_email = bound.presence
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      sent_at = @invite['email_code_sent_at'].to_i
      if sent_at.positive? && (Time.now.to_i - sent_at) < GitlabRs::InviteLinks::RESEND_SECONDS
        @pending_email = email
        @errors = [s_('GitlabRs|Please wait before requesting another code.')]
        @user = User.new(email: email)
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :too_many_requests
      end

      code = GitlabRs::InviteLinks.generate_code
      begin
        GitlabRs::InviteLinks.send_code_email!(email, code)
      rescue StandardError => e
        @errors = [s_('GitlabRs|Failed to send the verification email.')]
        @pending_email = email
        @user = User.new(email: email)
        Gitlab::AppLogger.error("gitlab-rs invite email failed: #{e.class}: #{e.message}") if defined?(Gitlab::AppLogger)
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :bad_gateway
      end

      @invite['pending_email'] = email
      @invite['email_code'] = code
      @invite['email_code_sent_at'] = Time.now.to_i
      @invite.delete('email_code_attempts')
      @invite.delete('verified_email')
      GitlabRs::InviteLinks.update!(@token, @invite)

      @pending_email = email
      @notice = s_('GitlabRs|We sent a 6-digit code to your email.')
      @user = User.new(email: email)
      render template: 'gitlab_rs/invite_links/show', formats: [:html]
    end

    def verify_code
      assign_invite_from_token
      return render_invalid_invite if @invite_invalid

      email = params[:email].to_s.strip.downcase.presence || @invite['pending_email'].to_s
      code = params[:code].to_s.gsub(/\s+/, '')
      expected = @invite['email_code'].to_s
      sent_at = @invite['email_code_sent_at'].to_i
      expired = sent_at.zero? || (Time.now.to_i - sent_at) > GitlabRs::InviteLinks::CODE_TTL_SECONDS
      email_ok = email.present? && ActiveSupport::SecurityUtils.secure_compare(email, @invite['pending_email'].to_s)
      code_ok = expected.length == code.length && expected.present? &&
        ActiveSupport::SecurityUtils.secure_compare(code, expected)

      unless email_ok && code_ok && !expired
        @pending_email = @invite['pending_email']
        @user = User.new(email: @pending_email)
        attempts = @invite['email_code_attempts'].to_i + 1
        if attempts >= GitlabRs::InviteLinks::MAX_CODE_ATTEMPTS
          @invite.delete('email_code')
          @invite.delete('email_code_attempts')
          GitlabRs::InviteLinks.update!(@token, @invite)
          @errors = [s_('GitlabRs|Too many incorrect codes. Request a new code.')]
          return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :too_many_requests
        end

        @invite['email_code_attempts'] = attempts
        GitlabRs::InviteLinks.update!(@token, @invite)
        @errors = [s_('GitlabRs|The verification code is incorrect or expired.')]
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      @invite['verified_email'] = email
      @invite.delete('email_code')
      @invite.delete('email_code_attempts')
      GitlabRs::InviteLinks.update!(@token, @invite)
      @verified_email = email
      @pending_email = nil
      @user = User.new(email: email)
      @notice = s_('GitlabRs|Email verified. Complete your account.')
      render template: 'gitlab_rs/invite_links/show', formats: [:html]
    end

    def accept
      @token = params[:token].to_s
      payload = nil
      ttl = GitlabRs::InviteLinks::TTL_SECONDS
      if params[:password].to_s != params[:password_confirmation].to_s
        assign_invite_from_token
        @user = User.new(name: params[:name], username: params[:username], email: @verified_email)
        @errors = [s_('GitlabRs|Password confirmation does not match.')]
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      ttl = GitlabRs::InviteLinks.remaining_ttl(@token)
      payload = GitlabRs::InviteLinks.consume!(@token)
      if payload.blank?
        @invite_invalid = true
        @user = User.new
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :not_found
      end

      verified = payload['verified_email'].to_s
      if verified.blank?
        GitlabRs::InviteLinks.restore!(@token, payload, ttl)
        @invite = payload
        @pending_email = payload['pending_email']
        @user = User.new(email: @pending_email)
        @errors = [s_('GitlabRs|Verify your email before creating the account.')]
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      bound = payload['bound_email'].to_s.strip.downcase
      if bound.present? && verified != bound
        GitlabRs::InviteLinks.restore!(@token, payload, ttl)
        @invite = payload
        @bound_email = bound
        @verified_email = verified
        @user = User.new(email: bound)
        @errors = [s_('GitlabRs|This invite is bound to a different email address.')]
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      creator = User.find_by(id: payload['created_by_id']) if payload['created_by_id'].present?
      creator ||= User.where(admin: true).order(:id).first

      flag = ActiveModel::Type::Boolean.new
      attrs = {
        name: params[:name].to_s,
        username: params[:username].to_s,
        email: verified,
        password: params[:password].to_s,
        password_confirmation: params[:password_confirmation].to_s,
        access_level: payload['access_level'],
        external: flag.cast(payload['external']) == true,
        can_create_group: payload['can_create_group'].nil? ? true : flag.cast(payload['can_create_group']) == true,
        private_profile: flag.cast(payload['private_profile']) == true,
        skip_confirmation: true,
        organization_id: payload['organization_id'].presence || invite_organization_id
      }
      attrs[:projects_limit] = payload['projects_limit'] unless payload['projects_limit'].nil?

      response = Users::CreateService.new(creator, attrs).execute
      @user = response.payload && response.payload[:user]

      unless response.success?
        GitlabRs::InviteLinks.restore!(@token, payload, ttl)
        @invite = payload
        @verified_email = verified
        @user ||= User.new(attrs.except(:password, :password_confirmation, :skip_confirmation, :organization_id))
        @errors = Array(response.message)
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      redirect_to new_user_session_path, notice: s_('GitlabRs|Account created. You can sign in now.')
    rescue Gitlab::Access::AccessDeniedError
      GitlabRs::InviteLinks.restore!(@token, payload, ttl) if payload
      @invite_invalid = true
      @user = User.new
      render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :forbidden
    rescue StandardError
      GitlabRs::InviteLinks.restore!(@token, payload, ttl) if payload
      raise
    end

    private

    def gitlab_rs_invite_layout
      action_name == 'create' ? false : 'devise'
    end

    def assign_invite_from_token
      @token = params[:token].to_s
      @invite = GitlabRs::InviteLinks.read(@token)
      @invite_invalid = @invite.blank?
      return if @invite_invalid

      @bound_email = @invite['bound_email'].to_s.strip.downcase.presence
      @pending_email = @invite['pending_email']
      @verified_email = @invite['verified_email']
    end

    def render_invalid_invite
      @user = User.new
      render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :not_found
    end

    def invite_projects_limit
      raw = params[:projects_limit]
      if raw.present?
        raw.to_i.clamp(0, Gitlab::Database::MAX_INT_VALUE)
      else
        Gitlab::CurrentSettings.default_projects_limit
      end
    end

    def invite_organization_id
      if defined?(Organizations::Organization)
        Organizations::Organization.find_by(id: 1)&.id || Organizations::Organization.first&.id
      end
    end

    def invite_email_error_message(reason)
      case reason
      when 'email_taken'
        s_('GitlabRs|This email is already in use.')
      when 'domain_not_allowed'
        s_('GitlabRs|This email domain is not allowed.')
      else
        s_('GitlabRs|Enter a valid email address.')
      end
    end
  end

  GitlabRs.const_set(:InviteLinksController, controller)

  if defined?(Admin::ApplicationSettingsController)
    unless Admin::ApplicationSettingsController.ancestors.include?(GitlabRs::InviteAllowlistSettings)
      Admin::ApplicationSettingsController.prepend(GitlabRs::InviteAllowlistSettings)
    end
  end
end
