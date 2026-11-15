import uk from "./uk.js";
import en from "./en.js";
import ru from "./ru.js";

const $ = id => document.getElementById(id);
const locales = { uk, en, ru };
const savedLanguage = localStorage.getItem("language");
let language = Object.hasOwn(locales, savedLanguage) ? savedLanguage : "uk";
const t = (key, values = {}) => (locales[language][key] ?? en[key] ?? locales[language].unknown)
  .replace(/\{(\w+)\}/g, (_, name) => values[name] ?? "");
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const appWindow = window.__TAURI__.window.getCurrentWindow();
let isMaximized = false;

function renderCaptionButtons() {
  for (const [id, key] of [["windowMinimize", "windowMinimize"],
    ["windowMaximize", isMaximized ? "windowRestore" : "windowMaximize"], ["windowClose", "windowClose"]]) {
    $(id).title = t(key);
    $(id).setAttribute("aria-label", t(key));
  }
  $("windowMaximize").firstElementChild.textContent = isMaximized ? "\uE923" : "\uE922";
}

async function refreshCaptionButtons() {
  isMaximized = await appWindow.isMaximized();
  renderCaptionButtons();
}

for (const [id, action] of [["windowMinimize", () => appWindow.minimize()],
  ["windowMaximize", async () => { await appWindow.toggleMaximize(); await refreshCaptionButtons(); }],
  ["windowClose", () => appWindow.close()]]) {
  $(id).addEventListener("click", () => { void action().catch(error => notify(errorKey(error))); });
}
window.addEventListener("resize", () => { void refreshCaptionButtons().catch(() => {}); });
void refreshCaptionButtons().catch(() => {});
void invoke("desktop_mica").then(active => {
  document.documentElement.classList.toggle("desktop-mica", active);
}).catch(() => {});

const icons = {
  menu: "M4 6h16M4 12h16M4 18h16",
  download: "M12 3v12m-5-5 5 5 5-5M5 16v4h14v-4",
  link: "M10 13a4 4 0 0 0 6 0l3-3a4 4 0 0 0-6-6l-2 2m2 5a4 4 0 0 0-6 0l-3 3a4 4 0 0 0 6 6l2-2",
  search: "M16 16l5 5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
  filter: "M3 5h18l-7 8v5l-4 2v-7z",
  sort: "M7 20V4m-4 4 4-4 4 4m6-4v16m-4-4 4 4 4-4",
  sortAscending: "M12 20V4m-6 6 6-6 6 6",
  sortDescending: "M12 4v16m-6-6 6 6 6-6",
  folder: "M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z",
  removeList: "M4 6h16M4 12h7m-7 6h7m3-3h7",
  image: "M4 4h16v16H4zm0 13 5-5 3 3 3-3 5 5m-6-9h.01",
  gif: "M4 5h16v14H4zm4 7a4 4 0 1 1 4 4m-4-1v-3h3",
  video: "M4 5h16v14H4zm6 4 5 3-5 3Z",
  audio: "M4 13v-1a8 8 0 0 1 16 0v1M4 13h4v6H4zm12 0h4v6h-4z",
  close: "M6 6l12 12M18 6 6 18",
  previous: "m15 6-6 6 6 6",
  next: "m9 6 6 6-6 6",
  pause: "M9 5v14m6-14v14",
  play: "m8 5 12 7-12 7z",
  retry: "M20 10a8 8 0 1 0-.8 6M20 4v6h-6"
};

function icon(name) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", "icon");
  const path = document.createElementNS(ns, "path");
  for (const [key, value] of Object.entries({ d: icons[name], fill: "none", stroke: "currentColor",
    "stroke-width": "1.7", "stroke-linecap": "round", "stroke-linejoin": "round" })) {
    path.setAttribute(key, value);
  }
  svg.append(path);
  return svg;
}
for (const node of document.querySelectorAll("[data-icon]")) node.append(icon(node.dataset.icon));

const app = { items: [], selected: new Set(), focus: null, detailsUrl: null, filter: "video", sortLoading: false, phase: "initial",
  error: null, queue: [], queueError: null, folder: "", tab: "find", submitting: false };
let noticeTimer;
let lastNotice;
const localizedNumber = value => new Intl.NumberFormat(language, { maximumFractionDigits: 1 }).format(value);
function sizeText(size) {
  if (size == null) return t("unknownValue");
  const units = ["B", "KB", "MB", "GB", "TB"];
  let unit = 0;
  while (size >= 1024 && unit < units.length - 1) { size /= 1024; unit++; }
  return localizedNumber(size) + " " + units[unit];
}
function element(tag, className, content) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (content != null) node.textContent = content;
  return node;
}
function button(label, handler, glyph, iconOnly = false) {
  const node = element("button", iconOnly ? "icon-button" : "");
  node.type = "button";
  if (glyph) node.append(icon(glyph));
  if (iconOnly) { node.setAttribute("aria-label", label); node.title = label; }
  else node.append(document.createTextNode(label));
  node.addEventListener("click", handler);
  return node;
}
function empty(node, glyph, title, hint) {
  node.replaceChildren(icon(glyph), element("h2", "", t(title)), element("p", "", t(hint)));
}
function notify(key) {
  lastNotice = key;
  $("notice").textContent = t(key);
  $("notice").hidden = false;
  clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => { $("notice").hidden = true; lastNotice = null; }, 6500);
}
const errorKey = error => typeof error === "string" ? error : "unknown";

const tabs = ["saved", "find", "queue"];

function renderNav() {
  const expanded = $("shell").classList.contains("nav-expanded");
  const toggle = $("navToggle");
  const label = t(expanded ? "collapseNavigation" : "expandNavigation");
  toggle.setAttribute("aria-expanded", String(expanded));
  toggle.setAttribute("aria-label", label);
  toggle.title = label;
}

$("shell").classList.toggle("nav-expanded", localStorage.getItem("navExpanded") === "true");
$("navToggle").addEventListener("click", () => {
  localStorage.setItem("navExpanded", String($("shell").classList.toggle("nav-expanded")));
  renderNav();
});

function showTab(tab, focus = false) {
  if (tab === "queue" && app.tab !== tab) queueFilesKey = undefined;
  if (tab !== "find") {
    $("filters").hidePopover();
    $("findSortMenu").hidePopover();
  }
  app.tab = tab;
  $("savedSearchBox").hidden = tab !== "saved";
  for (const key of tabs) {
    $(key + "Panel").hidden = tab !== key;
    const control = $(key + "Tab");
    control.setAttribute("aria-selected", String(tab === key));
    control.tabIndex = tab === key ? 0 : -1;
    if (focus && tab === key) control.focus();
  }
  if (tab === "queue") void refreshQueue();
  if (tab === "saved") void refreshSaved();
}
for (const tab of tabs) $(tab + "Tab").addEventListener("click", () => showTab(tab));
$("tabs").addEventListener("keydown", event => {
  if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
  event.preventDefault();
  const offset = event.key === "ArrowUp" ? -1 : 1;
  const index = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1
    : (tabs.indexOf(app.tab) + offset + tabs.length) % tabs.length;
  const tab = tabs[index];
  showTab(tab, true);
});

const thumbnails = new Map();
let thumbnailJobs = 0;
const pending = [];
const observer = new IntersectionObserver(entries => {
  for (const entry of entries) {
    entry.target.dataset.visible = String(entry.isIntersecting);
    if (entry.isIntersecting) loadImage(entry.target, entry.target.dataset.preview);
    else {
      entry.target.removeAttribute("src");
      delete entry.target.dataset.loaded;
    }
  }
}, { rootMargin: "100px" });

const savedVideoObserver = new IntersectionObserver(entries => {
  for (const { target, isIntersecting } of entries) {
    if (!isIntersecting) continue;
    savedVideoObserver.unobserve(target);
    const video = target.querySelector("video");
    if (video?.isConnected) video.src = video.dataset.source;
  }
}, { rootMargin: "100px" });

function previewData(url, priority = false) {
  if (thumbnails.has(url)) return thumbnails.get(url);
  const request = new Promise(resolve => {
    const job = () => {
      thumbnailJobs++;
      invoke("fetch_preview", { url }).then(resolve, () => resolve(null)).finally(() => {
        thumbnailJobs--;
        nextPreview();
      });
    };
    if (priority) pending.unshift(job);
    else pending.push(job);
  });
  thumbnails.set(url, request);
  // Bound retained preview data for large galleries.
  if (thumbnails.size > 14) thumbnails.delete(thumbnails.keys().next().value);
  nextPreview();
  return request;
}
function nextPreview() {
  while (thumbnailJobs < 4 && pending.length) pending.shift()();
}
function loadImage(img, url, priority = false) {
  if (img.hasAttribute("data-loaded") && img.hasAttribute("src")) return;
  const wantsImage = () => img.isConnected && (priority || img.dataset.visible === "true");
  if (wantsImage()) setPhotoPreviewState(img, "loading");
  previewData(url, priority).then(data => {
    if (!wantsImage()) return;
    if (data) img.src = data;
    else setPhotoPreviewState(img, "failed");
  });
}

function isSearchPhoto(item) {
  return !item.local && !item.page_asset && item.kind === "image";
}

function isSearchImageAsset(item) {
  return !item.local && item.page_asset && (item.kind === "image" || item.kind === "gif");
}

function searchMediaName(item) {
  return isSearchPhoto(item) && item.photo_title ? item.photo_title : item.name;
}

function setPhotoPreviewState(img, state) {
  const thumb = img.parentElement;
  const status = thumb?.querySelector(".photo-preview-status");
  if (!status) return;
  thumb.dataset.previewState = state;
  status.hidden = state === "ready";
  status.textContent = state === "loading" ? t("previewLoading") : state === "failed" ? t("previewUnavailable") : "";
  thumb.dispatchEvent(new Event("photo-preview-state"));
}

function retryPhotoPreview(item, image) {
  thumbnails.delete(item.preview);
  loadImage(image, item.preview, true);
  const card = Array.from($("gallery").children).find(card => card.dataset.url === item.url);
  const cardImage = card?.querySelector(".thumb img");
  if (cardImage !== image && cardImage?.dataset.visible === "true") loadImage(cardImage, item.preview, true);
  if (app.detailsUrl === item.url) {
    const panelImage = $("findDetails").querySelector(".thumb img");
    if (panelImage && panelImage !== image) loadImage(panelImage, item.preview, true);
  }
}

const galleryFilters = [
  ["video", "videos"], ["image", "images"], ["gif", "gif"],
  ["audio", "audioList"], ["assets", "pageAssets"]
];

const searchSortFields = [
  { key: "resolution", label: "dimensions", value: item => {
    const height = item.kind === "video" && qualityHeight(item.qualities?.[0] ?? {});
    if (height > 0) return height;
    if ((item.measuredDimensions || youtubeId(item)) && item.width > 0 && item.height > 0) {
      return Math.min(item.width, item.height);
    }
  }},
  { key: "duration", label: "duration", value: item => item.duration },
  { key: "size", label: "fileSize", value: item => item.size }
];
let searchSortKey;
let searchSortAscending = false;
let metadataScan;

function matchesFilter(item, filter) {
  if (filter === "assets") return Boolean(item.page_asset);
  return !item.page_asset && item.kind === filter;
}

function visibleItems() {
  const items = app.items.filter(item => matchesFilter(item, app.filter));
  const field = activeSearchSort();
  if (field) items.sort((first, second) => {
    const a = sortValue(field, first);
    const b = sortValue(field, second);
    if (a < 0 || b < 0) return Number(a < 0) - Number(b < 0);
    return searchSortAscending ? a - b : b - a;
  });
  return items;
}

function activeSearchSort() {
  return searchSortFields.find(field => field.key === searchSortKey && searchSortApplies(field));
}

function sortValue(field, item) {
  const value = field.value(item);
  return Number.isFinite(value) ? value : -1;
}

function searchSortApplies(field) {
  if (field.key === "size") return true;
  return app.items.some(item => matchesFilter(item, app.filter) && (field.key === "duration"
    ? item.kind === "video" || item.kind === "audio"
    : item.kind === "video" || item.kind === "image" || item.kind === "gif"));
}

