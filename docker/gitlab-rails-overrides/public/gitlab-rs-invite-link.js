(function () {
  'use strict';

  var root = document.getElementById('js-gitlab-rs-invite-root');
  var openBtn = document.getElementById('js-gitlab-rs-invite-open');
  if (!root || !openBtn || root.dataset.gitlabRsBound === '1') {
    return;
  }
  root.dataset.gitlabRsBound = '1';

  var createPath = root.dataset.createPath || '/-/gitlab-rs/admin/invite_links';

  function csrfToken() {
    var meta = document.querySelector('meta[name="csrf-token"]');
    return meta ? meta.getAttribute('content') : '';
  }

  function injectStyles() {
    if (document.getElementById('gitlab-rs-invite-style')) {
      return;
    }
    var style = document.createElement('style');
    style.id = 'gitlab-rs-invite-style';
    style.textContent = [
      '#gitlab-rs-invite-overlay{position:fixed;inset:0;background:rgba(23,23,23,.48);z-index:10000;display:flex;align-items:center;justify-content:center;padding:16px;}',
      '#gitlab-rs-invite-dialog{background:#fff;color:#181818;border-radius:6px;max-width:560px;width:100%;box-shadow:0 8px 24px rgba(0,0,0,.18);padding:20px 24px;}',
      '#gitlab-rs-invite-dialog h2{margin:0 0 8px;font-size:18px;font-weight:600;}',
      '#gitlab-rs-invite-dialog p{margin:0 0 12px;line-height:1.5;}',
      '#gitlab-rs-invite-dialog label.gitlab-rs-opt{display:flex;gap:8px;align-items:flex-start;margin:8px 0;cursor:pointer;}',
      '#gitlab-rs-invite-url{width:100%;padding:8px 10px;border:1px solid #bfbfbf;border-radius:4px;font-family:inherit;word-break:break-all;}',
      '#gitlab-rs-invite-msg{min-height:1.4em;margin:8px 0;color:#c91c00;}',
      '#gitlab-rs-invite-footer{display:flex;justify-content:flex-end;gap:8px;margin-top:16px;}'
    ].join('');
    document.head.appendChild(style);
  }

  function closeModal() {
    var overlay = document.getElementById('gitlab-rs-invite-overlay');
    if (overlay) {
      overlay.remove();
    }
  }

  function selectedAccess() {
    var checked = document.querySelector('input[name="gitlab-rs-invite-access"]:checked');
    return checked ? checked.value : 'regular';
  }

  function copyText(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      return navigator.clipboard.writeText(text);
    }
    var area = document.createElement('textarea');
    area.value = text;
    document.body.appendChild(area);
    area.select();
    document.execCommand('copy');
    area.remove();
    return Promise.resolve();
  }

  function openModal() {
    injectStyles();
    closeModal();
    var overlay = document.createElement('div');
    overlay.id = 'gitlab-rs-invite-overlay';
    overlay.innerHTML = [
      '<div id="gitlab-rs-invite-dialog" role="dialog" aria-modal="true">',
      '<h2>一次性邀请链接</h2>',
      '<p>只设置初始权限。用户打开链接后自行填写名称、用户名、邮箱和密码。</p>',
      '<label class="gitlab-rs-opt"><input type="radio" name="gitlab-rs-invite-access" value="regular" checked> Regular（普通用户）</label>',
      '<label class="gitlab-rs-opt"><input type="radio" name="gitlab-rs-invite-access" value="admin"> Administrator（管理员）</label>',
      '<label class="gitlab-rs-opt"><input type="checkbox" id="gitlab-rs-invite-external"> External（外部用户）</label>',
      '<div id="gitlab-rs-invite-result" style="display:none">',
      '<label for="gitlab-rs-invite-url">邀请链接（7 天内一次性有效）</label>',
      '<input id="gitlab-rs-invite-url" readonly>',
      '</div>',
      '<div id="gitlab-rs-invite-msg"></div>',
      '<div id="gitlab-rs-invite-footer">',
      '<button type="button" class="btn btn-default btn-md gl-button" id="gitlab-rs-invite-cancel">关闭</button>',
      '<button type="button" class="btn btn-confirm btn-md gl-button" id="gitlab-rs-invite-copy" style="display:none">复制链接</button>',
      '<button type="button" class="btn btn-confirm btn-md gl-button" id="gitlab-rs-invite-go">生成链接</button>',
      '</div>',
      '</div>'
    ].join('');
    document.body.appendChild(overlay);
    overlay.addEventListener('click', function (event) {
      if (event.target === overlay) {
        closeModal();
      }
    });
    document.getElementById('gitlab-rs-invite-cancel').addEventListener('click', closeModal);
    document.getElementById('gitlab-rs-invite-go').addEventListener('click', generateLink);
    document.getElementById('gitlab-rs-invite-copy').addEventListener('click', function () {
      var url = document.getElementById('gitlab-rs-invite-url').value;
      copyText(url).then(function () {
        document.getElementById('gitlab-rs-invite-msg').textContent = '已复制到剪贴板';
        document.getElementById('gitlab-rs-invite-msg').style.color = '#217645';
      });
    });
  }

  function generateLink() {
    var msg = document.getElementById('gitlab-rs-invite-msg');
    var go = document.getElementById('gitlab-rs-invite-go');
    msg.style.color = '#c91c00';
    msg.textContent = '';
    go.disabled = true;
    fetch(createPath, {
      method: 'POST',
      credentials: 'same-origin',
      headers: {
        'Content-Type': 'application/json',
        'Accept': 'application/json',
        'X-CSRF-Token': csrfToken()
      },
      body: JSON.stringify({
        access_level: selectedAccess(),
        external: document.getElementById('gitlab-rs-invite-external').checked
      })
    }).then(function (res) {
      return res.json().then(function (body) {
        return { ok: res.ok, body: body };
      });
    }).then(function (result) {
      go.disabled = false;
      if (!result.ok || !result.body || !result.body.url) {
        msg.textContent = (result.body && result.body.message) || '生成失败';
        return;
      }
      document.getElementById('gitlab-rs-invite-result').style.display = 'block';
      document.getElementById('gitlab-rs-invite-url').value = result.body.url;
      document.getElementById('gitlab-rs-invite-copy').style.display = 'inline-flex';
      go.style.display = 'none';
      msg.style.color = '#217645';
      msg.textContent = '链接已生成，发给用户即可。';
    }).catch(function () {
      go.disabled = false;
      msg.textContent = '生成失败';
    });
  }

  openBtn.addEventListener('click', function (event) {
    event.preventDefault();
    openModal();
  });
})();
