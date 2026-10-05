// 局域网快传页面。所有网络请求都经过 proto.js 的加密客户端；本文件只管界面。
import { loadCrypto, LanShareClient, Unauthorized, WrongPin, HttpError, ServerError, keyFromFragment } from "./proto.js";

const $ = (id) => document.getElementById(id);
const SVG_NS = "http://www.w3.org/2000/svg";
const ICONS = {
  download: "M12 4v11M7 10l5 5 5-5M5 20h14",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  copy: "M9 9h10v10H9zM5 15V5h10",
};
const TYPES = [
  [["jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp", "svg", "tif", "tiff", "raw", "dng"], "image"],
  [["mp4", "mov", "avi", "mkv", "webm", "m4v", "3gp", "wmv", "flv"], "video"],
  [["mp3", "wav", "m4a", "flac", "aac", "ogg", "wma", "amr"], "audio"],
  [["pdf"], "pdf"],
  [["doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv", "rtf", "pages", "numbers", "key"], "doc"],
  [["zip", "rar", "7z", "tar", "gz", "bz2", "xz"], "archive"],
  [["apk", "exe", "msi", "dmg", "ipa"], "app"],
];
const ERRORS = {
  disk_full: "电脑磁盘空间不足",
  changed: "文件在电脑上被改动了，请刷新后重试",
  not_found: "文件已经不在了",
  text_too_long: "文字太长了（最多 10000 字）",
  empty_text: "文字不能为空",
  too_many_uploads: "同时上传的文件太多，请稍后再试",
};

let client;
let isDesktop = false;
let pollTimer = null;
let pollCount = 0;
let failures = 0;
let lastFilesKey = "";
let lastTextsKey = "";
let busy = 0;
let qrUrl = null;

// ---------- 小工具 ----------

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function icon(name) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("class", "i");
  svg.setAttribute("viewBox", "0 0 24 24");
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", ICONS[name]);
  svg.appendChild(path);
  return svg;
}

function fmtSize(n) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
  return (i === 0 ? String(Math.round(n)) : n.toFixed(1)) + " " + units[i];
}

function fmtTime(seconds) {
  const d = new Date(seconds * 1000);
  const pad = (x) => String(x).padStart(2, "0");
  const hm = pad(d.getHours()) + ":" + pad(d.getMinutes());
  if (d.toDateString() === new Date().toDateString()) return hm;
  return (d.getMonth() + 1) + "月" + d.getDate() + "日 " + hm;
}

function badgeFor(name) {
  const dot = name.lastIndexOf(".");
  const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
  const kind = (TYPES.find(([exts]) => exts.includes(ext)) || [null, ""])[1];
  return el("span", "badge " + kind, ext ? ext.slice(0, 4).toUpperCase() : "FILE");
}

let toastTimer = null;
function toast(text) {
  const t = $("toast");
  t.textContent = text;
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 2200);
}

function describe(error) {
  if (error instanceof ServerError) return ERRORS[error.code] || "出错了（" + error.code + "）";
  if (error instanceof HttpError) return "出错了（HTTP " + error.status + "）";
  return "网络中断，请重试";
}

// ---------- 进入 / 登录 ----------

function showLogin(message) {
  stopPolling();
  lastFilesKey = "";
  lastTextsKey = "";
  $("booting").hidden = true;
  $("offline").hidden = true;
  $("app").hidden = true;
  $("waiting").hidden = true;
  $("login").hidden = false;
  $("loginMsg").textContent = message || "";
}

/** 局域网里的新设备：先等电脑上点“允许”。等的时候显示验证码，方便核对电脑上那一条是不是自己。被拒绝返回 false。 */
async function waitForApproval() {
  for (;;) {
    const info = await client.rpc({ op: "info" });
    if (info.ok) return true;
    if (info.error === "rejected") {
      client.forget();
      showLogin("电脑上拒绝了这台设备的连接");
      return false;
    }
    if (info.error !== "pending_approval" && info.error !== "approval_busy") throw new ServerError(info.error);
    showWaiting(info.code);
    await new Promise((resolve) => setTimeout(resolve, 1500));
  }
}

