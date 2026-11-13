/* page-home.js - 首页 Vue 组件（页面结构层）
 * 主页可移动控件系统：
 *   - 每个控件（软件名/头像/版本卡/启动按钮/运行卡片）均为绝对定位。
 *   - 默认不可拖动；点击右下角「编辑布局」进入编辑模式后，按住控件主体拖动
 *     （移动超过 5px 视为拖动，原地点击保持原有交互）。
 *   - 编辑期间「启动游戏」按钮置灰且所有控件点击被拦截；
 *     只有点击「确认」才会保存位置并恢复启动按钮，点击「取消」恢复原布局。
 *   - 头像：编辑模式下拖动移动 + 滚轮缩放（0.5x ~ 2.0x），默认点击进入账户管理。
 *   - 个性化设置页「恢复主页默认布局」会派发 versepc-home-layout-reset 事件，
 *     本组件监听后清空已保存布局并回到默认位置。
 */

import { gameStore } from '../../core/game-store.js';

import { loadAccounts } from '../../features/accounts/accounts-list.js';
import { appState } from '../../core/app-state.js';
import { updateGameStatus, stopGameInstance } from '../../features/launch/launch.js';
import { loadVersions } from '../../features/versions/versions-list.js';
import { navigateToPage } from '../../core/navigation.js';
import { showToast } from '../../features/misc/ui-components.js';

const LAYOUT_KEY = 'versepc_home_layout_v1';
const AVATAR_MIN = 0.5, AVATAR_MAX = 2.0, DRAG_THRESHOLD = 5;

function loadHomeLayout() {
  try {
    return JSON.parse(localStorage.getItem(LAYOUT_KEY) || '{}') || {};
  } catch (e) { return {}; }
}
function saveHomeLayout(layout) {
  try { localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout)); } catch (e) {}
}
function clampPos(v, min, max) {
  return Math.max(min, Math.min(max, v));
}

