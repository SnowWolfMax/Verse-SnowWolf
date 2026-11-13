/* versions-bedrock.js — 基岩版下载页
 * 清单来自 MineBBS（Windows 基岩版），下载走 123 云盘分享解析（登录态只保存在本机）。
 */
import { API } from '../../core/api.js';
import { bridge } from '../../core/tauri-bridge.js';
import { escapeHtml, formatDate, formatSize, formatSpeed } from '../../core/utils.js';
import { showToast } from '../misc/ui-components.js';

const S = {
  list: null,
  loading: false,
  sessionId: '',
  mode: '',
  pollTimer: null,
  loginTimer: null,
  pendingInstall: null,
};

export function resetBedrockCache() {
  S.list = null;
}

function closeBedrockModal(id) {
  const modal = document.getElementById(id);
  if (modal) {
    modal.classList.remove('modal-visible');
    setTimeout(() => modal.remove(), 250);
  }
}

function showModal(id, html) {
  closeBedrockModal(id);
  document.body.insertAdjacentHTML('beforeend',
    `<div class="modal" id="${id}" style="display:flex">${html}</div>`);
  requestAnimationFrame(() => document.getElementById(id)?.classList.add('modal-visible'));
}

// 基岩版是 Windows（UWP）专属，macOS / Linux 上直接显示暂不支持
function isWindowsPlatform() {
  const sys = bridge && bridge.system;
  if (sys && typeof sys.isWindows === 'boolean') return sys.isWindows;
  const p = (navigator.platform || '').toLowerCase();
  const ua = (navigator.userAgent || '').toLowerCase();
  return p.indexOf('win') >= 0 || ua.indexOf('windows') >= 0;
}