function showWaiting(code) {
  $("booting").hidden = true;
  $("login").hidden = true;
  $("waiting").hidden = false;
  $("waitCode").textContent = code || "····";
  $("waitBusy").hidden = !!code;
}

async function enterApp() {
  if (!(await waitForApproval())) return;
  await loadInfo();
  $("booting").hidden = true;
  $("login").hidden = true;
  $("waiting").hidden = true;
  $("app").hidden = false;
  refresh();
  startPolling();
}

/** 口令和“口令登录已暂停”提示（只有本机界面会看到暂停提示和重新开启按钮）。 */
function showPin(info) {
  $("pin").textContent = info.pin;
  $("pin").classList.toggle("paused", !!info.pin_paused);
  $("pinPaused").hidden = !(isDesktop && info.pin_paused);
}

$("pinResume").addEventListener("click", async () => {
  $("pinResume").disabled = true;
  try {
    const r = await client.rpc({ op: "pin_resume" });
    if (!r.ok) throw new ServerError(r.error);
    showPin({ pin: r.pin, pin_paused: false });
    toast("已重新开启口令登录，口令换成了 " + r.pin);
  } catch {
    toast("没能重新开启，请稍后再试");
  } finally {
    $("pinResume").disabled = false;
  }
});

async function loadInfo() {
  const info = await client.rpc({ op: "info" });
  client.rememberKey(info.key);
  isDesktop = !!info.local;
  document.body.classList.toggle("desktop", isDesktop);
  document.body.classList.toggle("mobile", !isDesktop);
  const main = info.urls[0].replace(/\/$/, "");
  $("url").textContent = main;
  showPin(info);
  const others = info.urls.slice(1).map((u) => u.replace(/\/$/, ""));
  $("others").hidden = others.length === 0;
  $("others").textContent = "扫码地址打不开时，在手机浏览器里试试：" + others.join("、");
  if (qrUrl) URL.revokeObjectURL(qrUrl);
  qrUrl = URL.createObjectURL(new Blob([info.qr_svg], { type: "image/svg+xml" }));
  $("qr").src = qrUrl;
  $("connectFold").open = isDesktop;
  $("connectTitle").textContent = isDesktop ? "用手机扫码连接" : "让其他设备也连上来";
  $("openFolder").hidden = !isDesktop;
  $("trayNote").hidden = !isDesktop; // v1 用启动气泡提示这句话；v2 的托盘库没有气泡，改成写在本机界面上
  $("chipText").textContent = isDesktop ? "运行中 · " + main.replace("http://", "") : "已连接";
  $("dropTitle").textContent = isDesktop ? "把文件拖到窗口里，或点这里选择" : "选择文件发送";
  $("dropHint").textContent = isDesktop ? "所有连上的手机、电脑都能下载" : "可以多选，照片、视频、文档都行";
}

/** 会话失效（多半是电脑上的程序重启过）：先试着用存下的密钥重连，不行再回口令页。 */
async function recover() {
  stopPolling();
  client.serverIdCache = null;
  try {
    if (await client.resume()) {
      await enterApp();
      return;
    }
  } catch { /* 连不上就回口令页 */ }
  showLogin("电脑上的程序重启过，请重新扫码或输入口令");
}

