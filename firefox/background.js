const host = "local.media.downloader";
const api = globalThis.browser ?? globalThis.chrome;
const sharing = typeof api.runtime.getBrowserInfo === "function" ? { data_collection: ["websiteActivity"] } : null;
const videoExtension = /^(mp4|mov|m4v|webm|mkv|avi)$/i;
const videoTypes = {
  "video/mp4": "mp4", "video/quicktime": "mov", "video/x-m4v": "m4v", "video/webm": "webm",
  "video/x-matroska": "mkv", "video/matroska": "mkv", "video/x-msvideo": "avi",
  "video/avi": "avi", "video/msvideo": "avi"
};
const mediaRequests = { urls: ["*://*/*"], types: ["media"] };
const redirects = new Map();
api.webRequest.onBeforeRedirect.addListener(({requestId, url, tabId}) => {
  if (tabId < 0 || redirects.has(requestId)) return;
  if (redirects.size >= 128) redirects.delete(redirects.keys().next().value);
  redirects.set(requestId, url);
}, mediaRequests);
api.webRequest.onErrorOccurred.addListener(({requestId}) => redirects.delete(requestId), mediaRequests);
api.webRequest.onHeadersReceived.addListener(details => {
  const { requestId, tabId, frameId, method, statusCode, responseHeaders, url } = details;
  if (statusCode >= 300 && statusCode < 400) return;
  const original = redirects.get(requestId) ?? url;
  redirects.delete(requestId);
  if (tabId < 0 || details.incognito || method !== "GET" || ![200, 206].includes(statusCode)) return;
  const type = responseHeaders?.find(header => header.name.toLowerCase() === "content-type")
    ?.value?.split(";")[0].trim().toLowerCase();
  const extension = Object.hasOwn(videoTypes, type) ? videoTypes[type] : null;
  if (!extension) return;
  void api.tabs.sendMessage(tabId, { type: "videoResponse", url: original, extension }, { frameId }).catch(() => {});
}, mediaRequests, ["responseHeaders"]);

api.permissions.onRemoved.addListener(change => {
  if (change.data_collection?.includes("websiteActivity")) {
    void api.storage.local.remove("videoTransfer").catch(() => {});
  }
});
api.runtime.onInstalled.addListener(() => {
  // Remove the dynamically registered script used by 0.1.11 and earlier.
  void api.scripting.unregisterContentScripts({ ids: ["video"] }).catch(() => {});
});

async function startDownload(message, sender) {
  const { url: value, type, quality, title, formatId, videoFile } = message;
  const tab = sender.tab;
  if (!tab || tab.incognito || !Number.isInteger(sender.frameId) || sender.frameId < 0 || !sender.url
      || typeof value !== "string" || value.length > 8192) return { error: "sourceChanged" };
  const page = new URL(sender.url);
  const url = new URL(value);
  const pageVideo = page.pathname === "/watch" ? page.searchParams.get("v")
    : /^\/(?:shorts|live|embed)\/([\w-]{11})\/?$/.exec(page.pathname)?.[1];
  const youtube = ["https://youtube.com", "https://www.youtube.com", "https://m.youtube.com"].includes(page.origin)
    && /^[\w-]{11}$/.test(pageVideo ?? "")
    && value === "https://www.youtube.com/watch?v=" + pageVideo;
  if (!/^https?:$/.test(page.protocol) || !/^https?:$/.test(url.protocol) || url.username || url.password
      || (videoFile !== undefined && (type !== "saveVideo" || youtube
        || typeof videoFile !== "string" || !videoExtension.test(videoFile)))
      || (!youtube && videoFile === undefined
        && !/\.(?:mp4|mov|m4v|webm|mkv|avi|jpe?g|png|webp|gif|avif|mp3|m4a|ogg|oga|wav)\/*$/i.test(url.pathname))
      || !await api.permissions.contains({
        origins: [page, url].map(address => address.protocol + "//" + address.hostname + "/*")
      })) return { error: "sourceAccess" };
  if (type === "saveVideo" || type === "videoQualities") {
    if (type === "videoQualities" ? !youtube :
      !Number.isInteger(quality) || quality < 0 || quality > 8640 || typeof title !== "string"
      || (formatId !== undefined && (!youtube || typeof formatId !== "string" || !/^[\w-]{1,48}(?:\+[\w-]{1,48})?$/.test(formatId)))) {
      return { error: "sourceChanged" };
    }
    const { videoTransfer } = await api.storage.local.get("videoTransfer");
    if (videoTransfer !== true || (sharing && !await api.permissions.contains(sharing))) return { error: "allowClipSailTransfer" };
    try {
      if (type === "videoQualities") {
        const result = await api.runtime.sendNativeMessage(host, { youtube_qualities: url.href });
        if (result?.ok && Array.isArray(result.qualities)) return { qualities: result.qualities };
        return { error: result?.error || "connectClipSail" };
      }
      const name = Array.from(title.replace(/[\u0000-\u001f\u007f-\u009f]/g, " ").trim()).slice(0, 180).join("");
      const result = await api.runtime.sendNativeMessage(host, {
        url: url.href, origin: youtube ? url.origin : page.origin, title: name, quality,
        auto_start: true,
        ...(formatId ? { format_id: formatId } : {}),
        ...(videoFile ? { video_file: videoFile } : {})
      });
      return result?.ok ? { status: "sentToClipSail" }
        : { error: "connectClipSail" };
    } catch {
      return { error: "connectClipSail" };
    }
  }
  return { error: "sourceChanged" };
}

api.runtime.onMessage.addListener((message, sender, reply) => {
  if (!["saveVideo", "videoQualities"].includes(message?.type)) return;
  void startDownload(message, sender).catch(() => ({ error: "downloadFailed" })).then(reply);
  return true;
});