export async function renderBedrockVersions(container) {
  if (!container) return;

  if (!isWindowsPlatform()) {
    container.innerHTML = `
      <div class="empty-state" style="padding:48px 0;text-align:center">
        <p class="empty-text" style="font-size:14px">基岩版暂不支持当前系统</p>
        <p class="empty-text" style="font-size:12px;margin-top:6px">基岩版只有 Windows 版（UWP 应用），macOS 和 Linux 无法安装和启动</p>
      </div>`;
    return;
  }

  // 正在加载中：继续保持转圈，不能把「还没拿到的数据」当成空列表渲染成「暂无」
  if (S.loading) {
    container.innerHTML = '<div class="loading-spinner"><div class="spinner"></div><p>正在获取基岩版列表...</p></div>';
    return;
  }

  if (!S.list) {
    S.loading = true;
    container.innerHTML = '<div class="loading-spinner"><div class="spinner"></div><p>正在获取基岩版列表...</p></div>';
    try {
      const data = await API.getBedrockVersions(false);
      S.list = (data && data.versions) || [];
    } catch (e) {
      S.loading = false;
      container.innerHTML = `
        <p class="empty-text">加载基岩版列表失败：${escapeHtml(e.message || String(e))}</p>
        <button class="btn btn-secondary btn-sm" id="bedrock-retry-btn" style="margin-top:12px">重试</button>`;
      document.getElementById('bedrock-retry-btn')?.addEventListener('click', () => {
        S.list = null;
        renderBedrockVersions(container);
      });
      return;
    }
    S.loading = false;
    // 用户可能已经切走了，此时容器不在页面上，下一次进入会重新渲染
  }

  const versions = S.list || [];

  container.innerHTML = `
    <div class="bedrock-legal-tip" style="margin-bottom:10px;padding:10px 14px;border-radius:8px;background:var(--red-glow);border:1px solid var(--red);font-size:12px;color:var(--red);font-weight:600;line-height:1.7">
      基岩版主要对正版玩家提供正版启动服务，非正版玩家请在下载 24 小时内卸载，支持正版游戏！
    </div>
    <div class="bedrock-list-tip" style="margin-bottom:12px;padding:10px 14px;border-radius:8px;background:var(--bg-secondary);border:1px solid var(--border-color);font-size:12px;color:var(--text-muted);line-height:1.6">
      版本清单来自 mcappx（data.mcappx.com）。点「下载」会打开 mcappx 版本库，
      找到对应版本后点它旁边的「123云盘」下载，再回来点「从本地包导入」。
      支持 zip 和 appx / msix（走系统部署，需要先开启旁加载）；系统里已有基岩版就点「识别已装的」。
    </div>
    <div style="display:flex;gap:8px;margin-bottom:12px">
      <button class="btn btn-accent btn-sm" id="bedrock-import-btn">从本地包导入</button>
      <button class="btn btn-secondary btn-sm" id="bedrock-adopt-btn">识别已装的</button>
      <button class="btn btn-secondary btn-sm" id="bedrock-refresh-btn">刷新列表</button>
    </div>
    ${versions.length === 0 ? '<p class="empty-text">暂无基岩版版本</p>' : versions.map(v => {
      const link = (v.links && v.links[0]) || '';
      const abstract = (v.abstract || '').replace(/\s+/g, ' ');
      // mcappx 只给日期（2025-09-30），MineBBS 回退时才是完整时间戳
      const dateText = /^\d{4}-\d{2}-\d{2}$/.test(v.time || '')
        ? v.time.replace(/-/g, '/')
        : formatDate(v.time);
      const sizeText = v.size ? ` · ${formatSize(v.size)}` : '';
      return `
      <div class="version-item" data-bedrock-version="${escapeHtml(v.version)}">
        <div class="version-item-left">
          <div class="version-item-icon release"><img src="img/icons/bedrock.webp" alt="基岩版" class="version-icon-img" style="image-rendering:auto;border-radius:8px"></div>
          <div class="version-item-info">
            <span class="version-item-name">${escapeHtml(v.version)}</span>
            <span class="version-item-meta">基岩版 · ${dateText}${sizeText}</span>
            ${abstract ? `<span class="version-item-meta" style="display:block;margin-top:2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;max-width:520px" title="${escapeHtml(abstract)}">${escapeHtml(abstract)}</span>` : ''}
          </div>
        </div>
        <div class="version-item-actions">
          ${v.installed
            ? '<span style="font-size:12px;color:var(--text-muted);padding:6px 10px">已安装</span>'
            : `<button class="btn btn-accent btn-sm" data-bedrock-download="${escapeHtml(v.manifestVersion || v.version)}" data-bedrock-link="${escapeHtml(link)}">下载</button>`}
        </div>
      </div>`;
    }).join('')}
  `;

  container.querySelectorAll('[data-bedrock-download]').forEach(btn => {
    btn.addEventListener('click', () => requestBedrockDownload(btn.dataset.bedrockDownload, btn.dataset.bedrockLink));
  });
  document.getElementById('bedrock-import-btn')?.addEventListener('click', () => importLocalPackage(container));
  document.getElementById('bedrock-adopt-btn')?.addEventListener('click', async (e) => {
    const btn = e.currentTarget;
    const old = btn.textContent;
    btn.disabled = true;
    btn.textContent = '检测中...';
    try {
      const r = await API.bedrockAdoptInstalled();
      if (r && r.success) {
        showToast(`已登记系统里的基岩版 ${r.version || ''}`.trim(), 'success');
        S.list = null;
        renderBedrockVersions(container);
        return;
      }
      showToast((r && r.error) || '系统里没找到基岩版', 'error');
    } catch (err) {
      showToast('检测失败：' + (err.message || err), 'error');
    }
    btn.disabled = false;
    btn.textContent = old;
  });
  document.getElementById('bedrock-refresh-btn')?.addEventListener('click', () => {
    S.list = null;
    renderBedrockVersions(container);
  });
}

async function requestBedrockDownload(version, link) {
  try {
    const r = await API.bedrockOpenShare(version, link || '');
    if (r && r.site === 'mcappx') {
      showToast(`已打开 ${version} 的 mcappx 页面，点「前往云盘」下载，再回来点「从本地包导入」`, 'success');
    } else {
      showToast(`已在浏览器打开 ${version} 的下载页，下载后回来点「从本地包导入」`, 'success');
    }
  } catch (e) {
    showToast('打开下载页失败：' + (e.message || e), 'error');
  }
}

