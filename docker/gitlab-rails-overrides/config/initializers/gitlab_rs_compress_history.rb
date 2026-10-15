# frozen_string_literal: true

# gitlab-rs overlay: compress git history from the project page.
#   POST /api/v4/projects/:id/repository/compress_history
#   mode=gc     -> Repositories::HousekeepingService :eager (Maintainer+)
#   mode=squash -> rewrite default branch to 1 commit (Owner, confirm path_with_namespace)

module GitlabRs
  class CompressHistoryService
    EmptyRepository = Class.new(StandardError)
    SquashFailed = Class.new(StandardError)

    def initialize(project, user, commit_message: nil)
      @project = project
      @user = user
      @commit_message = commit_message.presence || 'Compressed git history'
    end

    def squash!
      repo = @project.repository
      raise EmptyRepository, 'empty repository' if repo.nil? || repo.empty?

      branch = repo.root_ref
      head = repo.commit
      raise EmptyRepository, 'no HEAD' unless branch && head

      initial = repo.initial_commit
      if initial && initial.id == head.id
        return {
          mode: 'squash',
          status: 'noop',
          sha: head.id,
          branch: branch,
          message: 'already a single root commit'
        }
      end

      new_sha = create_root_commit!(repo, head)
      raise SquashFailed, 'squash produced no commit' if new_sha.blank?

      old_sha = head.id
      update_branch!(repo, branch, new_sha, old_sha)
      expire_caches!(repo)
      schedule_prune!

      {
        mode: 'squash',
        status: 'ok',
        sha: new_sha,
        old_sha: old_sha,
        branch: branch
      }
    end

    private

    def create_root_commit!(repo, head)
      tree_id = head.tree_id
      raise SquashFailed, 'HEAD has no tree' if tree_id.blank?

      git_dir = git_dir_for(repo)
      gitaly = '/opt/gitlab/embedded/bin/gitaly'
      config = '/var/opt/gitlab/gitaly/config.toml'
      raise SquashFailed, 'gitaly binary missing' unless File.executable?(gitaly)
      raise SquashFailed, 'gitaly config missing' unless File.readable?(config)
      raise SquashFailed, "git dir missing for #{repo.raw.relative_path}" unless git_dir

      name = @user.name.to_s.presence || 'GitLab'
      email = @user.email.to_s.presence || 'git@gitlab.local'
      env = {
        'GIT_AUTHOR_NAME' => name,
        'GIT_AUTHOR_EMAIL' => email,
        'GIT_COMMITTER_NAME' => name,
        'GIT_COMMITTER_EMAIL' => email
      }
      cmd = [
        gitaly, 'git', '-c', config, '--',
        "--git-dir=#{git_dir}",
        'commit-tree', tree_id.to_s,
        '-m', @commit_message.to_s
      ]
      output, status = Gitlab::Popen.popen(cmd, nil, env)
      sha = output.to_s.strip
      unless status == 0 && sha.match?(/\A[0-9a-f]{40,64}\z/)
        raise SquashFailed, "commit-tree failed (status=#{status}): #{output}"
      end

      sha
    end

    def update_branch!(repo, branch, new_sha, old_sha)
      repo.raw.update_branch(branch, user: @user, newrev: new_sha, oldrev: old_sha)
    rescue Gitlab::Git::PreReceiveError, Gitlab::Git::CommandError, GRPC::BadStatus
      repo.raw.write_ref("refs/heads/#{branch}", new_sha, old_ref: old_sha)
    end

    def expire_caches!(repo)
      repo.expire_branches_cache if repo.respond_to?(:expire_branches_cache)
      repo.expire_root_ref_cache if repo.respond_to?(:expire_root_ref_cache)
      repo.expire_all_method_caches if repo.respond_to?(:expire_all_method_caches)
      return unless defined?(::ProjectCacheWorker)

      ::ProjectCacheWorker.perform_async(@project.id, [], %w[repository_size])
    end

    def schedule_prune!
      ::Repositories::HousekeepingService.new(@project, :prune).execute
    rescue ::Repositories::HousekeepingService::LeaseTaken
      nil
    end

    def git_dir_for(repo)
      relative = repo.raw.relative_path
      roots = []
      cfg = '/var/opt/gitlab/gitaly/config.toml'
      if File.readable?(cfg)
        File.read(cfg).scan(/^\s*path\s*=\s*"([^"]+)"/) { roots << ::Regexp.last_match(1) }
      end
      roots << '/var/opt/gitlab/git-data/repositories'
      roots.uniq.each do |root|
        path = File.join(root, relative)
        return path if File.directory?(path)
      end
      nil
    end
  end
end