function renderSearchSortMenu() {
  const menu = $("findSortMenu");
  const selected = activeSearchSort()?.key;
  const options = [{ label: "foundOrder" }, ...searchSortFields.filter(searchSortApplies)];
  menu.replaceChildren(...options.map(({ key, label }) => {
    const control = button(t(label), () => {
      searchSortKey = key;
      menu.hidePopover();
      $("findSort").focus();
      void refreshSearchSort();
    });
    control.setAttribute("role", "menuitemradio");
    control.setAttribute("aria-checked", String(key === selected));
    return control;
  }));
}

function inspectSize(item, sourceOrigin = item.source_origin) {
  item.inspect ??= invoke("inspect_media", { url: item.url, sourceOrigin }).catch(() => null);
  return item.inspect.then(info => {
    if (!info) return;
    item.extension = info.extension;
    item.size ??= info.size;
  });
}

function probeMedia(item, signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise(resolve => {
    const player = element(item.kind === "audio" ? "audio" : "video");
    const finish = () => {
      const duration = Number.isFinite(player.duration) ? player.duration : undefined;
      clearTimeout(timer);
      signal.removeEventListener("abort", finish);
      player.onloadedmetadata = null;
      player.onerror = null;
      player.removeAttribute("src");
      player.load();
      resolve(duration);
    };
    const timer = setTimeout(finish, 8000);
    signal.addEventListener("abort", finish, { once: true });
    player.preload = "metadata";
    player.onloadedmetadata = () => {
      if (Number.isFinite(player.duration)) item.duration ??= player.duration;
      if (player.videoWidth && player.videoHeight) {
        item.width = player.videoWidth;
        item.height = player.videoHeight;
        item.measuredDimensions = true;
      }
      finish();
    };
    player.onerror = finish;
    player.src = item.url;
  });
}

function probeImage(item) {
  return previewData(item.preview, true).then(data => new Promise(resolve => {
    if (!data) {
      resolve();
      return;
    }
    const img = new Image();
    img.onload = () => {
      item.width = img.naturalWidth;
      item.height = img.naturalHeight;
      item.measuredDimensions = true;
      resolve();
    };
    img.onerror = resolve;
    img.src = data;
  }));
}

function canProbe(item, field) {
  if (item.preview_only || youtubeId(item)) return false;
  if (field.key === "size") return true;
  if (field.key === "duration") return item.kind === "video" || item.kind === "audio";
  return item.kind === "video" || Boolean(item.preview && (item.kind === "image" || item.kind === "gif"));
}

async function refreshSearchSort() {
  metadataScan?.abort();
  const scan = new AbortController();
  metadataScan = scan;
  const field = activeSearchSort();
  const pending = field ? app.items.filter(item => matchesFilter(item, app.filter)
    && sortValue(field, item) < 0 && canProbe(item, field)) : [];
  app.sortLoading = pending.length > 0;
  $("resultsContent").scrollTop = 0;
  renderGallery();
  let index = 0;
  await Promise.all(Array.from({ length: Math.min(4, pending.length) }, async () => {
    while (!scan.signal.aborted && index < pending.length) {
      const item = pending[index++];
      if (sortValue(field, item) >= 0) continue;
      if (field.key === "size") await inspectSize(item);
      else if (item.kind === "video" || item.kind === "audio") await probeMedia(item, scan.signal);
      else await probeImage(item);
    }
  }));
  if (scan.signal.aborted || !app.sortLoading) return;
  app.sortLoading = false;
  renderGallery();
}

$("findSortDirection").addEventListener("click", () => {
  searchSortAscending = !searchSortAscending;
  $("resultsContent").scrollTop = 0;
  renderGallery();
});

function mediaThumbnail(item, className, { still = false } = {}) {
  const thumb = element("span", className);
  thumb.append(icon(item.kind));
  if (!still && item.local && item.kind === "video") {
    const video = element("video");
    video.preload = "metadata";
    video.muted = true;
    video.setAttribute("aria-hidden", "true");
    video.hidden = true;
    video.dataset.source = item.url;
    video.addEventListener("loadedmetadata", () => {
      video.currentTime = Number.isFinite(video.duration) ? Math.min(0.2, video.duration / 2) : 0;
    });
    const showFrame = () => { video.hidden = false; };
    video.addEventListener("loadeddata", showFrame, { once: true });
    video.addEventListener("seeked", showFrame, { once: true });
    video.addEventListener("error", () => video.remove(), { once: true });
    thumb.append(video);
    // Observe the visible tile while its video is hidden.
    savedVideoObserver.observe(thumb);
  } else if (item.preview) {
    const img = appendThumbnailImage(thumb);
    const photo = isSearchPhoto(item);
    const asset = isSearchImageAsset(item);
    if (photo) {
      thumb.append(element("span", "photo-preview-status"));
      setPhotoPreviewState(img, "loading");
    }
    if (photo || asset) {
      img.addEventListener("load", () => {
        if (asset && item.preview === item.url) {
          const mime = img.src.match(/^data:image\/([a-z0-9+-]+);/)?.[1];
          if (mime) item.extension = ({ jpeg: "jpg", "svg+xml": "svg", "x-icon": "ico" })[mime] ?? mime;
        }
        if (item.preview === item.url && mediaExtension(item)?.toLowerCase() !== "svg"
            && img.naturalWidth && img.naturalHeight) {
          item.width = img.naturalWidth;
          item.height = img.naturalHeight;
          item.measuredDimensions = true;
        }
        if (photo) setPhotoPreviewState(img, "ready");
      });
    }
    img.addEventListener("error", () => {
      if (photo) {
        img.removeAttribute("src");
        delete img.dataset.loaded;
        setPhotoPreviewState(img, "failed");
        return;
      }
      observer.unobserve(img);
      img.remove();
    });
    if (item.local) {
      img.loading = "lazy";
      img.src = item.preview;
    } else {
      img.dataset.preview = item.preview;
      observer.observe(img);
    }
  }
  return thumb;
}

function appendThumbnailImage(thumb) {
  const img = element("img");
  img.alt = "";
  img.addEventListener("load", () => { img.dataset.loaded = ""; });
  thumb.append(img);
  return img;
}

function qualityHeight(quality) {
  return quality.width && quality.height ? Math.min(quality.width, quality.height) : quality.height;
}

function qualityPicker(item, showSizes = false) {
  const control = element("select");
  control.setAttribute("aria-label", t("qualityFor", { name: item.name }));
  for (const [index, quality] of item.qualities.entries()) {
    const resolution = qualityHeight(quality);
    const description = resolution
      ? t("qualityResolution", { n: resolution, format: quality.extension.toUpperCase() })
      : t("qualityUnspecified", { format: quality.extension.toUpperCase() });
    const name = index === 0 ? t("qualityBest", { description }) : description;
    const option = element("option", "", showSizes && quality.size > 0 ? `${name} · ${sizeText(quality.size)}` : name);
    option.dataset.name = name;
    option.value = String(index);
    control.append(option);
  }
  control.value = String(item.qualityIndex ?? 0);
  control.addEventListener("change", () => {
    item.qualityIndex = Number(control.value);
  });
  if (showSizes) control.addEventListener("focus", async () => {
    for (const [index, quality] of item.qualities.entries()) {
      if (document.activeElement !== control || !control.isConnected) break;
      if (quality.size != null || quality.format_id || !quality.url
          || new URL(quality.url).pathname.toLowerCase().endsWith(".m3u8")) continue;
      const source = { url: quality.url };
      await inspectSize(source, item.source_origin);
      if (source.size > 0 && control.isConnected) {
        const option = control.options[index];
        option.textContent = `${option.dataset.name} · ${sizeText(source.size)}`;
      }
    }
  });
  const label = element("label", "card-quality");
  label.append(element("span", "", t("downloadQuality")), control);
  return label;
}

const qualityProbes = new WeakMap();

function probeQuality(item, quality) {
  if (!quality.url) return Promise.resolve(null);
  if (!qualityProbes.has(quality)) {
    const url = new URL(quality.url);
    const pending = url.pathname.toLowerCase().endsWith(".m3u8")
      ? invoke("inspect_stream_resolution", { url: quality.url, sourceOrigin: item.source_origin })
      : invoke("set_preview_source", { url: quality.url, sourceOrigin: item.source_origin }).then(async () => {
        const video = { url: quality.url, kind: "video" };
        await probeMedia(video, new AbortController().signal);
        return video.width && video.height ? [video.width, video.height] : null;
      });
    qualityProbes.set(quality, pending.catch(() => null));
  }
  return qualityProbes.get(quality);
}

function downloadQualityDetails(item) {
  const section = element("section", "details-qualities");
  const heading = element("h3", "", t("downloadQuality"));
  const metadata = element("dl", "metadata");
  section.append(heading, metadata);
  const qualities = item.qualities?.length ? item.qualities : [item];
  const refreshRows = qualities.map(quality => {
    const row = element("div");
    const label = element("dt");
    const value = element("dd");
    row.append(label, value);
    metadata.append(row);

    let measured;
    const dimensions = () => {
      if (measured) return measured;
      const source = !quality.format_id && quality.url === item.url && item.measuredDimensions ? item : quality;
      return source.width && source.height ? [source.width, source.height] : null;
    };
    function refresh() {
      const size = dimensions();
      const height = size ? Math.min(...size) : qualityHeight(quality);
      const format = quality.extension ?? mediaExtension(item);
      label.textContent = height ? `${height}p` : format?.toUpperCase() ?? t("video");
      value.textContent = size ? `${size[0]} × ${size[1]} px`
        : height ? t("qualityHeightOnly", { n: quality.height }) : t("unknownValue");
    }
    if (!dimensions() && quality !== item && quality.url) {
      row.tabIndex = 0;
      const resolve = async () => {
        if (dimensions()) return;
        value.textContent = t("readingMetadata");
        measured = await probeQuality(item, quality);
        refresh();
      };
      row.addEventListener("pointerenter", resolve);
      row.addEventListener("focus", resolve);
    }
    refresh();
    return refresh;
  });
  return { section, refresh: () => refreshRows.forEach(refresh => refresh()) };
}

function renderGallery() {
  for (const img of $("gallery").querySelectorAll("img")) observer.unobserve(img);
  const items = visibleItems();
  const categoryFocus = $("filters").contains(document.activeElement) ? document.activeElement.dataset.kind : null;
  const hasResults = app.phase === "results";
  const sort = activeSearchSort();
  if (!items.includes(app.focus)) app.focus = items[0] ?? null;
  $("resultCount").hidden = app.items.length === 0;
  $("resultCount").textContent = localizedNumber(items.length);
  $("resultsStatus").hidden = items.length > 0;
  $("gallery").hidden = items.length === 0;
  $("gallery").dataset.mediaType = app.filter;
  $("findFilterBar").hidden = !hasResults || app.items.length === 0;
  const sortButton = $("findSort");
  if ($("findFilterBar").hidden) {
    $("filters").hidePopover();
    $("findSortMenu").hidePopover();
  }
  const criterion = t(sort?.label ?? "foundOrder");
  $("findSortLabel").textContent = criterion;
  sortButton.setAttribute("aria-label", t("sortResultsBy", { field: criterion }));
  const sortDirection = $("findSortDirection");
  const direction = t(searchSortAscending ? "sortSmallestFirst" : "sortLargestFirst");
  sortDirection.disabled = !sort;
  sortDirection.title = direction;
  sortDirection.setAttribute("aria-label", t("changeSortDirection", { direction }));
  sortDirection.firstElementChild.textContent = searchSortAscending ? "\uE74A" : "\uE74B";
  $("findSortStatus").hidden = !app.sortLoading;
  $("findSortStatus").textContent = t("readingMetadata");
  $("allControl").hidden = items.length === 0;
  $("searchError").hidden = !app.error;
  $("url").setAttribute("aria-invalid", String(app.error === "invalidUrl"));
  $("gallery").setAttribute("aria-busy", String(app.phase === "loading" || app.sortLoading));
  $("searchError").textContent = app.error ? t(app.error) : "";
  $("openBlockedPage").hidden = !["pageForbidden", "pageUnauthorized"].includes(app.error);
  $("collectionStatus").hidden = !hasResults || !app.collectionPartial;
  $("collectionStatus").textContent = t("collectionPartial");
  if (app.phase === "loading") {
    $("resultsStatus").replaceChildren(icon("search"), element("p", "", t(app.collectionScan ? "scanningCollection" : "searching")));
  } else if (app.phase === "error") {
    empty($("resultsStatus"), "search", "searchErrorTitle", "searchErrorHint");
  } else if (app.phase === "initial") {
    empty($("resultsStatus"), "search", "initialTitle", "initialHint");
  } else if (app.items.length) {
    empty($("resultsStatus"), "search", "emptyCategory", "emptyCategoryHint");
  } else {
    empty($("resultsStatus"), "image", "emptyTitle", "emptyHint");
  }

  $("filters").replaceChildren();
  for (const [kind, key] of galleryFilters) {
    const glyph = kind === "assets" ? "folder" : kind;
    const active = app.filter === kind;
    const label = t(key);
    const control = button(label, () => {
      app.filter = kind;
      $("filters").hidePopover();
      $("findMediaType").focus();
      void refreshSearchSort();
    }, glyph);
    control.dataset.kind = kind;
    control.setAttribute("role", "menuitemradio");
    control.setAttribute("aria-checked", String(active));
    $("filters").append(control);
    if (active) {
      $("findMediaTypeIcon").replaceChildren(icon(glyph));
      $("findMediaTypeLabel").textContent = label;
      $("findMediaType").setAttribute("aria-label", t("findMediaTypeLabel", { type: label }));
    }
  }
  if (categoryFocus) $("filters").querySelector(`[data-kind="${categoryFocus}"]`)?.focus();
  const canBatch = app.items.filter(item => !item.preview_only).length > 1;
  $("gallery").replaceChildren(...items.map(item => mediaCard(item, canBatch)));
  updateSelection();
  renderFileDetails(app, items, "findDetails");
  positionSearchMenus();
}

