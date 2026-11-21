const api = globalThis.browser ?? globalThis.chrome;
document.documentElement.lang = api.i18n.getUILanguage();
const t = key => api.i18n.getMessage(key);
const status = document.getElementById("status");
const list = document.getElementById("videos");
const share = document.getElementById("share");
const sharingStatus = document.getElementById("sharingStatus");
const sharing = typeof api.runtime.getBrowserInfo === "function" ? { data_collection: ["websiteActivity"] } : null;
const siteAccess = { origins: ["*://*/*"] };
document.documentElement.lang = api.i18n.getUILanguage();
document.title = t("app");
for (const element of document.querySelectorAll("[data-message]")) {
  element.textContent = t(element.dataset.message);
}
status.textContent = t("checking");
const filter = document.getElementById("mediaType");
const bulk = document.getElementById("downloadSelected");
const selected = new Set();
const choices = new Map();
let rows = [];
let settingsTab;
const send = (tab, message, frameId = 0) => api.tabs.sendMessage(tab.id, message, { frameId });
const updateBulk = () => { bulk.disabled = selected.size === 0; };

async function download(tab, key) {
  const item = rows.find(item => item.key === key);
  if (!item) return "sourceChanged";
  const result = await send(tab, { type: "start", id: item.id, variant: choices.get(key) }, item.frameId);
  return result.error ?? result.status;
}

function addItem(tab, item) {
  const row = document.createElement("li");
  const check = document.createElement("input");
  check.type = "checkbox";
  check.checked = selected.has(item.key);
  check.setAttribute("aria-label", api.i18n.getMessage("selectNamed", item.name));
  check.addEventListener("change", () => {
    if (check.checked) selected.add(item.key);
    else selected.delete(item.key);
    updateBulk();
  });
  const media = document.createElement("div");
  media.className = "media";
  const name = document.createElement("span");
  const button = document.createElement("button");
  const rowStatus = document.createElement("small");
  rowStatus.setAttribute("role", "status");
  name.textContent = item.name;
  button.type = "button";
  button.textContent = t("download");
  button.setAttribute("aria-label", api.i18n.getMessage("downloadNamed", item.name));
  button.addEventListener("click", async () => {
    button.disabled = true;
    rowStatus.textContent = "";
    try {
      rowStatus.textContent = t(await download(tab, item.key));
    } catch {
      rowStatus.textContent = t("downloadFailed");
    } finally {
      button.disabled = false;
    }
  });
  media.append(name);
  const details = document.createElement("small");
  const dimensions = item.dimensions?.filter(Boolean).join(" × ");
  const duration = item.duration >= 1 ? Math.floor(item.duration) : 0;
  details.textContent = [t({ video: "videos", img: "images", audio: "audio" }[item.kind]),
    dimensions, duration ? Math.floor(duration / 60) + ":" + String(duration % 60).padStart(2, "0") : ""]
    .filter(Boolean).join(" · ");
  media.append(details, rowStatus);
  if (item.variants?.length > 1) {
    const select = document.createElement("select");
    select.setAttribute("aria-label", api.i18n.getMessage("qualityNamed", item.name));
    for (const [index, variant] of item.variants.entries()) {
      const option = document.createElement("option");
      const label = [variant.height ? variant.height + "p" : "", variant.ext.toUpperCase()].filter(Boolean).join(" · ");
      option.textContent = index === 0 ? api.i18n.getMessage("highest", label) : label;
      option.value = variant.value;
      select.append(option);
    }
    if (choices.has(item.key)) select.value = choices.get(item.key);
    select.addEventListener("change", () => choices.set(item.key, select.value));
    media.append(select);
  }
  row.append(check, media, button);
  list.append(row);
}

function render(tab) {
  list.replaceChildren();
  for (const item of rows) {
    if (filter.value === "all" || item.kind === filter.value) addItem(tab, item);
  }
  list.hidden = !list.childElementCount;
  updateBulk();
}

