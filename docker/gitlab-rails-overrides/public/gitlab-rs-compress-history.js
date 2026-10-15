(function () {
  'use strict';

  var root = document.getElementById('js-gitlab-rs-compress-history');
  if (!root || root.dataset.gitlabRsBound === '1') {
    return;
  }
  root.dataset.gitlabRsBound = '1';

  var cfg = {
    projectId: root.dataset.projectId,
    projectPath: root.dataset.projectPath || '',
    apiPath: root.dataset.apiPath,
    canSquash: root.dataset.canSquash === 'true'
  };

  function csrfToken() {
    var meta = document.querySelector('meta[name="csrf-token"]');
    return meta ? meta.getAttribute('content') : '';
  }

  function injectStyles() {
    if (document.getElementById('gitlab-rs-compress-history-style')) {
      return;
    }
    var style = document.createElement('style');
    style.id = 'gitlab-rs-compress-history-style';
    style.textContent = [
      '#gitlab-rs-compress-overlay{position:fixed;inset:0;background:rgba(23,23,23,.48);z-index:10000;display:flex;align-items:center;justify-content:center;padding:16px;}',
      '#gitlab-rs-compress-dialog{background:#fff;color:#181818;border-radius:6px;max-width:520px;width:100%;box-shadow:0 8px 24px rgba(0,0,0,.18);padding:20px 24px;}',
      '#gitlab-rs-compress-dialog h2{margin:0 0 12px;font-size:18px;font-weight:600;}',
      '#gitlab-rs-compress-dialog p{margin:0 0 12px;line-height:1.5;}',
      '#gitlab-rs-compress-dialog label.gitlab-rs-opt{display:flex;gap:8px;align-items:flex-start;margin:8px 0;cursor:pointer;}',
      '#gitlab-rs-compress-confirm-wrap{margin:12px 0 4px;display:none;}',
      '#gitlab-rs-compress-confirm{width:100%;padding:6px 10px;border:1px solid #bfbfbf;border-radius:4px;font-family:inherit;}',
      '#gitlab-rs-compress-msg{min-height:1.4em;margin:8px 0;color:#c91c00;}',
      '#gitlab-rs-compress-footer{display:flex;justify-content:flex-end;gap:8px;margin-top:16px;}'
    ].join('');
    document.head.appendChild(style);
  }

  function selectedMode() {
    var checked = document.querySelector('input[name="gitlab-rs-compress-mode"]:checked');
    return checked ? checked.value : 'gc';
  }

  function syncConfirmUi() {
    var squash = selectedMode() === 'squash';
    var wrap = document.getElementById('gitlab-rs-compress-confirm-wrap');
    var go = document.getElementById('gitlab-rs-compress-go');
    if (wrap) {
      wrap.style.display = squash ? 'block' : 'none';
    }
    if (!go) {
      return;
    }
    if (!squash) {
      go.disabled = false;
      go.className = 'btn btn-confirm btn-md gl-button';
      return;
    }
    go.className = 'btn btn-danger btn-md gl-button';
    if (!cfg.canSquash) {
      go.disabled = true;
      return;
    }
    var input = document.getElementById('gitlab-rs-compress-confirm');
    go.disabled = !input || input.value !== cfg.projectPath;
  }

  function setMessage(text, ok) {
    var el = document.getElementById('gitlab-rs-compress-msg');
    if (!el) {
      return;
    }
    el.textContent = text || '';
    el.style.color = ok ? '#217645' : '#c91c00';
  }

  function closeModal() {
    var overlay = document.getElementById('gitlab-rs-compress-overlay');
    if (overlay) {
      overlay.remove();
    }
    if (escapeHandler) {
      document.removeEventListener('keydown', escapeHandler);
      escapeHandler = null;
    }
  }

  var escapeHandler = null;

  function openModal() {
    if (document.getElementById('gitlab-rs-compress-overlay')) {
      return;
    }
    injectStyles();

    var overlay = document.createElement('div');
    overlay.id = 'gitlab-rs-compress-overlay';
    overlay.setAttribute('role', 'dialog');
    overlay.setAttribute('aria-modal', 'true');

    var dialog = document.createElement('div');
    dialog.id = 'gitlab-rs-compress-dialog';

    var title = document.createElement('h2');
    title.textContent = '压缩 Git 历史';
    dialog.appendChild(title);

    var intro = document.createElement('p');
    intro.textContent = '选择一种操作。将历史压成 1 个提交会改写默认分支，无法恢复。';
    dialog.appendChild(intro);

    var gcLabel = document.createElement('label');
    gcLabel.className = 'gitlab-rs-opt';
    var gcRadio = document.createElement('input');
    gcRadio.type = 'radio';
    gcRadio.name = 'gitlab-rs-compress-mode';
    gcRadio.value = 'gc';
    gcRadio.checked = true;
    gcRadio.addEventListener('change', syncConfirmUi);
    gcLabel.appendChild(gcRadio);
    var gcText = document.createElement('span');
    gcText.textContent = '运行 git gc（清理不可达对象，保留提交历史）';
    gcLabel.appendChild(gcText);
    dialog.appendChild(gcLabel);

    var squashLabel = document.createElement('label');
    squashLabel.className = 'gitlab-rs-opt';
    var squashRadio = document.createElement('input');
    squashRadio.type = 'radio';
    squashRadio.name = 'gitlab-rs-compress-mode';
    squashRadio.value = 'squash';
    squashRadio.disabled = !cfg.canSquash;
    squashRadio.addEventListener('change', syncConfirmUi);
    squashLabel.appendChild(squashRadio);
    var squashText = document.createElement('span');
    squashText.textContent = cfg.canSquash
      ? '将默认分支压成 1 个提交（破坏性操作）'
      : '将默认分支压成 1 个提交（需要 Owner 权限）';
    squashLabel.appendChild(squashText);
    dialog.appendChild(squashLabel);

    var confirmWrap = document.createElement('div');
    confirmWrap.id = 'gitlab-rs-compress-confirm-wrap';
    var confirmHint = document.createElement('p');
    confirmHint.textContent = '请输入完整项目路径 ' + cfg.projectPath + ' 以确认：';
    confirmWrap.appendChild(confirmHint);
    var confirmInput = document.createElement('input');
    confirmInput.id = 'gitlab-rs-compress-confirm';
    confirmInput.type = 'text';
    confirmInput.autocomplete = 'off';
    confirmInput.placeholder = cfg.projectPath;
    confirmInput.addEventListener('input', syncConfirmUi);
    confirmWrap.appendChild(confirmInput);
    dialog.appendChild(confirmWrap);

    var msg = document.createElement('div');
    msg.id = 'gitlab-rs-compress-msg';
    dialog.appendChild(msg);

    var footer = document.createElement('div');
    footer.id = 'gitlab-rs-compress-footer';

    var cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'btn btn-default btn-md gl-button';
    cancel.innerHTML = '<span class="gl-button-text">取消</span>';
    cancel.addEventListener('click', closeModal);

    var go = document.createElement('button');
    go.type = 'button';
    go.id = 'gitlab-rs-compress-go';
    go.className = 'btn btn-confirm btn-md gl-button';
    go.innerHTML = '<span class="gl-button-text">确认</span>';
    go.addEventListener('click', submit);

    footer.appendChild(cancel);
    footer.appendChild(go);
    dialog.appendChild(footer);

    overlay.appendChild(dialog);
    overlay.addEventListener('click', function (ev) {
      if (ev.target === overlay) {
        closeModal();
      }
    });
    if (escapeHandler) {
      document.removeEventListener('keydown', escapeHandler);
    }
    escapeHandler = function (ev) {
      if (ev.key === 'Escape') {
        closeModal();
      }
    };
    document.addEventListener('keydown', escapeHandler);
    document.body.appendChild(overlay);
    syncConfirmUi();
  }

  function submit() {
    var mode = selectedMode();
    var go = document.getElementById('gitlab-rs-compress-go');
    if (go) {
      go.disabled = true;
    }
    setMessage('正在执行…', true);

    var body = { mode: mode };
    if (mode === 'squash') {
      body.confirm_phrase = (document.getElementById('gitlab-rs-compress-confirm') || {}).value || '';
      body.commit_message = 'Compressed git history';
    }

    fetch(cfg.apiPath, {
      method: 'POST',
      credentials: 'same-origin',
      headers: {
        'Content-Type': 'application/json',
        Accept: 'application/json',
        'X-CSRF-Token': csrfToken(),
        'X-Requested-With': 'XMLHttpRequest'
      },
      body: JSON.stringify(body)
    }).then(function (res) {
      return res.json().catch(function () {
        return {};
      }).then(function (data) {
        data = data || {};
        if (!res.ok) {
          var err = data.message || data.error || ('HTTP ' + res.status);
          if (typeof err !== 'string') {
            err = JSON.stringify(err);
          }
          throw new Error(err);
        }
        return data;
      });
    }).then(function (data) {
      if (mode === 'gc') {
        setMessage('已安排 git gc', true);
        setTimeout(closeModal, 1200);
        return;
      }
      var sha = data.sha ? String(data.sha).slice(0, 8) : '';
      setMessage('已压成 1 个提交' + (sha ? ' (' + sha + ')' : ''), true);
      setTimeout(function () {
        window.location.reload();
      }, 1200);
    }).catch(function (err) {
      setMessage(err && err.message ? err.message : String(err), false);
      if (go) {
        go.disabled = false;
        syncConfirmUi();
      }
    });
  }

  function insertButton(container) {
    if (document.getElementById('gitlab-rs-compress-history-btn')) {
      return;
    }
    var btn = document.createElement('button');
    btn.type = 'button';
    btn.id = 'gitlab-rs-compress-history-btn';
    btn.className = 'btn btn-default btn-md gl-button gl-ml-2';
    btn.setAttribute('data-testid', 'gitlab-rs-compress-history');
    var span = document.createElement('span');
    span.className = 'gl-button-text';
    span.textContent = '压缩历史';
    btn.appendChild(span);
    btn.addEventListener('click', openModal);
    container.appendChild(btn);
  }

  function tryInsert() {
    var container = document.querySelector('[data-testid="tree-controls-container"]');
    if (container) {
      var existing = document.getElementById('gitlab-rs-compress-history-btn');
      if (existing && container.contains(existing)) {
        return true;
      }
      if (existing) {
        existing.remove();
      }
      insertButton(container);
      return true;
    }
    return false;
  }

  function boot() {
    tryInsert();
    var header = document.getElementById('js-repository-blob-header-app');
    if (!header || header.dataset.gitlabRsObserved === '1') {
      return;
    }
    header.dataset.gitlabRsObserved = '1';
    var obs = new MutationObserver(function () {
      tryInsert();
    });
    obs.observe(header, { childList: true, subtree: true });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
  document.addEventListener('turbo:load', boot);
})();
