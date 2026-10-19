# frozen_string_literal: true

# Upstream GitLab hard-codes CI failure messages in several Ruby classes.
# Translate them at request time so HTML, JSON API and GraphQL all honor the
# user's locale without replacing upstream source files.
module GitlabRs
  module CiI18n
    # Upstream validators and a few models also add hard-coded English
    # validation messages. Translating at ActiveModel::Error#message covers
    # HTML forms, JSON API responses and GraphQL without touching upstream.
    module ValidationMessages
      include FastGettext::Translation

      MIN_BITS_PATTERN = /\Amust be at least (\d+) bits\z/
      FORBIDDEN_KEY_TYPE_PATTERN = /\Atype is forbidden\. Must be (.+)\z/

      def message
        raw_message = super
        return raw_message unless raw_message.is_a?(String)

        case raw_message
        when MIN_BITS_PATTERN
          s_('must be at least %{bits} bits') % { bits: Regexp.last_match(1) }
        when FORBIDDEN_KEY_TYPE_PATTERN
          s_('type is forbidden. Must be %{types}') % { types: Regexp.last_match(1) }
        else
          # cached_find returns nil for unknown msgids, leaving the text intact.
          FastGettext.cached_find(raw_message) || raw_message
        end
      end
    end

    # Service objects return hard-coded English messages that are rendered in
    # flash notices and JSON responses. Translate them on access so every
    # caller honors the current locale without patching upstream services.
    module ServiceResponseMessages
      def message
        GitlabRs::CiI18n.translate_payload(super)
      end
    end

    MESSAGE_KEYS = %w[message error errors].freeze
    FLASH_KEYS = %w[alert notice error warning].freeze

    def self.translate_payload(value)
      case value
      when String
        FastGettext.cached_find(value) || value
      when Array
        value.map { |item| translate_payload(item) }
      when Hash
        localize_json_messages(value)
      else
        value
      end
    end

    def self.localize_json_messages(value, key = nil)
      case value
      when Hash
        value.each_with_object({}) do |(child_key, child_value), result|
          result[child_key] = localize_json_messages(child_value, child_key)
        end
      when Array
        value.map { |item| localize_json_messages(item, key) }
      when String
        MESSAGE_KEYS.include?(key.to_s) ? (FastGettext.cached_find(value) || value) : value
      else
        value
      end
    end

    # Controllers render a few user-facing messages directly as JSON.
    module RenderJsonMessages
      def render(*args, &block)
        args = args.map do |arg|
          next arg unless arg.is_a?(Hash)

          arg = arg.merge(json: GitlabRs::CiI18n.localize_json_messages(arg[:json])) if arg[:json].is_a?(Hash)
          arg = arg.merge(plain: GitlabRs::CiI18n.translate_payload(arg[:plain])) if arg[:plain].is_a?(String)
          arg = arg.merge(text: GitlabRs::CiI18n.translate_payload(arg[:text])) if arg[:text].is_a?(String)
          arg
        end

        super(*args, &block)
      end
    end

    module FlashMessages
      def []=(key, value)
        if GitlabRs::CiI18n::FLASH_KEYS.include?(key.to_s) && value.is_a?(String)
          value = GitlabRs::CiI18n.translate_payload(value)
        end

        super
      end
    end

    module GrapeErrorMessages
      def error!(message, *args)
        super(GitlabRs::CiI18n.translate_payload(message), *args)
      end
    end

    module GrapeExceptionMessages
      def message
        GitlabRs::CiI18n.translate_payload(super)
      end
    end

    module GraphQLErrorMessages
      def initialize(message, ast_node: nil, ast_nodes: nil, options: nil, extensions: nil)
        message = GitlabRs::CiI18n.translate_payload(message) if message.is_a?(String)
        super(message, ast_node: ast_node, ast_nodes: ast_nodes, options: options, extensions: extensions)
      end
    end

    module CommitStatusMessages
      def self.prepended(base)
        base.singleton_class.prepend(ClassMethods)
      end

      module ClassMethods
        def callout_failure_messages
          super.transform_values { |message| message && s_(message) }
        end
      end

      private

      def job_router_failure_msg
        if respond_to?(:error_job_messages) && error_job_messages.any?
          error_job_messages.first.content
        else
          s_('Please contact your administrator.')
        end
      end

      def help_page_link(doc_link)
        ActionController::Base.helpers.link_to(s_('How do I fix it?'), doc_link)
      end
    end

    module PipelineMessages
      def self.prepended(base)
        base.singleton_class.prepend(ClassMethods)
      end

      module ClassMethods
        def failure_reasons
          super.transform_values { |message| message && s_(message) }
        end
      end

      def status_title
        return unless auto_canceled?

        s_('Pipeline is redundant and is auto-canceled by Pipeline #%{id}') % { id: auto_canceled_by_id }
      end
    end

    module BuildMessages
      private

      def tooltip_for_badge(status)
        status.badge_tooltip.sub(/\A[a-z]/) { |char| char.upcase }
      end
    end

    module CompletionReasonMessages
      def self.prepended(base)
        base.singleton_class.prepend(ClassMethods)
      end

      module ClassMethods
        def rules_failure_message
          s_('The resulting pipeline would have been empty. Review the %{rules_link} configuration.') %
            { rules_link: ci_docs_link('rules', 'rules') }
        end

        def workflow_rules_failure_message
          s_('The pipeline did not run. Review the %{workflow_rules_link} configuration for the pipeline.') %
            { workflow_rules_link: ci_docs_link('workflow:rules', 'workflowrules') }
        end
      end
    end

    module FailedStatusMessages
      private

      def failure_reason_message
        s_(self.class.reasons.fetch(subject.failure_reason.to_sym))
      end

      def description
        return "- (#{failure_reason_message})" unless subject.failure_reason.to_sym == :job_router_failure

        custom_message = fetch_custom_message
        if custom_message.present?
          "- (#{failure_reason_message} #{custom_message})"
        else
          "- (#{failure_reason_message} #{s_('Please contact your administrator.')})"
        end
      end
    end

    module PlayManualMessages
      def action_title
        s_('Run all manual')
      end
    end
  end
end

Rails.application.config.to_prepare do
  {
    ::ActiveModel::Error => GitlabRs::CiI18n::ValidationMessages,
    ::ServiceResponse => GitlabRs::CiI18n::ServiceResponseMessages,
    ::ActionController::Base => GitlabRs::CiI18n::RenderJsonMessages,
    ::ActionDispatch::Flash::FlashHash => GitlabRs::CiI18n::FlashMessages,
    ::Grape::DSL::InsideRoute => GitlabRs::CiI18n::GrapeErrorMessages,
    ::Grape::Endpoint => GitlabRs::CiI18n::GrapeErrorMessages,
    ::Grape::Exceptions::Base => GitlabRs::CiI18n::GrapeExceptionMessages,
    ::GraphQL::ExecutionError => GitlabRs::CiI18n::GraphQLErrorMessages,
    ::CommitStatusPresenter => GitlabRs::CiI18n::CommitStatusMessages,
    Ci::PipelinePresenter => GitlabRs::CiI18n::PipelineMessages,
    Ci::BuildPresenter => GitlabRs::CiI18n::BuildMessages,
    Ci::Pipeline => GitlabRs::CiI18n::CompletionReasonMessages,
    Gitlab::Ci::Status::Build::Failed => GitlabRs::CiI18n::FailedStatusMessages,
    Gitlab::Ci::Status::Stage::PlayManual => GitlabRs::CiI18n::PlayManualMessages
  }.each do |klass, mod|
    klass.prepend(mod) unless klass.ancestors.include?(mod)
  end
end
