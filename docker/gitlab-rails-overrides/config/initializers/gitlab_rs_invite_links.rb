# frozen_string_literal: true

# gitlab-rs overlay: admin one-time invite links for self-registration.
#   POST /-/gitlab-rs/admin/invite_links
#   GET/POST /-/gitlab-rs/invite/:token

module GitlabRs
  module InviteLinks
    REDIS_PREFIX = 'gitlab-rs:invite-link:'
    TTL_SECONDS = 7 * 24 * 60 * 60
    TOKEN_PATTERN = /\A[A-Za-z0-9_-]{16,128}\z/.freeze

    class << self
      def redis(&block)
        Gitlab::Redis::SharedState.with(&block)
      end

      def key(token)
        "#{REDIS_PREFIX}#{token}"
      end

      def store!(payload)
        token = SecureRandom.urlsafe_base64(24)
        json = Gitlab::Json.dump(payload)
        redis { |r| r.set(key(token), json, nx: true, ex: TTL_SECONDS) }
        token
      end

      def read(token)
        return unless token.to_s.match?(TOKEN_PATTERN)

        raw = redis { |r| r.get(key(token)) }
        return if raw.blank?

        Gitlab::Json.parse(raw)
      rescue JSON::ParserError
        nil
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
    end
  end
end

module GitlabRs
  class InviteLinksMiddleware
    ADMIN_PATH = %r{\A/-/gitlab-rs/admin/invite_links/?\z}.freeze
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

      if (match = INVITE_PATH.match(path))
        action = method == 'POST' ? 'accept' : 'show'
        return dispatch(env, action, token: match[1]) unless method == 'HEAD' && action == 'show'

        return dispatch(env, 'show', token: match[1])
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

Rails.application.config.after_initialize do
  next if defined?(GitlabRs::InviteLinksController) && GitlabRs::InviteLinksController < ApplicationController

  controller = Class.new(ApplicationController) do
    skip_before_action :authenticate_user!, only: [:show, :accept], raise: false
    skip_before_action :check_password_expiration, only: [:show, :accept], raise: false
    skip_before_action :rejected_user, only: [:show, :accept], raise: false

    layout :gitlab_rs_invite_layout

    def create
      unless current_user&.admin?
        return render json: { message: '403 Forbidden' }, status: :forbidden
      end

      access_level = params[:access_level].to_s
      access_level = 'regular' unless %w[regular admin].include?(access_level)
      external = ActiveModel::Type::Boolean.new.cast(params[:external])

      payload = {
        'access_level' => access_level,
        'external' => external,
        'can_create_group' => access_level == 'admin' ? true : !external,
        'private_profile' => ActiveModel::Type::Boolean.new.cast(params[:private_profile]),
        'projects_limit' => invite_projects_limit,
        'created_by_id' => current_user.id,
        'organization_id' => invite_organization_id
      }

      token = GitlabRs::InviteLinks.store!(payload)
      render json: {
        url: GitlabRs::InviteLinks.public_url(token),
        expires_in_days: 7
      }, status: :created
    end

    def show
      @token = params[:token].to_s
      @invite = GitlabRs::InviteLinks.read(@token)
      @user = User.new
      if @invite.blank?
        @invite_invalid = true
      end

      render template: 'gitlab_rs/invite_links/show', formats: [:html]
    end

    def accept
      @token = params[:token].to_s
      if params[:password].to_s != params[:password_confirmation].to_s
        @invite = GitlabRs::InviteLinks.read(@token)
        @invite_invalid = @invite.blank?
        @user = User.new(name: params[:name], username: params[:username], email: params[:email])
        @errors = [s_('GitlabRs|Password confirmation does not match.')]
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      payload = GitlabRs::InviteLinks.consume!(@token)
      if payload.blank?
        @invite_invalid = true
        @user = User.new
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :not_found
      end

      creator = User.find_by(id: payload['created_by_id']) if payload['created_by_id'].present?
      creator ||= User.where(admin: true).order(:id).first

      attrs = {
        name: params[:name].to_s,
        username: params[:username].to_s,
        email: params[:email].to_s,
        password: params[:password].to_s,
        password_confirmation: params[:password_confirmation].to_s,
        access_level: payload['access_level'],
        external: payload['external'],
        can_create_group: payload['can_create_group'],
        private_profile: payload['private_profile'],
        projects_limit: payload['projects_limit'],
        skip_confirmation: true,
        organization_id: payload['organization_id']
      }.compact

      response = Users::CreateService.new(creator, attrs).execute
      @user = response.payload && response.payload[:user]

      unless response.success?
        GitlabRs::InviteLinks.restore!(@token, payload)
        @invite = payload
        @user ||= User.new(attrs.except(:password, :password_confirmation, :skip_confirmation, :organization_id))
        @errors = Array(response.message)
        return render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :unprocessable_entity
      end

      redirect_to new_user_session_path, notice: s_('GitlabRs|Account created. You can sign in now.')
    rescue Gitlab::Access::AccessDeniedError
      GitlabRs::InviteLinks.restore!(@token, payload) if payload
      @invite_invalid = true
      @user = User.new
      render template: 'gitlab_rs/invite_links/show', formats: [:html], status: :forbidden
    end

    private

    def gitlab_rs_invite_layout
      action_name == 'create' ? false : 'devise'
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
  end

  GitlabRs.const_set(:InviteLinksController, controller)
end
