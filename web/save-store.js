// Auth + cloud/local save store for the web build.
//
// The engine (wasm worker) is unaware of where saves live: it posts
// `{ palSave: slot, data: Uint8Array }` to the main thread. This module:
//
//   1. Always writes localStorage (`pal-save-{1..5}`) as offline cache.
//   2. When the user is logged in against `/api/auth/*`, dual-writes to
//      `/api/saves` under that account (Bearer token).
//   3. Without a session (or without the API), stays local-only.
//
// Loaded as a classic script; exposes window.PalSaveStore.

(function (global) {
  "use strict";

  const SAVE_SLOTS = [1, 2, 3, 4, 5];
  const LOCAL_KEY = (slot) => `pal-save-${slot}`;
  const TOKEN_KEY = "pal-auth-token";
  const USER_KEY = "pal-auth-user";

  function u8ToB64(u8) {
    let bin = "";
    for (let i = 0; i < u8.length; i += 0x8000) {
      bin += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
    }
    return btoa(bin);
  }

  function b64ToU8(b64) {
    const bin = atob(b64);
    const u8 = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
    return u8;
  }

  function readToken() {
    try { return localStorage.getItem(TOKEN_KEY); } catch (_) { return null; }
  }

  function writeSession(token, username) {
    try {
      if (token) localStorage.setItem(TOKEN_KEY, token);
      else localStorage.removeItem(TOKEN_KEY);
      if (username) localStorage.setItem(USER_KEY, username);
      else localStorage.removeItem(USER_KEY);
    } catch (_) {}
  }

  function readCachedUser() {
    try { return localStorage.getItem(USER_KEY); } catch (_) { return null; }
  }

  /**
   * @param {{ authBase?: string, savesBase?: string, status?: (msg: string) => void, onAuthChange?: (state) => void }} [opts]
   */
  async function createSaveStore(opts) {
    opts = opts || {};
    const authBase = (opts.authBase || "/api/auth").replace(/\/$/, "");
    const savesBase = (opts.savesBase || "/api/saves").replace(/\/$/, "");
    const status = opts.status || function () {};
    const onAuthChange = opts.onAuthChange || function () {};

    let apiAvailable = false;
    let token = readToken();
    let username = readCachedUser();
    let serverEnabled = false; // true only when logged in + API up

    function authHeaders(extra) {
      const h = Object.assign({}, extra || {});
      if (token) h["Authorization"] = "Bearer " + token;
      return h;
    }

    async function probeApi() {
      // /api/auth/me with no token → 401 means the auth API exists.
      try {
        const resp = await fetch(authBase + "/me", {
          headers: authHeaders(),
          cache: "no-store",
        });
        if (resp.status === 401 || resp.status === 200) {
          apiAvailable = true;
          if (resp.status === 200) {
            const data = await resp.json();
            username = data.username;
            serverEnabled = true;
            writeSession(token, username);
            return;
          }
          // Token missing or invalid.
          if (token) {
            writeSession(null, null);
            token = null;
            username = null;
          }
          serverEnabled = false;
          return;
        }
        apiAvailable = false;
        serverEnabled = false;
      } catch (_) {
        apiAvailable = false;
        serverEnabled = false;
      }
    }

    await probeApi();

    function readLocal(slot) {
      try {
        const b64 = localStorage.getItem(LOCAL_KEY(slot));
        if (!b64) return null;
        return b64ToU8(b64);
      } catch (_) {
        return null;
      }
    }

    function writeLocal(slot, u8) {
      try {
        localStorage.setItem(LOCAL_KEY(slot), u8ToB64(u8));
      } catch (e) {
        console.warn("localStorage save failed:", e);
      }
    }

    async function readServer(slot) {
      try {
        const resp = await fetch(savesBase + "/" + slot, {
          headers: authHeaders(),
          cache: "no-store",
        });
        if (resp.status === 401) {
          // Session expired mid-game.
          token = null;
          username = null;
          serverEnabled = false;
          writeSession(null, null);
          onAuthChange(getState());
          return null;
        }
        if (resp.status === 404) return null;
        if (!resp.ok) throw new Error("HTTP " + resp.status);
        return new Uint8Array(await resp.arrayBuffer());
      } catch (e) {
        console.warn("server load slot " + slot + " failed:", e);
        return null;
      }
    }

    async function writeServer(slot, u8) {
      const resp = await fetch(savesBase + "/" + slot, {
        method: "PUT",
        headers: authHeaders({ "Content-Type": "application/octet-stream" }),
        body: u8,
      });
      if (resp.status === 401) {
        token = null;
        username = null;
        serverEnabled = false;
        writeSession(null, null);
        onAuthChange(getState());
        throw new Error("session expired");
      }
      if (!resp.ok) throw new Error("HTTP " + resp.status);
    }

    async function seedInto(files) {
      if (serverEnabled) {
        status("syncing cloud saves…");
        await Promise.all(SAVE_SLOTS.map(async function (slot) {
          const remote = await readServer(slot);
          const local = readLocal(slot);
          if (remote) {
            files[slot + ".RPG"] = remote;
            writeLocal(slot, remote);
          } else if (local) {
            files[slot + ".RPG"] = local;
            try {
              await writeServer(slot, local);
            } catch (e) {
              console.warn("migrate slot " + slot + " to server failed:", e);
            }
          }
        }));
        return;
      }

      for (let i = 0; i < SAVE_SLOTS.length; i++) {
        const slot = SAVE_SLOTS[i];
        const local = readLocal(slot);
        if (local) files[slot + ".RPG"] = local;
      }
    }

    function persist(slot, data) {
      const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);
      writeLocal(slot, u8);
      if (!serverEnabled) return;
      writeServer(slot, u8).catch(function (e) {
        console.warn("cloud save slot " + slot + " failed:", e);
      });
    }

    async function register(user, password) {
      if (!apiAvailable) throw new Error("cloud API unavailable");
      const resp = await fetch(authBase + "/register", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username: user, password: password }),
      });
      const data = await resp.json().catch(function () { return {}; });
      if (!resp.ok) throw new Error(data.error || ("HTTP " + resp.status));
      token = data.token;
      username = data.username;
      serverEnabled = true;
      writeSession(token, username);
      onAuthChange(getState());
      return getState();
    }

    async function login(user, password) {
      if (!apiAvailable) throw new Error("cloud API unavailable");
      const resp = await fetch(authBase + "/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username: user, password: password }),
      });
      const data = await resp.json().catch(function () { return {}; });
      if (!resp.ok) throw new Error(data.error || ("HTTP " + resp.status));
      token = data.token;
      username = data.username;
      serverEnabled = true;
      writeSession(token, username);
      onAuthChange(getState());
      return getState();
    }

    async function logout() {
      if (token && apiAvailable) {
        try {
          await fetch(authBase + "/logout", {
            method: "POST",
            headers: authHeaders(),
          });
        } catch (_) {}
      }
      token = null;
      username = null;
      serverEnabled = false;
      writeSession(null, null);
      onAuthChange(getState());
      return getState();
    }

    /**
     * After login/register mid-session: pull cloud saves into localStorage
     * (and the live worker PAL_FILES is NOT updated — reload advised for
     * in-game load menu accuracy; new saves still go to the account).
     */
    async function syncFromCloud() {
      if (!serverEnabled) return { pulled: 0 };
      let pulled = 0;
      await Promise.all(SAVE_SLOTS.map(async function (slot) {
        const remote = await readServer(slot);
        if (remote) {
          writeLocal(slot, remote);
          pulled++;
        } else {
          const local = readLocal(slot);
          if (local) {
            try {
              await writeServer(slot, local);
            } catch (_) {}
          }
        }
      }));
      return { pulled: pulled };
    }

    function getState() {
      return {
        apiAvailable: apiAvailable,
        serverEnabled: serverEnabled,
        username: username,
        mode: serverEnabled ? "cloud" : (apiAvailable ? "local+login" : "local"),
      };
    }

    onAuthChange(getState());

    return {
      getState: getState,
      seedInto: seedInto,
      persist: persist,
      register: register,
      login: login,
      logout: logout,
      syncFromCloud: syncFromCloud,
      // Back-compat fields for main.js status line.
      get playerId() { return username || "(guest)"; },
      get serverEnabled() { return serverEnabled; },
      get mode() { return getState().mode; },
      get username() { return username; },
      get apiAvailable() { return apiAvailable; },
    };
  }

  global.PalSaveStore = {
    createSaveStore: createSaveStore,
  };
})(typeof window !== "undefined" ? window : self);