async function importLocalPackage(container) {
  let result;
  try {
    result = await bridge.selectFile({ title: '选择基岩版安装包（zip / appx / msix）' });
  } catch (e) {
    showToast('打开文件选择器失败：' + (e.message || e), 'error');
    return;
  }
  if (!result || result.cancelled || result.canceled || !result.path) return;
  const filePath = result.path;
  if (!/\.(zip|appx|appxbundle|msix|msixbundle)$/i.test(filePath)) {
    showToast('请选择 zip / appx / msix / msixbundle 安装包', 'error');
    return;
  }
  const isPackage = !/\.zip$/i.test(filePath);
  const baseName = (filePath.split(/[\\/]/).pop() || '').replace(/\.(zip|appx|appxbundle|msix|msixbundle)$/i, '');
  const guessed = baseName.replace(/[\\/:*?"<>|]/g, '_').trim() || 'bedrock';
  const input = window.prompt('请输入版本号（用于在列表里标记已安装）', guessed);
  if (input === null) return;
  const version = input.trim() || guessed;

  showToast(isPackage ? '正在交给系统部署，可能要几分钟…' : '正在解压安装，大文件可能需要几十秒…', 'info');
  try {
    const r = await API.bedrockInstallLocal(version, filePath);
    if (!r || !r.success) {
      showToast('安装失败：' + ((r && r.error) || '未知错误'), 'error');
      return;
    }
    // 后端改成异步会话：拿到 sessionId 后开进度弹窗轮询，避免部署几分钟把 HTTP 拖超时
    S.sessionId = r.sessionId;
    S.mode = 'import';
    showProgressModal(version, 'import');
    pollBedrockInstall();
  } catch (e) {
    showToast('安装失败：' + (e.message || e), 'error');
  }
}

function showLoginModal() {
  showModal('bedrock-login-modal', `
    <div class="modal-content" style="max-width:520px">
      <div class="modal-header">
        <h3>登录 123 云盘</h3>
        <button class="modal-close" id="bedrock-login-close">&times;</button>
      </div>
      <div class="modal-body">
        <p style="font-size:13px;color:var(--text-secondary);line-height:1.7;margin-bottom:10px">
          基岩版文件放在 123 云盘分享里。点下方按钮打开登录窗口，登录成功后会自动读回登录态并关闭窗口。
        </p>
        <p style="font-size:12px;color:var(--text-muted);line-height:1.7;margin-bottom:14px">
          登录信息只保存在本机（123pan-token.txt），不经过任何服务器。
        </p>
        <div id="bedrock-login-status" style="font-size:12px;color:var(--text-muted);min-height:20px"></div>
      </div>
      <div class="modal-footer">
        <button class="btn btn-secondary" id="bedrock-login-cancel">取消</button>
        <button class="btn btn-primary" id="bedrock-login-open">打开登录窗口</button>
      </div>
    </div>`);

  const statusEl = document.getElementById('bedrock-login-status');
  const close = () => {
    if (S.loginTimer) { clearInterval(S.loginTimer); S.loginTimer = null; }
    S.pendingInstall = null;
    closeBedrockModal('bedrock-login-modal');
  };
  document.getElementById('bedrock-login-close')?.addEventListener('click', close);
  document.getElementById('bedrock-login-cancel')?.addEventListener('click', close);

  document.getElementById('bedrock-login-open')?.addEventListener('click', async () => {
    const btn = document.getElementById('bedrock-login-open');
    if (btn) btn.disabled = true;
    if (statusEl) statusEl.textContent = '正在打开登录窗口...';
    try {
      await API.bedrockLoginStart();
    } catch (e) {
      if (statusEl) statusEl.textContent = '打开登录窗口失败：' + (e.message || e);
      if (btn) btn.disabled = false;
      return;
    }
    if (S.loginTimer) clearInterval(S.loginTimer);
    S.loginTimer = setInterval(async () => {
      try {
        const st = await API.bedrockLoginStatus();
        if (st.loggedIn) {
          clearInterval(S.loginTimer);
          S.loginTimer = null;
          if (statusEl) statusEl.textContent = '登录成功';
          showToast('123 云盘登录成功', 'success');
          const pending = S.pendingInstall;
          S.pendingInstall = null;
          closeBedrockModal('bedrock-login-modal');
          if (pending) startBedrockInstall(pending.version, pending.link);
        } else if (st.message) {
          if (statusEl) statusEl.textContent = st.message;
        }
      } catch (e) {
        clearInterval(S.loginTimer);
        S.loginTimer = null;
        if (statusEl) statusEl.textContent = '读取登录状态失败：' + (e.message || e);
        if (btn) btn.disabled = false;
      }
    }, 1000);
  });
}

function showProgressModal(version, mode = 'download') {
  S.mode = mode;
  const isImport = mode === 'import';
  showModal('bedrock-progress-modal', `
    <div class="modal-content" style="max-width:520px">
      <div class="modal-header">
        <h3>${isImport ? '安装' : '下载'}基岩版 ${escapeHtml(version)}</h3>
        <button class="modal-close" id="bedrock-progress-close">&times;</button>
      </div>
      <div class="modal-body">
        <div class="install-progress-container">
          <div class="install-info">
            <span id="bedrock-stage" style="font-weight:600">准备中...</span>
            <span id="bedrock-percent" style="font-weight:600;color:var(--accent)">0%</span>
          </div>
          <div class="progress-bar" style="margin-bottom:12px">
            <div class="progress-fill" id="bedrock-progress-fill" style="width:0%"></div>
          </div>
          <div id="bedrock-message" class="install-message" style="margin-bottom:4px"></div>
          <div id="bedrock-speed" class="install-speed"></div>
        </div>
      </div>
      <div class="modal-footer">
        <button id="bedrock-cancel-btn" class="btn ${isImport ? 'btn-secondary' : 'btn-danger'}">${isImport ? '关闭' : '取消下载'}</button>
      </div>
    </div>`);

  document.getElementById('bedrock-progress-close')?.addEventListener('click', closeProgress);
  document.getElementById('bedrock-cancel-btn')?.addEventListener('click', async () => {
    // 导入是系统部署，中途没法取消；这里只收起弹窗，后台继续装
    if (S.mode === 'import') { closeProgress(); return; }
    const btn = document.getElementById('bedrock-cancel-btn');
    if (btn) { btn.disabled = true; btn.textContent = '正在取消...'; }
    try { await API.bedrockInstallCancel(S.sessionId); } catch (e) { /* 忽略 */ }
    setTimeout(closeProgress, 400);
  });
}

function closeProgress() {
  if (S.pollTimer) { clearInterval(S.pollTimer); S.pollTimer = null; }
  S.sessionId = '';
  closeBedrockModal('bedrock-progress-modal');
}

async function startBedrockInstall(version, link) {
  try {
    const r = await API.bedrockInstall(version, link);
    if (!r.success) {
      showToast(r.message || r.error || '无法开始下载', r.alreadyInstalled ? 'info' : 'error');
      return;
    }
    S.sessionId = r.sessionId;
    showProgressModal(version);
    pollBedrockInstall();
  } catch (e) {
    showToast('下载请求失败：' + (e.message || e), 'error');
  }
}

function pollBedrockInstall() {
  if (S.pollTimer) clearInterval(S.pollTimer);
  S.pollTimer = setInterval(async () => {
    try {
      const st = await API.bedrockInstallProgress(S.sessionId);
      const fill = document.getElementById('bedrock-progress-fill');
      const percent = document.getElementById('bedrock-percent');
      const stage = document.getElementById('bedrock-stage');
      const message = document.getElementById('bedrock-message');
      const speed = document.getElementById('bedrock-speed');

      const pct = Math.max(0, Math.min(100, st.progress || 0));
      if (fill) fill.style.width = pct + '%';
      if (percent) percent.textContent = pct + '%';
      if (stage) {
        const map = S.mode === 'import'
          ? { running: '正在安装', completed: '安装完成', failed: '安装失败', cancelled: '已取消' }
          : { running: '正在下载', completed: '安装完成', failed: '安装失败', cancelled: '已取消' };
        stage.textContent = map[st.status] || '正在处理...';
      }
      if (message) message.textContent = st.message || '';
      if (speed) {
        const parts = [];
        if (st.totalBytes > 0) parts.push(`${formatSize(st.downloadedBytes || 0)} / ${formatSize(st.totalBytes)}`);
        else if (st.downloadedBytes > 0) parts.push(formatSize(st.downloadedBytes));
        if (st.speed > 0) parts.push(formatSpeed(st.speed));
        speed.textContent = parts.join(' · ');
      }

      if (st.status === 'completed') {
        clearInterval(S.pollTimer);
        S.pollTimer = null;
        S.sessionId = '';
        showToast('基岩版安装完成', 'success');
        S.list = null;
        document.dispatchEvent(new CustomEvent('versepc:bedrock-installed'));
        setTimeout(() => closeBedrockModal('bedrock-progress-modal'), 1200);
      } else if (st.status === 'failed' || st.status === 'cancelled') {
        clearInterval(S.pollTimer);
        S.pollTimer = null;
        if (st.status === 'failed') {
          const msg = st.error || st.message || '下载失败';
          if (/登录|过期|401|未授权|token/i.test(msg)) {
            API.bedrockLogout().catch(() => {});
            showToast('123 云盘登录态已失效，已清除，请重新点下载并登录', 'error');
          } else {
            showToast(msg, 'error');
          }
          const btn = document.getElementById('bedrock-cancel-btn');
          if (btn) { btn.disabled = false; btn.textContent = '关闭'; btn.onclick = closeProgress; }
        } else {
          setTimeout(closeProgress, 500);
        }
      }
    } catch (e) {
      clearInterval(S.pollTimer);
      S.pollTimer = null;
      showToast('获取下载进度失败：' + (e.message || e), 'error');
    }
  }, 500);
}