$("loginForm").addEventListener("submit", async (event) => {
  event.preventDefault();
  const pin = $("pinInput").value.trim();
  if (!/^\d{6}$/.test(pin)) {
    $("loginMsg").textContent = "口令是 6 位数字";
    return;
  }
  $("loginBtn").disabled = true;
  $("loginMsg").textContent = "";
  try {
    client.serverIdCache = null;
    await client.pairWithPin(pin);
    $("pinInput").value = "";
    await enterApp();
  } catch (e) {
    if (e instanceof WrongPin) $("loginMsg").textContent = "口令不对，请看电脑上显示的 6 位数字";
    else if (e instanceof HttpError && e.status === 429) $("loginMsg").textContent = "尝试太频繁，请 1 分钟后再试";
    else if (e instanceof HttpError && e.status === 423) $("loginMsg").textContent = "有人连续输错口令太多次，口令登录已暂停。请在电脑上的主界面里重新开启，或者直接扫码";
    else $("loginMsg").textContent = "连不上电脑上的程序，请确认它还在运行";
  } finally {
    $("loginBtn").disabled = false;
  }
});

// ---------- 轮询 ----------

function startPolling() {
  stopPolling();
  pollTimer = setInterval(() => { if (!document.hidden) refresh(); }, 2000);
}

function stopPolling() {
  if (pollTimer) clearInterval(pollTimer);
  pollTimer = null;
}

document.addEventListener("visibilitychange", () => {
  if (!document.hidden && pollTimer) refresh();
});

async function refresh() {
  try {
    const [files, texts] = await Promise.all([client.rpc({ op: "list" }), client.rpc({ op: "texts" })]);
    renderFiles(files.files);
    renderTexts(texts.texts);
    if (isDesktop) renderApprovals((await client.rpc({ op: "approvals" })).devices);
    // 本机界面每 10 秒看一次口令状态：被暂停时要让电脑前的人看到
    if (isDesktop && ++pollCount % 5 === 0) showPin(await client.rpc({ op: "info" }));
    failures = 0;
    $("offline").hidden = true;
  } catch (e) {
    if (e instanceof Unauthorized) return recover();
    failures += 1;
    if (failures >= 2) $("offline").hidden = false;
  }
}

// ---------- 新设备确认（电脑上） ----------

let lastApprovalsKey = "";

function renderApprovals(devices) {
  document.title = devices.length ? "（" + devices.length + "）有设备等确认 · 局域网快传" : "局域网快传";
  const key = JSON.stringify(devices);
  if (key === lastApprovalsKey) return;
  lastApprovalsKey = key;
  $("approvals").hidden = devices.length === 0;
  const list = $("approvalList");
  list.textContent = "";
  for (const d of devices) {
    const li = el("li", "conn-row");
    const who = el("span");
    who.appendChild(el("b", "", d.device + " "));
    who.appendChild(el("span", "muted", d.ip));
    li.appendChild(who);
    li.appendChild(el("span", "pin", d.code));
    const allow = el("button", "btn", "允许");
    allow.type = "button";
    allow.addEventListener("click", () => decide(d, true));
    const deny = el("button", "ghost", "拒绝");
    deny.type = "button";
    deny.addEventListener("click", () => decide(d, false));
    const buttons = el("span");
    buttons.appendChild(allow);
    buttons.appendChild(document.createTextNode(" "));
    buttons.appendChild(deny);
    li.appendChild(buttons);
    list.appendChild(li);
  }
}

async function decide(device, allow) {
  try {
    const r = await client.rpc({ op: allow ? "approve" : "reject", id: device.id });
    toast(r.ok ? (allow ? "已允许 " : "已拒绝 ") + device.device : "这台设备已经不在等了");
  } catch {
    toast("操作失败，请重试");
  }
  refresh();
}

// ---------- 文件列表 ----------