async function initButtons(tab, hideHere) {
  const hostname = new URL(tab.url).hostname;
  const page = document.getElementById("togglePage");
  const site = document.getElementById("toggleSite");
  const global = document.getElementById("toggleGlobal");
  const refresh = async () => {
    const { buttons = {} } = await api.storage.local.get("buttons");
    page.textContent = t(hideHere ? "enablePage" : "disablePage");
    site.textContent = t(buttons.sites?.[hostname] === true ? "enableSite" : "disableSite");
    global.textContent = t(buttons.off ? "enableGlobal" : "disableGlobal");
  };
  page.onclick = async () => { hideHere = await send(tab, { type: "togglePage" }); await refresh(); };
  site.onclick = async () => {
    const { buttons = {} } = await api.storage.local.get("buttons");
    const sites = { ...buttons.sites };
    if (sites[hostname] === true) delete sites[hostname];
    else sites[hostname] = true;
    await api.storage.local.set({ buttons: { ...buttons, sites } });
    await refresh();
  };
  global.onclick = async () => {
    const { buttons = {} } = await api.storage.local.get("buttons");
    await api.storage.local.set({ buttons: { ...buttons, off: !buttons.off } });
    await refresh();
  };
  await refresh();
  document.getElementById("buttonSettings").hidden = false;
}

async function inspect(tab) {
  // Injection also covers tabs that were open during installation or permission changes.
  const frames = await api.scripting.executeScript({
    target: { tabId: tab.id, allFrames: true }, files: ["content.js"]
  }).catch(async () => {
    await api.scripting.executeScript({ target: { tabId: tab.id }, files: ["content.js"] });
    return [{frameId: 0}];
  });
  const inspected = await Promise.all(frames.slice(0, 60).map(async ({frameId}) => ({
    frameId, result: await send(tab, { type: "inspect" }, frameId).catch(() => null)
  })));
  const root = inspected.find(frame => frame.frameId === 0)?.result;
  const youtube = inspected.find(frame => frame.result?.youtubePlayerId);
  settingsTab = tab;
  selected.clear();
  choices.clear();
  const seen = new Set();
  rows = inspected.flatMap(({frameId, result}) => (result?.items ?? [])
    .map(item => ({ ...item, frameId, key: frameId + ":" + item.id })))
    .filter(item => {
      const source = item.variants[0]?.value;
      if (!source || seen.has(source)) return false;
      seen.add(source);
      return true;
    }).slice(0, 60);
  document.getElementById("controls").hidden = bulk.hidden = rows.length === 0;
  render(tab);
  const quality = document.getElementById("chooseVideoQuality");
  const qualityControls = document.getElementById("youtubeQualityControls");
  quality.hidden = !youtube;
  quality.onclick = async () => {
    quality.disabled = true;
    status.textContent = t("checkingQualities");
    try {
      const answer = await send(tab, { type: "videoQualities", id: youtube.result.youtubePlayerId }, youtube.frameId);
      if (!answer.qualities?.length) {
        status.textContent = t(answer.error ?? "youtubeUnavailable");
        return;
      }
      qualityControls.replaceChildren();
      for (const [index, variant] of answer.qualities.entries()) {
        const height = variant.width && variant.height ? Math.min(variant.width, variant.height) : variant.height;
        const label = [height ? height + "p" : "", variant.extension.toUpperCase()].filter(Boolean).join(" · ");
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = index === 0 ? api.i18n.getMessage("highest", label) : label;
        button.onclick = async () => {
          for (const control of qualityControls.children) control.disabled = true;
          try {
            const response = await send(tab, {
              type: "start", id: youtube.result.youtubePlayerId,
              selection: { url: answer.url, formatId: variant.format_id }
            }, youtube.frameId);
            status.textContent = t(response.error ?? response.status);
          } catch {
            status.textContent = t("downloadFailed");
          } finally {
            for (const control of qualityControls.children) control.disabled = false;
          }
        };
        qualityControls.append(button);
      }
      status.textContent = "";
      quality.hidden = true;
      qualityControls.hidden = false;
      qualityControls.firstElementChild?.focus();
    } catch {
      status.textContent = t("downloadFailed");
    } finally {
      quality.disabled = false;
    }
  };
  const copyLink = document.getElementById("copyVideoLink");
  copyLink.hidden = !youtube?.result.videoLink;
  status.textContent = youtube || rows.length ? ""
    : t(inspected.some(frame => frame.result?.error === "protected") ? "protected" : "unavailable");
  copyLink.onclick = async () => {
    try {
      const current = await send(tab, { type: "inspect" }, youtube.frameId);
      if (!current.videoLink) {
        copyLink.hidden = true;
        status.textContent = t(current.error);
        return;
      }
      await navigator.clipboard.writeText(current.videoLink);
      status.textContent = t("videoLinkCopied");
    } catch {
      status.textContent = t("videoLinkCopyFailed");
    }
  };
  if (root) await initButtons(tab, root.hideHere);
}