module API
  class GitlabRsCompressHistory < ::API::Base
    before { authenticate! }

    feature_category :source_code_management

    helpers ::API::Helpers

    helpers do
      def gitlab_rs_confirm_phrase_matches?(phrase, expected)
        return false if phrase.blank? || expected.blank?
        return false if phrase.bytesize != expected.bytesize

        ActiveSupport::SecurityUtils.secure_compare(phrase, expected)
      rescue ArgumentError
        false
      end
    end

    resource :projects do
      desc 'Compress git history (gc or squash to one commit)' do
        detail 'gitlab-rs overlay: run git gc or rewrite the default branch to a single commit'
        success code: 201
        failure [
          { code: 400, message: 'Bad request' },
          { code: 401, message: 'Unauthorized' },
          { code: 403, message: 'Forbidden' },
          { code: 409, message: 'Conflict' }
        ]
        tags %w[projects]
      end
      params do
        requires :id, types: [String, Integer], desc: 'The ID or URL-encoded path of the project'
        requires :mode, type: String, values: %w[gc squash], desc: '`gc` to run housekeeping, `squash` to rewrite to one commit'
        optional :confirm_phrase, type: String, desc: 'Must equal path_with_namespace when mode=squash'
        optional :commit_message, type: String, desc: 'Commit message for the squashed commit'
      end
      post ':id/repository/compress_history' do
        authorize_admin_project

        project = user_project
        mode = params[:mode].to_s

        if mode == 'gc'
          begin
            ::Repositories::HousekeepingService.new(project, :eager).execute do
              next unless defined?(::Gitlab::Audit::Auditor)

              ::Gitlab::Audit::Auditor.audit(
                name: 'manually_trigger_housekeeping',
                author: current_user,
                scope: project,
                target: project,
                message: 'Housekeeping task: eager (compress history gc)',
                created_at: DateTime.current
              )
            end
          rescue ::Repositories::HousekeepingService::LeaseTaken => e
            conflict!(e.message)
          end

          status 201
          { mode: 'gc', status: 'scheduled' }
        else
          unless Ability.allowed?(current_user, :remove_project, project)
            forbidden!('Only project owners can squash history into one commit')
          end

          expected = project.path_with_namespace.to_s
          unless gitlab_rs_confirm_phrase_matches?(params[:confirm_phrase].to_s, expected)
            error!({ message: 'confirm phrase mismatch' }, 400)
          end

          begin
            result = ::GitlabRs::CompressHistoryService.new(
              project,
              current_user,
              commit_message: params[:commit_message]
            ).squash!
          rescue ::GitlabRs::CompressHistoryService::EmptyRepository => e
            error!({ message: e.message }, 400)
          rescue ::GitlabRs::CompressHistoryService::SquashFailed => e
            error!({ message: e.message }, 500)
          end

          status 201
          result
        end
      end
    end
  end
end

Rails.application.config.after_initialize do
  next unless defined?(API::API) && defined?(API::GitlabRsCompressHistory)

  already = API::API.routes.any? { |r| r.origin.to_s.include?('compress_history') }
  API::API.mount ::API::GitlabRsCompressHistory unless already
end

module GitlabRs
  class CompressHistoryMiddleware
    PATH = %r{\A/-/gitlab-rs/projects/([^/]+)/compress_history/?\z}.freeze

    def initialize(app)
      @app = app
    end

    def call(env)
      if env['REQUEST_METHOD'] == 'POST' && env['PATH_INFO'] =~ PATH
        env = env.dup
        env['action_dispatch.request.path_parameters'] = {
          controller: 'gitlab_rs/compress_history',
          action: 'create',
          id: CGI.unescape(Regexp.last_match(1))
        }
        return GitlabRs::CompressHistoryController.action(:create).call(env)
      end

      @app.call(env)
    end
  end
end

Rails.application.config.middleware.use GitlabRs::CompressHistoryMiddleware

Rails.application.config.after_initialize do
  next if defined?(GitlabRs::CompressHistoryController) && GitlabRs::CompressHistoryController < ApplicationController

  controller = Class.new(ApplicationController) do
    before_action :authenticate_user!

    def create
      project = find_gitlab_rs_project
      unless project
        return render json: { message: '404 Not found' }, status: :not_found
      end
      unless can?(current_user, :admin_project, project)
        return render json: { message: '403 Forbidden' }, status: :forbidden
      end

      mode = params[:mode].to_s
      case mode
      when 'gc'
        begin
          ::Repositories::HousekeepingService.new(project, :eager).execute
        rescue ::Repositories::HousekeepingService::LeaseTaken => e
          return render json: { message: e.message }, status: :conflict
        end
        render json: { mode: 'gc', status: 'scheduled' }, status: :created
      when 'squash'
        unless can?(current_user, :remove_project, project)
          return render json: { message: 'Only project owners can squash history into one commit' }, status: :forbidden
        end

        expected = project.path_with_namespace.to_s
        phrase = params[:confirm_phrase].to_s
        unless phrase.bytesize == expected.bytesize && ActiveSupport::SecurityUtils.secure_compare(phrase, expected)
          return render json: { message: 'confirm phrase mismatch' }, status: :bad_request
        end

        begin
          result = ::GitlabRs::CompressHistoryService.new(
            project,
            current_user,
            commit_message: params[:commit_message]
          ).squash!
        rescue ::GitlabRs::CompressHistoryService::EmptyRepository => e
          return render json: { message: e.message }, status: :bad_request
        rescue ::GitlabRs::CompressHistoryService::SquashFailed => e
          return render json: { message: e.message }, status: :internal_server_error
        end

        render json: result, status: :created
      else
        render json: { message: 'unknown mode' }, status: :bad_request
      end
    end

    private

    def find_gitlab_rs_project
      id = params[:id].to_s
      if id.match?(/\A\d+\z/)
        Project.find_by(id: id.to_i)
      else
        Project.find_by_full_path(id)
      end
    end
  end

  GitlabRs.const_set(:CompressHistoryController, controller)
end