function renderFiles(files) {
  const key = JSON.stringify(files) + isDesktop;
  if (key === lastFilesKey) return;
  lastFilesKey = key;
  $("fileCount").textContent = files.length ? files.length + " 个" : "";
  const list = $("fileList");
  list.textContent = "";
  if (files.length === 0) {
    list.appendChild(el("li", "empty", "还没有文件。从任何一台设备发送的文件都会出现在这里。"));
    return;
  }
  for (const f of files) {
    const li = el("li");
    const row = el("a", "file");
    row.href = "#";
    row.appendChild(badgeFor(f.name));
    const main = el("span", "file-main");
    main.appendChild(el("div", "file-name", f.name));
    const meta = el("div", "file-meta", fmtSize(f.size) + " · " + fmtTime(f.mtime));
    main.appendChild(meta);
    row.appendChild(main);
    const act = el("span", "file-act");
    act.appendChild(icon(isDesktop ? "folder" : "download"));
    row.appendChild(act);
    row.title = isDesktop ? "在文件夹中显示" : "下载";
    row.addEventListener("click", (event) => {
      event.preventDefault();
      if (isDesktop) client.rpc({ op: "reveal", id: f.id }).then((r) => { if (!r.ok) toast(ERRORS[r.error] || "打不开"); });
      else download(f, meta);
    });
    li.appendChild(row);
    list.appendChild(li);
  }
}

async function download(f, meta) {
  const original = meta.textContent;
  busy += 1;
  try {
    const blob = await client.downloadFile(f, {
      onProgress: (n) => { meta.textContent = "下载中 " + Math.floor((n / Math.max(f.size, 1)) * 100) + "%"; },
    });
    const url = URL.createObjectURL(blob);
    const a = el("a");
    a.href = url;
    a.download = f.name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 60_000);
    toast("已下载 " + f.name);
  } catch (e) {
    if (e instanceof Unauthorized) return recover();
    toast(describe(e));
  } finally {
    busy -= 1;
    meta.textContent = original;
  }
}

$("openFolder").addEventListener("click", () => { client.rpc({ op: "open_folder" }).catch(() => {}); });

// ---------- 文字 ----------

async function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) {
    try { await navigator.clipboard.writeText(text); return true; } catch { /* 回退到下面的办法 */ }
  }
  // 手机用 http://局域网IP 访问不是“安全上下文”，没有 navigator.clipboard
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.className = "visually-hidden";
  document.body.appendChild(area);
  area.select();
  area.setSelectionRange(0, text.length);
  let ok = false;
  try { ok = document.execCommand("copy"); } catch { ok = false; }
  area.remove();
  return ok;
}

function renderTexts(items) {
  const key = JSON.stringify(items);
  if (key === lastTextsKey) return;
  lastTextsKey = key;
  const list = $("textList");
  list.textContent = "";
  for (const m of items) {
    const li = el("li", "msg");
    li.appendChild(el("div", "msg-text", m.text));
    const bar = el("div", "msg-bar");
    bar.appendChild(el("span", "msg-time", fmtTime(m.time)));
    const button = el("button", "ghost");
    button.type = "button";
    button.appendChild(icon("copy"));
    button.appendChild(document.createTextNode("复制"));
    button.addEventListener("click", async () => {
      toast((await copyText(m.text)) ? "已复制" : "复制失败，请长按文字手动复制");
    });
    bar.appendChild(button);
    li.appendChild(bar);
    list.appendChild(li);
  }
}

async function sendText() {
  const input = $("textInput");
  const text = input.value;
  if (!text.trim()) return;
  const button = $("sendText");
  button.disabled = true;
  try {
    const r = await client.rpc({ op: "text_add", text });
    if (r.ok) {
      input.value = "";
      refresh();
    } else {
      toast(ERRORS[r.error] || "发送失败");
    }
  } catch (e) {
    if (e instanceof Unauthorized) return recover();
    toast("发送失败，请检查网络");
  } finally {
    button.disabled = false;
  }
}

$("sendText").addEventListener("click", sendText);
$("textInput").addEventListener("keydown", (event) => {
  if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
    event.preventDefault();
    sendText();
  }
});

// ---------- 上传 ----------

const queue = [];
let uploading = false;

function addFiles(fileList) {
  for (const file of Array.from(fileList)) {
    const li = el("li", "up");
    const row = el("div", "up-row");
    row.appendChild(el("span", "up-name", file.name));
    const status = el("span", "up-status", "等待中 · " + fmtSize(file.size));
    row.appendChild(status);
    const bar = el("div", "bar");
    const fill = el("div", "fill");
    bar.appendChild(fill);
    li.appendChild(row);
    li.appendChild(bar);
    $("queue").prepend(li);
    queue.push({ file, li, fill, status });
  }
  pump();
}

