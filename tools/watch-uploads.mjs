// 真机测速：盯着共享文件夹，记录每个上传从开始到落盘的时间（只读，不改任何文件）。
//
// 用法：node tools/watch-uploads.mjs [共享文件夹，默认 下载\LanShare]
//
// 开始时刻 = 隐藏临时文件 .lanshare-*.part 出现（v1：收到第一批数据时建；v2：upload_begin 时建）
// 结束时刻 = 临时文件消失、同时出现一个新的正式文件（收全后改名）
// 每 100 ms 看一次，误差约 0.1 秒；手机上传的大文件（几百 MB）误差可以忽略。
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { homedir } from "node:os";

const dir = process.argv[2] || join(homedir(), "Downloads", "LanShare");
const isTemp = (n) => n.startsWith(".lanshare-") && n.endsWith(".part");
const list = () => {
  try { return readdirSync(dir); } catch { return []; }
};

let known = new Set(list().filter((n) => !isTemp(n)));
const active = new Map(); // 临时文件名 → 开始时刻
const fmt = (ms) => new Date(ms).toLocaleTimeString("zh-CN", { hour12: false });
console.log(`盯着 ${dir}（Ctrl+C 结束）`);

setInterval(() => {
  const now = Date.now();
  const names = list();
  for (const n of names.filter(isTemp)) {
    if (!active.has(n)) {
      active.set(n, now);
      console.log(`${fmt(now)}  开始收一个文件（${n}）`);
    }
  }
  const finishedTemps = [...active.keys()].filter((n) => !names.includes(n));
  const fresh = names.filter((n) => !isTemp(n) && !known.has(n));
  for (const name of fresh) {
    known.add(name);
    const temp = finishedTemps.shift();
    if (!temp) continue; // 不是经由上传来的（比如在资源管理器里拷进来的）
    const started = active.get(temp);
    active.delete(temp);
    let size = 0;
    try { size = statSync(join(dir, name)).size; } catch { /* 已被挪走 */ }
    const secs = (now - started) / 1000;
    const mib = size / 2 ** 20;
    console.log(`${fmt(now)}  收完：${name}  ${mib.toFixed(1)} MiB，用时 ${secs.toFixed(1)} 秒，平均 ${(mib / secs).toFixed(1)} MiB/s`);
  }
  for (const n of finishedTemps) active.delete(n); // 放弃的上传
}, 100);