function positionSearchMenus() {
  for (const [menuId, triggerId] of [["filters", "findMediaType"], ["findSortMenu", "findSort"]]) {
    const popup = $(menuId);
    if (!popup.matches(":popover-open")) continue;
    const anchor = $(triggerId).getBoundingClientRect();
    const below = anchor.bottom + 6;
    const top = below + popup.offsetHeight <= innerHeight - 8 ? below : Math.max(8, anchor.top - popup.offsetHeight - 6);
    popup.style.top = `${top}px`;
    popup.style.left = `${Math.max(8, Math.min(anchor.left, innerWidth - popup.offsetWidth - 8))}px`;
  }
}

window.addEventListener("resize", positionSearchMenus);
window.addEventListener("scroll", positionSearchMenus, { passive: true });

function mediaCard(item, canBatch = false) {
    const card = element("article", "card");
    card.dataset.url = item.url;
    const state = item.local ? saved : app;
    const panelId = item.local ? "savedDetails" : "findDetails";
    const open = button(item.name, () => {
      state.focus = item;
      state.detailsUrl = item.url;
      renderFileDetails(state, item.local ? savedItems() : visibleItems(), panelId);
      if (!item.local) updateSelection();
    });
    open.className = "card-button";
    open.setAttribute("aria-controls", panelId);
    open.setAttribute("aria-keyshortcuts", "Space");
    open.setAttribute("aria-label", t("fileDetailsFor", { name: searchMediaName(item) }));
    open.addEventListener("focus", () => {
      state.focus = item;
      if (!item.local) updateSelection();
    });
    open.addEventListener("keydown", event => {
      if (event.key !== " " || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
      event.preventDefault();
      if (!event.repeat) openPreview(item);
    });
    const thumb = mediaThumbnail(item, "thumb");
    if (item.local) {
      const media = thumb.querySelector("img,video");
      media?.addEventListener(media.tagName === "VIDEO" ? "loadedmetadata" : "load", () => {
        const width = media.videoWidth || media.naturalWidth;
        const height = media.videoHeight || media.naturalHeight;
        if (width && height) {
          item.riverRatio = width / height;
          scheduleSavedLayout();
        }
      });
      open.title = item.name;
      open.replaceChildren(thumb);
      if (item.kind === "video" || item.kind === "audio") {
        const badge = element("span", "tile-type");
        badge.append(icon(item.kind));
        open.append(badge);
      }
      card.append(open);
      return card;
    }
    const title = element("span", "card-name", searchMediaName(item));
    title.title = title.textContent;
    const caption = element("span", "card-type", searchCardCaption(item));
    if (isSearchPhoto(item) || isSearchImageAsset(item)) thumb.querySelector("img")?.addEventListener("load", () => {
      caption.textContent = searchCardCaption(item);
    });
    open.replaceChildren(thumb, title, caption);
    card.append(open);
    const photoImage = isSearchPhoto(item) ? thumb.querySelector("img") : null;
    if (photoImage) {
      const retry = button(t("retry"), () => {
        open.focus({ preventScroll: true });
        thumb.querySelector(".photo-preview-status").setAttribute("role", "status");
        retryPhotoPreview(item, photoImage);
      }, "retry", true);
      retry.classList.add("card-retry");
      retry.hidden = true;
      thumb.addEventListener("photo-preview-state", () => {
        retry.hidden = thumb.dataset.previewState !== "failed";
      });
      card.append(retry);
    }
    if (item.preview_only || !canBatch) return card;
    const checkLabel = element("label", "card-check");
    const check = element("input");
    check.type = "checkbox";
    check.setAttribute("aria-label", t("selectItem", { name: searchMediaName(item) }));
    check.addEventListener("change", () => {
      if (check.checked) {
        app.selected.add(item.url);
        app.focus = item;
      } else {
        app.selected.delete(item.url);
      }
      updateSelection();
    });
    checkLabel.append(check);
    card.append(checkLabel);
    return card;
}

function searchCardCaption(item) {
  if (isSearchImageAsset(item)) {
    const format = mediaExtension(item)?.toUpperCase() ?? t("unknownValue");
    const dimensions = assetDimensions(item);
    return dimensions ? t("formatWithDimensions", { format, dimensions }) : format;
  }
  if (isSearchPhoto(item) && item.measuredDimensions && item.width && item.height) {
    return t("photoWithDimensions", { dimensions: `${item.width} × ${item.height}` });
  }
  if (item.preview_only || item.kind !== "video") return t(item.preview_only ? "previewOnly" : item.kind);
  const quality = qualityHeight(item.qualities?.[0] ?? (item.measuredDimensions ? item : {}));
  const duration = Number.isFinite(item.duration) ? durationText(item.duration) : null;
  return [quality && `${quality}p`, duration].filter(Boolean).join(" · ") || t("video");
}

function assetDimensions(item) {
  if (mediaExtension(item)?.toLowerCase() !== "svg" && item.measuredDimensions && item.width && item.height) {
    return `${item.width} × ${item.height}`;
  }
  return null;
}

function refreshSearchCardCaption(item) {
  const card = Array.from($("gallery").children).find(card => card.dataset.url === item.url);
  const caption = card?.querySelector(".card-type");
  if (caption) caption.textContent = searchCardCaption(item);
}

const savedSizes = {
  small: { height: 190, widthFraction: 0.13 },
  medium: { height: 274, widthFraction: 0.175 },
  large: { height: 350, widthFraction: 0.225 }
};
const savedSize = localStorage.getItem("savedSize");
const savedSortBy = localStorage.getItem("savedSortBy");
const saved = { items: [], filter: "all", focus: null, detailsUrl: null, loading: false, error: false,
  sortBy: ["modified_at", "name"].includes(savedSortBy) ? savedSortBy : "created_at",
  sortOrder: localStorage.getItem("savedSortOrder") === "ascending" ? "ascending" : "descending",
  mode: localStorage.getItem("savedView") === "square" ? "square" : "flow",
  size: Object.hasOwn(savedSizes, savedSize) ? savedSize : "medium", rows: [], frame: 0, periods: [], currentPeriod: null };
const detailFrames = new WeakMap();

const fileTitleWidths = new WeakMap();
const fileTitleObserver = new ResizeObserver(entries => {
  for (const { target, contentRect: { width } } of entries) {
    if (!width || width === fileTitleWidths.get(target)) continue;
    fileTitleWidths.set(target, width);
    const tail = target.lastElementChild;
    const suffix = tail.dataset.suffix;
    const prefix = target.title.slice(0, -suffix.length);
    const text = target.firstChild;
    text.data = prefix;
    tail.textContent = suffix;
    target.classList.remove("details-title-truncated");
    if (target.scrollHeight <= target.clientHeight) continue;
    target.classList.add("details-title-truncated");
    const characters = Array.from(new Intl.Segmenter().segment(prefix), part => part.segment);
    const show = length => {
      while (length && !characters[length - 1].trim()) length--;
      const split = Math.max(0, length - 1);
      text.data = characters.slice(0, split).join("");
      tail.textContent = characters.slice(split, length).join("") + "…" + suffix;
    };
    let low = 0;
    let high = characters.length;
    while (low < high) {
      const midpoint = Math.ceil((low + high) / 2);
      show(midpoint);
      if (target.scrollHeight <= target.clientHeight && target.scrollWidth <= target.clientWidth) low = midpoint;
      else high = midpoint - 1;
    }
    show(low);
  }
});

function renderFileDetails(state, items, panelId) {
  const panel = $(panelId);
  const item = items.find(candidate => candidate.url === state.detailsUrl);
  const cards = panel.previousElementSibling.querySelectorAll(".card");
  for (const card of cards) {
    const active = card.dataset.url === item?.url;
    card.querySelector(".card-button").setAttribute("aria-expanded", String(active));
    if (panelId === "savedDetails") card.classList.toggle("active", active);
  }
  panel.hidden = !item;
  detailFrames.get(panel)?.();
  for (const image of panel.querySelectorAll("img")) observer.unobserve(image);
  const oldHeading = panel.querySelector(".file-details-title");
  if (oldHeading) fileTitleObserver.unobserve(oldHeading);
  panel.replaceChildren();
  if (!item) return;

  const name = searchMediaName(item);
  const heading = element("h2", "file-details-title", name);
  const suffix = name.match(/(?:\d{2,5}x\d{2,5})?\.[a-z0-9]{1,8}$/i)?.[0];
  heading.title = name;
  heading.setAttribute("aria-label", name);
  if (suffix) {
    const tail = element("span", "details-title-suffix", suffix);
    tail.dataset.suffix = suffix;
    heading.replaceChildren(document.createTextNode(name.slice(0, -suffix.length)), tail);
    fileTitleObserver.observe(heading);
  }
  heading.id = panelId + "Title";
  panel.setAttribute("aria-labelledby", heading.id);
  const thumb = mediaThumbnail(item, "thumb", { still: true });
  const details = describeMedia(item, item.local ? null : youtubeId(item));
  const header = element("div", "details-header");
  const close = button(t("closeDetails"), () => {
    state.detailsUrl = null;
    renderFileDetails(state, items, panelId);
    Array.from(cards).find(card => card.dataset.url === item.url)?.querySelector("button").focus();
  }, "close", true);
  close.classList.add("details-close");
  header.append(heading, close);
  const body = element("div", "details-body");
  panel.append(header, body);
  const preview = button(t("openQuickPreview"), () => openPreview(item), "search", true);
  preview.classList.add("details-preview");
  if (item.local) preview.title = t("quickPreviewShortcut");
  preview.prepend(thumb);
  body.append(preview);
  const previewDuration = details.duration ? element("p", "details-preview-caption muted") : null;
  if (previewDuration) {
    renderPreviewDuration();
    details.duration.closest("div").remove();
    body.append(previewDuration);
  }
  const information = element("details", "details-information");
  information.append(element("summary", "", t("previewDetails")), details.metadata);
  const fileName = element("div");
  fileName.append(element("dt", "", t("fileName")), element("dd", "", item.name));
  details.metadata.prepend(fileName);
  let updateQualities;
  if (!item.local) {
    if (item.kind === "video" && !item.preview_only) {
      const { section, refresh } = downloadQualityDetails(item);
      body.append(section);
      updateQualities = refresh;
    }
    if (isSearchImageAsset(item)) {
      const updateAssetDetails = () => {
        details.format.textContent = mediaExtension(item)?.toUpperCase() ?? t("unknownValue");
        details.dimensions.textContent = assetDimensions(item) ?? t("unknownValue");
      };
      updateAssetDetails();
      thumb.querySelector("img")?.addEventListener("load", () => {
        updateAssetDetails();
        refreshSearchCardCaption(item);
      });
    }
    if (isSearchPhoto(item)) {
      const original = button(t("openOriginalImage"), () => {
        void invoke("open_page", { url: item.url }).catch(error => notify(errorKey(error)));
      }, "link");
      original.classList.add("details-original");
      original.title = item.url;
      information.append(original);
      const img = thumb.querySelector("img");
      if (img) {
        const status = thumb.querySelector(".photo-preview-status");
        status.id = "findPhotoPreviewStatus";
        status.setAttribute("role", "status");
        preview.setAttribute("aria-describedby", status.id);
        img.addEventListener("load", () => {
          if (item.measuredDimensions) details.dimensions.textContent = `${item.width} × ${item.height}`;
        });
        const retry = button(t("retry"), () => {
          preview.focus({ preventScroll: true });
          retryPhotoPreview(item, img);
        }, "retry");
        retry.classList.add("details-retry");
        retry.hidden = true;
        thumb.addEventListener("photo-preview-state", () => { retry.hidden = thumb.dataset.previewState !== "failed"; });
        body.append(retry);
      }
    }
  }
  body.append(information);

  function renderPreviewDuration() {
    if (!previewDuration) return;
    const seconds = item.previewDuration ?? item.duration;
    previewDuration.hidden = !Number.isFinite(seconds);
    previewDuration.textContent = t("previewDuration", { duration: durationText(seconds) });
  }
  if (item.local && item.kind === "video") {
    const cached = item.still;
    if (cached) {
      const image = appendThumbnailImage(thumb);
      image.src = cached;
    } else {
      const probe = element("video");
      const timeout = setTimeout(cleanup, 5000);
      probe.crossOrigin = "anonymous";
      probe.muted = true;
      probe.preload = "auto";
      probe.addEventListener("error", cleanup, { once: true });
      probe.addEventListener("loadedmetadata", () => {
        updateMediaDetails(item, probe, details);
        renderPreviewDuration();
        if (probe.duration > 0.2) probe.currentTime = 0.2;
      });
      probe.addEventListener("seeked", capture);
      probe.addEventListener("loadeddata", capture);
      detailFrames.set(panel, cleanup);
      probe.src = item.url;

      function capture() {
        if (probe.seeking || !probe.videoWidth || probe.readyState < 2) return;
        const canvas = element("canvas");
        const scale = Math.min(1, 480 / Math.max(probe.videoWidth, probe.videoHeight));
        canvas.width = Math.max(1, Math.round(probe.videoWidth * scale));
        canvas.height = Math.max(1, Math.round(probe.videoHeight * scale));
        try {
          canvas.getContext("2d").drawImage(probe, 0, 0, canvas.width, canvas.height);
          if (!thumb.isConnected) return;
          item.still = canvas.toDataURL("image/jpeg", 0.78);
          const image = appendThumbnailImage(thumb);
          image.src = item.still;
        } catch { /* Keep the file icon when a frame is unavailable. */ }
        finally { cleanup(); }
      }
      function cleanup() {
        clearTimeout(timeout);
        detailFrames.delete(panel);
        probe.removeAttribute("src");
        probe.load();
      }
    }
  }
  if (item.local) {
    const openFolder = button(t("openFolder"), async () => {
      try { await invoke("open_folder", { id: item.id }); }
      catch (error) { notify(errorKey(error)); }
    }, "folder");
    body.append(openFolder);
  }
  if (!item.local && !item.preview_only && !youtubeId(item)) {
    if (item.size == null || !mediaExtension(item)) {
      void inspectSize(item).then(() => {
        details.size.textContent = sizeText(item.size);
        details.format.textContent = mediaExtension(item)?.toUpperCase() ?? t("unknownValue");
      });
    }
    if (details.duration && (!Number.isFinite(item.duration)
        || (item.kind === "video" && !item.measuredDimensions))) {
      const scan = new AbortController();
      detailFrames.set(panel, () => scan.abort());
      void invoke("set_preview_source", { url: item.url, sourceOrigin: item.source_origin })
        .catch(() => {})
        .then(() => probeMedia(item, scan.signal))
        .then(seconds => {
          if (scan.signal.aborted) return;
          item.previewDuration ??= seconds;
          if (item.width && item.height) details.dimensions.textContent = item.width + " × " + item.height;
          renderPreviewDuration();
          updateQualities?.();
          refreshSearchCardCaption(item);
        });
    }
  }
}

function savedItems() {
  const query = $("savedSearch").value.trim().toLocaleLowerCase(language);
  const collator = new Intl.Collator(language, { numeric: true, sensitivity: "base" });
  const direction = saved.sortOrder === "ascending" ? 1 : -1;
  return saved.items.filter(item => (saved.filter === "all" || item.kind === saved.filter) &&
    item.name.toLocaleLowerCase(language).includes(query)).sort((a, b) => {
    const nameOrder = collator.compare(a.name, b.name);
    if (saved.sortBy === "name") return direction * nameOrder || a.url.localeCompare(b.url);
    const first = a[saved.sortBy];
    const second = b[saved.sortBy];
    if (first == null || second == null) {
      const unknownOrder = Number(first == null) - Number(second == null);
      return unknownOrder || nameOrder || a.url.localeCompare(b.url);
    }
    return direction * (first - second) || nameOrder || a.url.localeCompare(b.url);
  });
}

function savedAspect(item) {
  const ratio = item.riverRatio ?? (item.kind === "video" ? 16 / 9 : 4 / 3);
  return Math.max(0.45, Math.min(2.9, ratio));
}

function scheduleSavedLayout() {
  if (saved.frame) return;
  saved.frame = requestAnimationFrame(() => {
    saved.frame = 0;
    const gallery = $("savedGallery");
    const width = gallery.clientWidth;
    if (!width) return;
    if (saved.mode === "square") {
      updateSavedTimeline();
      return;
    }
    const gap = Number.parseFloat(getComputedStyle(gallery).getPropertyValue("--gallery-gap"));
    const { height, widthFraction } = savedSizes[saved.size];
    const preferred = Math.min(height, Math.max(124, width * widthFraction));
    for (const tiles of saved.rows) {
      let start = 0;
      let aspect = 0;
      for (const [index, tile] of tiles.entries()) {
        const ratio = savedAspect(tile.item);
        const availableWithTile = width - gap * (index - start);
        if (index > start && (aspect + ratio) * preferred >= availableWithTile) {
          const before = (availableWithTile + gap) / aspect;
          const after = availableWithTile / (aspect + ratio);
          if (before - preferred < preferred - after) {
            sizeSavedRow(tiles, start, index, before, availableWithTile + gap);
            start = index;
            aspect = 0;
          }
        }
        aspect += ratio;
        const available = width - gap * (index - start);
        const full = aspect * preferred >= available;
        if (!full && index < tiles.length - 1) continue;
        sizeSavedRow(tiles, start, index + 1, full ? available / aspect : preferred, full ? available : null);
        start = index + 1;
        aspect = 0;
      }
    }
    updateSavedTimeline();
  });
}

function sizeSavedRow(tiles, start, end, height, available) {
  let pixels = 0;
  for (let index = start; index < end; index++) {
    const { item, card } = tiles[index];
    const width = available != null && index === end - 1
      ? available - pixels : Math.floor(savedAspect(item) * height);
    card.style.width = `${width}px`;
    card.style.height = `${Math.floor(height)}px`;
    pixels += width;
  }
}

function markSavedPeriod(control) {
  if (saved.currentPeriod === control) return;
  saved.currentPeriod?.removeAttribute("aria-current");
  control?.setAttribute("aria-current", "true");
  saved.currentPeriod = control;
}

function updateSavedTimeline() {
  if (!saved.periods.length || $("savedPanel").hidden) return;
  const panel = $("savedStream");
  const edge = Math.max(0, panel.getBoundingClientRect().top) + 32;
  const atEnd = panel.scrollTop > 0 && panel.scrollTop + panel.clientHeight >= panel.scrollHeight - 2;
  const current = atEnd ? saved.periods.at(-1)
    : saved.periods.findLast(period => period.heading.getBoundingClientRect().top <= edge) ?? saved.periods[0];
  markSavedPeriod(current.control);
}

new ResizeObserver(scheduleSavedLayout).observe($("savedGallery"));
$("savedStream").addEventListener("scroll", updateSavedTimeline, { passive: true });
window.addEventListener("scroll", updateSavedTimeline, { passive: true });

const savedMenuGroups = [
  ["savedSort", "savedSortMenu", [
    ["sortBy", () => [["created_at", "savedCreated", "\uE787"], ["modified_at", "savedModified", "\uE70F"],
      ["name", "fileName", "\uE8D2"]]],
    ["sortOrder", () => [["ascending", "savedAscending", "\uE74A"], ["descending", "savedDescending", "\uE74B"]]]
  ]],
  ["savedFilters", "savedFilterMenu", [
    ["filter", () => [["all", "savedAll", "\uE7AA"], ...[
      ["image", "images", "\uEB9F"], ["video", "videos", "\uE714"],
      ["gif", "gif", "\uE8B9"], ["audio", "audioList", "\uE8D6"]
    ].filter(([kind]) => saved.items.some(item => item.kind === kind))]]
  ]],
  ["savedView", "savedViewMenu", [
    ["mode", () => [["flow", "savedFlow", "\uE80A"], ["square", "savedSquare", "\uE8A9"]]],
    ["size", () => [["small", "savedSmall", "\uECA5"], ["medium", "savedMedium", "\uE8A9"], ["large", "savedLarge", "\uE739"]]]
  ]]
];

for (const [triggerId, menuId] of [...savedMenuGroups, ["findMediaType", "filters"], ["findSort", "findSortMenu"], ["queueMedia", "queueMediaMenu"],
  ["queueConnections", "queueConnectionsMenu"]]) {
  const trigger = $(triggerId);
  const menu = $(menuId);
  menu.addEventListener("toggle", event => {
    const open = event.newState === "open";
    trigger.setAttribute("aria-expanded", String(open));
    if (!open) return;
    if (triggerId === "findSort") renderSearchSortMenu();
    if (triggerId === "findMediaType" || triggerId === "findSort") {
      positionSearchMenus();
    } else {
      const anchor = trigger.getBoundingClientRect();
      menu.style.left = `${Math.max(8, anchor.right - menu.offsetWidth)}px`;
      menu.style.top = `${Math.min(anchor.bottom + 6, innerHeight - menu.offsetHeight - 8)}px`;
    }
    (menu.querySelector('[aria-checked="true"]') ?? menu.querySelector("button:not(:disabled)"))?.focus();
  });
  menu.addEventListener("keydown", event => {
    if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const items = Array.from(menu.querySelectorAll("button:not(:disabled)"));
    if (!items.length) return;
    const offset = event.key === "ArrowUp" ? -1 : 1;
    const index = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
      : (items.indexOf(document.activeElement) + offset + items.length) % items.length;
    items[index]?.focus();
  });
  trigger.addEventListener("keydown", event => {
    if (event.key === "ArrowDown" && !menu.matches(":popover-open")) {
      event.preventDefault();
      menu.showPopover({ source: trigger });
    }
  });
}

function renderSavedMenus() {
  for (const [triggerId, menuId, groups] of savedMenuGroups) {
    $(triggerId).hidden = !saved.items.length;
    const menu = $(menuId);
    const sections = groups.map(([setting, options]) => {
      const group = element("div", "saved-menu-group");
      group.setAttribute("role", "group");
      for (const [value, key, glyph] of options()) {
        const option = button(t(key), () => {
          saved[setting] = value;
          if (setting === "size") localStorage.setItem("savedSize", value);
          if (setting === "mode") localStorage.setItem("savedView", value);
          if (setting === "sortBy" || setting === "sortOrder") {
            localStorage.setItem(setting === "sortBy" ? "savedSortBy" : "savedSortOrder", value);
            $("savedStream").scrollTop = 0;
          }
          menu.hidePopover();
          $(triggerId).focus();
          renderSaved();
        });
        const symbol = element("span", "fluent-icon", glyph);
        symbol.setAttribute("aria-hidden", "true");
        option.prepend(symbol);
        option.setAttribute("role", "menuitemradio");
        option.setAttribute("aria-checked", String(saved[setting] === value));
        group.append(option);
      }
      return group;
    });
    menu.replaceChildren(...sections);
  }
  $("savedFilters").toggleAttribute("data-active", saved.filter !== "all");
  $("savedSort").toggleAttribute("data-active", saved.sortBy !== "created_at" || saved.sortOrder !== "descending");
  $("savedView").toggleAttribute("data-active", saved.mode !== "flow" || saved.size !== "medium");
}

function renderSaved() {
  const items = savedItems();
  const gallery = $("savedGallery");
  const oldFocus = gallery.contains(document.activeElement) ? document.activeElement.closest(".card")?.dataset.url : null;
  saved.focus = items.find(item => item.url === saved.focus?.url) ?? null;
  if (!items.some(item => item.url === saved.detailsUrl)) saved.detailsUrl = null;
  const counts = new Map();
  for (const item of items) counts.set(item.kind, (counts.get(item.kind) ?? 0) + 1);
  const plural = new Intl.PluralRules(language);
  $("savedCount").textContent = ["image", "video", "gif", "audio"].filter(kind => counts.has(kind)).map(kind => {
    const n = counts.get(kind);
    return t(`saved_${kind}_${plural.select(n)}`, { n: localizedNumber(n) });
  }).join(", ");
  $("refreshSaved").disabled = saved.loading;
  $("savedStream").hidden = !items.length;
  gallery.dataset.mode = saved.mode;
  gallery.dataset.size = saved.size;
  renderSavedMenus();
  const status = $("savedStatus");
  status.hidden = items.length > 0;
  if (saved.loading) status.replaceChildren(element("p", "", t("loadingSaved")));
  else if (saved.error) empty(status, "image", "savedError", "savedErrorHint");
  else if (saved.items.length) empty(status, "search", "savedNoMatch", "savedNoMatchHint");
  else empty(status, "image", "savedEmpty", "savedHint");
  for (const thumb of gallery.querySelectorAll(".thumb")) savedVideoObserver.unobserve(thumb);
  const dateFormat = new Intl.DateTimeFormat(language, { dateStyle: "long" });
  const monthFormat = new Intl.DateTimeFormat(language, { month: "long", year: "numeric" });
  const fragment = document.createDocumentFragment();
  const periods = new Map();
  const byName = saved.sortBy === "name";
  let lastDay;
  let grid;
  let tiles;
  saved.rows = [];
  for (const item of items) {
    const timestamp = byName ? null : item[saved.sortBy];
    const date = timestamp == null ? null : new Date(timestamp);
    const day = byName ? null : date ? dateFormat.format(date) : t("savedUndated");
    if (day !== lastDay) {
      lastDay = day;
      const group = element(byName ? "div" : "section", "saved-day");
      grid = element("div", "river");
      tiles = [];
      saved.rows.push(tiles);
      if (!byName) {
        const heading = element("h2", "", day);
        heading.id = `saved-day-${saved.rows.length}`;
        group.setAttribute("aria-labelledby", heading.id);
        group.append(heading);
        const year = date?.getFullYear();
        const month = date ? monthFormat.format(date) : day;
        if (!periods.has(month)) periods.set(month, { heading, year });
      }
      group.append(grid);
      fragment.append(group);
    }
    const card = mediaCard(item);
    grid.append(card);
    tiles.push({ card, item });
  }
  gallery.replaceChildren(fragment);
  const timeline = $("savedTimeline");
  const multipleYears = new Set(Array.from(periods.values(), period => period.year).filter(year => year != null)).size > 1;
  const years = new Set();
  const stops = [];
  for (const [month, { heading, year }] of periods) {
    const showYear = multipleYears && year != null && !years.has(year);
    years.add(year);
    stops.push({ label: showYear ? String(year) : month, heading, showYear });
  }
  timeline.hidden = stops.length < 2;
  saved.currentPeriod = null;
  saved.periods = stops.map(({ label, heading, showYear }) => {
    const control = button(label, () => {
      heading.tabIndex = -1;
      heading.focus({ preventScroll: true });
      heading.scrollIntoView({ block: "start" });
      markSavedPeriod(control);
    }, null, true);
    if (showYear) control.append(element("span", "", label));
    return { heading, control };
  });
  timeline.replaceChildren(...saved.periods.map(({ control }) => control));
  renderFileDetails(saved, items, "savedDetails");
  if (oldFocus) Array.from(gallery.querySelectorAll(".card")).find(card => card.dataset.url === oldFocus)
    ?.querySelector("button").focus({ preventScroll: true });
  scheduleSavedLayout();
}

async function refreshSaved() {
  if (saved.loading) return;
  saved.loading = true;
  saved.error = false;
  if (!saved.items.length) renderSaved();
  else $("refreshSaved").disabled = true;
  try {
    const files = await invoke("saved_gallery");
    const previous = new Map(saved.items.map(item => [item.id, item]));
    saved.items = files.map(({ path, ...file }) => {
      const url = window.__TAURI__.core.convertFileSrc(path);
      const preview = file.kind === "image" || file.kind === "gif" ? url : null;
      const cached = previous.get(file.id);
      const sameFile = cached?.url === url && cached.size === file.size && cached.modified_at === file.modified_at;
      return { ...file, url, preview, local: true, riverRatio: sameFile ? cached.riverRatio : undefined,
        still: sameFile ? cached.still : undefined };
    });
    if (saved.filter !== "all" && !saved.items.some(item => item.kind === saved.filter)) saved.filter = "all";
  } catch {
    saved.items = [];
    saved.error = true;
  } finally {
    saved.loading = false;
    renderSaved();
  }
}
$("refreshSaved").addEventListener("click", refreshSaved);
$("savedSearch").addEventListener("input", renderSaved);

function singleDownload(visible = visibleItems().filter(item => !item.preview_only)) {
  return app.selected.size === 0 && visible.length === 1 ? visible[0] : null;
}

function updateSelection() {
  for (const card of $("gallery").children) {
    const checked = app.selected.has(card.dataset.url);
    const checkbox = card.querySelector("input");
    if (checkbox) checkbox.checked = checked;
    card.classList.toggle("selected", checked);
    card.classList.toggle("active", app.focus?.url === card.dataset.url);
  }
  const visible = visibleItems().filter(item => !item.preview_only);
  $("allControl").hidden = visible.length < 2;
  const selected = visible.filter(item => app.selected.has(item.url)).length;
  $("selectAll").checked = visible.length > 0 && selected === visible.length;
  $("selectAll").indeterminate = selected > 0 && selected < visible.length;
  const outside = app.selected.size - selected;
  const count = $("selectionCount");
  const label = t(outside ? "selectedOutsideFilter" : "selected", {
    n: localizedNumber(app.selected.size), outside: localizedNumber(outside)
  });
  if (count.textContent !== label) count.textContent = label;
  const single = singleDownload(visible);
  $("gallery").toggleAttribute("data-single-download", Boolean(single));
  $("selectionCount").hidden = Boolean(single);
  $("enqueueLabel").textContent = t(single ? "downloadFile" : "downloadSelected");
  $("enqueue").disabled = app.submitting || (!single && app.selected.size === 0) || !app.folder;
}
$("selectAll").addEventListener("change", event => {
  for (const item of visibleItems().filter(item => !item.preview_only)) {
    if (event.target.checked) app.selected.add(item.url);
    else app.selected.delete(item.url);
  }
  updateSelection();
});

function youtubeId(item) {
  if (item.kind !== "video") return null;
  const url = new URL(item.url);
  if (url.origin !== "https://www.youtube.com") return null;
  const id = url.pathname === "/watch" ? url.searchParams.get("v") : /^\/embed\/([\w-]{11})\/?$/.exec(url.pathname)?.[1];
  return /^[\w-]{11}$/.test(id ?? "") ? id : null;
}

let previewItems = [];
let previewIndex = -1;

function openPreview(item) {
  detailFrames.get($("findDetails"))?.();
  if (!item || $("zoom").open) return;
  previewItems = item.local ? savedItems() : visibleItems();
  const index = previewItems.indexOf(item);
  if (index < 0) return;
  const strip = $("previewStrip");
  clearPreviewStrip();
  $("previewRail").hidden = previewItems.length < 2;
  if (previewItems.length > 1) {
    const thumbnails = document.createDocumentFragment();
    previewItems.forEach((candidate, position) => {
      const control = button(candidate.name, () => selectPreview(position), null, true);
      control.tabIndex = -1;
      const thumbnail = mediaThumbnail(candidate, "strip-image");
      if (candidate.kind === "video") thumbnail.classList.add("strip-video");
      control.append(thumbnail);
      thumbnails.append(control);
    });
    strip.append(thumbnails);
  }
  previewIndex = -1;
  $("zoomInformation").open = false;
  $("zoom").showModal();
  selectPreview(index);
}

function clearPreviewStrip() {
  const strip = $("previewStrip");
  for (const img of strip.querySelectorAll("img")) observer.unobserve(img);
  for (const tile of strip.querySelectorAll(".strip-video")) savedVideoObserver.unobserve(tile);
  for (const video of strip.querySelectorAll("video[src]")) {
    video.removeAttribute("src");
    video.load();
  }
  strip.replaceChildren();
}

function scrollPreviewThumbnail() {
  const strip = $("previewStrip");
  const active = strip.children[previewIndex];
  if (!active) return;
  const bounds = strip.getBoundingClientRect();
  const tile = active.getBoundingClientRect();
  if (tile.left < bounds.left + 4) strip.scrollLeft -= bounds.left + 4 - tile.left;
  else if (tile.right > bounds.right - 4) strip.scrollLeft += tile.right - bounds.right + 4;
}

function selectPreview(index) {
  const item = previewItems[index];
  if (!item || ($("zoom").open && index === previewIndex)) return;
  const strip = $("previewStrip");
  const focused = document.activeElement;
  const previous = $("previousPreview");
  const next = $("nextPreview");
  const outgoing = strip.children[previewIndex];
  if (outgoing) {
    outgoing.removeAttribute("aria-current");
    outgoing.tabIndex = -1;
  }
  previewIndex = index;
  const active = strip.children[index];
  if (active) {
    active.setAttribute("aria-current", "true");
    active.tabIndex = 0;
  }
  previous.disabled = index === 0;
  next.disabled = index === previewItems.length - 1;
  const check = $("previewCheck");
  $("previewMark").hidden = item.preview_only || item.local;
  check.checked = app.selected.has(item.url);
  if (strip.contains(focused) || $("zoomActions").contains(focused) || (focused === previous && previous.disabled)
      || (focused === next && next.disabled) || (focused === check && item.preview_only)) {
    active?.focus({ preventScroll: true });
  }
  scrollPreviewThumbnail();
  if (!item.local) {
    app.focus = item;
    updateSelection();
  }
  clearPreviewMedia();
  renderPreview(item);
  $("previewToolbar").hidden = $("previewMark").hidden && !$("zoomActions").childElementCount;
}

function clearPreviewMedia() {
  const media = $("zoomMedia");
  const player = media.querySelector("video, audio");
  if (player) {
    player.pause();
    player.removeAttribute("src");
    player.load();
  }
  media.replaceChildren();
  $("zoomActions").replaceChildren();
  $("zoomDetails").replaceChildren();
}

function durationText(value) {
  if (!Number.isFinite(value)) return t("unknownValue");
  const total = Math.max(0, Math.floor(value));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor(total / 60) % 60;
  const seconds = String(total % 60).padStart(2, "0");
  return hours ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}` : `${minutes}:${seconds}`;
}
function mediaExtension(item) {
  return item.extension ?? [new URL(item.url).pathname, item.name]
    .map(value => value.match(/\.(mp4|m4v|webm|jpe?g|png|gif|svg|ico|webp|avif|mp3|m4a|wav|ogg|oga)$/i)?.[1])
    .find(Boolean);
}

function describeMedia(item, id) {
  const metadata = element("dl", "metadata");
  const detail = (key, value) => {
    const row = element("div");
    const result = element("dd", "", value);
    row.append(element("dt", "", t(key)), result);
    metadata.append(row);
    return result;
  };
  detail("kind", t(item.kind));
  const format = detail("format", id && !item.preview_only ? "MP4" : mediaExtension(item)?.toUpperCase() ?? t("unknownValue"));
  const size = detail("size", sizeText(item.size));
  const duration = item.kind === "video" || item.kind === "audio" ? detail("duration", durationText(item.duration)) : null;
  const dimensions = detail("dimensions", item.width && item.height ? item.width + " × " + item.height : t("unknownValue"));
  detail("source", item.local ? t("localFile") : new URL(item.url).host);
  return { metadata, format, size, duration, dimensions };
}

function updateMediaDetails(item, player, { duration, dimensions }) {
  if (duration && Number.isFinite(player.duration)) {
    item.duration ??= player.duration;
    duration.textContent = durationText(player.duration);
  }
  if (player.videoWidth && player.videoHeight) {
    item.width = player.videoWidth;
    item.height = player.videoHeight;
    item.measuredDimensions = true;
    dimensions.textContent = item.width + " × " + item.height;
  }
  if (!item.local && item.kind === "video") refreshSearchCardCaption(item);
}

function renderPreview(item) {
  const id = item.local ? null : youtubeId(item);
  const directMedia = (item.kind === "video" || (item.local && item.kind === "audio")) && !item.preview_only && !id;
  const frame = element("div", "preview-frame");
  let player;
  let playing = false;
  const status = element("p", "", t(item.preview ? "previewLoading" : "previewUnavailable"));
  status.setAttribute("role", "status");
  frame.append(icon(item.kind), status);
  $("zoomTitle").textContent = searchMediaName(item);
  const details = describeMedia(item, id);
  const { metadata, size, dimensions } = details;
  $("zoomMedia").append(frame);
  const actions = $("zoomActions");
  if (item.local) actions.append(button(t("openFolder"), async () => {
    try { await invoke("open_folder", { id: item.id }); }
    catch (error) { notify(errorKey(error)); }
  }, "folder"));
  if (item.local && item.can_check_qualities) {
    const controls = element("div", "preview-quality");
    const status = element("p", "preview-note");
    status.setAttribute("role", "status");
    const check = button(t("checkVideoQualities"), async () => {
      check.disabled = true;
      status.textContent = t("checkingVideoQualities");
      try {
        const qualities = await invoke("saved_video_qualities", { id: item.id });
        if (!controls.isConnected) return;
        if (qualities.length === 1) {
          status.textContent = t("noOtherVideoQualities");
          return;
        }
        const available = { name: item.name, qualities };
        const picker = qualityPicker(available);
        const download = button(t("downloadCopy"), async () => {
          download.disabled = true;
          try {
            const quality = qualities[available.qualityIndex ?? 0];
            await invoke("enqueue_saved_quality", { id: item.id, formatId: quality.format_id });
            status.textContent = t("queued");
          } catch (error) { status.textContent = t(errorKey(error)); }
          finally { download.disabled = false; }
        }, "download");
        controls.replaceChildren(picker, download, status);
        status.textContent = "";
        picker.querySelector("select").focus();
      } catch (error) { status.textContent = t(errorKey(error)); }
      finally { check.disabled = false; }
    });
    controls.append(check, status);
    actions.append(controls);
  }
  const openSavedFile = item.local ? button(t("openSavedFile"), async () => {
    try { await invoke("open_saved_file", { id: item.id }); }
    catch (error) { notify(errorKey(error)); }
  }) : null;
  if (openSavedFile) {
    openSavedFile.hidden = true;
    actions.append(openSavedFile);
  }
  if (id || directMedia) {
    const start = () => {
      if (playing && directMedia) {
        void player.play().then(() => { play.hidden = true; }, () => {});
        return;
      }
      if (playing) return;
      playing = true;
      play.hidden = true;
      if (openSavedFile) openSavedFile.hidden = true;
      if (id) {
        player = element("iframe", "preview-player");
        player.title = t("youtubePlayer");
        player.referrerPolicy = "strict-origin-when-cross-origin";
        player.sandbox = "allow-scripts allow-same-origin allow-presentation";
        player.allow = "autoplay; encrypted-media; fullscreen";
        player.src = "https://www.youtube.com/embed/" + id + "?autoplay=1";
      } else {
        player = element(item.kind === "audio" ? "audio" : "video", "preview-player");
        player.controls = true;
        player.preload = "metadata";
        player.disablePictureInPicture = true;
        player.setAttribute("controlslist", "nodownload");
        player.setAttribute("aria-label", item.name);
        player.addEventListener("error", () => {
          if (!frame.isConnected) return;
          playing = false;
          status.textContent = t(item.local ? "localPlaybackError" : "playbackError");
          frame.replaceChildren(icon(item.kind), status);
          play.hidden = false;
          if (openSavedFile) openSavedFile.hidden = false;
          if (document.activeElement === document.body) play.focus();
        });
        player.addEventListener("loadedmetadata", () => updateMediaDetails(item, player, details));
      }
      frame.replaceChildren(player);
      if (directMedia) {
        const current = player;
        const configure = !item.local ? invoke("set_preview_source", {
          url: item.url, sourceOrigin: item.source_origin
        }).catch(() => {}) : Promise.resolve();
        void configure.then(() => {
          if (!current.isConnected) return;
          current.src = item.url;
          return current.play().catch(error => {
            if (error.name === "NotAllowedError" && current.isConnected) play.hidden = false;
          });
        });
      }
    };
    const play = button(t(item.kind === "audio" ? "playAudio" : "playVideo"), start, "play");
    play.classList.add("primary");
    actions.append(play);
    if (id) actions.append(button(t("openYoutube"), async () => {
      try { await invoke("open_youtube", { id }); }
      catch (error) { notify(errorKey(error)); }
    }));
    if (id && item.preview_only) actions.append(element("p", "preview-note", t("youtubeUnavailable")));
    if (item.kind === "video") start();
  } else if (item.preview_only) {
    actions.append(element("p", "preview-note", t("embeddedVideo")));
  }
  $("zoomDetails").append(metadata);

  if (item.preview) {
    const img = element("img");
    img.alt = searchMediaName(item);
    img.addEventListener("load", () => {
      if (!frame.isConnected || playing) return;
      frame.replaceChildren(img);
      if (item.kind === "image" || item.kind === "gif") {
        item.width = img.naturalWidth;
        item.height = img.naturalHeight;
        item.measuredDimensions = true;
        dimensions.textContent = item.width + " × " + item.height;
      }
    });
    img.addEventListener("error", () => {
      if (!frame.isConnected || playing) return;
      status.textContent = t("previewUnavailable");
      if (openSavedFile) openSavedFile.hidden = false;
    });
    const source = item.local ? Promise.resolve(item.preview) : previewData(item.preview, true);
    source.then(data => {
      if (!frame.isConnected || playing) return;
      if (data) img.src = data;
      else status.textContent = t("previewUnavailable");
    });
  }
  if (item.size == null && !item.preview_only && !id && !item.local) {
    inspectSize(item).then(() => {
      if (size.isConnected) size.textContent = sizeText(item.size);
    });
  }
}

$("closeZoom").addEventListener("click", () => $("zoom").close());
$("previousPreview").addEventListener("click", () => selectPreview(previewIndex - 1));
$("nextPreview").addEventListener("click", () => selectPreview(previewIndex + 1));
$("previewCheck").addEventListener("change", event => {
  const item = previewItems[previewIndex];
  if (!item || item.preview_only || item.local) return;
  if (event.target.checked) app.selected.add(item.url);
  else app.selected.delete(item.url);
  updateSelection();
});
$("zoom").addEventListener("close", () => {
  if ($("zoom").open) return;
  clearPreviewMedia();
  clearPreviewStrip();
  previewItems = [];
  previewIndex = -1;
});
$("zoom").addEventListener("click", event => {
  const dialog = $("zoom");
  if (event.target !== dialog) return;
  const { left, right, top, bottom } = dialog.getBoundingClientRect();
  if (event.clientX < left || event.clientX > right || event.clientY < top || event.clientY > bottom) dialog.close();
});
document.addEventListener("keydown", event => {
  if (event.defaultPrevented || event.isComposing
      || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const dialog = $("zoom");
  if (dialog.open) {
    if (event.target.closest("video, audio, iframe, select")) return;
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      selectPreview(previewIndex + (event.key === "ArrowLeft" ? -1 : 1));
      return;
    }
    if (event.key !== " " || event.target.closest("input, summary, .preview-rail button, .preview-actions button")) return;
    event.preventDefault();
    if (!event.repeat) dialog.close();
    return;
  }
  if (event.key === "Escape" && app.tab === "find" && !$("findDetails").hidden
      && !document.querySelector("dialog[open], :popover-open") && !event.target.closest("select")) {
    event.preventDefault();
    $("findDetails").querySelector(".details-close")?.click();
    return;
  }
  if (event.key !== " " || app.tab === "queue" || document.querySelector("dialog[open]")) return;
  const item = app.tab === "saved" ? saved.focus : app.focus;
  if (!item) return;
  const target = event.target;
  const cardButton = target.closest(".card-button");
  if (!cardButton && (target.isContentEditable || target.closest("input, textarea, select, button, a, summary"))) return;
  event.preventDefault();
  if (!event.repeat) openPreview(item);
});


$("searchForm").addEventListener("submit", async event => {
  event.preventDefault();
  if (app.phase === "loading") return;
  const url = $("url").value.trim();
  app.searchUrl = url;
  app.collectionScan = $("scanCollection").checked;
  app.collectionPartial = false;
  metadataScan?.abort();
  app.sortLoading = false;
  app.items = [];
  app.focus = null;
  app.detailsUrl = null;
  app.selected.clear();
  $("filters").hidePopover();
  $("findSortMenu").hidePopover();
  app.error = null;
  app.phase = "loading";
  $("searchButton").disabled = true;
  renderGallery();
  try {
    if (app.collectionScan) {
      const result = await invoke("find_collection", { url });
      app.items = result.items;
      app.collectionPartial = result.partial;
    } else {
      app.items = await invoke("find_media", { url });
    }
    app.phase = "results";
    app.filter = galleryFilters.find(([kind]) => app.items.some(item => matchesFilter(item, kind)))?.[0] ?? "video";
    app.focus = visibleItems()[0] ?? null;
  } catch (error) {
    app.phase = "error";
    app.error = errorKey(error);
  } finally {
    $("searchButton").disabled = false;
    void refreshSearchSort();
  }
});

$("openBlockedPage").addEventListener("click", async () => {
  try { await invoke("open_page", { url: app.searchUrl }); }
  catch (error) { notify(errorKey(error)); }
});

function renderFolder() {
  $("folderPath").textContent = app.folder || t("unknownValue");
  $("folderPath").title = app.folder;
  $("directFolder").value = app.folder || "";
  $("directFolder").title = app.folder || "";
}
$("pickFolder").addEventListener("click", async () => {
  $("pickFolder").disabled = true;
  try {
    app.folder = await invoke("pick_folder") || app.folder;
    renderFolder();
  } catch (error) { notify(errorKey(error)); }
  finally { $("pickFolder").disabled = false; }
});
const downloadOptions = $("downloadOptions");
let pendingDownloads = [];

const directDialog = $("directDialog");
const directUrl = $("directUrl");
let directDraft = {};

function renderDirectForm() {
  const { info, error, phase } = directDraft;
  $("directInfo").hidden = !info;
  $("directName").textContent = info?.name ?? "";
  $("directSize").textContent = info ? sizeText(info.size) : "";
  $("directStatus").hidden = phase !== "checking";
  $("directStatus").textContent = phase === "checking" ? t("checkingLink") : "";
  $("directError").textContent = error ? t(error) : "";
  $("directError").hidden = !error;
  directUrl.setAttribute("aria-invalid", String(Boolean(error && !info)));
  directUrl.readOnly = phase === "saving" || phase === "folder";
  $("directSubmit").textContent = t(info ? "downloadNow" : "checkLink");
  $("directSubmit").disabled = Boolean(phase);
  $("directPickFolder").disabled = Boolean(phase);
  $("directCancel").disabled = phase === "saving" || phase === "folder";
}

$("addDirectDownload").addEventListener("click", () => {
  directDraft = {};
  $("directForm").reset();
  renderFolder();
  renderDirectForm();
  directDialog.showModal();
});
directUrl.addEventListener("input", () => {
  directDraft = {};
  renderDirectForm();
});
$("directCancel").addEventListener("click", () => directDialog.close());
directDialog.addEventListener("cancel", event => {
  if (directDraft.phase === "saving" || directDraft.phase === "folder") event.preventDefault();
});
directDialog.addEventListener("close", () => {
  if (directDialog.open) return;
  directDraft = {};
  $("addDirectDownload").focus();
});
$("directPickFolder").addEventListener("click", async () => {
  const draft = directDraft;
  draft.phase = "folder";
  draft.error = null;
  renderDirectForm();
  try {
    app.folder = await invoke("pick_folder") || app.folder;
    renderFolder();
  } catch (error) {
    draft.error = errorKey(error);
  } finally {
    draft.phase = null;
    if (directDraft === draft) renderDirectForm();
  }
});
$("directForm").addEventListener("submit", async event => {
  event.preventDefault();
  const draft = directDraft;
  if (draft.phase) return;
  const url = directUrl.value.trim();
  draft.phase = draft.info ? "saving" : "checking";
  draft.error = null;
  renderDirectForm();
  try {
    if (draft.info) {
      await invoke("enqueue", { items: [{ url, ...draft.info, direct: true }] });
      queueKind = "all";
      queueCategory = "all";
      directDialog.close();
      renderQueue();
      void refreshQueue();
    } else {
      draft.info = await invoke("inspect_direct", { url });
    }
  } catch (error) {
    draft.error = errorKey(error);
  } finally {
    draft.phase = null;
    if (directDraft === draft && directDialog.open) renderDirectForm();
  }
});

$("enqueue").addEventListener("click", () => {
  const single = singleDownload();
  const items = (single ? [single] : app.items.filter(item => app.selected.has(item.url)))
    .map(item => ({ ...item }));
  const variants = items.filter(item => item.qualities?.length > 1);
  if (variants.length === 0) {
    void downloadSelected(items);
    return;
  }
  pendingDownloads = items;
  $("downloadOptionsList").replaceChildren(...variants.map(item => {
    const picker = qualityPicker(item, true);
    picker.firstElementChild.textContent = item.name;
    return picker;
  }));
  downloadOptions.showModal();
  $("downloadOptionsList").querySelector("select").focus();
});

$("cancelDownloadOptions").addEventListener("click", () => downloadOptions.close());
downloadOptions.addEventListener("close", () => {
  pendingDownloads = [];
  $("downloadOptionsList").replaceChildren();
});
$("downloadOptionsForm").addEventListener("submit", event => {
  event.preventDefault();
  const items = pendingDownloads;
  downloadOptions.close();
  void downloadSelected(items);
});

async function downloadSelected(selection) {
  if (app.submitting || !selection.length) return;
  app.submitting = true;
  updateSelection();
  try {
    const items = selection.map(item => {
      const quality = item.qualities?.[item.qualityIndex ?? 0];
      return {
        url: quality?.url ?? item.url,
        name: item.name,
        size: quality ? quality.size : item.size,
        preview_only: item.preview_only,
        format_id: quality?.format_id,
        source_origin: item.source_origin
      };
    });
    await invoke("enqueue", { items });
    app.selected.clear();
    showTab("queue", true);
  } catch (error) { notify(errorKey(error)); }
  finally { app.submitting = false; updateSelection(); }
}

const jobNodes = new Map();
for (const [id, chrome] of [["connectFirefox", false], ["connectChrome", true]]) $(id).addEventListener("click", async event => {
  const control = event.currentTarget;
  $("queueConnectionsMenu").hidePopover();
  $("queueConnections").focus();
  control.disabled = true;
  try {
    await invoke("connect_firefox", { chrome });
    notify(chrome ? "chromeConnected" : "firefoxConnected");
  } catch (error) {
    notify(chrome && errorKey(error) === "firefoxConnectError" ? "chromeConnectError" : errorKey(error));
  } finally {
    control.disabled = false;
  }
});
let queueCategory = "all";
let queueKind = "all";
let selectedJob;
let queueSort;
const speeds = new Map();
const queueMediaOptions = Array.from($("queueMediaMenu").querySelectorAll("[data-kind]"));
const queueActions = [
  ["resume", "resume", "play", ["deferred", "paused", "failed"]],
  ["pause", "pause", "pause", ["running", "queued"]],
  ["open", "openFolder", "folder", ["completed"]],
  ["remove", "remove", "removeList", ["queued", "deferred", "paused", "failed", "completed"]]
];
const busyQueueJobs = new Set();
let queueFiles = new Map();
let queueFilesKey;
let queueDateFormat;
const queueProbes = new Set();
const queueProbeSignal = new AbortController().signal;
let queueProbing = false;
const queueFileObserver = new IntersectionObserver(entries => {
  for (const entry of entries) {
    if (entry.isIntersecting) queueProbes.add(entry.target);
    else queueProbes.delete(entry.target);
  }
  void probeQueueVideos();
}, { root: $("queuePanel") });

async function probeQueueVideos() {
  if (queueProbing) return;
  queueProbing = true;
  try {
    for (const row of queueProbes) {
      queueProbes.delete(row);
      const file = queueFiles.get(row.dataset.id);
      if (!row.isConnected || app.tab !== "queue" || !file || file.probed) continue;
      file.probed = true;
      queueFileObserver.unobserve(row);
      await probeMedia(file, queueProbeSignal);
      renderQueue();
    }
  } finally { queueProbing = false; }
}

async function readQueueFiles() {
  if (app.tab !== "queue") return;
  const completed = app.queue.filter(job => !job.browser && job.status === "completed");
  const key = JSON.stringify(completed.map(job => [job.id, job.file]));
  if (queueFilesKey === key) return;
  queueFilesKey = key;
  if (!completed.length) {
    queueFiles.clear();
    return;
  }
  try {
    const ids = new Set(completed.map(job => job.id));
    const files = await invoke("saved_gallery");
    if (queueFilesKey !== key) return;
    queueFiles = new Map(files.filter(file => ids.has(file.id)).map(file => {
      const cached = queueFiles.get(file.id);
      const unchanged = cached?.path === file.path && cached.modified_at === file.modified_at;
      return [file.id, unchanged ? cached : { ...file, url: window.__TAURI__.core.convertFileSrc(file.path) }];
    }));
    renderQueue();
  } catch {
    // The queue remains usable if a saved file can no longer be read.
  }
}

function queueMediaKind(job) {
  if (job.format_id) return "video";
  const name = job.file || job.name;
  if (/\.(mp4|webm|m4v|mkv|mov|avi)$/i.test(name)) return "video";
  if (/\.(jpg|jpeg|png|webp|avif|bmp|heic|heif|tiff?)$/i.test(name)) return "image";
  if (/\.gif$/i.test(name)) return "gif";
  if (/\.(mp3|m4a|wav|ogg|oga|flac|opus|aac)$/i.test(name)) return "audio";
  if (/\.(svg|ico)$/i.test(name)) return "asset";
  if (job.direct) return "file";
}

function visibleJobs() {
  const jobs = app.queue.filter(job => {
    if (queueCategory === "completed" && job.status !== "completed") return false;
    if (queueCategory === "unfinished" && job.status === "completed") return false;
    return queueKind === "all" || queueMediaKind(job) === queueKind;
  });
  if (!queueSort) return jobs;
  const { field, sign } = queueSort;
  const value = job => field === "name" ? job.file || job.name
    : field === "status" ? t(job.status) : field === "speed" ? speeds.get(job.id)?.rate ?? 0 : job.total ?? 0;
  return jobs.sort((a, b) => {
    const first = value(a);
    const second = value(b);
    return sign * (typeof first === "string" ? first.localeCompare(second, language) : first - second);
  });
}

async function queueAction(id, action, statuses) {
  const job = app.queue.find(item => item.id === id && !item.browser);
  if (!job || !statuses.includes(job.status) || busyQueueJobs.has(id) || document.querySelector("dialog[open]")) return;
  busyQueueJobs.add(id);
  try {
    if (action === "remove" && job.status !== "completed") {
      const dialog = $("removeDialog");
      dialog.returnValue = "";
      const close = new Promise(resolve => dialog.addEventListener("close", resolve, { once: true }));
      dialog.showModal();
      await close;
      if (dialog.returnValue !== "remove") return;
    }
    if (action === "open") await invoke("open_folder", { id });
    else await invoke("change_job", { id, action });
    await refreshQueue();
  } catch (error) { notify(errorKey(error)); }
  finally { busyQueueJobs.delete(id); }
}

for (const control of $("queueCategories").querySelectorAll("button")) {
  control.onclick = () => {
    queueCategory = control.dataset.category;
    renderQueue();
  };
}
for (const control of queueMediaOptions) {
  control.onclick = () => {
    queueKind = control.dataset.kind;
    $("queueMediaMenu").hidePopover();
    renderQueue();
    $("queueMedia").focus();
  };
}
for (const control of $("queueTable").querySelectorAll("[data-sort]")) {
  control.onclick = () => {
    const field = control.dataset.sort;
    queueSort = { field, sign: queueSort?.field === field ? -queueSort.sign : 1 };
    renderQueue();
  };
}

function renderQueue() {
  const active = app.queue.filter(job => !["completed", "failed"].includes(job.status)).length;
  $("queueCount").hidden = active === 0;
  $("queueCount").textContent = localizedNumber(active);
  $("queueSummary").textContent = app.queue.length ? t("queueSummary", {
    done: localizedNumber(app.queue.filter(job => job.status === "completed").length),
    total: localizedNumber(app.queue.length)
  }) : "";
  $("queueError").hidden = !app.queueError;
  $("queueError").textContent = app.queueError ? t(app.queueError) : "";
  const jobs = visibleJobs();
  const container = $("jobs");
  const ids = new Set(jobs.map(job => job.id));
  const selected = jobs.find(job => job.id === selectedJob);
  if (!selected) selectedJob = undefined;
  const location = selected?.browser ? t("firefoxDownload") : selected?.folder;
  $("queueLocation").hidden = !location;
  $("queuePath").textContent = location || "";
  const showSpeed = jobs.some(job => job.status === "running");
  if (!showSpeed && $("queueSpeed").contains(document.activeElement)) $("queueTable").querySelector('[data-sort="status"]').focus();
  $("queueSpeed").hidden = !showSpeed;
  for (const control of $("queueCategories").querySelectorAll("button")) {
    control.setAttribute("aria-pressed", String(control.dataset.category === queueCategory));
  }
  for (const control of queueMediaOptions) {
    const current = control.dataset.kind === queueKind;
    control.setAttribute("aria-checked", String(current));
    if (current) {
      const kind = t(control.querySelector("[data-t]").dataset.t);
      const label = [t("queueMedia"), kind].join(" · ");
      $("queueMediaLabel").textContent = queueKind === "all" ? t("queueMedia") : kind;
      $("queueMedia").title = label;
      $("queueMedia").setAttribute("aria-label", label);
    }
  }
  $("queueMedia").toggleAttribute("data-active", queueKind !== "all");
  for (const control of $("queueTable").querySelectorAll("[data-sort]")) {
    const sorted = queueSort?.field === control.dataset.sort;
    if (sorted) {
      control.parentElement.setAttribute("aria-sort", queueSort.sign === 1 ? "ascending" : "descending");
    } else control.parentElement.removeAttribute("aria-sort");
    const glyph = sorted ? (queueSort.sign === 1 ? "sortAscending" : "sortDescending") : "sort";
    control.querySelector("path").setAttribute("d", icons[glyph]);
  }
  for (const [id, view] of jobNodes) {
    if (ids.has(id)) continue;
    if (view.row.contains(document.activeElement)) $("queueMedia").focus();
    queueFileObserver.unobserve(view.row);
    queueProbes.delete(view.row);
    view.row.remove();
    jobNodes.delete(id);
  }
  $("queueEmpty").hidden = jobs.length > 0;
  const emptyMessage = jobs.length ? "" : t(app.queue.length ? "queueNoMatches" : "queueEmpty");
  if ($("queueEmpty").textContent !== emptyMessage) $("queueEmpty").textContent = emptyMessage;
  for (const [index, job] of jobs.entries()) {
    if (!jobNodes.has(job.id)) {
      const row = element("tr", "job");
      row.dataset.id = job.id;
      const info = element("td", "job-info");
      const title = button(job.name, () => { selectedJob = job.id; renderQueue(); });
      title.className = "job-name";
      const meta = element("div", "job-meta");
      info.append(title, meta);
      const size = element("td", "job-size");
      const status = element("td", "job-state");
      const text = element("div", "job-status");
      const savedMark = element("span", "fluent-icon", "\uE73E");
      savedMark.setAttribute("aria-hidden", "true");
      const statusLabel = element("span");
      text.append(savedMark, statusLabel);
      const bar = element("progress", "job-progress");
      status.append(text, bar);
      const speed = element("td", "job-speed");
      const actionsCell = element("td", "job-actions-cell");
      const actions = element("div", "job-actions");
      const controls = queueActions.map(([action, key, glyph, statuses]) => {
        const control = button(t(key), () => void queueAction(job.id, action, statuses), glyph, true);
        const label = action === "remove" ? element("span", "job-action-label", t(key)) : null;
        if (label) control.append(label);
        control.dataset.action = action;
        actions.append(control);
        return { control, key, statuses, label };
      });
      actionsCell.append(actions);
      row.append(info, size, status, speed, actionsCell);
      row.addEventListener("click", event => {
        if (event.target.closest("button")) return;
        selectedJob = job.id;
        renderQueue();
      });
      jobNodes.set(job.id, { row, title, meta, size, text, savedMark, statusLabel, bar, speed, controls });
    }
    const view = jobNodes.get(job.id);
    if (container.children[index] !== view.row) {
      const focused = view.row.contains(document.activeElement) ? document.activeElement : null;
      container.insertBefore(view.row, container.children[index] ?? null);
      focused?.focus({ preventScroll: true });
    }
    view.row.classList.toggle("selected", selectedJob === job.id);
    view.title.setAttribute("aria-pressed", String(selectedJob === job.id));
    view.title.textContent = job.file || job.name;
    view.title.title = view.title.textContent;
    const file = queueFiles.get(job.id);
    const dimensions = file?.width && file.height ? file.width + " × " + file.height : "";
    const date = file?.created_at != null ? queueDateFormat.format(file.created_at) : "";
    const details = [dimensions, date].filter(Boolean).join(" · ");
    if (view.meta.textContent !== details) view.meta.textContent = details;
    view.meta.hidden = !details;
    view.meta.title = [dimensions && t("dimensions") + " · " + dimensions,
      date && t("savedCreated") + " · " + date].filter(Boolean).join(" / ");
    if (app.tab === "queue" && file?.kind === "video" && !file.probed) queueFileObserver.observe(view.row);
    for (const { control, key, statuses, label } of view.controls) {
      const available = !job.browser && statuses.includes(job.status);
      if (!available && document.activeElement === control) view.title.focus({ preventScroll: true });
      control.hidden = !available;
      control.title = t(key);
      control.setAttribute("aria-label", control.title);
      if (label) label.textContent = control.title;
    }
    view.text.className = "job-status " + job.status;
    view.savedMark.hidden = job.status !== "completed";
    view.statusLabel.textContent = t(job.status) + (job.error ? " · " + t(job.error) : "");
    view.size.textContent = job.total != null ? sizeText(job.total) : job.done ? sizeText(job.done) : "—";
    const rate = speeds.get(job.id)?.rate;
    view.speed.hidden = !showSpeed;
    view.speed.textContent = rate > 0 ? t("sizePerSecond", { size: sizeText(rate) }) : "";
    view.bar.hidden = !["running", "pausing"].includes(job.status)
      && !(job.total > 0 && ["paused", "firefoxOffline"].includes(job.status));
    view.bar.setAttribute("aria-label", t("progress", { name: job.name }));
    view.bar.max = Math.max(1, job.total ?? 1);
    if (job.total != null) view.bar.value = job.done;
    else view.bar.removeAttribute("value");

  }
  void readQueueFiles();
}

let browserVideo;
let deciding = false;
async function refreshBrowserVideo() {
  if (document.querySelector("dialog[open]")) return;
  const video = await invoke("next_browser_video").catch(() => null);
  if (!video || document.querySelector("dialog[open]")) return;
  browserVideo = video;
  $("browserSaveSource").textContent = [video.origin, video.format.toUpperCase(), video.quality && video.quality + "p"]
    .filter(Boolean).join(" · ");
  $("browserSaveName").value = video.name;
  $("browserSaveFolder").value = app.folder;
  $("browserSaveError").hidden = true;
  const quality = $("browserSaveQuality");
  const status = $("browserQualityStatus");
  quality.replaceChildren();
  quality.hidden = true;
  status.hidden = !video.can_check_qualities;
  status.textContent = video.can_check_qualities ? t("checkingVideoQualities") : "";
  const submitButtons = $("browserSaveForm").querySelectorAll('button[type="submit"]');
  for (const button of submitButtons) button.disabled = video.can_check_qualities;
  $("browserSave").showModal();
  $("browserSaveName").select();
  if (!video.can_check_qualities) return;
  try {
    const qualities = await invoke("browser_video_qualities", { id: video.id });
    if (browserVideo !== video) return;
    video.qualities = qualities;
    quality.append(qualityPicker(video));
    quality.hidden = false;
    status.hidden = true;
    if (!deciding) for (const button of submitButtons) button.disabled = false;
  } catch (error) {
    if (browserVideo === video) status.textContent = t(errorKey(error));
  }
}

async function decideBrowserVideo(action) {
  if (deciding || !browserVideo) return;
  if (action !== "cancel" && browserVideo.can_check_qualities && !browserVideo.qualities) return;
  deciding = true;
  const form = $("browserSaveForm");
  for (const control of form.querySelectorAll("button, select")) control.disabled = true;
  try {
    const formatId = browserVideo.qualities?.[browserVideo.qualityIndex ?? 0]?.format_id ?? null;
    await invoke("decide_browser_video", { id: browserVideo.id, action, name: $("browserSaveName").value, formatId });
    browserVideo = null;
    $("browserSave").close();
    if (action !== "cancel") {
      queueCategory = "all";
      queueKind = "all";
      showTab("queue", true);
    }
  } catch (error) {
    $("browserSaveError").textContent = t(errorKey(error));
    $("browserSaveError").hidden = false;
  } finally {
    deciding = false;
    for (const control of form.querySelectorAll("button, select")) {
      control.disabled = control.type === "submit" && browserVideo?.can_check_qualities && !browserVideo.qualities;
    }
  }
}

$("browserSaveForm").onsubmit = event => {
  event.preventDefault();
  void decideBrowserVideo(event.submitter?.value ?? "now");
};
$("browserCancel").onclick = () => void decideBrowserVideo("cancel");
$("browserSave").addEventListener("cancel", event => {
  event.preventDefault();
  void decideBrowserVideo("cancel");
});
$("browserPickFolder").onclick = async event => {
  const control = event.currentTarget;
  control.disabled = true;
  try {
    const folder = await invoke("pick_folder");
    if (folder) $("browserSaveFolder").value = folder;
  } catch (error) {
    $("browserSaveError").textContent = t(errorKey(error));
    $("browserSaveError").hidden = false;
  } finally { control.disabled = false; }
};
let refreshing = false;
let firefoxJobs = [];
let nextFirefoxCheck = 0;
async function refreshQueue() {
  if (refreshing) return;
  refreshing = true;
  try {
    const state = await invoke("snapshot");
    if (Date.now() >= nextFirefoxCheck) {
      nextFirefoxCheck = Date.now() + 2000;
      const reports = await invoke("firefox_downloads").catch(() => []);
      firefoxJobs = reports.flatMap(report => report.jobs.map(job => ({
        ...job, id: report.profile + ":" + job.id, browser: true
      })));
    }
    app.folder = state.folder;
    app.queue = [...state.jobs, ...firefoxJobs];
    const now = performance.now();
    const running = new Set();
    for (const job of app.queue) {
      if (job.status !== "running") continue;
      running.add(job.id);
      const previous = speeds.get(job.id);
      const rate = previous ? Math.max(0, (job.done - previous.done) * 1000 / (now - previous.time)) : 0;
      speeds.set(job.id, { done: job.done, time: now, rate });
    }
    for (const id of speeds.keys()) if (!running.has(id)) speeds.delete(id);
    app.queueError = state.error;
    renderFolder();
    renderQueue();
    updateSelection();
    await refreshBrowserVideo();
  } catch (error) {
    app.queueError = errorKey(error);
    renderQueue();
  } finally { refreshing = false; }
}

let updateChannel;
let updateVersion;
let updateStatus;
let updateWorking = false;

function renderUpdate() {
  const version = $("appVersion");
  version.hidden = !updateChannel?.version;
  if (updateChannel?.version) {
    version.textContent = `v${updateChannel.version}`;
    version.title = t("appVersion", { version: updateChannel.version });
  }
  const label = t(updateStatus ?? "updateChecking", { version: updateVersion ?? updateChannel?.version });
  $("updateStatus").textContent = label;
  $("updatesButton").toggleAttribute("data-ready", Boolean(updateVersion));
  const name = updateVersion ? t("updateAvailable", { version: updateVersion }) : t("updates");
  $("updatesButton").setAttribute("aria-label", name);
  $("updatesButton").title = name;
  $("installUpdate").hidden = !updateVersion;
  $("installUpdate").disabled = updateWorking;
  $("updateReleases").hidden = updateChannel?.kind !== "portable";
}

async function checkUpdates() {
  if (updateWorking) return;
  updateWorking = true;
  updateStatus = "updateChecking";
  renderUpdate();
  try {
    updateChannel ??= await invoke("update_channel");
    renderUpdate();
    if (updateChannel.kind === "pullora") updateStatus = "updatePullora";
    else if (updateChannel.kind === "portable") updateStatus = "updatePortable";
    else {
      updateVersion = await invoke("check_update");
      updateStatus = updateVersion ? "updateAvailable" : "updateCurrent";
    }
  } catch { updateStatus = "updateCheckFailed"; }
  finally {
    updateWorking = false;
    renderUpdate();
  }
}

$("updatesButton").addEventListener("click", () => {
  $("updatesDialog").showModal();
  if (!updateVersion) void checkUpdates();
});
$("updateReleases").addEventListener("click", () => {
  void invoke("open_page", { url: "https://github.com/CpPrice11/ClipSail/releases/latest" }).catch(error => notify(errorKey(error)));
});
$("installUpdate").addEventListener("click", async () => {
  if (updateWorking) return;
  updateWorking = true;
  updateStatus = "updateInstalling";
  renderUpdate();
  try { await invoke("install_update"); }
  catch (error) {
    updateStatus = errorKey(error);
    if (updateStatus === "updateCurrent") updateVersion = null;
  } finally {
    updateWorking = false;
    renderUpdate();
  }
});

function translate() {
  void invoke("set_tray_language", { open: t("trayOpen"), quit: t("trayQuit") }).catch(() => {});
  document.documentElement.lang = language;
  document.title = t("app");
  queueDateFormat = new Intl.DateTimeFormat(language, { dateStyle: "medium", timeStyle: "short" });
  for (const node of document.querySelectorAll("[data-t]")) node.textContent = t(node.dataset.t);
  for (const [id, key] of [["language", "language"], ["tabs", "app"], ["savedFilters", "filter"],
    ["findTab", "find"], ["queueTab", "downloads"], ["savedTab", "savedGallery"], ["savedSearch", "savedSearch"],
    ["savedSort", "savedSort"], ["savedView", "savedView"], ["refreshSaved", "refreshSaved"], ["savedTimeline", "savedTimeline"],
    ["queueCategories", "queueCategories"], ["queueConnections", "queueConnections"],
    ["closeZoom", "close"], ["previewRail", "previewStrip"], ["previousPreview", "previousPreview"], ["nextPreview", "nextPreview"]]) {
    $(id).setAttribute("aria-label", t(key));
  }
  for (const id of ["closeZoom", "previousPreview", "nextPreview"]) $(id).title = t(id === "closeZoom" ? "close" : id);
  for (const id of ["findTab", "queueTab", "savedTab", "savedFilters", "savedSort", "savedView", "refreshSaved", "queueConnections"]) $(id).title = $(id).getAttribute("aria-label");
  $("savedSearch").placeholder = t("savedSearch");
  if (lastNotice) $("notice").textContent = t(lastNotice);
  renderGallery();
  renderSaved();
  renderQueue();
  renderFolder();
  renderDirectForm();
  renderUpdate();
  renderNav();
  renderCaptionButtons();
}
$("language").value = language;
$("language").addEventListener("change", event => {
  language = event.target.value;
  localStorage.setItem("language", language);
  translate();
});
translate();
void refreshQueue();
setInterval(refreshQueue, 650);
void checkUpdates();
setInterval(() => { if (!updateVersion) void checkUpdates(); }, 6 * 60 * 60 * 1000);

