// Login / register panel for the web build. Uses PalSaveStore auth methods.
// Loaded as a classic script; exposes window.PalAuthUI.

(function (global) {
  "use strict";

  function el(tag, attrs, children) {
    const node = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (k) {
        if (k === "className") node.className = attrs[k];
        else if (k === "text") node.textContent = attrs[k];
        else if (k.indexOf("on") === 0 && typeof attrs[k] === "function") {
          node.addEventListener(k.slice(2).toLowerCase(), attrs[k]);
        } else {
          node.setAttribute(k, attrs[k]);
        }
      });
    }
    (children || []).forEach(function (c) {
      if (c == null) return;
      node.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    });
    return node;
  }

  /**
   * @param {{ store: object, mount?: HTMLElement, onAfterAuth?: () => void }} opts
   */
  function mountAuthUI(opts) {
    const store = opts.store;
    const mount = opts.mount || document.getElementById("authbar");
    const onAfterAuth = opts.onAfterAuth || function () {};
    if (!mount) return { update: function () {} };

    // If the cloud API is not present (static host), keep the bar minimal.
    function render() {
      const state = store.getState();
      mount.innerHTML = "";

      if (!state.apiAvailable) {
        mount.appendChild(el("span", {
          className: "auth-status",
          text: "本地存檔（無雲端服務）",
          title: "使用 web/serve.py 啟動可啟用帳號與雲存檔",
        }));
        return;
      }

      if (state.username) {
        mount.appendChild(el("span", {
          className: "auth-status",
          text: "已登入 · " + state.username,
          title: "雲存檔綁定此帳號，換裝置登入同一帳號即可同步",
        }));
        mount.appendChild(el("button", {
          type: "button",
          className: "auth-btn",
          text: "登出",
          onClick: async function () {
            await store.logout();
            render();
          },
        }));
        return;
      }

      mount.appendChild(el("span", {
        className: "auth-status",
        text: "遊客 · 僅本地存檔",
      }));
      mount.appendChild(el("button", {
        type: "button",
        className: "auth-btn",
        text: "登入",
        onClick: function () { openModal("login"); },
      }));
      mount.appendChild(el("button", {
        type: "button",
        className: "auth-btn auth-btn-primary",
        text: "註冊",
        onClick: function () { openModal("register"); },
      }));
    }

    function openModal(mode) {
      closeModal();
      const backdrop = el("div", { className: "auth-modal-backdrop", id: "auth-modal" });
      const panel = el("div", { className: "auth-modal" });
      const title = el("h2", {
        text: mode === "register" ? "註冊帳號" : "登入",
      });
      const err = el("div", { className: "auth-error", id: "auth-error" });
      const userInput = el("input", {
        type: "text",
        id: "auth-user",
        autocomplete: mode === "register" ? "username" : "username",
        placeholder: "用戶名（3–32 位字母數字下劃線）",
        maxlength: "32",
        spellcheck: "false",
      });
      const passInput = el("input", {
        type: "password",
        id: "auth-pass",
        autocomplete: mode === "register" ? "new-password" : "current-password",
        placeholder: "密碼（至少 6 位）",
      });
      const submit = el("button", {
        type: "button",
        className: "auth-btn auth-btn-primary",
        text: mode === "register" ? "註冊並登入" : "登入",
      });
      const cancel = el("button", {
        type: "button",
        className: "auth-btn",
        text: "取消",
        onClick: closeModal,
      });
      const switchMode = el("button", {
        type: "button",
        className: "auth-link",
        text: mode === "register" ? "已有帳號？去登入" : "沒有帳號？去註冊",
        onClick: function () {
          closeModal();
          openModal(mode === "register" ? "login" : "register");
        },
      });
      const row = el("div", { className: "auth-actions" }, [submit, cancel]);
      const hint = el("p", {
        className: "auth-hint",
        text: "登入後存檔會同步到伺服器；本地仍保留一份副本。",
      });

      panel.appendChild(title);
      panel.appendChild(err);
      panel.appendChild(userInput);
      panel.appendChild(passInput);
      panel.appendChild(row);
      panel.appendChild(switchMode);
      panel.appendChild(hint);
      backdrop.appendChild(panel);
      backdrop.addEventListener("click", function (e) {
        if (e.target === backdrop) closeModal();
      });
      document.body.appendChild(backdrop);
      userInput.focus();

      async function doSubmit() {
        err.textContent = "";
        const u = userInput.value.trim();
        const p = passInput.value;
        if (!u || !p) {
          err.textContent = "請填寫用戶名和密碼";
          return;
        }
        submit.disabled = true;
        try {
          if (mode === "register") await store.register(u, p);
          else await store.login(u, p);
          statusBusy("同步雲存檔…");
          await store.syncFromCloud();
          closeModal();
          render();
          onAfterAuth();
        } catch (e) {
          err.textContent = e.message || String(e);
          submit.disabled = false;
        }
      }

      submit.addEventListener("click", doSubmit);
      passInput.addEventListener("keydown", function (e) {
        if (e.key === "Enter") doSubmit();
      });
      userInput.addEventListener("keydown", function (e) {
        if (e.key === "Enter") passInput.focus();
      });
    }

    function closeModal() {
      const m = document.getElementById("auth-modal");
      if (m) m.remove();
    }

    function statusBusy(msg) {
      const s = document.getElementById("status");
      if (s) s.textContent = msg;
    }

    render();
    return { update: render, openLogin: function () { openModal("login"); } };
  }

  global.PalAuthUI = { mountAuthUI: mountAuthUI };
})(typeof window !== "undefined" ? window : self);