const PageHome = {
  data() {
    return {
      layoutReady: false,
      editingLayout: false,
      _draftChanged: {}
    };
  },
  template: `
    <div class="home-page" :class="{ 'home-layout-pending': !layoutReady, 'home-editing': editingLayout }" ref="homePage">
      <!-- 软件名称（编辑模式下按住拖动，点击回主页） -->
      <div class="home-page-title topbar-title" ref="titleEl" data-widget="title"
           :title="editingLayout ? '拖动调整位置' : '点击回主页'">
        Verse<span class="home-page-title-accent">PC2</span>
      </div>

      <!-- 头像控件（编辑模式下拖动 + 滚轮缩放；默认点击进入账户管理） -->
      <div class="home-avatar-widget" ref="avatarEl" data-widget="avatar"
           :title="editingLayout ? '拖动调整位置 · 滚轮缩放' : '点击进入账户'">
        <div class="home-avatar-scale" ref="avatarScale">
          <div class="home-avatar-box" id="home-avatar-box">
            <div class="home-avatar-img-wrap" id="home-avatar"><img src="img/icon.png" alt="" class="home-avatar-img"></div>
          </div>
          <div class="home-account-bar" id="home-account-bar">
            <span class="account-name" id="home-player-name">未登录</span>
            <span class="account-type" id="home-account-type">离线模式</span>
          </div>
        </div>
      </div>

      <!-- 版本列表卡片（编辑模式下按住拖动） -->
      <div class="home-version-card-wrap" ref="versionEl" data-widget="version"
           :title="editingLayout ? '拖动调整位置' : '点击切换版本'">
        <div class="home-current-version-card" id="home-current-version-card" title="点击切换版本">
          <!-- 由 JS 渲染：图标 + 版本名 + 加载器标签 + 右侧箭头 -->
        </div>
      </div>

      <!-- 启动按钮（默认点击启动；编辑模式下置灰锁定，确认布局后才能点击） -->
      <button id="home-launch-btn" ref="launchEl" class="btn btn-primary btn-lg home-launch-btn"
              :class="{ 'home-launch-btn-locked': editingLayout }"
              :aria-disabled="editingLayout ? 'true' : 'false'"
              :title="editingLayout ? '编辑中，确认布局后才能启动' : '点击启动'">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polygon points="5 3 19 12 5 21 5 3"/></svg>
        启动游戏
      </button>

      <!-- 启动任务 / 运行中游戏卡片（编辑模式下按住拖动，有运行实例时显示） -->
      <div class="home-running-card" ref="runningEl" v-if="gameInstances.length > 0"
           :title="editingLayout ? '拖动调整位置' : ''">
        <div class="home-running-card-header">
          <span class="home-running-dot"></span>
          <span class="home-running-title">启动任务</span>
          <span class="home-running-count" v-if="gameInstances.length > 1">({{ gameInstances.length }})</span>
        </div>
        <div class="home-running-list">
          <div class="home-running-item" v-for="inst in gameInstances" :key="inst.sessionId">
            <div class="home-running-info">
              <div class="home-running-name">{{ inst.versionId }}</div>
              <div class="home-running-meta">PID: {{ inst.pid }} · {{ formatElapsed(inst) }}</div>
            </div>
            <button class="home-running-stop" @click.stop="stopInstance(inst.sessionId)">停止</button>
          </div>
        </div>
      </div>

      <!-- 布局编辑入口：默认只显示「编辑布局」，编辑中显示取消/确认 -->
      <div class="home-edit-toolbar" :class="{ 'home-edit-toolbar-active': editingLayout }">
        <span v-if="editingLayout" class="home-edit-hint">拖动控件调整位置</span>
        <button v-if="!editingLayout" type="button" id="home-edit-layout-btn"
                class="btn btn-secondary btn-sm" @click.stop="beginLayoutEdit">编辑布局</button>
        <template v-else>
          <button type="button" id="home-edit-cancel-btn"
                  class="btn btn-secondary btn-sm" @click.stop="cancelLayoutEdit">取消</button>
          <button type="button" id="home-edit-confirm-btn"
                  class="btn btn-primary btn-sm" @click.stop="confirmLayoutEdit">确认</button>
        </template>
      </div>
    </div>
  `,
  computed: {
    gameInstances() {
      return gameStore ? gameStore.instances : [];
    }
  },
  methods: {
    // 基岩版：账号在游戏内（Xbox）登录，启动器侧账号栏改成提示
    applyBedrockAccountUI(isBedrock) {
      const prev = this._bedrockApplied;
      this._bedrockApplied = isBedrock;
      const bar = document.getElementById('home-account-bar');
      if (bar) bar.dataset.bedrock = isBedrock ? '1' : '0';
      if (!isBedrock) {
        // 只有从基岩版切回 Java 版才需要重画；首次加载由 loadAccounts 自己处理
        if (prev === true) loadAccounts().catch(function () {});
        return;
      }
      const nameEl = document.getElementById('home-player-name');
      const typeEl = document.getElementById('home-account-type');
      const avatar = document.getElementById('home-avatar');
      const box = document.getElementById('home-avatar-box');
      if (nameEl) nameEl.textContent = '请在游戏内登录';
      if (typeEl) typeEl.textContent = '基岩版';
      if (box) box.classList.remove('home-avatar-add');
      if (avatar) {
        avatar.innerHTML = '<img src="img/icons/bedrock.webp" alt="" class="home-avatar-img" style="image-rendering:auto;border-radius:12px">';
      }
    },
    // 轮询检测当前启动版本是否基岩版（appState 是普通对象，Vue 观测不到）
    syncBedrockAccount() {
      const vid = (typeof appState !== 'undefined' && appState.currentLaunchVersionId) || '';
      const list = (typeof appState !== 'undefined' && appState.installedVersions) || [];
      const v = vid ? list.find(x => x.id === vid) : null;
      this.applyBedrockAccountUI(!!(v && v.isBedrock));
    },
    widgetByKey(key) {
      return {
        title: this.$refs.titleEl,
        avatar: this.$refs.avatarEl,
        version: this.$refs.versionEl,
        launch: this.$refs.launchEl,
        running: this.$refs.runningEl
      }[key];
    },
    // ── 布局编辑 ──
    /** 进入编辑：只记录草稿位置，不写 localStorage */
    beginLayoutEdit() {
      if (this.editingLayout) return;
      this.editingLayout = true;
      this._draftChanged = {};
    },
    /** 取消编辑：丢弃草稿，恢复已保存布局 */
    cancelLayoutEdit() {
      if (!this.editingLayout) return;
      this.editingLayout = false;
      this._draftChanged = {};
      this.applyLayout();
    },
    /** 确认编辑：把本次拖动/缩放写入 localStorage，再恢复普通模式 */
    confirmLayoutEdit() {
      if (!this.editingLayout) return;
      const layout = loadHomeLayout();
      for (const [key, pos] of Object.entries(this._draftChanged)) {
        layout[key] = layout[key] || {};
        if (Number.isFinite(pos.x) && Number.isFinite(pos.y)) {
          layout[key].x = pos.x;
          layout[key].y = pos.y;
        }
        if (key === 'avatar') layout.avatar.s = pos.s || this._avatarScale || 1;
      }
      saveHomeLayout(layout);
      this.editingLayout = false;
      this._draftChanged = {};
      this.applyLayout();
    },
    markDraftChanged(key, el) {
      this._draftChanged[key] = {
        x: parseInt(el.style.left, 10) || 0,
        y: parseInt(el.style.top, 10) || 0
      };
      if (key === 'avatar') this._draftChanged.avatar.s = this._avatarScale || 1;
    },
    // 有保存位置 → 应用（clamp 到窗口内）；没有 → 清除内联样式回退 CSS 默认（精确居中/停靠）。
    applyLayout(force) {
      const page = this.$refs.homePage;
      if (!page) return;
      // 页面隐藏时 clientWidth/Height 为 0，按 0 计算会把所有控件压到左上角；
      // 页面重新可见后尺寸变化会由 ResizeObserver 重新应用一次
      if (!page.clientWidth || !page.clientHeight) return;
      const layout = force ? {} : loadHomeLayout();
      const pw = page.clientWidth, ph = page.clientHeight;
      // 默认位置的横向基准取启动器窗口中心，而不是会随经典侧栏收放变宽的 .home-page 中心。
      // 这样收起/固定导航栏时，未拖动过的默认控件不会整体横向漂移。
      const pageRect = page.getBoundingClientRect();
      const viewportWidth = document.documentElement.clientWidth || window.innerWidth || 0;
      const defaultCenterLeft = viewportWidth > 0
        ? (viewportWidth / 2) - pageRect.left
        : pw / 2;
      for (const key of ['title', 'avatar', 'version', 'launch', 'running']) {
        const el = this.widgetByKey(key);
        if (!el) continue;
        const pos = (!force && layout[key]) ? layout[key] : null;
        if (pos) {
          const w = el.offsetWidth || 10, h = el.offsetHeight || 10;
          el.style.left = clampPos(pos.x, 0, Math.max(0, pw - w)) + 'px';
          el.style.top = clampPos(pos.y, 0, Math.max(0, ph - h)) + 'px';
          el.style.right = 'auto';
          el.style.bottom = 'auto';
          // 已保存位置由 left/top 决定，必须清掉 CSS 默认的 translateX(-50%)，
          // 否则控件会再偏移半个自身宽度；同时也能阻止按钮按下时被通用 scale 规则推走。
          el.style.transform = 'none';
        } else {
          // 回退 CSS 默认：标题/头像/版本卡/启动按钮水平居中，运行卡片左下
          el.style.left = '';
          el.style.top = '';
          el.style.right = '';
          el.style.bottom = '';
          el.style.transform = '';
          // 居中控件以窗口中心为 left（CSS 的 translateX(-50%) 继续负责自身半宽修正）；
          // 运行卡片保持 CSS 的左下停靠，不参与窗口居中。
          if (key !== 'running') el.style.left = defaultCenterLeft + 'px';
        }
      }
      // 头像缩放（恢复默认时重置为 1）
      const s = (!force && layout.avatar && layout.avatar.s) || 1;
      this._avatarScale = s;
      if (this.$refs.avatarScale) {
        this.$refs.avatarScale.style.transform = 'scale(' + s + ')';
      }
      // 首次成功应用后再显示控件，避免从 CSS 默认位置跳到持久化位置
      this.layoutReady = true;
    },
    /**
     * 编辑模式下按住控件主体拖动：移动超过阈值 = 拖动并记录草稿；
     * 非编辑模式不产生拖动，原地松手仍派发 click，保持控件原有交互。
     */
    bindDrag(el, key, onTap, excludeSelector) {
      if (!el) return;
      const self = this;
      el.addEventListener('pointerdown', function (e) {
        if (e.button !== 0) return;
        if (excludeSelector && e.target.closest && e.target.closest(excludeSelector)) return;
        const page = self.$refs.homePage;
        const rect = el.getBoundingClientRect();
        const pageRect = page.getBoundingClientRect();
        const ox = rect.left - pageRect.left;
        const oy = rect.top - pageRect.top;
        const sx = e.clientX, sy = e.clientY;
        let moved = false;
        const onMove = function (ev) {
          if (!self.editingLayout) return;
          const dx = ev.clientX - sx, dy = ev.clientY - sy;
          if (!moved && Math.abs(dx) < DRAG_THRESHOLD && Math.abs(dy) < DRAG_THRESHOLD) return;
          if (!moved) {
            moved = true;
            el.classList.add('home-dragging');
            // 退出 CSS 默认定位（水平居中 transform / 右侧停靠 right），此后由内联 left/top 全权控制。
            // ox/oy 取自 getBoundingClientRect（真实视觉位置），清 transform 不会产生跳变。
            el.style.transform = 'none';
            el.style.right = 'auto';
            el.style.bottom = 'auto';
          }
          ev.preventDefault();
          const w = el.offsetWidth || 10, h = el.offsetHeight || 10;
          el.style.left = clampPos(ox + dx, 0, Math.max(0, pageRect.width - w)) + 'px';
          el.style.top = clampPos(oy + dy, 0, Math.max(0, pageRect.height - h)) + 'px';
        };
        const onUp = function () {
          document.removeEventListener('pointermove', onMove);
          document.removeEventListener('pointerup', onUp);
          document.removeEventListener('pointercancel', onUp);
          el.classList.remove('home-dragging');
          if (moved && self.editingLayout) {
            self.markDraftChanged(key, el);
            // 吞掉拖动结束后派发的 click，防止误触内部交互（启动/进账户/切换版本）
            const swallow = function (ev) {
              ev.stopPropagation();
              ev.preventDefault();
              el.removeEventListener('click', swallow, true);
            };
            el.addEventListener('click', swallow, true);
            setTimeout(function () { el.removeEventListener('click', swallow, true); }, 350);
          } else if (!moved && !self.editingLayout && typeof onTap === 'function') {
            // 原地松手（未拖动）= 点击：控件自身没有 click 绑定（如头像）时由这里触发；
            // 编辑模式下点击被拦截，只允许拖动
            onTap();
          }
        };
        document.addEventListener('pointermove', onMove);
        document.addEventListener('pointerup', onUp);
        document.addEventListener('pointercancel', onUp);
      });
    },
    /** 编辑模式下拦截控件点击，只有工具栏按钮可用 */
    setupEditClickGuard() {
      const self = this;
      const page = this.$refs.homePage;
      if (!page) return;
      page.addEventListener('click', function (e) {
        if (!self.editingLayout) return;
        if (e.target && e.target.closest && e.target.closest('.home-edit-toolbar')) return;
        e.preventDefault();
        e.stopImmediatePropagation();
      }, true);
    },
    setupWidgets() {
      const self = this;
      // 主页控件内的 <img>（头像、版本图标）默认是浏览器原生可拖拽元素：
      // 按住拖动时浏览器接管拖拽，光标显示「禁止放置」并取消 pointermove/pointerup，
      // 自定义拖动完全失效。头像图片由账户逻辑动态替换，故在页面根节点上统一拦截 dragstart。
      if (this.$refs.homePage) {
        this.$refs.homePage.addEventListener('dragstart', function (e) {
          e.preventDefault();
        });
      }
      // 软件名：编辑模式下拖动 + 点击回主页
      this.bindDrag(this.$refs.titleEl, 'title', function () {
        if (typeof navigateToPage === 'function') navigateToPage('home');
      });
      // 头像：编辑模式下拖动 + 滚轮缩放，默认点击进账户
      this.bindDrag(this.$refs.avatarEl, 'avatar', function () {
        // 基岩版账号在游戏内（Xbox）登录，账户页对它没有意义
        if (self._bedrockApplied) {
          if (typeof showToast === 'function') showToast('基岩版请在游戏内登录（Xbox 账号）', 'info');
          return;
        }
        if (typeof navigateToPage === 'function') navigateToPage('accounts');
      });
      const avatarEl = this.$refs.avatarEl;
      const scaleWrap = this.$refs.avatarScale;
      if (avatarEl && scaleWrap) {
        const layout = loadHomeLayout();
        this._avatarScale = (layout.avatar && layout.avatar.s) || 1;
        scaleWrap.style.transform = 'scale(' + this._avatarScale + ')';
        avatarEl.addEventListener('wheel', function (e) {
          if (!self.editingLayout) return;
          e.preventDefault();
          self._avatarScale = clampPos(self._avatarScale + (e.deltaY < 0 ? 0.1 : -0.1), AVATAR_MIN, AVATAR_MAX);
          scaleWrap.style.transform = 'scale(' + self._avatarScale + ')';
          self._draftChanged.avatar = self._draftChanged.avatar || {};
          self._draftChanged.avatar.s = self._avatarScale;
        }, { passive: false });
      }
      // 版本卡：编辑模式下拖动（内部点击切版本不受影响）
      this.bindDrag(this.$refs.versionEl, 'version', null);
      // 启动按钮：编辑模式下拖动；默认点击启动由 init-setup-nav 的 click 监听负责
      this.bindDrag(this.$refs.launchEl, 'launch', null);
      // 运行卡片：编辑模式下拖动（排除内部「停止」按钮，避免点击停止被拖动吞掉）
      this.bindDrag(this.$refs.runningEl, 'running', null, '.home-running-stop');
      this.setupEditClickGuard();
      // 窗口尺寸变化时把控件收回可视区域；编辑中保留草稿位置，不重读已保存布局
      window.addEventListener('resize', function () { if (!self.editingLayout) self.applyLayout(); });
      // 页面自身尺寸变化同样要重新收敛：导航模式切换的 padding 过渡、侧栏展开/收起、
      // 全屏切换都不会触发 window.resize，只监听窗口事件会漏掉这些情况
      if (typeof ResizeObserver !== 'undefined' && this.$refs.homePage) {
        this._layoutObserver = new ResizeObserver(function () { if (!self.editingLayout) self.applyLayout(); });
        this._layoutObserver.observe(this.$refs.homePage);
      } else {
        // 无 ResizeObserver 时兜底显示，避免布局未应用导致控件一直隐藏
        setTimeout(function () {
          self.applyLayout();
          if (!self.layoutReady) self.layoutReady = true;
        }, 200);
      }
      // 页面从隐藏变为可见后重新应用布局：切页返回时页面尺寸可能刚恢复，
      // 必须等可见尺寸稳定后再按持久化位置收敛控件，避免控件停在左上角。
      window.addEventListener('versepc-page-shown', function (e) {
        if (!e || !e.detail || e.detail.pageId !== 'page-home' || self.editingLayout) return;
        requestAnimationFrame(function () {
          self.applyLayout();
          self.syncRunningCard();
        });
      });
      // 个性化页「恢复主页默认布局」
      window.addEventListener('versepc-home-layout-reset', function () {
        self.editingLayout = false;
        self._draftChanged = {};
        self.applyLayout(true);
      });
    },
    /** 运行卡片由 v-if 延迟挂载：出现后应用保存位置并绑定拖动 */
    syncRunningCard() {
      const rEl = this.$refs.runningEl;
      if (!rEl || this._boundRunningEl === rEl) return;
      this._boundRunningEl = rEl;
      const page = this.$refs.homePage;
      const layout = loadHomeLayout();
      const pos = layout.running;
      if (pos && page) {
        rEl.style.left = clampPos(pos.x, 0, Math.max(0, page.clientWidth - (rEl.offsetWidth || 10))) + 'px';
        rEl.style.top = clampPos(pos.y, 0, Math.max(0, page.clientHeight - (rEl.offsetHeight || 10))) + 'px';
        rEl.style.right = 'auto';
        rEl.style.bottom = 'auto';
      }
      this.bindDrag(rEl, 'running', null, '.home-running-stop');
    },
    // 计算运行时长（每秒由 store.now 触发重新渲染）
    formatElapsed(inst) {
      const elapsed = Math.floor(((gameStore.now || Date.now()) - inst.startTime) / 1000);
      const mins = Math.floor(elapsed / 60);
      const secs = elapsed % 60;
      return mins > 0 ? `${mins}分${secs}秒` : `${secs}秒`;
    },
    stopInstance(sessionId) {
      if (typeof stopGameInstance === 'function') {
        stopGameInstance(sessionId);
      }
    }
  },
  beforeUnmount() {
    if (this._layoutObserver) {
      this._layoutObserver.disconnect();
      this._layoutObserver = null;
    }
    if (this._runningTimer) {
      clearInterval(this._runningTimer);
      this._runningTimer = null;
    }
    if (this._bedrockTimer) {
      clearInterval(this._bedrockTimer);
      this._bedrockTimer = null;
    }
  },
  mounted() {
    const self = this;
    const init = function () {
      if (typeof loadAccounts !== 'function' || typeof loadVersions !== 'function') {
        setTimeout(init, 80);
        return;
      }
      // 刷新账户显示（含头像加载）和版本列表（含主页版本卡片渲染）
      // 账户刷新是异步的，等它画完再按当前版本决定是否需要切成基岩版文案
      loadAccounts()
        .then(function () { self.syncBedrockAccount(); })
        .catch(function (e) { console.error('[PageHome] loadAccounts error:', e); });
      loadVersions().catch(function (e) { console.error('[PageHome] loadVersions error:', e); });
      // 拉取一次运行中游戏状态，让主页启动任务卡片尽快显示
      if (typeof updateGameStatus === 'function') {
        updateGameStatus().catch(function (e) { console.error('[PageHome] updateGameStatus error:', e); });
      }
      // 应用持久化布局 / 默认位置，并绑定拖动与缩放
      self.applyLayout();
      self.setupWidgets();
      // 运行卡片由 v-if 延迟挂载，轮询检测其出现并应用布局
      self.syncRunningCard();
      self._runningTimer = setInterval(function () { self.syncRunningCard(); }, 1200);
      // 版本切换后同步账号栏（基岩版显示「请在游戏内登录」）
      self.syncBedrockAccount();
      self._bedrockTimer = setInterval(function () { self.syncBedrockAccount(); }, 1200);
    };
    requestAnimationFrame(function () { setTimeout(init, 50); });
  }
};

// 导出供主入口使用
export default PageHome;