async function pump() {
  if (uploading) return;
  uploading = true;
  busy += 1;
  while (queue.length) {
    const job = queue.shift();
    const started = performance.now();
    const elapsed = () => Math.max((performance.now() - started) / 1000, 0.001);
    try {
      const saved = await client.uploadFile(job.file, {
        onProgress: (sent) => {
          const pct = job.file.size ? (sent / job.file.size) * 100 : 100;
          job.fill.style.width = pct.toFixed(1) + "%";
          job.status.textContent = pct.toFixed(0) + "% · " + fmtSize(sent / elapsed()) + "/s";
        },
      });
      job.li.classList.add("done");
      job.fill.style.width = "100%";
      job.status.textContent = "已发送 · " + fmtSize(job.file.size / elapsed()) + "/s";
      if (saved !== job.file.name) toast("有同名文件，已存为 " + saved);
      setTimeout(() => job.li.remove(), 6000);
      refresh();
    } catch (e) {
      job.li.classList.add("failed");
      job.status.textContent = describe(e);
      if (e instanceof Unauthorized) {
        while (queue.length) {
          const rest = queue.shift();
          rest.li.classList.add("failed");
          rest.status.textContent = "已取消";
        }
        recover();
        break;
      }
    }
  }
  uploading = false;
  busy -= 1;
}

$("picker").addEventListener("change", () => {
  addFiles($("picker").files);
  $("picker").value = "";
});

// 拖放：整个窗口都能接文件；只拦截带文件的拖拽，拖文字进输入框照常可用
let dragDepth = 0;
const hasFiles = (event) => event.dataTransfer && Array.from(event.dataTransfer.types || []).includes("Files");
window.addEventListener("dragenter", (event) => {
  if (!hasFiles(event) || $("app").hidden) return;
  event.preventDefault();
  dragDepth += 1;
  $("overlay").hidden = false;
});
window.addEventListener("dragleave", (event) => {
  if (!hasFiles(event)) return;
  dragDepth = Math.max(0, dragDepth - 1);
  if (dragDepth === 0) $("overlay").hidden = true;
});
window.addEventListener("dragover", (event) => { if (hasFiles(event)) event.preventDefault(); });
window.addEventListener("drop", (event) => {
  if (!hasFiles(event)) return;
  event.preventDefault();
  dragDepth = 0;
  $("overlay").hidden = true;
  if (!$("app").hidden && event.dataTransfer.files.length) addFiles(event.dataTransfer.files);
});

window.addEventListener("beforeunload", (event) => {
  if (busy > 0 || queue.length) {
    event.preventDefault();
    event.returnValue = "";
  }
});

// ---------- 启动 ----------

async function boot() {
  // 先把密钥从地址栏和历史记录里抹掉，再去加载加密模块（不让密钥在地址栏里多停留）
  const key = keyFromFragment(location.hash);
  if (key) history.replaceState(null, "", location.pathname);
  try {
    client = new LanShareClient(await loadCrypto("/lanshare.wasm"));
  } catch {
    $("booting").textContent = "加载加密模块失败，请刷新重试";
    return;
  }
  try {
    if (key) {
      await client.pairWithKey(key);
      return await enterApp();
    }
    if (await client.resume()) return await enterApp();
    showLogin();
  } catch (e) {
    showLogin(e instanceof Unauthorized ? "二维码已过期（电脑上的程序重启过），请重新扫码或输入口令"
                                        : "连不上电脑上的程序，请确认它还在运行");
  }
}

boot();

// 页面开着时又收到带新密钥的链接：只有 # 后面变了，浏览器不会重新加载页面。手动重新加载，按新密钥配对
window.addEventListener("hashchange", () => {
  if (keyFromFragment(location.hash)) location.reload();
});
