const host = "local.media.downloader";
const api = globalThis.browser ?? globalThis.chrome;
const sharing = typeof api.runtime.getBrowserInfo === "function" ? { data_collection: ["websiteActivity"] } : null;

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
  const { url: value, type, quality, title, formatId } = message;
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
      || (!youtube && !/\.(?:mp4|webm|jpe?g|png|webp|gif|avif|mp3|m4a|ogg|oga|wav)\/*$/i.test(url.pathname))
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
        auto_start: true, ...(formatId ? { format_id: formatId } : {})
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
