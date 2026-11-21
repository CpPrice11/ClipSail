(() => {
  if (globalThis.mediaDownloaderActive) return;
  globalThis.mediaDownloaderActive = true;
  const api = globalThis.browser ?? globalThis.chrome;

  const encrypted = new WeakSet();
  const buttons = new Map();
  const pending = new WeakSet();
  const ids = new WeakMap();
  let nextId = 0;
  const t = key => api.i18n.getMessage(key);
  const protectedMedia = media => media.mediaKeys || encrypted.has(media);
  const declaredSource = media => media.getAttribute("src") || media.getAttribute("data-src");
  const mediaPath = url => url.pathname.replace(/\/+$/, "");
  const imagePattern = /\.(?:jpe?g|png|webp|gif|avif)$/i;
  const audioPattern = /\.(?:mp3|m4a|ogg|oga|wav)$/i;
  const videoPattern = /\.(?:mp4|webm)$/i;
  const decorative = /(?:^|[\s/_-])(?:avatar|emoji|favicon|icon|logo|spinner|sprite|badge|placeholder|tracking|pixel|banner|advert)(?:[\s/_.-]|$)/i;
  let hideHere = false;
  let settings = {};

  function youtubeLink() {
    const page = new URL(location.href);
    if (page.protocol !== "https:" || !["youtube.com", "www.youtube.com", "m.youtube.com"].includes(page.hostname)) return null;
    const id = page.pathname === "/watch" ? page.searchParams.get("v")
      : /^\/(?:shorts|live|embed)\/([\w-]{11})\/?$/.exec(page.pathname)?.[1];
    return /^[\w-]{11}$/.test(id ?? "") ? "https://www.youtube.com/watch?v=" + id : null;
  }

  function youtubeSource(video) {
    return video.matches("video.html5-main-video") && !video.closest(".ad-showing, .ad-interrupting")
      && !protectedMedia(video) ? youtubeLink() : null;
  }

  function address(value, pattern) {
    if (!value || value.length > 8192) return null;
    try {
      const url = new URL(value, document.baseURI);
      if (!/^https?:$/.test(url.protocol) || url.username || url.password || !pattern.test(mediaPath(url))) return null;
      url.hash = "";
      return url;
    } catch {
      return null;
    }
  }

  function variants(media) {
    if (media instanceof HTMLImageElement) {
      const url = address(media.currentSrc, imagePattern);
      if (!url || url.origin !== location.origin || media.naturalWidth < 240 || media.naturalHeight < 160
          || decorative.test([url.pathname, media.id, media.className, media.alt].join(" "))) return [];
      return [{ url, height: 0 }];
    }
    if (media.error || protectedMedia(media)) return [];
    const isVideo = media instanceof HTMLVideoElement;
    const pattern = isVideo ? videoPattern : audioPattern;
    const sources = [...media.querySelectorAll(":scope > source[src], :scope > source[data-src]")].slice(0, 24);
    const candidate = media.currentSrc || declaredSource(media);
    const current = candidate ? address(candidate, pattern)
      : sources.map(source => address(declaredSource(source), pattern)).find(Boolean);
    if (!current) return [];
    if (!isVideo) return [{ url: current, height: 0 }];
    const resolution = (source, url) => {
      const match = [source?.dataset.quality, source?.dataset.height, source?.dataset.res, source?.dataset.resolution,
        source?.dataset.size, source?.getAttribute("res"), source?.getAttribute("size"), source?.getAttribute("label"),
        /(?:^|[^\da-z])(\d{3,4}p)(?:[^\da-z]|$)/i.exec(url.pathname)?.[1]]
        .find(value => /^\d{3,4}p?$/i.test(value ?? ""));
      const height = Number.parseInt(match, 10);
      return height >= 100 && height <= 8640 ? height : 0;
    };
    const options = new Map();
    for (const source of sources) {
      const url = address(declaredSource(source), pattern);
      if (!url) continue;
      const height = resolution(source, url);
      if (height || url.href === current.href) options.set(url.href, { url, height });
    }
    const currentHeight = options.get(current.href)?.height || resolution(media, current)
      || Math.min(media.videoWidth, media.videoHeight);
    options.set(current.href, { url: current, height: currentHeight });
    return [...options.values()].sort((a, b) => b.height - a.height);
  }

  async function youtubeQualities(video) {
    if (protectedMedia(video)) return { error: "protected" };
    const url = youtubeSource(video);
    if (!url) return { error: "sourceChanged" };
    try {
      const result = await api.runtime.sendMessage({ type: "videoQualities", url });
      return youtubeSource(video) === url ? { ...result, url } : { error: "sourceChanged" };
    } catch {
      return { error: "downloadFailed" };
    }
  }

  async function start(video, variant, selection) {
    if (protectedMedia(video)) return { error: "protected" };
    const options = variants(video);
    const option = variant ? options.find(option => option.url.href === variant) : options[0];
    const youtube = !variant && !option && youtubeSource(video);
    if (selection && (!youtube || selection.url !== youtube)) return { error: "sourceChanged" };
    if (!option && !youtube) return { error: variant ? "sourceChanged" : "unsupportedVideo" };
    // Let the browser inspect the actual media before offering to save an unloaded stream.
    if (!youtube && video instanceof HTMLMediaElement && !video.error
        && video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA) return { error: "playFirst" };
    if (pending.has(video)) return { error: "sourceChanged" };
    pending.add(video);
    try {
      return await api.runtime.sendMessage({
        type: "saveVideo", url: youtube || option.url.href, quality: option?.height ?? 0, title: video instanceof HTMLVideoElement ? document.title : "",
        formatId: selection?.formatId
      });
    } catch {
      return { error: "downloadFailed" };
    } finally {
      pending.delete(video);
    }
  }

  function renderButton(video) {
    const direct = variants(video).length > 0;
    const youtube = !direct && Boolean(youtubeSource(video));
    const mode = direct ? "direct" : youtube ? "youtube" : "unavailable";
    if (hideHere || settings.off || settings.sites?.[location.hostname] === true || document.fullscreenElement
        || !video.isConnected || video.getClientRects().length === 0) {
      buttons.get(video)?.remove();
      buttons.delete(video);
      return;
    }
    const old = buttons.get(video);
    const label = "downloadThisVideo";
    if (old?.isConnected && old.dataset.mode === mode) return;
    old?.remove();
    const host = document.createElement("span");
    host.dataset.action = label;
    host.dataset.direct = String(direct);
    host.dataset.mode = mode;
    const shadow = host.attachShadow({ mode: "closed" });
    const button = document.createElement("button");
    const status = document.createElement("span");
    const menu = document.createElement("div");
    const style = document.createElement("style");
    style.textContent = `
      :host { position: fixed; max-width: calc(100vw - 16px); z-index: 2147483646; font: 14px/1.4 "Segoe UI Variable", "Segoe UI", system-ui, sans-serif; color: #fafafa; }
      button { font: inherit; min-height: 32px; padding: 4px 10px; border-radius: 4px; border: 1px solid #454545; background: #333; color: #fafafa; cursor: pointer; }
      button:hover:not(:disabled) { background: #454545; }
      .download-trigger { display: grid; place-items: center; width: 36px; height: 36px; padding: 0; }
      .download-trigger[aria-expanded="true"] { color: #dc81d6; border-color: #dc81d6; }
      button:disabled { cursor: wait; opacity: .7; }
      div[hidden], span:empty { display: none; }
      button:focus-visible { outline: 2px solid #f7f7f7; outline-offset: 2px; }
      div, span:not(:empty) { position: absolute; top: calc(100% + 4px); right: 0; width: max-content; max-width: min(280px, calc(100vw - 16px)); background: #202020; color: #fafafa; padding: 8px; border: 1px solid #454545; border-radius: 6px; }
      div:not([hidden]), section:not(:empty) { display: grid; gap: 6px; } section { max-height: min(280px, 40vh); overflow-y: auto; padding: 2px; }
      div span:not(:empty) { position: static; width: auto; padding: 0; border: 0; }
      :host([hidden]) { display: none; }
    `;
    button.type = "button";
    button.className = "download-trigger";
    button.title = t(label);
    button.setAttribute("aria-label", button.title);
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", "20");
    svg.setAttribute("height", "20");
    svg.setAttribute("aria-hidden", "true");
    const path = document.createElementNS(svg.namespaceURI, "path");
    path.setAttribute("d", "M12 3v12m-5-5 5 5 5-5M5 16v4h14v-4");
    path.setAttribute("fill", "none");
    path.setAttribute("stroke", "currentColor");
    path.setAttribute("stroke-width", "1.8");
    path.setAttribute("stroke-linecap", "round");
    path.setAttribute("stroke-linejoin", "round");
    svg.append(path);
    button.append(svg);
    status.setAttribute("role", "status");
    menu.hidden = true;
    const choices = document.createElement("section");
    const menuStatus = document.createElement("span");
    menuStatus.setAttribute("role", "status");
    menu.id = "downloadOptions";
    menu.setAttribute("role", "group");
    menu.setAttribute("aria-label", t("downloadOptions"));
    menu.append(choices, menuStatus);
    let menuVersion = 0;
    let sending = false;
    const closeMenu = () => {
      menuVersion++;
      menu.hidden = true;
      button.setAttribute("aria-expanded", "false");
    };
    if (direct || youtube) {
      button.setAttribute("aria-expanded", "false");
      button.setAttribute("aria-controls", menu.id);
      shadow.addEventListener("keydown", event => {
        if (event.key !== "Escape" || menu.hidden) return;
        event.stopPropagation();
        closeMenu();
        button.focus();
      });
      shadow.addEventListener("focusout", event => {
        if (event.relatedTarget && !shadow.contains(event.relatedTarget) && !menu.hidden && choices.childElementCount && !sending) closeMenu();
      });
    }
    button.onclick = async event => {
      if (!event.isTrusted) return;
      status.textContent = "";
      if (!direct && !youtube) {
        const result = await start(video);
        status.textContent = t(result.error ?? result.status);
        return;
      }
      if (!menu.hidden) { closeMenu(); return; }
      choices.replaceChildren();
      menuStatus.textContent = youtube ? t("checkingQualities") : "";
      menu.hidden = false;
      button.setAttribute("aria-expanded", "true");
      const version = ++menuVersion;
      const result = youtube ? await youtubeQualities(video) : {
        qualities: variants(video).map(({ url, height }) => ({
          height, extension: mediaPath(url).split(".").pop(), format_id: url.href
        }))
      };
      if (menuVersion !== version || !host.isConnected) return;
      if (!result.qualities?.length) {
        menuStatus.textContent = t(result.error ?? "sourceChanged");
        return;
      }
      menuStatus.textContent = "";
      for (const [index, option] of result.qualities.entries()) {
        const height = option.width && option.height ? Math.min(option.width, option.height) : option.height;
        const text = [height ? height + "p" : "", option.extension.toUpperCase()].filter(Boolean).join(" · ");
        const choice = document.createElement("button");
        choice.type = "button";
        choice.textContent = index === 0 ? api.i18n.getMessage("highest", text) : text;
        choice.onclick = async event => {
          if (!event.isTrusted || sending) return;
          sending = true;
          try {
            const response = await start(video, direct ? option.format_id : undefined,
              youtube ? { url: result.url, formatId: option.format_id } : undefined);
            if (response.error) menuStatus.textContent = t(response.error);
            else {
              if (!menu.hidden) { closeMenu(); button.focus(); }
              status.textContent = t(response.status);
            }
          } finally { sending = false; }
        };
        choices.append(choice);
      }
      if (shadow.activeElement === button || !shadow.activeElement) choices.firstElementChild?.focus();
    };
    shadow.append(style, button, menu, status);
    document.documentElement.append(host);
    buttons.set(video, host);
  }

  function refresh() {
    for (const [video, host] of buttons) {
      if (!video.isConnected) {
        host.remove();
        buttons.delete(video);
      }
    }
    for (const video of document.querySelectorAll("video")) {
      renderButton(video);
      const host = buttons.get(video);
      if (!host) continue;
      const rect = video.getBoundingClientRect();
      const hidden = rect.width === 0 || rect.height === 0 || getComputedStyle(video).visibility !== "visible"
        || rect.bottom <= 0 || rect.top >= innerHeight || rect.right <= 0 || rect.left >= innerWidth;
      if (host.hidden !== hidden) host.hidden = hidden;
      host.style.top = Math.min(Math.max(4, innerHeight - 36), Math.max(4, rect.top + 8)) + "px";
      host.style.right = Math.max(4, innerWidth - rect.right + 8) + "px";
    }
  }

  let scheduled = false;
  function schedule() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => { scheduled = false; refresh(); });
  }
  document.addEventListener("encrypted", event => {
    if (event.target instanceof HTMLMediaElement) {
      encrypted.add(event.target);
      schedule();
    }
  }, true);
  for (const event of ["loadeddata", "playing", "emptied", "error"]) {
    document.addEventListener(event, schedule, true);
  }
  for (const event of ["fullscreenchange", "scroll", "resize"]) addEventListener(event, schedule, true);
  const updateSettings = value => { settings = value ?? {}; schedule(); };
  void api.storage.local.get("buttons").then(state => updateSettings(state.buttons));
  api.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes.buttons) updateSettings(changes.buttons.newValue);
  });
  new MutationObserver(schedule).observe(document, { childList: true, subtree: true, attributes: true,
    attributeFilter: ["src", "data-src", "srcset", "hidden", "width", "height", "class"] });
  schedule();

  api.runtime.onMessage.addListener((message, _sender, reply) => {
    if (message.type === "inspect") {
      const mediaElements = [...document.querySelectorAll("video, audio, img")].slice(0, 1000);
      const items = mediaElements.flatMap(media => {
        const options = variants(media);
        if (!options.length) return [];
        if (!ids.has(media)) ids.set(media, ++nextId);
        const path = mediaPath(options[0].url).split("/").pop();
        let name = path;
        try { name = decodeURIComponent(path); } catch { /* Preserve the encoded filename. */ }
        return [{
          id: ids.get(media), name: name.slice(0, 180), kind: media.tagName.toLowerCase(),
          dimensions: media instanceof HTMLImageElement ? [media.naturalWidth, media.naturalHeight]
            : media instanceof HTMLVideoElement ? [media.videoWidth, media.videoHeight] : [],
          duration: Number.isFinite(media.duration) ? media.duration : null,
          variants: options.map(({ url, height }) => ({ value: url.href, height, ext: mediaPath(url).split(".").pop() }))
        }];
      }).slice(0, 60);
      const youtubePlayer = mediaElements.find(media => media instanceof HTMLVideoElement
        && youtubeSource(media) && !variants(media).length);
      if (youtubePlayer && !ids.has(youtubePlayer)) ids.set(youtubePlayer, ++nextId);
      reply({
        items, hideHere,
        videoLink: youtubePlayer ? youtubeLink() : null,
        youtubePlayerId: youtubePlayer ? ids.get(youtubePlayer) : null,
        error: mediaElements.some(protectedMedia) ? "protected" : "unavailable"
      });
      return;
    }
    if (["start", "videoQualities"].includes(message.type) && Number.isInteger(message.id)) {
      const media = [...document.querySelectorAll("video, audio, img")].find(media => ids.get(media) === message.id);
      if (!media) {
        reply({ error: "sourceChanged" });
        return;
      }
      const result = message.type === "videoQualities" ? youtubeQualities(media)
        : start(media, message.variant, message.selection);
      void result.catch(() => ({ error: "downloadFailed" })).then(reply);
      return true;
    }
    if (message.type === "togglePage") {
      hideHere = !hideHere;
      schedule();
      reply(hideHere);
    }
  });
})();
