import uk from "./uk.js";
import en from "./en.js";

const $ = id => document.getElementById(id);
const locales = { uk, en };
let language = localStorage.getItem("language") === "en" ? "en" : "uk";
const t = (key, values = {}) => (locales[language][key] ?? en[key] ?? locales[language].unknown)
  .replace(/\{(\w+)\}/g, (_, name) => values[name] ?? "");
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);

const icons = {
  download: "M12 3v12m-5-5 5 5 5-5M5 16v4h14v-4",
  link: "M10 13a4 4 0 0 0 6 0l3-3a4 4 0 0 0-6-6l-2 2m2 5a4 4 0 0 0-6 0l-3 3a4 4 0 0 0 6 6l2-2",
  search: "M16 16l5 5M18 10a8 8 0 1 1-16 0 8 8 0 0 1 16 0Z",
  filter: "M3 5h18l-7 8v5l-4 2v-7z",
  sort: "M7 20V4m-4 4 4-4 4 4m6-4v16m-4-4 4 4 4-4",
  folder: "M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z",
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

const app = { items: [], selected: new Set(), focus: null, detailsUrl: null, filter: "all", filterLoading: false, phase: "initial",
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

const tabs = ["find", "queue", "saved"];

function showTab(tab, focus = false) {
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
    else entry.target.removeAttribute("src");
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
  previewData(url, priority).then(data => {
    if (img.isConnected && img.dataset.visible === "true" && data) img.src = data;
  });
}

const galleryFilters = [
  ["all", "mainMedia"], ["image", "images"], ["video", "videos"],
  ["gif", "gif"], ["audio", "audioList"], ["assets", "pageAssets"]
];

const mediaFilterFields = [
  { id: "filterSize", key: "size", unit: "filterMb", values: [1, 10, 50, 100], value: item => item.size / (1024 ** 2), known: item => item.size != null },
  { id: "filterDuration", key: "duration", unit: "filterSeconds", values: [10, 30, 60, 300], value: item => item.duration, known: item => Number.isFinite(item.duration) },
  { id: "filterResolution", key: "resolution", unit: "filterPixels", values: [480, 720, 1080, 2160], value: item => Math.min(item.width, item.height), known: item => (item.measuredDimensions || youtubeId(item)) && item.width > 0 && item.height > 0 }
];
let metadataScan;

function matchesFilter(item, filter) {
  if (filter === "assets") return Boolean(item.page_asset);
  return !item.page_asset && (filter === "all" || item.kind === filter);
}

function visibleItems() {
  const active = activeMediaFilters();
  return app.items.filter(item => matchesFilter(item, app.filter) && active.every(({ field, minimum }) =>
    field.known(item) && (minimum === "under" ? field.value(item) < field.values[0] : field.value(item) >= Number(minimum))
  ));
}

function activeMediaFilters() {
  return mediaFilterFields.flatMap(field => {
    const minimum = $(field.id).value;
    return minimum ? [{ field, minimum }] : [];
  });
}

function translateMediaFilters() {
  for (const field of mediaFilterFields) {
    const select = $(field.id);
    const selected = select.value;
    const options = [
      ["", t("filterAny")],
      ["under", t("filterUnder", { value: t(field.unit, { n: localizedNumber(field.values[0]) }) })],
      ...field.values.map(n => [String(n), t("filterAtLeast", { value: t(field.unit, { n: localizedNumber(n) }) })])
    ];
    select.replaceChildren(...options.map(([value, text]) => new Option(text, value)));
    select.value = selected;
  }
}

function inspectSize(item) {
  item.inspect ??= invoke("inspect_media", { url: item.url }).catch(() => null);
  return item.inspect.then(size => { item.size ??= size; });
}

function probeMedia(item, signal) {
  return new Promise(resolve => {
    const player = element(item.kind === "audio" ? "audio" : "video");
    const finish = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", finish);
      player.onloadedmetadata = null;
      player.onerror = null;
      player.removeAttribute("src");
      player.load();
      resolve();
    };
    const timer = setTimeout(finish, 8000);
    signal.addEventListener("abort", finish, { once: true });
    player.preload = "metadata";
    player.onloadedmetadata = () => {
      if (Number.isFinite(player.duration)) item.duration = player.duration;
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

async function refreshMediaFilters() {
  metadataScan?.abort();
  const scan = new AbortController();
  metadataScan = scan;
  const active = activeMediaFilters();
  const candidates = app.items.filter(item => matchesFilter(item, app.filter));
  const pending = candidates.filter(item => active.some(({ field }) => !field.known(item) && canProbe(item, field)));
  app.filterLoading = pending.length > 0;
  renderGallery();
  let index = 0;
  await Promise.all(Array.from({ length: Math.min(4, pending.length) }, async () => {
    while (!scan.signal.aborted && index < pending.length) {
      const item = pending[index++];
      const missing = active.filter(({ field }) => !field.known(item) && canProbe(item, field));
      if (missing.some(({ field }) => field.key === "size")) await inspectSize(item);
      if (scan.signal.aborted) return;
      if (missing.some(({ field }) => field.key !== "size")) {
        if (item.kind === "video" || item.kind === "audio") await probeMedia(item, scan.signal);
        else await probeImage(item);
      }
    }
  }));
  if (scan.signal.aborted || !app.filterLoading) return;
  app.filterLoading = false;
  renderGallery();
}

for (const { id } of mediaFilterFields) $(id).addEventListener("change", refreshMediaFilters);
$("resetMediaFilters").addEventListener("click", () => {
  for (const { id } of mediaFilterFields) $(id).value = "";
  void refreshMediaFilters();
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
    const img = element("img");
    img.alt = "";
    img.addEventListener("error", () => {
      observer.unobserve(img);
      img.remove();
    });
    thumb.append(img);
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

function qualityPicker(item, onChange) {
  const control = element("select");
  control.setAttribute("aria-label", t("qualityFor", { name: item.name }));
  for (const [index, quality] of item.qualities.entries()) {
    const resolution = quality.width && quality.height
      ? Math.min(quality.width, quality.height) : quality.height;
    const description = resolution
      ? t("qualityResolution", { n: resolution, format: quality.extension.toUpperCase() })
      : t("qualityUnspecified", { format: quality.extension.toUpperCase() });
    const option = element("option", "", index === 0 ? t("qualityBest", { description }) : description);
    option.value = String(index);
    control.append(option);
  }
  control.value = String(item.qualityIndex ?? 0);
  control.addEventListener("change", () => {
    item.qualityIndex = Number(control.value);
    onChange?.(control.value);
  });
  const label = element("label", "card-quality");
  label.append(element("span", "", t("downloadQuality")), control);
  return label;
}

function renderGallery() {
  for (const img of $("gallery").querySelectorAll("img")) observer.unobserve(img);
  const items = visibleItems();
  const categoryFocus = $("filters").contains(document.activeElement) ? document.activeElement.dataset.kind : null;
  const hasResults = app.phase === "results";
  const filterCount = activeMediaFilters().length;
  const hasFilters = filterCount > 0;
  if (!items.includes(app.focus)) app.focus = items[0] ?? null;
  $("resultCount").hidden = app.items.length === 0;
  $("resultCount").textContent = localizedNumber(items.length);
  $("resultsStatus").hidden = items.length > 0;
  $("gallery").hidden = items.length === 0;
  $("filters").hidden = !hasResults;
  $("mediaFilters").hidden = !hasResults || app.items.length === 0;
  $("mediaFilters").querySelector("summary").textContent = t(hasFilters ? "activeFilters" : "moreFilters", { n: localizedNumber(filterCount) });
  $("filterStatus").hidden = !app.filterLoading && (!hasFilters || items.length === 0);
  $("filterStatus").textContent = t(app.filterLoading ? "readingMetadata" : "filterKnownOnly");
  $("resetMediaFilters").disabled = !hasFilters;
  $("allControl").hidden = items.length === 0;
  $("searchError").hidden = !app.error;
  $("url").setAttribute("aria-invalid", String(app.error === "invalidUrl"));
  $("gallery").setAttribute("aria-busy", String(app.phase === "loading" || app.filterLoading));
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
  } else if (app.filterLoading) {
    $("resultsStatus").replaceChildren(icon("search"), element("p", "", t("readingMetadata")));
    $("filterStatus").hidden = true;
  } else if (hasFilters && app.items.some(item => matchesFilter(item, app.filter))) {
    empty($("resultsStatus"), "search", "noFilterMatches", "filterKnownOnly");
  } else if (app.filter === "all" && app.items.length > 0) {
    empty($("resultsStatus"), "image", "assetsOnlyTitle", "assetsOnlyHint");
  } else if (app.filter !== "all") {
    empty($("resultsStatus"), "search", "emptyCategory", "emptyCategoryHint");
  } else {
    empty($("resultsStatus"), "image", "emptyTitle", "emptyHint");
  }

  $("filters").replaceChildren();
  for (const [kind, key] of galleryFilters) {
    if (["audio", "assets"].includes(kind) && !app.items.some(item => matchesFilter(item, kind))) continue;
    const control = button(t(key), () => {
      app.filter = kind;
      void refreshMediaFilters();
    });
    control.dataset.kind = kind;
    control.setAttribute("aria-pressed", String(app.filter === kind));
    $("filters").append(control);
  }
  if (categoryFocus) $("filters").querySelector(`[data-kind="${categoryFocus}"]`)?.focus();
  $("gallery").replaceChildren(...items.map(mediaCard));
  updateSelection();
  renderFileDetails(app, items, "findDetails");
}

function mediaCard(item) {
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
    open.setAttribute("aria-label", t("fileDetailsFor", { name: item.name }));
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
    const title = element("span", "card-name", item.name);
    title.title = item.name;
    open.replaceChildren(thumb, title, element("span", "card-type", t(item.preview_only ? "previewOnly" : item.kind)));
    card.append(open);
    if (item.qualities?.length > 1) card.prepend(qualityPicker(item));
    if (item.preview_only) return card;
    const checkLabel = element("label", "card-check");
    const check = element("input");
    check.type = "checkbox";
    check.setAttribute("aria-label", t("selectItem", { name: item.name }));
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

const saved = { items: [], filter: "all", focus: null, detailsUrl: null, loading: false, error: false, rows: [], frame: 0 };
const detailFrames = new WeakMap();

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
  panel.replaceChildren();
  if (!item) return;

  const heading = element("h2", "file-details-title", item.name);
  heading.id = panelId + "Title";
  panel.setAttribute("aria-labelledby", heading.id);
  const thumb = mediaThumbnail(item, "thumb", { still: true });
  const details = describeMedia(item, item.local ? null : youtubeId(item));
  if (item.local) {
    const header = element("div", "details-header");
    const close = button(t("closeDetails"), () => {
      saved.detailsUrl = null;
      renderFileDetails(saved, savedItems(), panelId);
      Array.from(cards).find(card => card.dataset.url === item.url)?.querySelector("button").focus();
    }, "close", true);
    header.append(heading, close);
    panel.append(header);
  } else panel.append(heading);
  panel.append(thumb, details.metadata, element("p", "muted", t("quickPreviewShortcut")));
  if (item.local && item.kind === "video") {
    const cached = item.still;
    if (cached) {
      const image = element("img");
      image.alt = "";
      image.src = cached;
      thumb.append(image);
    } else {
      const probe = element("video");
      const timeout = setTimeout(cleanup, 5000);
      probe.crossOrigin = "anonymous";
      probe.muted = true;
      probe.preload = "auto";
      probe.addEventListener("error", cleanup, { once: true });
      probe.addEventListener("loadedmetadata", () => {
        updateMediaDetails(item, probe, details);
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
          const image = element("img");
          image.alt = "";
          image.src = item.still;
          thumb.append(image);
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
    panel.append(openFolder);
  }
  if (!item.local && !item.preview_only && item.size == null && !youtubeId(item)) {
    void inspectSize(item).then(() => { details.size.textContent = sizeText(item.size); });
  }
}

function savedItems() {
  const query = $("savedSearch").value.trim().toLocaleLowerCase(language);
  const oldest = $("savedSort").value === "oldest";
  return saved.items.filter(item => (saved.filter === "all" || item.kind === saved.filter) &&
    item.name.toLocaleLowerCase(language).includes(query)).sort((a, b) => {
    if (a.saved_at == null) return b.saved_at == null ? 0 : 1;
    if (b.saved_at == null) return -1;
    return oldest ? a.saved_at - b.saved_at : b.saved_at - a.saved_at;
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
    const width = $("savedGallery").clientWidth;
    if (!width) return;
    const gap = 12;
    const preferred = Math.min(272, Math.max(132, width / 5.5));
    for (const tiles of saved.rows) {
      let start = 0;
      let aspect = 0;
      for (const [index, tile] of tiles.entries()) {
        aspect += savedAspect(tile.item);
        const gaps = gap * (index - start);
        const available = width - gaps;
        const full = aspect * preferred >= available;
        if (!full && index < tiles.length - 1) continue;
        const height = full ? available / aspect : preferred;
        let pixels = 0;
        for (let j = start; j <= index; j++) {
          const { item, card } = tiles[j];
          const tileWidth = full && j === index ? available - pixels : Math.floor(savedAspect(item) * height);
          card.style.width = `${tileWidth}px`;
          card.style.height = `${Math.floor(height)}px`;
          pixels += tileWidth;
        }
        start = index + 1;
        aspect = 0;
      }
    }
  });
}

new ResizeObserver(scheduleSavedLayout).observe($("savedGallery"));

function renderSaved() {
  const items = savedItems();
  const gallery = $("savedGallery");
  const oldFocus = gallery.contains(document.activeElement) ? document.activeElement.closest(".card")?.dataset.url : null;
  saved.focus = items.find(item => item.url === saved.focus?.url) ?? null;
  if (!items.some(item => item.url === saved.detailsUrl)) saved.detailsUrl = null;
  const counts = new Map();
  for (const item of items) counts.set(item.kind, (counts.get(item.kind) ?? 0) + 1);
  const plural = new Intl.PluralRules(language);
  $("savedCount").textContent = galleryFilters.filter(([kind]) => counts.has(kind)).map(([kind]) => {
    const n = counts.get(kind);
    return t(`saved_${kind}_${plural.select(n)}`, { n: localizedNumber(n) });
  }).join(", ");
  $("refreshSaved").disabled = saved.loading;
  $("savedStream").hidden = !items.length;
  $("savedFilterControl").hidden = !saved.items.length;
  $("savedSortControl").hidden = !saved.items.length;
  const filters = $("savedFilters");
  filters.replaceChildren(...galleryFilters
    .filter(([kind]) => kind === "all" || saved.items.some(item => item.kind === kind))
    .map(([kind, key]) => new Option(t(kind === "all" ? "savedAll" : key), kind)));
  filters.value = saved.filter;
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
  let lastDay;
  let grid;
  let tiles;
  saved.rows = [];
  for (const item of items) {
    const date = item.saved_at == null ? null : new Date(item.saved_at * 1000);
    const day = date ? dateFormat.format(date) : t("savedUndated");
    if (day !== lastDay) {
      lastDay = day;
      const group = element("section", "saved-day");
      const heading = element("h2", "", day);
      heading.id = `saved-day-${saved.rows.length}`;
      group.setAttribute("aria-labelledby", heading.id);
      grid = element("div", "river");
      tiles = [];
      saved.rows.push(tiles);
      group.append(heading, grid);
      fragment.append(group);
      const year = date?.getFullYear();
      const month = date ? monthFormat.format(date) : day;
      if (!periods.has(month)) periods.set(month, { heading, year });
    }
    const card = mediaCard(item);
    grid.append(card);
    tiles.push({ card, item });
  }
  gallery.replaceChildren(fragment);
  const timeline = $("savedTimeline");
  const multipleYears = new Set(Array.from(periods.values(), period => period.year).filter(year => year != null)).size > 1;
  const stops = new Map();
  for (const [month, { heading, year }] of periods) {
    const label = multipleYears && year != null ? String(year) : month;
    if (!stops.has(label)) stops.set(label, heading);
  }
  timeline.hidden = stops.size < 2;
  timeline.replaceChildren(...Array.from(stops, ([label, heading]) => button(label, () => {
    heading.tabIndex = -1;
    heading.focus({ preventScroll: true });
    heading.scrollIntoView({ block: "start" });
  })));
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
      const sameFile = cached?.url === url && cached.size === file.size && cached.saved_at === file.saved_at;
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
$("savedSort").addEventListener("change", renderSaved);
$("savedFilters").addEventListener("change", event => {
  saved.filter = event.target.value;
  renderSaved();
});

function updateSelection() {
  for (const card of $("gallery").children) {
    const checked = app.selected.has(card.dataset.url);
    const checkbox = card.querySelector("input");
    if (checkbox) checkbox.checked = checked;
    card.classList.toggle("selected", checked);
    card.classList.toggle("active", app.focus?.url === card.dataset.url);
  }
  const visible = visibleItems().filter(item => !item.preview_only);
  $("allControl").hidden = visible.length === 0;
  const selected = visible.filter(item => app.selected.has(item.url)).length;
  $("selectAll").checked = visible.length > 0 && selected === visible.length;
  $("selectAll").indeterminate = selected > 0 && selected < visible.length;
  $("selectionCount").textContent = t("selected", { n: localizedNumber(app.selected.size) });
  $("enqueue").disabled = app.submitting || app.selected.size === 0 || !app.folder;
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

const durationText = value => Number.isFinite(value) ? t("filterSeconds", { n: localizedNumber(value) }) : t("unknownValue");
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
  const ext = item.name.match(/\.([a-z0-9]{2,5})$/i)?.[1];
  detail("format", id && !item.preview_only ? "MP4" : ext?.toUpperCase() ?? t("unknownValue"));
  const size = detail("size", sizeText(item.size));
  const duration = item.kind === "video" || item.kind === "audio" ? detail("duration", durationText(item.duration)) : null;
  const dimensions = detail("dimensions", item.width && item.height ? item.width + " × " + item.height : t("unknownValue"));
  detail("source", item.local ? t("localFile") : new URL(item.url).host);
  return { metadata, size, duration, dimensions };
}

function updateMediaDetails(item, player, { duration, dimensions }) {
  if (duration && Number.isFinite(player.duration)) {
    item.duration = player.duration;
    duration.textContent = durationText(item.duration);
  }
  if (player.videoWidth && player.videoHeight) {
    item.width = player.videoWidth;
    item.height = player.videoHeight;
    item.measuredDimensions = true;
    dimensions.textContent = item.width + " × " + item.height;
  }
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
  $("zoomTitle").textContent = item.name;
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
  if (!item.local && item.qualities?.length > 1) {
    actions.append(qualityPicker(item, value => {
      const card = Array.from($("gallery").children).find(card => card.dataset.url === item.url);
      const select = card?.querySelector(".card-quality select");
      if (select) select.value = value;
    }));
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
        player.src = item.url;
      }
      frame.replaceChildren(player);
      if (directMedia) void player.play().catch(error => {
        if (error.name === "NotAllowedError" && player.isConnected) play.hidden = false;
      });
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
    img.alt = item.name;
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
    if (event.key !== " " || event.target.closest("input, .preview-rail button, .preview-actions button")) return;
    event.preventDefault();
    if (!event.repeat) dialog.close();
    return;
  }
  if (event.key !== " " || app.tab === "queue" || document.querySelector("dialog[open]")) return;
  const item = app.tab === "saved" ? saved.focus : app.focus;
  if (!item) return;
  const target = event.target;
  const cardButton = target.closest(".card-button");
  if (!cardButton && (target.isContentEditable || target.closest("input, textarea, select, button, a"))) return;
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
  app.filterLoading = false;
  app.items = [];
  app.focus = null;
  app.detailsUrl = null;
  app.selected.clear();
  app.filter = "all";
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
    app.focus = visibleItems()[0] ?? null;
  } catch (error) {
    app.phase = "error";
    app.error = errorKey(error);
  } finally {
    $("searchButton").disabled = false;
    void refreshMediaFilters();
  }
});

$("openBlockedPage").addEventListener("click", async () => {
  try { await invoke("open_page", { url: app.searchUrl }); }
  catch (error) { notify(errorKey(error)); }
});

function renderFolder() {
  $("folderPath").textContent = app.folder || t("unknownValue");
  $("folderPath").title = app.folder;
}
$("pickFolder").addEventListener("click", async () => {
  $("pickFolder").disabled = true;
  try {
    app.folder = await invoke("pick_folder") || app.folder;
    renderFolder();
  } catch (error) { notify(errorKey(error)); }
  finally { $("pickFolder").disabled = false; }
});
$("enqueue").addEventListener("click", async () => {
  app.submitting = true;
  updateSelection();
  try {
    const items = app.items
      .filter(item => app.selected.has(item.url))
      .map(item => {
        const quality = item.qualities?.[item.qualityIndex ?? 0];
        return {
          url: quality?.url ?? item.url,
          name: item.name,
          size: quality ? quality.size : item.size,
          preview_only: item.preview_only,
          format_id: quality?.format_id
        };
      });
    await invoke("enqueue", { items });
    app.selected.clear();
    showTab("queue", true);
  } catch (error) { notify(errorKey(error)); }
  finally { app.submitting = false; updateSelection(); }
});

const jobNodes = new Map();
$("connectFirefox").addEventListener("click", async event => {
  const control = event.currentTarget;
  control.disabled = true;
  try {
    await invoke("connect_firefox");
    notify("firefoxConnected");
  } catch (error) {
    notify(errorKey(error));
  } finally {
    control.disabled = false;
  }
});
let queueCategory = "all";
let selectedJob;
let queueSort;
const speeds = new Map();

function visibleJobs() {
  const jobs = app.queue.filter(job => {
    if (queueCategory === "completed") return job.status === "completed";
    if (queueCategory === "unfinished") return job.status !== "completed";
    if (queueCategory === "video") return /\.(mp4|webm|m4v)$/i.test(job.file || job.name) || job.format_id;
    return true;
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

function updateQueueActions() {
  const job = app.queue.find(job => job.id === selectedJob && !job.browser);
  const status = job?.status;
  $("queueResume").disabled = !["deferred", "paused", "failed"].includes(status);
  $("queuePause").disabled = !["running", "queued"].includes(status);
  $("queueOpen").disabled = status !== "completed";
  $("queueRemove").disabled = !job || ["running", "pausing"].includes(status);
}

async function queueAction(action) {
  const job = app.queue.find(item => item.id === selectedJob && !item.browser);
  if (!job) return;
  if (action === "remove" && job.status !== "completed") {
    const dialog = $("removeDialog");
    dialog.returnValue = "";
    const close = new Promise(resolve => dialog.addEventListener("close", resolve, { once: true }));
    dialog.showModal();
    await close;
    if (dialog.returnValue !== "remove") return;
  }
  try {
    if (action === "open") await invoke("open_folder", { id: job.id });
    else await invoke("change_job", { id: job.id, action });
    await refreshQueue();
  } catch (error) { notify(errorKey(error)); }
}

$("queueAdd").onclick = () => { showTab("find"); $("url").focus(); };
for (const [key, action] of [["Resume", "resume"], ["Pause", "pause"], ["Open", "open"], ["Remove", "remove"]]) {
  $("queue" + key).onclick = () => void queueAction(action);
}
for (const control of $("queueCategories").querySelectorAll("button")) {
  control.onclick = () => {
    queueCategory = control.dataset.category;
    renderQueue();
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
  if (!ids.has(selectedJob)) selectedJob = undefined;
  updateQueueActions();
  for (const control of $("queueCategories").querySelectorAll("button")) {
    control.setAttribute("aria-pressed", String(control.dataset.category === queueCategory));
  }
  for (const control of $("queueTable").querySelectorAll("[data-sort]")) {
    if (queueSort?.field === control.dataset.sort) {
      control.parentElement.setAttribute("aria-sort", queueSort.sign === 1 ? "ascending" : "descending");
    } else control.parentElement.removeAttribute("aria-sort");
  }
  for (const [id, view] of jobNodes) if (!ids.has(id)) { view.row.remove(); jobNodes.delete(id); }
  $("queueEmpty").hidden = jobs.length > 0;
  $("queueEmpty").textContent = t(app.queue.length ? "emptyCategory" : "queueEmpty");
  for (const [index, job] of jobs.entries()) {
    if (!jobNodes.has(job.id)) {
      const row = element("tr", "job");
      const info = element("td", "job-info");
      const title = button(job.name, () => { selectedJob = job.id; renderQueue(); });
      title.className = "job-name";
      const folder = element("div", "job-folder");
      info.append(title, folder);
      const size = element("td", "job-size");
      const status = element("td", "job-state");
      const text = element("div", "job-status");
      const bar = element("progress", "job-progress");
      status.append(text, bar);
      const speed = element("td", "job-speed");
      row.append(info, size, status, speed);
      row.addEventListener("click", event => {
        if (event.target.closest("button")) return;
        selectedJob = job.id;
        renderQueue();
      });
      jobNodes.set(job.id, { row, title, folder, size, text, bar, speed });
    }
    const view = jobNodes.get(job.id);
    if (container.children[index] !== view.row) {
      const focused = document.activeElement === view.title;
      container.insertBefore(view.row, container.children[index] ?? null);
      if (focused) view.title.focus();
    }
    view.row.classList.toggle("selected", selectedJob === job.id);
    view.title.setAttribute("aria-pressed", String(selectedJob === job.id));
    view.title.textContent = job.file || job.name;
    view.title.title = view.title.textContent;
    view.folder.textContent = job.browser ? t("firefoxDownload") : job.folder;
    view.folder.title = view.folder.textContent;
    view.text.className = "job-status " + job.status;
    view.text.textContent = t(job.status) + (job.error ? " · " + t(job.error) : "");
    view.size.textContent = job.total != null ? sizeText(job.total) : job.done ? sizeText(job.done) : "—";
    const rate = speeds.get(job.id)?.rate;
    view.speed.textContent = rate > 0 ? t("sizePerSecond", { size: sizeText(rate) }) : "—";
    view.bar.hidden = !["running", "pausing", "paused", "completed", "firefoxOffline"].includes(job.status);
    view.bar.setAttribute("aria-label", t("progress", { name: job.name }));
    view.bar.max = Math.max(1, job.total ?? 1);
    if (job.status === "completed") view.bar.value = view.bar.max;
    else if (job.total != null) view.bar.value = job.done;
    else view.bar.removeAttribute("value");

  }
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
  for (const node of document.querySelectorAll("[data-t]")) node.textContent = t(node.dataset.t);
  for (const [id, key] of [["language", "language"], ["tabs", "app"], ["filters", "filter"], ["savedFilters", "filter"],
    ["findTab", "find"], ["queueTab", "downloads"], ["savedTab", "savedGallery"], ["savedSearch", "savedSearch"],
    ["savedSort", "savedSort"], ["refreshSaved", "refreshSaved"], ["savedTimeline", "savedTimeline"],
    ["queueCategories", "queueCategories"], ["closeZoom", "close"], ["previewRail", "previewStrip"], ["previousPreview", "previousPreview"], ["nextPreview", "nextPreview"]]) {
    $(id).setAttribute("aria-label", t(key));
  }
  for (const id of ["closeZoom", "previousPreview", "nextPreview"]) $(id).title = t(id === "closeZoom" ? "close" : id);
  for (const id of ["findTab", "queueTab", "savedTab", "savedFilters", "savedSort", "refreshSaved"]) $(id).title = $(id).getAttribute("aria-label");
  $("savedSearch").placeholder = t("savedSearch");
  if (lastNotice) $("notice").textContent = t(lastNotice);
  translateMediaFilters();
  renderGallery();
  renderSaved();
  renderQueue();
  renderFolder();
  renderUpdate();
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