filter.onchange = () => { selected.clear(); render(settingsTab); };
document.getElementById("selectAll").onclick = () => {
  for (const item of rows) if (filter.value === "all" || item.kind === filter.value) selected.add(item.key);
  render(settingsTab);
};
document.getElementById("clearAll").onclick = () => { selected.clear(); render(settingsTab); };
bulk.onclick = async () => {
  bulk.disabled = true;
  let failed = false;
  for (const id of [...selected]) {
    try {
      const result = await download(settingsTab, id);
      if (result !== "sentToClipSail") { failed = result; continue; }
      selected.delete(id);
    } catch { failed = true; }
  }
  status.textContent = t(failed === "allowClipSailTransfer" || failed === "connectClipSail"
    ? failed : failed ? "batchFailed" : "sentToClipSail");
  render(settingsTab);
};
async function init() {
  document.getElementById("grantSites").hidden = await api.permissions.contains(siteAccess);
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  if (!tab?.url || !/^https?:\/\//i.test(tab.url)) {
    status.textContent = t("openSite");
    return;
  }
  const url = new URL(tab.url);
  const site = url.protocol + "//" + url.hostname + "/*";
  if (!await api.permissions.contains({ origins: [site] })) {
    status.textContent = t("permission");
    return;
  }
  await inspect(tab);
}
document.getElementById("grantSites").onclick = async () => {
  try {
    if (await api.permissions.request(siteAccess)) await init();
  } catch { status.textContent = t("permission"); }
};
void init().catch(() => { status.textContent = t("inspectionFailed"); });

async function initSharing() {
  if ((await api.runtime.getPlatformInfo()).os !== "win") return;
  const { videoTransfer } = await api.storage.local.get("videoTransfer");
  share.setAttribute("aria-pressed", String(videoTransfer === true && (!sharing || await api.permissions.contains(sharing))));
  document.getElementById("connection").hidden = false;
  const appStatus = document.getElementById("appStatus");
  const connection = await api.runtime.sendNativeMessage("local.media.downloader", { ping: true }).catch(() => null);
  appStatus.textContent = t(connection?.ok ? "appConnected" : "appDisconnected");
  share.addEventListener("click", async () => {
    const enabled = share.getAttribute("aria-pressed") === "true";
    share.disabled = true;
    try {
      if (enabled) {
        await api.storage.local.remove("videoTransfer");
        if (sharing) await api.permissions.remove(sharing);
      } else {
        if (sharing && !await api.permissions.request(sharing)) return;
        await api.storage.local.set({ videoTransfer: true });
      }
      const allowed = !enabled && (!sharing || await api.permissions.contains(sharing));
      share.setAttribute("aria-pressed", String(allowed));
      sharingStatus.textContent = t(allowed ? "sharingOn" : "sharingOff");
    } catch {
      sharingStatus.textContent = t("sharingFailed");
    } finally {
      share.disabled = false;
    }
  });
}
void initSharing().catch(() => {});
