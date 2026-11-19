(function () {
  'use strict';

  var root = document.getElementById('js-gitlab-rs-invite-root');
  var openBtn = document.getElementById('js-gitlab-rs-invite-open');
  if (!root || !openBtn || root.dataset.gitlabRsBound === '1') {
    return;
  }
  root.dataset.gitlabRsBound = '1';

  var createPath = root.dataset.createPath || '/-/gitlab-rs/admin/invite_links';
  var REASONS = {
    invalid_email: '邮箱格式无效',
    email_taken: '该电子邮件已被使用',
    domain_not_allowed: '邮箱域名不在白名单中',
    duplicate_email: '重复的电子邮件',
    mail_failed: '邀请邮件发送失败',
    empty_batch: '请填写匿名条数或电子邮件',
    invalid_expiry: '过期小时数须为 1 至 720 的整数',
    invalid_anonymous_count: '匿名条数须为非负整数'
  };

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
      '#gitlab-rs-invite-dialog{background:#fff;color:#181818;border-radius:6px;max-width:760px;width:100%;max-height:92vh;overflow:auto;box-shadow:0 8px 24px rgba(0,0,0,.18);padding:20px 24px;}',
      '#gitlab-rs-invite-dialog h2{margin:0 0 8px;font-size:18px;font-weight:600;}',
      '#gitlab-rs-invite-dialog p{margin:0 0 12px;line-height:1.5;}',
      '#gitlab-rs-invite-dialog label.gitlab-rs-opt{display:flex;gap:8px;align-items:flex-start;margin:8px 0;cursor:pointer;}',
      '#gitlab-rs-invite-dialog .gitlab-rs-field{margin:10px 0;}',
      '#gitlab-rs-invite-dialog .gitlab-rs-field label{display:block;margin-bottom:4px;font-weight:600;}',
      '#gitlab-rs-invite-dialog .gitlab-rs-field input[type=number],#gitlab-rs-invite-dialog .gitlab-rs-field textarea{width:100%;padding:8px 10px;border:1px solid #bfbfbf;border-radius:4px;font-family:inherit;box-sizing:border-box;}',
      '#gitlab-rs-invite-dialog .gitlab-rs-field textarea{min-height:88px;resize:vertical;}',
      '#gitlab-rs-invite-dialog .gitlab-rs-help{margin:4px 0 0;color:#626262;font-size:13px;}',
      '#gitlab-rs-invite-receipt{width:100%;border-collapse:collapse;font-size:13px;margin-top:8px;}',
      '#gitlab-rs-invite-receipt th,#gitlab-rs-invite-receipt td{border-bottom:1px solid #eee;padding:6px 8px;text-align:left;vertical-align:top;word-break:break-all;}',
      '#gitlab-rs-invite-receipt-wrap{max-height:240px;overflow:auto;display:none;}',
      '#gitlab-rs-invite-msg{min-height:1.4em;margin:8px 0;color:#c91c00;}',
      '#gitlab-rs-invite-footer{display:flex;justify-content:flex-end;gap:8px;margin-top:16px;flex-wrap:wrap;}',
      '#gitlab-rs-invite-receipt .gitlab-rs-copy-one{padding:2px 8px;font-size:12px;}'
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

  function emailList() {
    return document.getElementById('gitlab-rs-invite-emails').value.split(/\r?\n/).map(function (line) {
      return line.trim();
    }).filter(Boolean);
  }

  function reasonText(reason) {
    return (reason && REASONS[reason]) || reason || '';
  }

  function renderReceipt(items) {
    var wrap = document.getElementById('gitlab-rs-invite-receipt-wrap');
    var body = document.getElementById('gitlab-rs-invite-receipt-body');
    body.innerHTML = '';
    (items || []).forEach(function (item) {
      var tr = document.createElement('tr');
      var kind = item.kind === 'bound' ? '绑定' : '匿名';
      var status = item.status === 'created' ? '成功' : '跳过';
      var mailed = item.kind === 'bound' ? (item.mailed ? '已发信' : '未发信') : '-';
      [kind, item.email || '-', status, mailed, reasonText(item.reason)].forEach(function (value) {
        var td = document.createElement('td');
        td.textContent = value;
        tr.appendChild(td);
      });
      var linkTd = document.createElement('td');
      if (item.url) {
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'btn btn-default btn-sm gl-button gitlab-rs-copy-one';
        btn.textContent = '复制链接';
        btn.addEventListener('click', function () {
          copyText(item.url).then(function () {
            var msgEl = document.getElementById('gitlab-rs-invite-msg');
            if (msgEl) {
              msgEl.style.color = '#217645';
              msgEl.textContent = '已复制链接';
            }
          });
        });
        linkTd.appendChild(btn);
      } else {
        linkTd.textContent = '-';
      }
      tr.appendChild(linkTd);
      body.appendChild(tr);
    });
    wrap.style.display = items && items.length ? 'block' : 'none';
  }

  function createdUrls(items) {
    return (items || []).filter(function (item) {
      return item.status === 'created' && item.url;
    }).map(function (item) {
      return item.url;
    });
  }

  function openModal() {
    injectStyles();
    closeModal();
    var overlay = document.createElement('div');
    overlay.id = 'gitlab-rs-invite-overlay';
    overlay.innerHTML = [
      '<div id="gitlab-rs-invite-dialog" role="dialog" aria-modal="true">',
      '<h2>一次性邀请链接</h2>',
      '<p>设置权限、过期时间和邀请对象。匿名链接由对方填写邮箱；绑定邮箱的链接会锁死该地址。</p>',
      '<label class="gitlab-rs-opt"><input type="radio" name="gitlab-rs-invite-access" value="regular" checked> Regular（普通用户）</label>',
      '<label class="gitlab-rs-opt"><input type="radio" name="gitlab-rs-invite-access" value="admin"> Administrator（管理员）</label>',
      '<label class="gitlab-rs-opt"><input type="checkbox" id="gitlab-rs-invite-external"> External（外部用户）</label>',
      '<div class="gitlab-rs-field"><label for="gitlab-rs-invite-expiry">过期小时数</label>',
      '<input id="gitlab-rs-invite-expiry" type="number" min="1" max="720" step="1" value="24">',
      '<p class="gitlab-rs-help">1 至 720，空白按 24 小时。</p></div>',
      '<div class="gitlab-rs-field"><label for="gitlab-rs-invite-anonymous">匿名邀请条数</label>',
      '<input id="gitlab-rs-invite-anonymous" type="number" min="0" step="1" value="1"></div>',
      '<div class="gitlab-rs-field"><label for="gitlab-rs-invite-emails">绑定邮箱列表</label>',
      '<textarea id="gitlab-rs-invite-emails" placeholder="每行一个邮箱"></textarea>',
      '<p class="gitlab-rs-help">失败项会跳过并写入回执，不设条数上限。</p></div>',
      '<label class="gitlab-rs-opt"><input type="checkbox" id="gitlab-rs-invite-send-email" checked> 向绑定邮箱发送邀请邮件</label>',
      '<div id="gitlab-rs-invite-receipt-wrap">',
      '<table id="gitlab-rs-invite-receipt"><thead><tr><th>类型</th><th>邮箱</th><th>状态</th><th>发信</th><th>原因</th><th>链接</th></tr></thead>',
      '<tbody id="gitlab-rs-invite-receipt-body"></tbody></table></div>',
      '<div id="gitlab-rs-invite-msg"></div>',
      '<div id="gitlab-rs-invite-footer">',
      '<button type="button" class="btn btn-default btn-md gl-button" id="gitlab-rs-invite-cancel">关闭</button>',
      '<button type="button" class="btn btn-default btn-md gl-button" id="gitlab-rs-invite-copy" style="display:none">复制全部链接</button>',
      '<button type="button" class="btn btn-confirm btn-md gl-button" id="gitlab-rs-invite-go">生成</button>',
      '</div></div>'
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
      var urls = (overlay.dataset.urls || '').split('\n').filter(Boolean);
      if (!urls.length) {
        return;
      }
      copyText(urls.join('\n')).then(function () {
        document.getElementById('gitlab-rs-invite-msg').textContent = '已复制到剪贴板';
        document.getElementById('gitlab-rs-invite-msg').style.color = '#217645';
      });
    });
  }

  function generateLink() {
    var msg = document.getElementById('gitlab-rs-invite-msg');
    var go = document.getElementById('gitlab-rs-invite-go');
    var copy = document.getElementById('gitlab-rs-invite-copy');
    var overlay = document.getElementById('gitlab-rs-invite-overlay');
    if (!msg || !go || !overlay) {
      return;
    }
    msg.style.color = '#c91c00';
    msg.textContent = '生成中...';
    go.disabled = true;
    if (copy) {
      copy.disabled = true;
    }
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
        external: document.getElementById('gitlab-rs-invite-external').checked,
        expiry_hours: document.getElementById('gitlab-rs-invite-expiry').value,
        anonymous_count: document.getElementById('gitlab-rs-invite-anonymous').value,
        emails: emailList(),
        send_email: document.getElementById('gitlab-rs-invite-send-email').checked
      })
    }).then(function (res) {
      return res.text().then(function (text) {
        var body = {};
        if (text) {
          try {
            body = JSON.parse(text);
          } catch (err) {
            body = { message: text.slice(0, 200) };
          }
        }
        return { ok: res.ok, body: body };
      });
    }).then(function (result) {
      if (!go.isConnected) {
        return;
      }
      go.disabled = false;
      if (copy) {
        copy.disabled = false;
      }
      if (!result.ok) {
        msg.textContent = (result.body && (REASONS[result.body.error] || result.body.message)) || '生成失败';
        return;
      }
      var items = (result.body && result.body.items) || [];
      renderReceipt(items);
      var urls = createdUrls(items);
      overlay.dataset.urls = urls.join('\n');
      if (copy) {
        copy.style.display = urls.length ? 'inline-flex' : 'none';
      }
      var created = items.filter(function (item) { return item.status === 'created'; }).length;
      var skipped = items.length - created;
      msg.style.color = created ? '#217645' : '#c91c00';
      msg.textContent = '已生成 ' + created + ' 条，跳过 ' + skipped + ' 条，有效期 ' + (result.body.expires_in_hours || 24) + ' 小时。';
    }).catch(function () {
      if (!go.isConnected) {
        return;
      }
      go.disabled = false;
      if (copy) {
        copy.disabled = false;
      }
      msg.textContent = '生成失败';
    });
  }

  openBtn.addEventListener('click', function (event) {
    event.preventDefault();
    openModal();
  });
})();
